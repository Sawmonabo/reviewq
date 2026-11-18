use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_protocol::{ErrorCode, ErrorDto, Failure};

pub(super) struct ApiError(pub(super) AppError);

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let error = self.0;
        let (status, code) = match error {
            AppError::InvalidRequest => (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized),
            AppError::Forbidden => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
            AppError::NotFound => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            AppError::Conflict => (StatusCode::CONFLICT, ErrorCode::Conflict),
            AppError::StaleRevision => (StatusCode::CONFLICT, ErrorCode::StaleRevision),
            AppError::IdempotencyConflict => (StatusCode::CONFLICT, ErrorCode::IdempotencyConflict),
            AppError::CursorExpired => (StatusCode::CONFLICT, ErrorCode::CursorExpired),
            AppError::PreconditionFailed => (
                StatusCode::PRECONDITION_FAILED,
                ErrorCode::PreconditionFailed,
            ),
            AppError::PayloadTooLarge => {
                (StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::PayloadTooLarge)
            }
            AppError::RangeNotSatisfiable => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                ErrorCode::RangeNotSatisfiable,
            ),
            AppError::Busy => (StatusCode::TOO_MANY_REQUESTS, ErrorCode::Busy),
            AppError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable),
            AppError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal),
        };
        (
            status,
            Json(Failure {
                cursor: None,
                error: ErrorDto {
                    code,
                    message: error.to_string(),
                    retryable: matches!(error, AppError::Busy | AppError::Unavailable),
                },
            }),
        )
            .into_response()
    }
}
