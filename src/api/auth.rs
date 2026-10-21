use axum::extract::{FromRequestParts, OriginalUri};
use axum::http::header;
use axum::http::request::Parts;

use super::{ApiError, AppState};
use crate::db::{self, User};

/// The authenticated caller, resolved from `Authorization: Bearer <key>`.
pub struct Auth {
    pub user: User,
    /// The raw token of the current request (used to prefill ShareX configs).
    pub token: String,
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                let (scheme, token) = v.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
            })
            .filter(|t| !t.is_empty())
            .ok_or(ApiError::Unauthorized)?;
        let Some((_key_id, user)) = db::authenticate(&state.ctx.db, token).await? else {
            tracing::warn!(path = %request_path(parts), "rejected unknown or revoked API key");
            return Err(ApiError::Unauthorized);
        };
        Ok(Auth {
            user,
            token: token.to_string(),
        })
    }
}

/// An authenticated caller with the admin role.
pub struct Admin(pub Auth);

/// The full request path; `parts.uri` lacks the `/api/v1` prefix inside the nested router.
fn request_path(parts: &Parts) -> &str {
    parts
        .extensions
        .get::<OriginalUri>()
        .map_or(parts.uri.path(), |u| u.0.path())
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let auth = Auth::from_request_parts(parts, state).await?;
        if !auth.user.is_admin() {
            tracing::warn!(user = %auth.user.username, path = %request_path(parts), "non-admin denied admin route");
            return Err(ApiError::Forbidden);
        }
        Ok(Admin(auth))
    }
}
