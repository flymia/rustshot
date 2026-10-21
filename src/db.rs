//! Database access through sqlx's `Any` driver, so the same code runs on
//! SQLite (single node) and PostgreSQL (multiple app instances).
//!
//! Portability rules for every query in here:
//! - placeholders are `$1, $2, ...` (understood by both backends)
//! - only TEXT and BIGINT columns, no booleans or native timestamps
//! - no aggregates whose result type differs per backend (e.g. SUM)

use std::time::Duration;

use serde::Serialize;
use sqlx::any::{AnyPoolOptions, install_default_drivers};
use sqlx::{Any, AnyPool, FromRow, Transaction};

pub type Db = AnyPool;

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_USER: &str = "user";

/// `last_used_at` of an API key is refreshed at most this often, to avoid
/// one write per request.
const KEY_TOUCH_INTERVAL_SECS: i64 = 60;

pub async fn connect(url: &str, max_connections: u32) -> anyhow::Result<Db> {
    install_default_drivers();
    let is_sqlite = url.starts_with("sqlite:");
    let pool = AnyPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await?;
    if is_sqlite {
        // WAL lets readers proceed while an upload is being written.
        // The setting is persisted in the database file.
        sqlx::query("PRAGMA journal_mode = WAL").execute(&pool).await?;
    }
    Ok(pool)
}

pub async fn migrate(db: &Db) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(db).await?;
    Ok(())
}

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(e) if e.is_unique_violation())
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub role: String,
    pub quota_bytes: Option<i64>,
    pub quota_uploads: Option<i64>,
    pub used_bytes: i64,
    pub upload_count: i64,
    pub created_at: i64,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == ROLE_ADMIN
    }
}

const USER_COLUMNS: &str =
    "id, username, role, quota_bytes, quota_uploads, used_bytes, upload_count, created_at";

pub struct NewUser<'a> {
    pub username: &'a str,
    pub role: &'a str,
    pub quota_bytes: Option<i64>,
    pub quota_uploads: Option<i64>,
}

pub async fn create_user(db: &Db, new: NewUser<'_>) -> sqlx::Result<User> {
    let user = User {
        id: crate::ids::new_id(),
        username: new.username.to_string(),
        role: new.role.to_string(),
        quota_bytes: new.quota_bytes,
        quota_uploads: new.quota_uploads,
        used_bytes: 0,
        upload_count: 0,
        created_at: now(),
    };
    sqlx::query(
        "INSERT INTO users (id, username, role, quota_bytes, quota_uploads, used_bytes, upload_count, created_at) \
         VALUES ($1, $2, $3, $4, $5, 0, 0, $6)",
    )
    .bind(&user.id)
    .bind(&user.username)
    .bind(&user.role)
    .bind(user.quota_bytes)
    .bind(user.quota_uploads)
    .bind(user.created_at)
    .execute(db)
    .await?;
    Ok(user)
}

pub async fn get_user(db: &Db, id: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn get_user_by_name(db: &Db, username: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users WHERE username = $1"
    )))
    .bind(username)
    .fetch_optional(db)
    .await
}

pub async fn list_users(db: &Db) -> sqlx::Result<Vec<User>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY username"
    )))
    .fetch_all(db)
    .await
}

/// Partial update. `Some(None)` clears a quota (= unlimited).
#[derive(Debug, Default)]
pub struct UserUpdate {
    pub role: Option<String>,
    pub quota_bytes: Option<Option<i64>>,
    pub quota_uploads: Option<Option<i64>>,
}

pub async fn update_user(db: &Db, id: &str, update: UserUpdate) -> sqlx::Result<Option<User>> {
    let mut tx = db.begin().await?;
    if let Some(role) = &update.role {
        sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
            .bind(role)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(quota) = update.quota_bytes {
        sqlx::query("UPDATE users SET quota_bytes = $1 WHERE id = $2")
            .bind(quota)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(quota) = update.quota_uploads {
        sqlx::query("UPDATE users SET quota_uploads = $1 WHERE id = $2")
            .bind(quota)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    get_user(db, id).await
}

/// Deletes the user row; API keys and upload rows go with it (ON DELETE CASCADE).
pub async fn delete_user(db: &Db, id: &str) -> sqlx::Result<bool> {
    let mut tx = db.begin().await?;
    // Explicit deletes so this does not depend on SQLite's foreign_keys pragma.
    sqlx::query("DELETE FROM uploads WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM api_keys WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let deleted = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(deleted.rows_affected() > 0)
}

/// Atomically reserves quota for one upload of `bytes`. Returns `false` if
/// that would exceed the user's quota. Doing check and increment in a single
/// statement makes concurrent uploads safe on every backend.
pub async fn reserve_quota(tx: &mut Transaction<'_, Any>, user_id: &str, bytes: i64) -> sqlx::Result<bool> {
    let res = sqlx::query(
        "UPDATE users SET used_bytes = used_bytes + $1, upload_count = upload_count + 1 \
         WHERE id = $2 \
           AND (quota_bytes IS NULL OR used_bytes + $1 <= quota_bytes) \
           AND (quota_uploads IS NULL OR upload_count + 1 <= quota_uploads)",
    )
    .bind(bytes)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected() == 1)
}

async fn release_quota(tx: &mut Transaction<'_, Any>, user_id: &str, bytes: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE users SET used_bytes = used_bytes - $1, upload_count = upload_count - 1 WHERE id = $2",
    )
    .bind(bytes)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ApiKey {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// Creates a key and returns the record plus the secret token (shown once).
pub async fn create_api_key(db: &Db, user_id: &str, name: &str) -> sqlx::Result<(ApiKey, String)> {
    let secret = crate::ids::new_api_key();
    let key = ApiKey {
        id: crate::ids::new_id(),
        user_id: user_id.to_string(),
        name: name.to_string(),
        prefix: secret.prefix,
        created_at: now(),
        last_used_at: None,
        revoked_at: None,
    };
    sqlx::query(
        "INSERT INTO api_keys (id, user_id, name, prefix, key_hash, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&key.id)
    .bind(&key.user_id)
    .bind(&key.name)
    .bind(&key.prefix)
    .bind(&secret.hash)
    .bind(key.created_at)
    .execute(db)
    .await?;
    Ok((key, secret.token))
}

pub async fn list_api_keys(db: &Db, user_id: &str) -> sqlx::Result<Vec<ApiKey>> {
    sqlx::query_as(
        "SELECT id, user_id, name, prefix, created_at, last_used_at, revoked_at \
         FROM api_keys WHERE user_id = $1 ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Revokes a key. With `user_id` set, only keys of that user are affected.
pub async fn revoke_api_key(db: &Db, key_id: &str, user_id: Option<&str>) -> sqlx::Result<bool> {
    let res =
        match user_id {
            Some(user_id) => sqlx::query(
                "UPDATE api_keys SET revoked_at = $1 WHERE id = $2 AND user_id = $3 AND revoked_at IS NULL",
            )
            .bind(now())
            .bind(key_id)
            .bind(user_id)
            .execute(db)
            .await?,
            None => {
                sqlx::query("UPDATE api_keys SET revoked_at = $1 WHERE id = $2 AND revoked_at IS NULL")
                    .bind(now())
                    .bind(key_id)
                    .execute(db)
                    .await?
            }
        };
    Ok(res.rows_affected() > 0)
}

#[derive(FromRow)]
struct KeyLookup {
    key_id: String,
    last_used_at: Option<i64>,
    #[sqlx(flatten)]
    user: User,
}

/// Resolves a bearer token to its (non-revoked) key ID and owner.
pub async fn authenticate(db: &Db, token: &str) -> sqlx::Result<Option<(String, User)>> {
    let hash = crate::ids::hash_api_key(token);
    let found: Option<KeyLookup> = sqlx::query_as(
        "SELECT k.id AS key_id, k.last_used_at, \
                u.id, u.username, u.role, u.quota_bytes, u.quota_uploads, u.used_bytes, u.upload_count, u.created_at \
         FROM api_keys k JOIN users u ON u.id = k.user_id \
         WHERE k.key_hash = $1 AND k.revoked_at IS NULL",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await?;
    let Some(found) = found else { return Ok(None) };

    let now = now();
    if found
        .last_used_at
        .is_none_or(|t| now - t >= KEY_TOUCH_INTERVAL_SECS)
    {
        sqlx::query("UPDATE api_keys SET last_used_at = $1 WHERE id = $2")
            .bind(now)
            .bind(&found.key_id)
            .execute(db)
            .await?;
    }
    Ok(Some((found.key_id, found.user)))
}

// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct Upload {
    pub id: String,
    pub user_id: String,
    pub slug: String,
    pub ext: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub width: i64,
    pub height: i64,
    pub sha256: String,
    pub storage_path: String,
    pub original_name: Option<String>,
    pub created_at: i64,
}

impl Upload {
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.slug, self.ext)
    }
}

const UPLOAD_COLUMNS: &str = "id, user_id, slug, ext, content_type, size_bytes, width, height, \
                              sha256, storage_path, original_name, created_at";

pub async fn insert_upload(tx: &mut Transaction<'_, Any>, upload: &Upload) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO uploads (id, user_id, slug, ext, content_type, size_bytes, width, height, \
                              sha256, storage_path, original_name, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(&upload.id)
    .bind(&upload.user_id)
    .bind(&upload.slug)
    .bind(&upload.ext)
    .bind(&upload.content_type)
    .bind(upload.size_bytes)
    .bind(upload.width)
    .bind(upload.height)
    .bind(&upload.sha256)
    .bind(&upload.storage_path)
    .bind(&upload.original_name)
    .bind(upload.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn get_upload_by_slug(db: &Db, slug: &str) -> sqlx::Result<Option<Upload>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {UPLOAD_COLUMNS} FROM uploads WHERE slug = $1"
    )))
    .bind(slug)
    .fetch_optional(db)
    .await
}

pub async fn get_upload(db: &Db, id: &str, user_id: &str) -> sqlx::Result<Option<Upload>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {UPLOAD_COLUMNS} FROM uploads WHERE id = $1 AND user_id = $2"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

pub async fn find_upload_by_hash(db: &Db, user_id: &str, sha256: &str) -> sqlx::Result<Option<Upload>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {UPLOAD_COLUMNS} FROM uploads WHERE user_id = $1 AND sha256 = $2 \
         ORDER BY created_at LIMIT 1"
    )))
    .bind(user_id)
    .bind(sha256)
    .fetch_optional(db)
    .await
}

/// Newest first. Ties on `created_at` are broken by ID for stable paging.
pub async fn list_uploads(db: &Db, user_id: &str, limit: i64, offset: i64) -> sqlx::Result<Vec<Upload>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {UPLOAD_COLUMNS} FROM uploads WHERE user_id = $1 \
         ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3"
    )))
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

/// Deletes the row and gives the quota back. Returns the deleted upload.
pub async fn delete_upload(db: &Db, id: &str, user_id: &str) -> sqlx::Result<Option<Upload>> {
    let mut tx = db.begin().await?;
    let upload: Option<Upload> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM uploads WHERE id = $1 AND user_id = $2 RETURNING {UPLOAD_COLUMNS}"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(upload) = &upload {
        release_quota(&mut tx, user_id, upload.size_bytes).await?;
    }
    tx.commit().await?;
    Ok(upload)
}
