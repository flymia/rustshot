use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer};

use super::auth::Admin;
use super::keys::{CreateKey, validate_key_name};
use super::{ApiError, ApiResult, AppState, KeyView, NewKeyView, UserView};
use crate::db::{self, NewUser, ROLE_USER, UserUpdate};
use crate::service::{validate_quota, validate_role, validate_username};

#[derive(Deserialize)]
pub struct CreateUser {
    username: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    quota_bytes: Option<i64>,
    #[serde(default)]
    quota_uploads: Option<i64>,
}

/// Absent field = leave unchanged, `null` = unlimited.
#[derive(Deserialize)]
pub struct PatchUser {
    #[serde(default)]
    role: Option<String>,
    #[serde(default, deserialize_with = "present")]
    quota_bytes: Option<Option<i64>>,
    #[serde(default, deserialize_with = "present")]
    quota_uploads: Option<Option<i64>>,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<i64>>, D::Error> {
    Option::<i64>::deserialize(d).map(Some)
}

fn bad(e: String) -> ApiError {
    ApiError::BadRequest(e)
}

async fn load_user(state: &AppState, id: &str) -> Result<db::User, ApiError> {
    db::get_user(&state.ctx.db, id).await?.ok_or(ApiError::NotFound)
}

pub async fn list_users(State(state): State<AppState>, _: Admin) -> ApiResult<Vec<UserView>> {
    let users = db::list_users(&state.ctx.db).await?;
    Ok(Json(users.into_iter().map(Into::into).collect()))
}

pub async fn create_user(
    State(state): State<AppState>,
    _: Admin,
    Json(body): Json<CreateUser>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    let new = NewUser {
        username: validate_username(&body.username).map_err(bad)?,
        role: validate_role(body.role.as_deref().unwrap_or(ROLE_USER)).map_err(bad)?,
        quota_bytes: validate_quota(body.quota_bytes).map_err(bad)?,
        quota_uploads: validate_quota(body.quota_uploads).map_err(bad)?,
    };
    match db::create_user(&state.ctx.db, new).await {
        Ok(user) => Ok((StatusCode::CREATED, Json(user.into()))),
        Err(e) if db::is_unique_violation(&e) => Err(ApiError::Conflict("username already taken".into())),
        Err(e) => Err(e.into()),
    }
}

pub async fn get_user(
    State(state): State<AppState>,
    _: Admin,
    Path(id): Path<String>,
) -> ApiResult<UserView> {
    Ok(Json(load_user(&state, &id).await?.into()))
}

pub async fn update_user(
    State(state): State<AppState>,
    _: Admin,
    Path(id): Path<String>,
    Json(body): Json<PatchUser>,
) -> ApiResult<UserView> {
    let update = UserUpdate {
        role: body
            .role
            .as_deref()
            .map(validate_role)
            .transpose()
            .map_err(bad)?
            .map(str::to_string),
        quota_bytes: body.quota_bytes.map(validate_quota).transpose().map_err(bad)?,
        quota_uploads: body.quota_uploads.map(validate_quota).transpose().map_err(bad)?,
    };
    let user = db::update_user(&state.ctx.db, &id, update)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(user.into()))
}

pub async fn delete_user(
    State(state): State<AppState>,
    Admin(auth): Admin,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if auth.user.id == id {
        return Err(ApiError::BadRequest("refusing to delete your own account".into()));
    }
    if !state.ctx.delete_user(&id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_keys(
    State(state): State<AppState>,
    _: Admin,
    Path(id): Path<String>,
) -> ApiResult<Vec<KeyView>> {
    let user = load_user(&state, &id).await?;
    let keys = db::list_api_keys(&state.ctx.db, &user.id).await?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub async fn create_key(
    State(state): State<AppState>,
    _: Admin,
    Path(id): Path<String>,
    Json(body): Json<CreateKey>,
) -> Result<(StatusCode, Json<NewKeyView>), ApiError> {
    let user = load_user(&state, &id).await?;
    let name = validate_key_name(&body.name)?;
    let (key, token) = db::create_api_key(&state.ctx.db, &user.id, name).await?;
    Ok((
        StatusCode::CREATED,
        Json(NewKeyView {
            key: key.into(),
            token,
        }),
    ))
}

pub async fn revoke_key(
    State(state): State<AppState>,
    _: Admin,
    Path((id, key_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    if !db::revoke_api_key(&state.ctx.db, &key_id, Some(&id)).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
