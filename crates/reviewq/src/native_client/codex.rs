//! Forward one exact Codex PreToolUse observation without altering the tool call.

use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reviewq_domain::identity::{NativeInputId, NativeSessionId, NativeTurnId};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{Failure, bounded, bridge, client_with_timeout, signed, verify_ready};
use crate::runtime;

const TOOL: &str = "mcp__reviewq__report_handoff";
const EVENT_LIMIT: usize = 144 * 1024;
const ARGUMENTS_LIMIT: usize = 128 * 1024;

#[derive(Deserialize)]
struct Event<'a> {
    hook_event_name: String,
    tool_name: String,
    session_id: String,
    turn_id: String,
    tool_use_id: String,
    transcript_path: Option<PathBuf>,
    #[serde(borrow)]
    tool_input: &'a RawValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Witness<'a> {
    session_id: &'a str,
    turn_id: &'a str,
    call_id: &'a str,
    arguments: &'a RawValue,
}

pub(crate) async fn codex_report_witness(
    profile: &Path,
    codex_home: Option<&Path>,
) -> Result<(), Failure> {
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take((EVENT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > EVENT_LIMIT {
        return Ok(());
    }
    let event: Event<'_> = serde_json::from_slice(&bytes)?;
    let Some(witness) = event.witness()? else {
        return Ok(());
    };
    let home = match codex_home {
        Some(home) => home,
        None => &runtime::codex_home()?,
    };
    if !event.belongs_to(home)? {
        return Ok(());
    }
    let body = serde_json::to_vec(&witness)?;
    if body.len() > EVENT_LIMIT {
        return Ok(());
    }
    let endpoint = runtime::read_native_endpoint(profile)?;
    let timeout = Duration::from_secs(3);
    let client = client_with_timeout(Some(timeout))?;
    tokio::time::timeout(timeout, async {
        let status = signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
        verify_ready(&endpoint, &status)?;
        let response = bridge(
            &client,
            &endpoint,
            "/api/v1/native/reports/codex-witness",
            body,
        )
        .await?;
        let accepted = response.status() == reqwest::StatusCode::NO_CONTENT;
        if !bounded(response, 8192).await?.is_empty() || !accepted {
            return Err("ReviewQ did not accept this Codex report witness.".into());
        }
        Ok(())
    })
    .await?
}

impl Event<'_> {
    fn witness(&self) -> Result<Option<Witness<'_>>, Failure> {
        if self.hook_event_name != "PreToolUse"
            || self.tool_name != TOOL
            || self.tool_input.get().len() > ARGUMENTS_LIMIT
            || !self.tool_input.get().starts_with('{')
        {
            return Ok(None);
        }
        let _: NativeSessionId = self.session_id.parse()?;
        let _: NativeTurnId = self.turn_id.parse()?;
        let _: NativeInputId = self.tool_use_id.parse()?;
        Ok(Some(Witness {
            session_id: &self.session_id,
            turn_id: &self.turn_id,
            call_id: &self.tool_use_id,
            arguments: self.tool_input,
        }))
    }

    fn belongs_to(&self, home: &Path) -> Result<bool, Failure> {
        let uid = rustix::process::getuid().as_raw();
        let home_metadata = home.symlink_metadata()?;
        if !home_metadata.is_dir() || home_metadata.uid() != uid || home.canonicalize()? != home {
            return Ok(false);
        }
        if std::env::var_os("CODEX_HOME").is_some_and(|current| Path::new(&current) != home) {
            return Ok(false);
        }
        let thread = std::env::var_os("CODEX_THREAD_ID");
        if thread
            .as_ref()
            .is_some_and(|current| current.to_str() != Some(&self.session_id))
            || std::env::var_os("CODEX_TOOL_CALL_ID")
                .is_some_and(|current| current.to_str() != Some(&self.tool_use_id))
        {
            return Ok(false);
        }
        if let Some(transcript) = self.transcript_path.as_deref() {
            let suffix = format!("-{}.jsonl", self.session_id);
            let name = transcript.file_name().and_then(|name| name.to_str());
            if !transcript.starts_with(home.join("sessions"))
                || !name.is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
            {
                return Ok(false);
            }
            let metadata = transcript.symlink_metadata()?;
            Ok(metadata.is_file()
                && metadata.uid() == uid
                && transcript.canonicalize()? == transcript)
        } else {
            Ok(thread.is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_exact_native_observation_and_preserves_raw_tool_input() -> Result<(), Failure> {
        let event = r#"{
            "hook_event_name": "PreToolUse",
            "tool_name": "mcp__reviewq__report_handoff",
            "session_id": "thread",
            "turn_id": "turn",
            "tool_use_id": "exec-fixture",
            "transcript_path": null,
            "cwd": "/work",
            "tool_input": { "handoff": {"outcome":"final"}, "report": {"findings":[{"id":"F-001"}]}}
        }"#;
        let parsed: Event<'_> = serde_json::from_str(event)?;
        let witness = serde_json::to_string(&parsed.witness()?.expect("one matching invocation"))?;
        assert_eq!(
            witness,
            r#"{"sessionId":"thread","turnId":"turn","callId":"exec-fixture","arguments":{ "handoff": {"outcome":"final"}, "report": {"findings":[{"id":"F-001"}]}}}"#,
        );

        let wrong_event = event.replace("\"PreToolUse\"", "\"PostToolUse\"");
        assert!(
            serde_json::from_str::<Event<'_>>(&wrong_event)?
                .witness()?
                .is_none()
        );
        let other_tool = event.replace(
            "mcp__reviewq__report_handoff",
            "mcp__reviewq__report_handoff_extra",
        );
        assert!(
            serde_json::from_str::<Event<'_>>(&other_tool)?
                .witness()?
                .is_none()
        );
        let oversized = event.replace("F-001", &"a".repeat(ARGUMENTS_LIMIT));
        assert!(
            serde_json::from_str::<Event<'_>>(&oversized)?
                .witness()?
                .is_none()
        );
        Ok(())
    }
}
