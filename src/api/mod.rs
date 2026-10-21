//! HTTP API. All `/api/v1` routes require `Authorization: Bearer <api key>`,
//! `/share/{slug}.{ext}` is public.

mod admin;
mod auth;
mod error;
mod keys;
mod share;
mod uploads;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method, header};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Serialize;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::db::{ApiKey, Upload, User};
use crate::service::Ctx;

pub use error::ApiError;

#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Maximum size of a single screenshot in bytes.
    pub max_upload_bytes: usize,
    /// Origins allowed to call the API from a browser (e.g. a web UI).
    pub cors_origins: Vec<String>,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub ctx: Ctx,
    pub config: ApiConfig,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// Multipart framing overhead allowed on top of the file size limit.
const MULTIPART_OVERHEAD: usize = 64 * 1024;

pub fn router(ctx: Ctx, config: ApiConfig) -> Router {
    let body_limit = config.max_upload_bytes + MULTIPART_OVERHEAD;
    let cors = cors_layer(&config.cors_origins);
    let state = AppState(Arc::new(Inner { ctx, config }));

    let api = Router::new()
        .route("/me", get(uploads::me))
        .route(
            "/uploads",
            get(uploads::list)
                .post(uploads::create)
                .layer(DefaultBodyLimit::max(body_limit)),
        )
        .route("/uploads/{id}", get(uploads::get).delete(uploads::delete))
        .route("/keys", get(keys::list).post(keys::create))
        .route("/keys/{id}", delete(keys::revoke))
        .route("/sharex", get(keys::sharex_config))
        .route("/admin/users", get(admin::list_users).post(admin::create_user))
        .route(
            "/admin/users/{id}",
            get(admin::get_user)
                .patch(admin::update_user)
                .delete(admin::delete_user),
        )
        .route(
            "/admin/users/{id}/keys",
            get(admin::list_keys).post(admin::create_key),
        )
        .route("/admin/users/{id}/keys/{key_id}", delete(admin::revoke_key))
        .fallback(|| async { ApiError::NotFound })
        .layer(cors);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/share/{file}", get(share::serve))
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

fn cors_layer(origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

// ---------------------------------------------------------------------------
// Response views
// ---------------------------------------------------------------------------

fn rfc3339(ts: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

#[derive(Serialize)]
pub struct UserView {
    pub id: String,
    pub username: String,
    pub role: String,
    pub quota_bytes: Option<i64>,
    pub quota_uploads: Option<i64>,
    pub used_bytes: i64,
    pub upload_count: i64,
    pub created_at: String,
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            username: u.username,
            role: u.role,
            quota_bytes: u.quota_bytes,
            quota_uploads: u.quota_uploads,
            used_bytes: u.used_bytes,
            upload_count: u.upload_count,
            created_at: rfc3339(u.created_at),
        }
    }
}

#[derive(Serialize)]
pub struct KeyView {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl From<ApiKey> for KeyView {
    fn from(k: ApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            prefix: k.prefix,
            created_at: rfc3339(k.created_at),
            last_used_at: k.last_used_at.map(rfc3339),
            revoked_at: k.revoked_at.map(rfc3339),
        }
    }
}

/// A freshly created key; the only response that ever contains the secret.
#[derive(Serialize)]
pub struct NewKeyView {
    #[serde(flatten)]
    pub key: KeyView,
    pub token: String,
}

#[derive(Serialize)]
pub struct UploadView {
    pub id: String,
    pub url: String,
    pub file_name: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub width: i64,
    pub height: i64,
    pub sha256: String,
    pub original_name: Option<String>,
    pub created_at: String,
}

impl UploadView {
    pub fn new(ctx: &Ctx, u: Upload) -> Self {
        Self {
            url: ctx.share_url(&u),
            file_name: u.file_name(),
            id: u.id,
            content_type: u.content_type,
            size_bytes: u.size_bytes,
            width: u.width,
            height: u.height,
            sha256: u.sha256,
            original_name: u.original_name,
            created_at: rfc3339(u.created_at),
        }
    }
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;
