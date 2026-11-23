use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_protocol::mcp::{
    AnswerReviewQuestion, ConfigureFollowups, GetReview, ListReviews, ReadTranscript,
    SendReviewMessage, StartReview,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ApiError, Entry};

mod gates;
mod operations;
mod report;
mod workflow;
use gates::Gates;

const MAX_TOOL_RESPONSE: usize = 512 * 1024;

#[derive(Clone)]
struct Server {
    entry: Arc<Entry>,
    tools: Arc<[Tool]>,
    gates: Arc<Gates>,
}

pub(super) fn mount(entry: Arc<Entry>) -> Router<Arc<Entry>> {
    let browser = entry.auth.endpoint.browser();
    let native = entry.auth.endpoint.native();
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts([browser.to_string(), native.to_string()])
        .enforce_origin_validation()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(160 * 1024);
    let application = Server {
        entry: entry.clone(),
        gates: Arc::new(Gates::default()),
        tools: vec![
            report::tool(),
            tool::<ListReviews>("ListReviews", "List scoped review summaries."),
            tool::<GetReview>("GetReview", "Read a registered review and its native runs."),
            tool::<ReadTranscript>(
                "ReadTranscript",
                "Read a bounded page of a native transcript.",
            ),
            tool::<SendReviewMessage>(
                "SendReviewMessage",
                "Queue an explicit message for the selected lead.",
            ),
            tool::<StartReview>("StartReview", "Start an explicitly selected native review."),
            tool::<AnswerReviewQuestion>(
                "AnswerReviewQuestion",
                "Answer one exact pending native question.",
            ),
            tool::<ConfigureFollowups>(
                "ConfigureFollowups",
                "Configure review follow-ups for an exact registered lead.",
            ),
        ]
        .into(),
    };
    let sdk = StreamableHttpService::new(
        move || Ok(application.clone()),
        NeverSessionManager::default().into(),
        config,
    );
    Router::new()
        .route_service("/mcp", sdk)
        .layer(middleware::from_fn_with_state(entry, authenticate))
}

fn tool<T: JsonSchema>(name: &'static str, description: &'static str) -> Tool {
    let schema = schemars::schema_for!(T)
        .to_value()
        .as_object()
        .cloned()
        .unwrap_or_default();
    Tool::new(name, description, schema)
}

async fn authenticate(
    State(entry): State<Arc<Entry>>,
    mut request: Request,
    next: Next,
) -> Response {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let Some(verifier) = header.and_then(verify_bearer).filter(|_| {
        request
            .headers()
            .get_all(header::AUTHORIZATION)
            .iter()
            .count()
            == 1
    }) else {
        return challenge();
    };
    match entry.store.authenticate_grant(verifier).await {
        Ok(actor) => {
            request.headers_mut().remove(header::AUTHORIZATION);
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Err(AppError::Unauthorized) => challenge(),
        Err(error) => ApiError(error).into_response(),
    }
}

fn verify_bearer(value: &str) -> Option<[u8; 32]> {
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || credential.len() != 43 {
        return None;
    }
    let mut key = [0; 32];
    let written = URL_SAFE_NO_PAD.decode_slice(credential, &mut key).ok()?;
    if written != key.len() {
        return None;
    }
    Some(Sha256::digest(key).into())
}

fn challenge() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer realm=\"ReviewQ\"")],
    )
        .into_response()
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("reviewq", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|page| page.cursor.is_some()) {
            return Err(ErrorData::invalid_params("invalid tool cursor", None));
        }
        // Claude supplies this same tool through its session-aware native hook.
        let claude = context
            .client_info()
            .is_some_and(|client| client.name == "claude-code")
            || context
                .extensions
                .get::<Parts>()
                .and_then(|parts| parts.headers.get(header::USER_AGENT))
                .and_then(|header| header.to_str().ok())
                .is_some_and(|agent| agent.starts_with("claude-code/"));
        let tools = self
            .tools
            .iter()
            .filter(|tool| !claude || tool.name != "report_handoff")
            .cloned()
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let actor = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<Actor>())
            .cloned()
            .ok_or_else(|| ErrorData::invalid_request("checkout authorization required", None))?;
        let deadline = if matches!(
            request.name.as_ref(),
            "SendReviewMessage" | "StartReview" | "ConfigureFollowups"
        ) {
            60
        } else {
            30
        };
        let cancelled = context.ct;
        let result = tokio::select! {
            _ = cancelled.cancelled() => return Err(ErrorData::internal_error("request cancelled", None)),
            result = tokio::time::timeout(Duration::from_secs(deadline), self.call(request, actor, context.meta)) => {
                result.unwrap_or(Err(AppError::Busy))
            }
        };
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                let (_status, failure) = super::error::failure(error);
                let text = serde_json::to_string(&failure.error)
                    .map_err(|_| ErrorData::internal_error("response unavailable", None))?;
                CallToolResult::error(vec![ContentBlock::text(text)])
            }
        };
        Ok(output.into())
    }
}

fn bounded_result(value: &Value) -> Result<CallToolResult, AppError> {
    let text = serde_json::to_string(value).map_err(|_| AppError::Internal)?;
    let result = CallToolResult::success(vec![ContentBlock::text(text)]);
    let encoded = serde_json::to_vec(&result).map_err(|_| AppError::Internal)?;
    if encoded.len() > MAX_TOOL_RESPONSE {
        return Err(AppError::PayloadTooLarge);
    }
    Ok(result)
}

impl Server {
    async fn call(
        &self,
        request: CallToolRequestParams,
        actor: Actor,
        meta: rmcp::model::RequestMetaObject,
    ) -> Result<CallToolResult, AppError> {
        let _permit = self.gates.enter(&actor).await?;
        let input = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            "report_handoff" => return report::submit(&self.entry, actor, &meta, input).await,
            "ListReviews" => operations::list(&self.entry, actor, arguments(input)?).await,
            "GetReview" => operations::get(&self.entry, actor, arguments(input)?).await,
            "ReadTranscript" => operations::transcript(&self.entry, actor, arguments(input)?).await,
            "SendReviewMessage" => operations::send(&self.entry, actor, arguments(input)?).await,
            "StartReview" => workflow::start(&self.entry, actor, arguments(input)?).await,
            "AnswerReviewQuestion" => workflow::answer(&self.entry, actor, arguments(input)?).await,
            "ConfigureFollowups" => {
                workflow::followups(&self.entry, actor, arguments(input)?).await
            }
            _ => Err(AppError::NotFound),
        }?;
        bounded_result(&result)
    }
}

fn arguments<T: DeserializeOwned>(object: JsonObject) -> Result<T, AppError> {
    serde_json::from_value(Value::Object(object)).map_err(|_| AppError::InvalidRequest)
}
