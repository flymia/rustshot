use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use serde::{Deserialize, Serialize};

use super::auth::Auth;
use super::{ApiError, ApiResult, AppState, UploadView, UserView};
use crate::db;
use crate::service::{NewScreenshot, Stored, UploadError};

pub async fn me(auth: Auth) -> Json<UserView> {
    Json(auth.user.into())
}

/// `POST /api/v1/uploads`
///
/// Accepts either `multipart/form-data` (the first part carrying a file,
/// conventionally named `file` - this is what ShareX sends) or the raw image
/// as request body, optionally naming it via the `X-Filename` header.
///
/// Re-uploading content the user already has returns that upload with `200`.
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    req: Request,
) -> Result<(StatusCode, Json<UploadView>), ApiError> {
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/form-data"));

    let (data, original_name) = if is_multipart {
        read_multipart(Multipart::from_request(req, &state).await.map_err(bad_request)?).await?
    } else {
        let name = req
            .headers()
            .get("x-filename")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let data = Bytes::from_request(req, &state).await.map_err(too_large_or_bad)?;
        (data, name)
    };

    let max = state.config.max_upload_bytes;
    if data.len() > max {
        return Err(UploadError::TooLarge {
            size: data.len(),
            max,
        }
        .into());
    }

    let original_name = original_name.map(sanitize_name).filter(|n| !n.is_empty());
    let stored = state
        .ctx
        .store_screenshot(
            &auth.user,
            NewScreenshot {
                data: &data,
                original_name,
                created_at: None,
            },
        )
        .await?;
    let status = match stored {
        Stored::Created(_) => StatusCode::CREATED,
        Stored::Existing(_) => StatusCode::OK,
    };
    Ok((status, Json(UploadView::new(&state.ctx, stored.into_upload()))))
}

async fn read_multipart(mut multipart: Multipart) -> Result<(Bytes, Option<String>), ApiError> {
    while let Some(field) = multipart.next_field().await.map_err(too_large_or_bad)? {
        if field.file_name().is_none() && field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().map(str::to_string);
        let data = field.bytes().await.map_err(too_large_or_bad)?;
        return Ok((data, name));
    }
    Err(ApiError::BadRequest("multipart body contains no file".into()))
}

fn bad_request(e: impl std::fmt::Display) -> ApiError {
    ApiError::BadRequest(e.to_string())
}

/// Body read failures are mostly the size limit kicking in.
fn too_large_or_bad<E: std::fmt::Display + HasStatus>(e: E) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::PayloadTooLarge("file too large".into())
    } else {
        ApiError::BadRequest(e.to_string())
    }
}

trait HasStatus {
    fn status(&self) -> StatusCode;
}

impl HasStatus for axum::extract::multipart::MultipartError {
    fn status(&self) -> StatusCode {
        self.status()
    }
}

impl HasStatus for axum::extract::rejection::BytesRejection {
    fn status(&self) -> StatusCode {
        self.status()
    }
}

/// Keeps only the final path component and strips control characters.
fn sanitize_name(name: String) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    base.chars()
        .filter(|c| !c.is_control())
        .take(255)
        .collect::<String>()
        .trim()
        .to_string()
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct UploadList {
    items: Vec<UploadView>,
    total: i64,
    limit: i64,
    offset: i64,
}

pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListQuery>,
) -> ApiResult<UploadList> {
    let limit = q.limit.clamp(1, 500);
    let offset = q.offset.max(0);
    let uploads = db::list_uploads(&state.ctx.db, &auth.user.id, limit, offset).await?;
    Ok(Json(UploadList {
        items: uploads
            .into_iter()
            .map(|u| UploadView::new(&state.ctx, u))
            .collect(),
        total: auth.user.upload_count,
        limit,
        offset,
    }))
}

pub async fn get(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> ApiResult<UploadView> {
    let upload = db::get_upload(&state.ctx.db, &id, &auth.user.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(UploadView::new(&state.ctx, upload)))
}

pub async fn delete(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .ctx
        .delete_upload(&id, &auth.user.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::sanitize_name;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("../../etc/passwd".into()), "passwd");
        assert_eq!(sanitize_name("C:\\Users\\me\\shot.png".into()), "shot.png");
        assert_eq!(sanitize_name("a\nb.png".into()), "ab.png");
    }
}
