//! Filesystem storage for screenshots.
//!
//! Layout below the storage root:
//!
//! ```text
//! <root>/<user_id>/<YYYY>/<MM>/<slug>.<ext>
//! <root>/.tmp/<uuid>            in-flight uploads, renamed into place
//! ```
//!
//! The database stores the path relative to the root, so the root can be
//! moved (or mounted elsewhere) without touching any rows.

use std::io;
use std::path::{Component, Path, PathBuf};

use time::OffsetDateTime;
use tokio::fs;
use tokio::io::AsyncWriteExt;

const TMP_DIR: &str = ".tmp";

#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
}

/// A fully written file waiting to be moved to its final location.
/// Dropping it without calling [`Storage::persist`] deletes the file.
pub struct TempFile {
    path: PathBuf,
    persisted: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Storage {
    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join(TMP_DIR)).await?;
        let root = fs::canonicalize(&root).await?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn relative_path(user_id: &str, created_at: OffsetDateTime, slug: &str, ext: &str) -> String {
        format!(
            "{user_id}/{:04}/{:02}/{slug}.{ext}",
            created_at.year(),
            created_at.month() as u8
        )
    }

    /// Resolves a stored relative path, refusing anything that could escape the root.
    pub fn absolute(&self, relative: &str) -> io::Result<PathBuf> {
        let rel = Path::new(relative);
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid storage path: {relative:?}"),
            ));
        }
        Ok(self.root.join(rel))
    }

    /// Writes `data` to a temporary file inside the storage root (same
    /// filesystem as the final location, so the later rename is atomic).
    pub async fn write_temp(&self, data: &[u8]) -> io::Result<TempFile> {
        let path = self.root.join(TMP_DIR).join(uuid::Uuid::new_v4().to_string());
        let temp = TempFile {
            path,
            persisted: false,
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp.path)
            .await?;
        file.write_all(data).await?;
        file.sync_all().await?;
        Ok(temp)
    }

    /// Health probe: creates and removes an empty temp file, which catches a
    /// missing, read-only or full volume.
    pub async fn check_writable(&self) -> io::Result<()> {
        self.write_temp(&[]).await.map(drop)
    }

    /// Moves a temp file to `relative`. Fails if the target already exists.
    pub async fn persist(&self, mut temp: TempFile, relative: &str) -> io::Result<()> {
        let target = self.absolute(relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await?;
        }
        if fs::try_exists(&target).await? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("storage path already taken: {relative}"),
            ));
        }
        fs::rename(&temp.path, &target).await?;
        temp.persisted = true;
        Ok(())
    }

    /// Deletes a stored file; a file that is already gone is not an error.
    pub async fn delete(&self, relative: &str) -> io::Result<()> {
        match fs::remove_file(self.absolute(relative)?).await {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Deletes every file of a user.
    pub async fn delete_user_dir(&self, user_id: &str) -> io::Result<()> {
        match fs::remove_dir_all(self.absolute(user_id)?).await {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_persist_delete() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let rel = "u1/2026/10/abc.png";

        let temp = storage.write_temp(b"data").await.unwrap();
        storage.persist(temp, rel).await.unwrap();
        assert_eq!(fs::read(storage.absolute(rel).unwrap()).await.unwrap(), b"data");

        let temp = storage.write_temp(b"other").await.unwrap();
        let temp_path = temp.path.clone();
        assert!(storage.persist(temp, rel).await.is_err());
        assert!(!temp_path.exists(), "failed persist must clean up the temp file");

        storage.delete(rel).await.unwrap();
        storage.delete(rel).await.unwrap();
    }

    #[test]
    fn rejects_traversal() {
        let storage = Storage {
            root: PathBuf::from("/srv/rustshot"),
        };
        assert!(storage.absolute("../etc/passwd").is_err());
        assert!(storage.absolute("/etc/passwd").is_err());
        assert!(storage.absolute("u/2026/01/x.png").is_ok());
    }

    #[test]
    fn layout() {
        let at = OffsetDateTime::from_unix_timestamp(1_761_000_000).unwrap();
        assert_eq!(
            Storage::relative_path("u1", at, "slug", "png"),
            "u1/2025/10/slug.png"
        );
    }
}
