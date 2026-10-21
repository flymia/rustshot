use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::{ApiError, AppState};
use crate::{db, ids};

/// `GET /share/{slug}.{ext}` - public, unauthenticated.
///
/// Files are immutable once uploaded, so they are served with long-lived
/// cache headers. `ServeFile` handles conditional and range requests.
pub async fn serve(
    State(state): State<AppState>,
    Path(file): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let (slug, ext) = file.rsplit_once('.').ok_or(ApiError::NotFound)?;
    if !ids::is_valid_slug(slug) {
        return Err(ApiError::NotFound);
    }
    let upload = db::get_upload_by_slug(&state.ctx.db, slug)
        .await?
        .filter(|u| u.ext == ext)
        .ok_or(ApiError::NotFound)?;
    let path = state
        .ctx
        .storage
        .absolute(&upload.storage_path)
        .map_err(|e| ApiError::Internal(e.into()))?;

    let mut res = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .map(Body::new);
    if res.status() == axum::http::StatusCode::NOT_FOUND {
        tracing::error!(path = %upload.storage_path, "file of existing upload is missing");
        return Ok(ApiError::NotFound.into_response());
    }
    tracing::debug!(file = %file, status = %res.status(), "share served");
    let headers = res.headers_mut();
    if let Ok(ct) = HeaderValue::from_str(&upload.content_type) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'"),
    );
    Ok(res)
}
