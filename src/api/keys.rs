use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use super::auth::Auth;
use super::{ApiError, ApiResult, AppState, KeyView, NewKeyView};
use crate::db;

#[derive(Deserialize)]
pub struct CreateKey {
    pub name: String,
}

pub fn validate_key_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::BadRequest("key name must be 1-100 characters".into()));
    }
    Ok(name)
}

pub async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Vec<KeyView>> {
    let keys = db::list_api_keys(&state.ctx.db, &auth.user.id).await?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<CreateKey>,
) -> Result<(StatusCode, Json<NewKeyView>), ApiError> {
    let name = validate_key_name(&body.name)?;
    let (key, token) = db::create_api_key(&state.ctx.db, &auth.user.id, name).await?;
    tracing::info!(user = %auth.user.username, key_id = %key.id, key_name = %key.name, "API key created");
    Ok((
        StatusCode::CREATED,
        Json(NewKeyView {
            key: key.into(),
            token,
        }),
    ))
}

pub async fn revoke(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !db::revoke_api_key(&state.ctx.db, &id, Some(&auth.user.id)).await? {
        return Err(ApiError::NotFound);
    }
    tracing::info!(user = %auth.user.username, key_id = %id, "API key revoked");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/sharex` - a ready-to-import ShareX custom uploader (.sxcu)
/// using the API key of this request.
pub async fn sharex_config(State(state): State<AppState>, auth: Auth) -> impl IntoResponse {
    tracing::info!(user = %auth.user.username, "ShareX config downloaded");
    let public_url = &state.ctx.public_url;
    let host = public_url.split("://").nth(1).unwrap_or(public_url);
    let config = json!({
        "Version": "15.0.0",
        "Name": format!("rustshot ({host})"),
        "DestinationType": "ImageUploader",
        "RequestMethod": "POST",
        "RequestURL": format!("{public_url}/api/v1/uploads"),
        "Headers": { "Authorization": format!("Bearer {}", auth.token) },
        "Body": "MultipartFormData",
        "FileFormName": "file",
        "URL": "{json:url}",
        "ErrorMessage": "{json:error}",
    });
    (
        [(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"rustshot.sxcu\"",
        )],
        Json(config),
    )
}
