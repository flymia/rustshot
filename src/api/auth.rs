use axum::extract::FromRequestParts;
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
        let (_key_id, user) = db::authenticate(&state.ctx.db, token)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        Ok(Auth {
            user,
            token: token.to_string(),
        })
    }
}

/// An authenticated caller with the admin role.
pub struct Admin(pub Auth);

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let auth = Auth::from_request_parts(parts, state).await?;
        if !auth.user.is_admin() {
            return Err(ApiError::Forbidden);
        }
        Ok(Admin(auth))
    }
}
