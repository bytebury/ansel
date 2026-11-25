//! JSON error responses.

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "image not found")
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, message)
    }

    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    /// Storage failures are logged in full but reported generically, so
    /// bucket names and endpoints never reach clients.
    pub fn storage(detail: impl std::fmt::Display) -> Self {
        tracing::error!("storage error: {detail}");
        Self::new(StatusCode::BAD_GATEWAY, "storage backend error")
    }

    pub fn internal(detail: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {detail}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.message }));
        if self.status == StatusCode::UNAUTHORIZED {
            (self.status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response()
        } else {
            (self.status, body).into_response()
        }
    }
}
