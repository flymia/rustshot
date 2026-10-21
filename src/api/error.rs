use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::service::UploadError;

/// Errors are returned as `{"error": "<message>"}`.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error("admin privileges required")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    UnsupportedMediaType(String),
    #[error("upload quota exceeded")]
    QuotaExceeded,
    #[error("internal server error")]
    Internal(#[source] anyhow::Error),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            // 507 Insufficient Storage: the user's storage allowance is used up.
            ApiError::QuotaExceeded => StatusCode::INSUFFICIENT_STORAGE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::Internal(e) = &self {
            tracing::error!(error = ?e, "request failed");
        }
        let mut res = (self.status(), Json(json!({ "error": self.to_string() }))).into_response();
        if matches!(self, ApiError::Unauthorized) {
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl From<UploadError> for ApiError {
    fn from(e: UploadError) -> Self {
        match e {
            UploadError::Image(e) => ApiError::UnsupportedMediaType(e.to_string()),
            e @ UploadError::TooLarge { .. } => ApiError::PayloadTooLarge(e.to_string()),
            UploadError::QuotaExceeded => ApiError::QuotaExceeded,
            UploadError::Db(e) => ApiError::Internal(e.into()),
            UploadError::Io(e) => ApiError::Internal(e.into()),
            e @ UploadError::SlugExhausted => ApiError::Internal(e.into()),
        }
    }
}
