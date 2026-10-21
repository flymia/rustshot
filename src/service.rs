//! Operations shared by the HTTP API and the CLI.

use time::OffsetDateTime;

use crate::db::{self, Db, Upload, User};
use crate::image::{self, ImageError};
use crate::storage::Storage;

/// Everything needed to perform operations; cheap to clone.
#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
    pub storage: Storage,
    /// Base URL used to build share links, without trailing slash.
    pub public_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("file too large ({size} bytes, maximum is {max})")]
    TooLarge { size: usize, max: usize },
    #[error("upload quota exceeded")]
    QuotaExceeded,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not allocate a unique link")]
    SlugExhausted,
}

pub struct NewScreenshot<'a> {
    pub data: &'a [u8],
    pub original_name: Option<String>,
    /// Defaults to now. The CLI import uses the file's modification time.
    pub created_at: Option<OffsetDateTime>,
}

const SLUG_ATTEMPTS: usize = 5;

impl Ctx {
    pub fn share_url(&self, upload: &Upload) -> String {
        format!("{}/share/{}", self.public_url, upload.file_name())
    }

    /// Validates, stores and records a screenshot, charging the user's quota.
    ///
    /// Ordering guarantees that a committed row always has its file:
    /// the file is written to a temp location first, then inside one DB
    /// transaction quota is reserved, the row inserted and the file moved
    /// into place - only then is the transaction committed.
    pub async fn store_screenshot(&self, user: &User, new: NewScreenshot<'_>) -> Result<Upload, UploadError> {
        let info = image::inspect(new.data)?;
        let size = new.data.len() as i64;
        let created_at = new.created_at.unwrap_or_else(OffsetDateTime::now_utc);
        let sha256 = crate::ids::sha256_hex(new.data);
        let temp = self.storage.write_temp(new.data).await?;

        let mut temp = Some(temp);
        for _ in 0..SLUG_ATTEMPTS {
            let slug = crate::ids::new_slug();
            let ext = info.format.ext();
            let upload = Upload {
                id: crate::ids::new_id(),
                user_id: user.id.clone(),
                storage_path: Storage::relative_path(&user.id, created_at, &slug, ext),
                slug,
                ext: ext.to_string(),
                content_type: info.format.content_type().to_string(),
                size_bytes: size,
                width: info.width.into(),
                height: info.height.into(),
                sha256: sha256.clone(),
                original_name: new.original_name.clone(),
                created_at: created_at.unix_timestamp(),
            };

            let mut tx = self.db.begin().await?;
            if !db::reserve_quota(&mut tx, &user.id, size).await? {
                return Err(UploadError::QuotaExceeded);
            }
            match db::insert_upload(&mut tx, &upload).await {
                Ok(()) => {}
                // Practically impossible with 134 random bits, but handled anyway.
                Err(e) if db::is_unique_violation(&e) => continue,
                Err(e) => return Err(e.into()),
            }
            self.storage
                .persist(
                    temp.take().expect("temp file consumed once"),
                    &upload.storage_path,
                )
                .await?;
            if let Err(e) = tx.commit().await {
                let _ = self.storage.delete(&upload.storage_path).await;
                return Err(e.into());
            }
            return Ok(upload);
        }
        Err(UploadError::SlugExhausted)
    }

    /// Deletes an upload of `user_id` (row, quota and file).
    pub async fn delete_upload(&self, id: &str, user_id: &str) -> anyhow::Result<Option<Upload>> {
        let Some(upload) = db::delete_upload(&self.db, id, user_id).await? else {
            return Ok(None);
        };
        if let Err(e) = self.storage.delete(&upload.storage_path).await {
            // The row is gone, so the file is unreachable; just leave a trace.
            tracing::warn!(path = %upload.storage_path, error = %e, "failed to delete file");
        }
        Ok(Some(upload))
    }

    /// Deletes a user with all keys, uploads and files.
    pub async fn delete_user(&self, user_id: &str) -> anyhow::Result<bool> {
        if !db::delete_user(&self.db, user_id).await? {
            return Ok(false);
        }
        self.storage.delete_user_dir(user_id).await?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Input validation shared by API and CLI
// ---------------------------------------------------------------------------

/// Usernames: 1-64 chars of `a-z 0-9 . _ -`, starting with a letter or digit.
pub fn validate_username(name: &str) -> Result<&str, String> {
    let valid = (1..=64).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if valid {
        Ok(name)
    } else {
        Err(
            "username must be 1-64 characters of a-z, 0-9, '.', '_' or '-' and start with a letter or digit"
                .into(),
        )
    }
}

pub fn validate_role(role: &str) -> Result<&str, String> {
    match role {
        db::ROLE_ADMIN | db::ROLE_USER => Ok(role),
        _ => Err(format!(
            "role must be '{}' or '{}'",
            db::ROLE_ADMIN,
            db::ROLE_USER
        )),
    }
}

pub fn validate_quota(quota: Option<i64>) -> Result<Option<i64>, String> {
    match quota {
        Some(q) if q < 0 => Err("quota must not be negative".into()),
        q => Ok(q),
    }
}
