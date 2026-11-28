use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_protocol::deadlines::{GRANT_CHECK_LIMIT, LONG, NOT_STARTED, tool_deadline};
use reviewq_protocol::ledger::LedgerTool;
use reviewq_protocol::mcp::{
    AnswerAgentQuestion, GetFindings, GetReview, MANAGEMENT_TOOLS, ManagementTool,
    ReadAgentTranscript, SearchReviews, SendAgentMessage, SetThreadFollowups, StartReview,
    UNKNOWN_MANAGEMENT_TOOL_CLAUDE, UNKNOWN_TOOL_CODEX,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, RequestMetaObject, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};

use super::{ApiError, Entry};

mod gates;
mod ledger;
mod lookups;
mod operations;
mod workflow;
use gates::Gates;

// Every wait for a turn ends by the longest tool deadline.
const _: () = assert!(LONG.as_nanos() == reviewq_app::wait::LONGEST.as_nanos());

const MAX_TOOL_RESPONSE: usize = 512 * 1024;
/// Largest MCP request body. It is set well above the largest ledger tool input, and meant to
/// exceed the arguments a model writes in one tool call, so a call whose input is too large
/// still reaches the ledger validator and gets its field error. rmcp answers a larger body with `413` before
/// any tool code runs; the body is read under the router's 30-second body timeout and only
/// after the grant is authenticated, or after the store did not check it in time.
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
/// The memory MCP requests may hold at once, waiting or running. A request is charged its body
/// (its `Content-Length`, or the largest body when it names none) plus `REQUEST_COST` for what
/// it holds besides; one waits for its share before its body is read, so a request still
/// waiting for memory holds only its headers. It binds for large bodies, about 62 of the
/// largest at once; ordinary calls of a few kilobytes reach the process's open-file limit first.
const REQUEST_MEMORY: usize = 64 * 1024 * 1024;
/// What one request is charged besides its body: its task, the parsed call and its reply.
const REQUEST_COST: usize = 32 * 1024;

/// When a request reached the MCP server. A tool call's deadline runs from here, so the time
/// it waited for memory and for its grant check counts toward it.
#[derive(Clone, Copy)]
struct Arrived(Instant);

/// The state of the `/mcp` admission: the service and the memory MCP requests may hold.
#[derive(Clone)]
struct Admission {
    entry: Arc<Entry>,
    memory: Arc<Semaphore>,
}

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
        .with_max_request_body_bytes(MAX_REQUEST_BODY_BYTES);
    let application = Server {
        entry: entry.clone(),
        gates: Arc::new(Gates::default()),
        tools: MANAGEMENT_TOOLS
            .into_iter()
            .map(management_tool)
            .chain(ledger::tools())
            .collect(),
    };
    let sdk = StreamableHttpService::new(
        move || Ok(application.clone()),
        NeverSessionManager::default().into(),
        config,
    );
    let admission = Admission {
        entry,
        memory: Arc::new(Semaphore::new(REQUEST_MEMORY)),
    };
    Router::new()
        .route_service("/mcp", sdk)
        .layer(middleware::from_fn_with_state(admission, authenticate))
}

/// One management tool as the MCP server lists it: name, description and input schema.
fn management_tool(tool: ManagementTool) -> Tool {
    let schema = match tool {
        ManagementTool::SearchReviews => schemars::schema_for!(SearchReviews),
        ManagementTool::GetReview => schemars::schema_for!(GetReview),
        ManagementTool::GetFindings => schemars::schema_for!(GetFindings),
        ManagementTool::ReadAgentTranscript => schemars::schema_for!(ReadAgentTranscript),
        ManagementTool::SendAgentMessage => schemars::schema_for!(SendAgentMessage),
        ManagementTool::StartReview => schemars::schema_for!(StartReview),
        ManagementTool::AnswerAgentQuestion => schemars::schema_for!(AnswerAgentQuestion),
        ManagementTool::SetThreadFollowups => schemars::schema_for!(SetThreadFollowups),
    };
    let schema = schema.to_value().as_object().cloned().unwrap_or_default();
    Tool::new(tool.name(), tool.description(), schema)
}

/// Admits one MCP request: it waits for its share of `REQUEST_MEMORY` before its body is read,
/// then for the store to check its grant, both within `GRANT_CHECK_LIMIT` of its arrival, the
/// shortest tool deadline, which runs from the same moment.
async fn authenticate(
    State(admission): State<Admission>,
    mut request: Request,
    next: Next,
) -> Response {
    let arrived = Arrived(Instant::now());
    let checked_by = arrived.0 + GRANT_CHECK_LIMIT;
    let Ok(Ok(_memory)) = timeout_at(
        checked_by,
        admission
            .memory
            .clone()
            .acquire_many_owned(request_cost(&request)),
    )
    .await
    else {
        return ApiError::from(AppError::Busy).into_response();
    };
    let entry = admission.entry;
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
    let checked = timeout_at(checked_by, entry.store.authenticate_grant(verifier)).await;
    match checked {
        Ok(Ok(actor)) => {
            request.headers_mut().remove(header::AUTHORIZATION);
            request.extensions_mut().insert(actor);
            request.extensions_mut().insert(arrived);
            next.run(request).await
        }
        Ok(Err(AppError::Unauthorized)) => challenge(),
        Ok(Err(error)) => ApiError::from(error).into_response(),
        Err(_) => unchecked(request).await,
    }
}

/// What a request is charged of `REQUEST_MEMORY`: its body, at most the largest one, and
/// `REQUEST_COST`.
fn request_cost(request: &Request) -> u32 {
    let body = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(MAX_REQUEST_BODY_BYTES, |length| {
            length.min(MAX_REQUEST_BODY_BYTES)
        });
    u32::try_from(body + REQUEST_COST).unwrap_or(u32::MAX)
}

/// The answer to a request whose grant the store did not check in time, so nothing ran. A tool
/// call gets the sentence that says it was not started as its error result, which the model
/// reads; any other request is refused as unavailable. Only here is an unauthenticated body
/// read, within the same limit as an authenticated one.
async fn unchecked(request: Request) -> Response {
    let unavailable = || ApiError::from(AppError::Unavailable).into_response();
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES).await else {
        return unavailable();
    };
    let Ok(call) = serde_json::from_slice::<Value>(&body) else {
        return unavailable();
    };
    let (Some(id), Some("tools/call")) =
        (call.get("id"), call.get("method").and_then(Value::as_str))
    else {
        return unavailable();
    };
    let result = CallToolResult::error(vec![ContentBlock::text(NOT_STARTED)]);
    let reply = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });
    axum::Json(reply).into_response()
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

    /// Lists every tool, except the ledger tools for a Claude client, whose plugin registers
    /// its own.
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|page| page.cursor.is_some()) {
            return Err(ErrorData::invalid_params("invalid tool cursor", None));
        }
        let claude = ledger::claude_client(&context);
        let tools = self
            .tools
            .iter()
            .filter(|tool| !claude || LedgerTool::from_name(&tool.name).is_none())
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
        let arrived = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<Arrived>())
            .map_or_else(Instant::now, |arrived| arrived.0);
        let deadline = tool_deadline(&request.name);
        let until = arrived + deadline.limit;
        let claude = ledger::claude_client(&context);
        let meta = context.meta;
        let cancelled = context.ct;
        let answered = async {
            // A turn that comes as the deadline passes is not taken: the call never started.
            let permit = match timeout_at(until, self.gates.enter(&actor)).await {
                Ok(permit) if Instant::now() < until => permit,
                _ => return Err(NOT_STARTED),
            };
            let call = async { self.call(permit?, request, &meta, actor, claude).await };
            timeout_at(until, call)
                .await
                .map_err(|_| deadline.timed_out)
        };
        let result = tokio::select! {
            _ = cancelled.cancelled() => return Err(ErrorData::internal_error("request cancelled", None)),
            result = answered => result,
        };
        let text = match result {
            Ok(Ok(output)) => return Ok(output.into()),
            Ok(Err(error)) => super::error::tool_error_text(error),
            Err(sentence) => sentence.to_owned(),
        };
        Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into())
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
    /// Runs one tool call in its turn. A reviewing tool binds its caller from the request's `_meta`. A name
    /// the server does not list gets the host's sentence asking for the current tools, which a
    /// session still holding an older tool list needs after an update. A Claude client reaches
    /// this server only for the management tools, through its checkout's MCP server entry, so
    /// its sentence names that entry's reconnect rather than the plugin's reload.
    async fn call(
        &self,
        _turn: gates::Permit,
        request: CallToolRequestParams,
        meta: &RequestMetaObject,
        actor: Actor,
        claude: bool,
    ) -> Result<CallToolResult, ApiError> {
        let input = request.arguments.unwrap_or_default();
        if let Some(tool) = LedgerTool::from_name(&request.name) {
            return ledger::call(&self.entry, tool, meta, actor, input).await;
        }
        let Some(tool) = ManagementTool::from_name(&request.name) else {
            let text = if claude {
                UNKNOWN_MANAGEMENT_TOOL_CLAUDE
            } else {
                UNKNOWN_TOOL_CODEX
            };
            return Ok(CallToolResult::error(vec![ContentBlock::text(text)]));
        };
        let entry = &self.entry;
        let result = match tool {
            ManagementTool::SearchReviews => {
                operations::list(entry, actor, arguments(input)?).await
            }
            ManagementTool::GetReview => operations::get(entry, actor, arguments(input)?).await,
            ManagementTool::GetFindings => lookups::findings(entry, actor, arguments(input)?).await,
            ManagementTool::ReadAgentTranscript => {
                operations::transcript(entry, actor, arguments(input)?).await
            }
            ManagementTool::SendAgentMessage => {
                operations::send(entry, actor, arguments(input)?).await
            }
            ManagementTool::StartReview => workflow::start(entry, actor, arguments(input)?).await,
            ManagementTool::AnswerAgentQuestion => {
                workflow::answer(entry, actor, arguments(input)?).await
            }
            ManagementTool::SetThreadFollowups => {
                workflow::followups(entry, actor, arguments(input)?).await
            }
        }?;
        Ok(bounded_result(&result)?)
    }
}

fn arguments<T: DeserializeOwned>(object: JsonObject) -> Result<T, AppError> {
    serde_json::from_value(Value::Object(object)).map_err(|_| AppError::InvalidRequest)
}
