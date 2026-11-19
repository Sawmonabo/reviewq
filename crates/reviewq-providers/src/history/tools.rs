use std::io::{self, Read, Write};

use reviewq_app::access::AppError;
use reviewq_app::history::ToolState;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};
use struson::writer::{JsonStreamWriter, JsonWriter};

use super::records::{Text, Window, capture, invalid};

pub(super) const MAX_NATIVE_PARTS: u16 = 32;
const AUX_PREVIEW_BYTES: usize = 256;

pub(super) fn part_window(window: Window, index: u16) -> Window {
    match window.part {
        None => Window {
            start: 0,
            limit: window.limit.min(AUX_PREVIEW_BYTES),
            part: None,
        },
        Some(target) if target == index => Window {
            part: None,
            ..window
        },
        Some(_) => Window {
            start: 0,
            limit: 0,
            part: None,
        },
    }
}

pub(super) fn body<R: Read>(
    reader: &mut JsonStreamReader<R>,
    window: Window,
) -> Result<Text, AppError> {
    match reader.peek().map_err(invalid)? {
        ValueType::String => capture(reader, window, 0),
        ValueType::Null => {
            reader.skip_value().map_err(invalid)?;
            Ok(Text::default())
        }
        _ => {
            let mut writer = JsonStreamWriter::new(Slice {
                window,
                text: Text::default(),
            });
            reader.transfer_to(&mut writer).map_err(invalid)?;
            Ok(writer.finish_document().map_err(invalid)?.text)
        }
    }
}

struct Slice {
    window: Window,
    text: Text,
}

impl Write for Slice {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let start = self.text.bytes;
        let end = start
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("native content length overflow"))?;
        let low = start.max(self.window.start);
        let high = end.min(self.window.start.saturating_add(self.window.limit as u64));
        if low < high {
            self.text
                .picked
                .extend_from_slice(&bytes[(low - start) as usize..(high - start) as usize]);
        }
        self.text.bytes = end;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn label(name: &str) -> &'static str {
    match name {
        "Bash" | "PowerShell" | "CommandExecution" | "exec_command" => "CommandExecution",
        "Read" => "FileRead",
        "Glob" | "list_files" => "FileSearch",
        "Grep" => "TextSearch",
        "Write" | "Edit" | "MultiEdit" | "FileChange" | "apply_patch" => "FileEdit",
        "WebSearch" | "web_search" => "WebSearch",
        "WebFetch" | "web_fetch" => "WebFetch",
        "WebFind" => "WebFind",
        "ImageView" | "view_image" => "ImageView",
        "ImageGeneration" | "image_gen" => "ImageGeneration",
        "CodeMode" | "code_mode" => "CodeExecution",
        "Agent" | "Task" | "spawn_agent" | "CollabAgentToolCall" => "AgentSpawn",
        "SendMessage" | "send_input" => "AgentMessage",
        "followup_task" => "AgentFollowup",
        "wait_agent" => "AgentWait",
        "interrupt_agent" => "AgentInterrupt",
        "resume_agent" => "AgentResume",
        "close_agent" => "AgentClose",
        "TodoWrite" | "TaskCreate" | "TaskUpdate" | "ExitPlanMode" => "Plan",
        "AskUserQuestion" => "UserQuestion",
        _ if name.starts_with("mcp__") => "MCP",
        _ => "Tool",
    }
}

pub(crate) fn state(value: Option<&str>) -> ToolState {
    match value {
        Some("in_progress" | "inProgress" | "running") => ToolState::Running,
        Some("completed" | "success") => ToolState::Completed,
        Some("failed" | "error" | "declined") => ToolState::Failed,
        _ => ToolState::Unknown,
    }
}
