use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub struct ApiError(pub StatusCode, pub String);
impl ApiError {
    pub fn bad_request(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::BAD_REQUEST, error.to_string())
    }
    pub fn unauthorized(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::UNAUTHORIZED, error.to_string())
    }
    pub fn forbidden(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::FORBIDDEN, error.to_string())
    }
    pub fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "resource not found".into())
    }
    pub fn conflict(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::CONFLICT, error.to_string())
    }
    pub fn unavailable(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
    }
    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error,"request failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal server error".into(),
        )
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        if let Some(db) = error.as_database_error() {
            if db.is_unique_violation() {
                return Self::conflict("name or request already exists");
            }
            if db.is_foreign_key_violation() {
                return Self::bad_request("referenced resource does not exist");
            }
        }
        Self::internal(error)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error":self.1}))).into_response()
    }
}
