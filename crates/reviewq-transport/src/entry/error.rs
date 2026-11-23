use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_protocol::{ErrorCode, ErrorDto, Failure};
use serde::de::DeserializeOwned;

pub(super) enum ApiError {
    Application(AppError),
    // The proof gate rejected the request before performing the operation.
    BrowserProofRejected,
}

pub(super) struct ApiPath<T>(pub(super) T);
pub(super) struct ApiQuery<T>(pub(super) T);
pub(super) struct ApiBytes(pub(super) Bytes);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for ApiPath<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| AppError::InvalidRequest.into())
    }
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| AppError::InvalidRequest.into())
    }
}

impl<S: Send + Sync> FromRequest<S> for ApiBytes {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Bytes::from_request(request, state)
            .await
            .map(Self)
            .map_err(|error| {
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    AppError::PayloadTooLarge
                } else {
                    AppError::InvalidRequest
                }
                .into()
            })
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        Self::Application(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Application(error) => {
                let (status, failure) = failure(error);
                (status, Json(failure)).into_response()
            }
            Self::BrowserProofRejected => {
                let (status, failure) = failure(AppError::Forbidden);
                (status, [("x-reviewq-csrf-rejected", "1")], Json(failure)).into_response()
            }
        }
    }
}

pub(super) fn failure(error: AppError) -> (StatusCode, Failure) {
    let (status, code) = match error {
        AppError::InvalidRequest => (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest),
        AppError::TicketUnverified => (
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::TicketUnverified,
        ),
        AppError::ReportCorrectionRequired => {
            (StatusCode::UNPROCESSABLE_ENTITY, ErrorCode::InvalidRequest)
        }
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
        AppError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::PayloadTooLarge),
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
        Failure {
            cursor: None,
            error: ErrorDto {
                code,
                message: error.to_string(),
                retryable: matches!(error, AppError::Busy | AppError::Unavailable),
            },
        },
    )
}
