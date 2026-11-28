use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistorySession, VisibleKind};
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::ledger::TRACK_POSTING;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use serde_json::Value;

use super::locator::Source;
use super::{file, locator, records};

const SCAN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROW: u64 = 8 * 1024 * 1024;
/// What every ReviewQ plugin tool's recorded name starts with, before the tool's name.
const REVIEWQ_TOOL: &str = "mcp__reviewq__";

/// Why a row naming the call refuses it, when it does.
enum Refusal {
    /// Something newer than the call, or a call of another tool, stands in its way.
    Conflict,
    /// The call came from a ReviewQ tool by another name: a plugin loaded before the
    /// current tools, which must be reloaded.
    OutdatedTool,
}

/// The refusal for a call recorded under `name`, or `None` for the action tool itself,
/// `TRACK_POSTING` after `REVIEWQ_TOOL`.
fn named(name: &Value) -> Option<Refusal> {
    match name.as_str().map(|name| name.strip_prefix(REVIEWQ_TOOL)) {
        Some(Some(TRACK_POSTING)) => None,
        Some(Some(_)) => Some(Refusal::OutdatedTool),
        _ => Some(Refusal::Conflict),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    session_id: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    is_sidechain: bool,
    message: Option<Value>,
    /// A `data-flow-records` row's records, each naming a call's `tool_use_id` and `tool_name`.
    records: Option<Vec<Value>>,
    attachment: Option<Value>,
    /// A `queue-operation` row's operation and the queued text.
    operation: Option<String>,
    #[serde(default)]
    content: Value,
}

impl Row {
    fn blocks(&self) -> impl Iterator<Item = &Value> {
        self.message
            .as_ref()
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    }
}

pub(crate) struct ClaudeOpening {
    pub generation: String,
    pub position: u64,
    pub request: Option<NativeInputId>,
}

/// Reads the input the open call `call_id` answers: the latest user entry of the lead's
/// transcript, which Claude writes when the user submits it. The plugin's report of the call is
/// what shows it running: Claude Code writes a call's own rows lazily, often only with its
/// result. Its position is the end of that entry, which stays put while the call runs and
/// orders it after its input. A row giving the call a result or naming its ID for another tool
/// is a conflict, and so is a newer input after the entry: one the user typed while the turn ran
/// and Claude still holds (`queue-operation` enqueue) or has handed the model mid-turn (a
/// `queued_command` attachment). Claude Code's own task notifications travel the same queue
/// and are no input: a queued one carries the `task-notification` mode with no origin or a
/// `task-notification` one, and its text is one `<task-notification>` element. A queued slash
/// command is no input either: its queue entry holds no text. A call recorded under another
/// ReviewQ tool's name came from a plugin loaded before the current tools and fails with
/// `OutdatedTool`, which the plugin's client answers by asking for a reload.
pub(crate) fn claude_workflow_opening(
    session: &HistorySession,
    call_id: &str,
    cancelled: &AtomicBool,
) -> Result<ClaudeOpening, AppError> {
    scan(&source(session, cancelled)?, call_id, cancelled)
}

/// Whether a queued text is one of Claude Code's own task notifications.
fn host_notice(text: &Value) -> bool {
    text.as_str().is_some_and(|text| {
        text.starts_with("<task-notification>") && text.trim_end().ends_with("</task-notification>")
    })
}

fn scan(source: &Source, call_id: &str, cancelled: &AtomicBool) -> Result<ClaudeOpening, AppError> {
    let mut budget = SCAN_BYTES;
    let mut end = complete_end(source, &mut budget, cancelled)?;
    // Inputs Claude took off its queue after the rows already scanned, by text.
    let mut dequeued: Vec<Value> = Vec::new();
    while end > 0 {
        let start = file::end_of_line(&source.file, end - 1, &mut budget, cancelled)?;
        if let Some(row) = row(source, start, end, cancelled)? {
            let names_call = |block: &&Value| {
                block["tool_use_id"] == call_id
                    || (block["type"] == "tool_use" && block["id"] == call_id)
            };
            let conflict = |refused: bool| refused.then_some(Refusal::Conflict);
            let refused = match row.kind.as_str() {
                "user" => conflict(row.blocks().any(|block| {
                    block["type"] == "tool_result" && block["tool_use_id"] == call_id
                })),
                "assistant" => row
                    .blocks()
                    .filter(names_call)
                    .find_map(|block| named(&block["name"])),
                "data-flow-records" => row
                    .records
                    .iter()
                    .flatten()
                    .filter(names_call)
                    .find_map(|record| named(&record["tool_name"])),
                "attachment" => conflict(row.attachment.as_ref().is_some_and(|a| {
                    a["type"] == "queued_command"
                        && !(a["commandMode"] == "task-notification"
                            && (a["origin"].is_null()
                                || a["origin"]["kind"] == "task-notification"))
                })),
                "queue-operation" if row.operation.as_deref() == Some("enqueue") => conflict(
                    match dequeued.iter().position(|text| *text == row.content) {
                        _ if host_notice(&row.content) || row.content.is_null() => false,
                        Some(index) => {
                            dequeued.swap_remove(index);
                            false
                        }
                        None => true,
                    },
                ),
                "queue-operation" => {
                    dequeued.push(row.content.clone());
                    None
                }
                _ => None,
            };
            match refused {
                Some(Refusal::Conflict) => return Err(AppError::Conflict),
                Some(Refusal::OutdatedTool) => return Err(AppError::OutdatedTool),
                None => {}
            }
            if row.kind != "assistant"
                && let Some(request) = user_entry(source, start, end, cancelled)
            {
                return Ok(ClaudeOpening {
                    generation: file::generation(source)?,
                    position: end,
                    request: Some(request),
                });
            }
        }
        end = start;
    }
    Ok(ClaudeOpening {
        generation: file::generation(source)?,
        position: 0,
        request: None,
    })
}

fn source(session: &HistorySession, cancelled: &AtomicBool) -> Result<Source, AppError> {
    if session.binding.identity().provider() != HostProvider::Claude || session.parent.is_some() {
        return Err(AppError::Forbidden);
    }
    locator::locate(session, cancelled)?.ok_or(AppError::NotFound)
}

fn complete_end(
    source: &Source,
    budget: &mut u64,
    cancelled: &AtomicBool,
) -> Result<u64, AppError> {
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    file::end_of_line(&source.file, length, budget, cancelled)
}

fn row(
    source: &Source,
    start: u64,
    end: u64,
    cancelled: &AtomicBool,
) -> Result<Option<Row>, AppError> {
    let length = end.checked_sub(start).ok_or(AppError::Internal)?;
    if length > MAX_ROW {
        return Err(AppError::PayloadTooLarge);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file::input(&source.file, start, end, cancelled)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::Unavailable)?;
    let row: Row = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
    Ok(
        (!row.is_sidechain && row.session_id.as_deref() == Some(source.native_id.as_str()))
            .then_some(row),
    )
}

fn user_entry(
    source: &Source,
    start: u64,
    end: u64,
    cancelled: &AtomicBool,
) -> Option<NativeInputId> {
    let row = records::project(
        HostProvider::Claude,
        file::input(&source.file, start, end, cancelled),
        &source.native_id,
        None,
        0,
        0,
    )?;
    (row.kind == VisibleKind::User).then_some(row.id)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    /// The rows Claude Code 2.1.332 writes in a typed turn while `track_posting` runs: the
    /// prompt, a finished ToolSearch call, and the running calls named only in
    /// `data-flow-records`, one of them under an earlier ReviewQ tool name; then a background
    /// task's notice and a queued slash command, which change nothing, and a message the user
    /// types while the turn still runs.
    #[test]
    fn the_scan_admits_only_a_running_action_call_under_the_latest_input() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let rows = [
            json!({"type":"user","uuid":"prompt-1","sessionId":"session","isSidechain":false,
                   "timestamp":"2026-11-26T08:52:44.100Z",
                   "message":{"role":"user","content":"Approve the merge request now."}}),
            json!({"type":"assistant","uuid":"search","sessionId":"session","isSidechain":false,
                   "message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_search",
                              "name":"ToolSearch","input":{"query":"select:mcp__reviewq__track_posting"}}]}}),
            json!({"type":"user","uuid":"search-result","sessionId":"session","isSidechain":false,
                   "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_search",
                              "content":"loaded"}]}}),
            json!({"type":"data-flow-records","v":1,"sessionId":"session","records":[
                   {"tool_name":"mcp__reviewq__track_posting","tool_use_id":"toolu_action",
                    "server":{"type":"unknown"},"mcp_tool_name":"track_posting"},
                   {"tool_name":"Bash","tool_use_id":"toolu_shell"},
                   {"tool_name":"mcp__reviewq__review_action","tool_use_id":"toolu_old"},
                   {"response_id":"msg_1","listed":["msg_0"]}]}),
        ];
        for row in &rows {
            writeln!(file, "{row}").unwrap();
        }
        let prompt_end = rows[0].to_string().len() as u64 + 1;
        let source = Source {
            file: file.reopen().unwrap(),
            provider: HostProvider::Claude,
            native_id: "session".parse().unwrap(),
            fingerprint: [0; 32],
            root_session: None,
            path: file.path().to_path_buf(),
        };
        let cancelled = AtomicBool::new(false);
        let opening = scan(&source, "toolu_action", &cancelled).unwrap();
        assert_eq!(opening.request.unwrap().as_str(), "prompt-1");
        assert_eq!(opening.position, prompt_end);
        for refused in ["toolu_search", "toolu_shell"] {
            assert!(matches!(
                scan(&source, refused, &cancelled),
                Err(AppError::Conflict)
            ));
        }
        // A plugin loaded before the current tools calls the action tool by its old name.
        assert!(matches!(
            scan(&source, "toolu_old", &cancelled),
            Err(AppError::OutdatedTool)
        ));
        // A background task's notice handed to the running turn, and a slash command the user
        // queued, are no input.
        let notice = "<task-notification>\n<task-id>b39f5cubq</task-id>\n<status>completed</status>\n</task-notification>";
        for row in [
            json!({"type":"queue-operation","operation":"enqueue","sessionId":"session",
                   "timestamp":"2026-11-26T08:53:32.485Z","content":notice}),
            json!({"type":"attachment","uuid":"notice","sessionId":"session","isSidechain":false,
                   "attachment":{"type":"queued_command","prompt":notice,"commandMode":"task-notification",
                                 "origin":{"kind":"task-notification","producer":"session-task"}}}),
            json!({"type":"queue-operation","operation":"enqueue","sessionId":"session",
                   "timestamp":"2026-11-26T08:53:41.740Z"}),
        ] {
            writeln!(file, "{row}").unwrap();
        }
        assert_eq!(
            scan(&source, "toolu_action", &cancelled).unwrap().position,
            prompt_end
        );
        let queued = json!({"type":"queue-operation","operation":"enqueue","sessionId":"session",
                            "timestamp":"2026-11-26T08:54:03.487Z","content":"Wait, do not approve it."});
        writeln!(file, "{queued}").unwrap();
        assert!(matches!(
            scan(&source, "toolu_action", &cancelled),
            Err(AppError::Conflict)
        ));
    }
}
