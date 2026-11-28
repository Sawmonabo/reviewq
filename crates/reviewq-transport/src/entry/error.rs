use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::{AppError, SourceProblem};
use reviewq_protocol::field_errors::{FieldError, bounded_field_errors};
use reviewq_protocol::{ErrorCode, ErrorDto, Failure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub(super) enum ApiError {
    Application(AppError),
    // The proof gate rejected the request before performing the operation.
    BrowserProofRejected,
    // The tool input broke these schema or value rules; the agent corrects them in its turn.
    // The list is uncapped, from the validator or the app alike, and is capped once where the
    // reply is written.
    InvalidFields(Vec<FieldError>),
    // The operation was refused before anything changed, for a reason the person reads: a
    // `409` with that sentence as the error message.
    Refused(String),
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
        match error {
            AppError::InvalidFields(errors) => Self::InvalidFields(
                errors
                    .into_iter()
                    .map(|error| FieldError::new(error.path, error.message))
                    .collect(),
            ),
            error => Self::Application(error),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Application(error) => {
                let wait = retry_after(&error);
                let (status, failure) = failure(error);
                let mut response = (status, Json(failure)).into_response();
                if let Some(wait) = wait {
                    response.headers_mut().insert(header::RETRY_AFTER, wait);
                }
                response
            }
            Self::InvalidFields(errors) => field_errors(AppError::InvalidRequest, &capped(errors)),
            Self::Refused(message) => {
                let (status, failure) = refused(message);
                (status, Json(failure)).into_response()
            }
            Self::BrowserProofRejected => {
                let (status, failure) = failure(AppError::Forbidden);
                (status, [("x-reviewq-csrf-rejected", "1")], Json(failure)).into_response()
            }
        }
    }
}

fn field_errors(error: AppError, errors: &[FieldError]) -> Response {
    let (status, failure) = failure(error);
    let body = serde_json::json!({
        "cursor": failure.cursor,
        "error": failure.error,
        "fieldErrors": errors,
    });
    (status, Json(body)).into_response()
}

/// The text of an MCP tool error, which the model reads to correct its call: for refused
/// fields, `field_error_text` of the errors capped as the HTTP reply caps them; for any other
/// failure, the protocol's error object as JSON.
pub(super) fn tool_error_text(error: ApiError) -> String {
    let (_status, failure) = match error {
        ApiError::InvalidFields(errors) => return field_error_text(&capped(errors)),
        ApiError::Application(error) => failure(error),
        ApiError::BrowserProofRejected => failure(AppError::Forbidden),
        ApiError::Refused(message) => refused(message),
    };
    serde_json::to_string(&failure.error).unwrap_or(failure.error.message)
}

/// The text a model reads for a refused ledger tool call that arrived as a tool route's error
/// reply, which is how the Claude bridge receives it. It is the text `tool_error_text` gives
/// the MCP server for the same refusal. `None` when `body` is not the common error reply.
pub fn ledger_refusal_text(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Refusal {
        error: Value,
        field_errors: Option<Vec<FieldError>>,
    }
    let refusal: Refusal = serde_json::from_slice(body).ok()?;
    match refusal.field_errors {
        Some(errors) => Some(field_error_text(&errors)),
        None => serde_json::to_string(&refusal.error).ok(),
    }
}

/// Refused fields as the model reads them: one line per error, `path: message`, after a line
/// that asks for the fields to be corrected. A refusal whose errors are all at the root names
/// no field, so its messages stand alone, one per line, without that first line. `errors` is
/// the list as a reply carries it, already capped.
pub fn field_error_text(errors: &[FieldError]) -> String {
    let mut lines = Vec::with_capacity(errors.len() + 1);
    if errors.is_empty() || errors.iter().any(|error| !error.path.is_empty()) {
        lines.push(format!(
            "{} Correct these fields and call again.",
            AppError::InvalidFields(Vec::new())
        ));
    }
    lines.extend(errors.iter().map(|error| {
        if error.path.is_empty() {
            error.message.clone()
        } else {
            format!("{}: {}", error.path, error.message)
        }
    }));
    lines.join("\n")
}

/// Caps a refusal's field errors exactly once, whether they came from the protocol's validator
/// or from the app: at most `MAX_FIELD_ERRORS` in field order, then one entry that counts the
/// rest.
fn capped(errors: Vec<FieldError>) -> Vec<FieldError> {
    bounded_field_errors(errors.into_iter().collect())
}

/// The common `409` conflict reply, carrying `message` in place of the generic text.
fn refused(message: String) -> (StatusCode, Failure) {
    let (status, mut failure) = failure(AppError::Conflict);
    failure.error.message = message;
    (status, failure)
}

pub(super) fn failure(error: AppError) -> (StatusCode, Failure) {
    let (status, code) = match error {
        AppError::InvalidRequest | AppError::InvalidFields(_) => {
            (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest)
        }
        AppError::TicketUnverified => (
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::TicketUnverified,
        ),
        AppError::Unauthorized => (StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized),
        AppError::Forbidden => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
        AppError::NotFound | AppError::SessionFolderTooLarge => {
            (StatusCode::NOT_FOUND, ErrorCode::NotFound)
        }
        AppError::Conflict => (StatusCode::CONFLICT, ErrorCode::Conflict),
        AppError::StaleRevision => (StatusCode::CONFLICT, ErrorCode::StaleRevision),
        AppError::IdempotencyConflict => (StatusCode::CONFLICT, ErrorCode::IdempotencyConflict),
        AppError::CursorExpired => (StatusCode::CONFLICT, ErrorCode::CursorExpired),
        AppError::PreconditionFailed => (
            StatusCode::PRECONDITION_FAILED,
            ErrorCode::PreconditionFailed,
        ),
        AppError::OutdatedTool => (StatusCode::PRECONDITION_FAILED, ErrorCode::OutdatedTool),
        AppError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::PayloadTooLarge),
        AppError::RangeNotSatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            ErrorCode::RangeNotSatisfiable,
        ),
        AppError::Busy => (StatusCode::TOO_MANY_REQUESTS, ErrorCode::Busy),
        AppError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable),
        AppError::Source(problem) => source_status(problem),
        AppError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal),
    };
    (
        status,
        Failure {
            cursor: None,
            error: ErrorDto {
                code,
                message: error.to_string(),
                retryable: match error {
                    AppError::Busy | AppError::Unavailable => true,
                    AppError::Source(problem) => source_retryable(problem),
                    _ => false,
                },
            },
        },
    )
}

/// The `Retry-After` header for a reply that fails with `error`: whole seconds, rounded up,
/// when GitLab or Jira named how long ReviewQ must wait.
pub(super) fn retry_after(error: &AppError) -> Option<HeaderValue> {
    let AppError::Source(problem) = error else {
        return None;
    };
    let wait = problem.retry_after()?;
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    Some(HeaderValue::from(seconds.max(1)))
}

/// The reply status and code for a GitLab or Jira problem: an address to correct is a bad
/// request; a wait is `busy`, the service rate-limiting ReviewQ; a web page that refused the
/// request is `forbidden`, a token problem until shown otherwise; a wrong saved address, a
/// gateway page or a bot check is an unavailable service.
fn source_status(problem: SourceProblem) -> (StatusCode, ErrorCode) {
    match problem {
        SourceProblem::Address(_) => (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest),
        SourceProblem::Wait(..) => (StatusCode::TOO_MANY_REQUESTS, ErrorCode::Busy),
        SourceProblem::RefusedPage(_) | SourceProblem::Refused(_) => {
            (StatusCode::FORBIDDEN, ErrorCode::Forbidden)
        }
        SourceProblem::WrongAddress(_)
        | SourceProblem::GatewayPage
        | SourceProblem::BotCheck(_) => (StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable),
    }
}

/// Whether trying again can succeed without the person changing Settings.
fn source_retryable(problem: SourceProblem) -> bool {
    matches!(
        problem,
        SourceProblem::BotCheck(_) | SourceProblem::GatewayPage | SourceProblem::Wait(..)
    )
}
