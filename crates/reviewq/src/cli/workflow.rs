use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use reqwest::{Client, StatusCode};
use reviewq_domain::identity::NativeSessionId;
use reviewq_protocol::{ResourceIdDto, SessionKeyDto, Success};
use reviewq_transport::ServiceEndpoint;
use rustix::fs::{Mode, OFlags};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{native_client, runtime};

use super::Failure;

#[derive(Args)]
pub(super) struct Profile {
    #[arg(long, global = true, conflicts_with = "profile")]
    runtime_root: Option<PathBuf>,
    #[arg(long, global = true, requires = "runtime_root")]
    port: Option<u16>,
}

pub(super) struct Context {
    client: Client,
    endpoint: ServiceEndpoint,
    pub session: SessionKeyDto,
    pub call: Option<ResourceIdDto>,
}

pub(super) enum ReplyError {
    Rejected(StatusCode),
    ReplyLost,
}

impl Context {
    pub(super) async fn connect(
        profile: Option<PathBuf>,
        scope: Profile,
        needs_call: bool,
    ) -> Result<Self, Failure> {
        let thread: NativeSessionId = std::env::var("CODEX_THREAD_ID")
            .ok()
            .and_then(|id| id.parse().ok())
            .ok_or("Run this command from the registered Codex conversation.")?;
        let call = if needs_call {
            Some(
                std::env::var("CODEX_TOOL_CALL_ID")
                    .ok()
                    .and_then(ResourceIdDto::new)
                    .ok_or("Run this command inside the original Codex tool call.")?,
            )
        } else {
            None
        };
        if scope
            .runtime_root
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err("The isolated runtime root must be absolute.".into());
        }
        let profile = runtime::profile(scope.runtime_root.or(profile))?;
        let endpoint = runtime::read_endpoint(&profile)?;
        if scope
            .port
            .is_some_and(|port| endpoint.browser().port() != port)
        {
            return Err("The active service does not match the requested isolated port.".into());
        }
        let client = native_client::client()?;
        let ready = native_client::signed(&client, &endpoint, "GET", "/api/v1/status", b"").await?;
        native_client::verify_ready(&endpoint, &ready)?;
        let body = serde_json::to_vec(
            &serde_json::json!({"protocolVersion":1,"threadId":thread.as_str()}),
        )?;
        let registered = native_client::bridge(
            &client,
            &endpoint,
            "/api/v1/native/codex/registration",
            body,
        )
        .await?;
        if !registered.status().is_success() {
            return Err("This Codex conversation has no verified ReviewQ lead. Use reviewq link with the review target.".into());
        }
        let body = native_client::bounded(registered, 8192).await?;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Registration {
            lead_session_key: SessionKeyDto,
        }
        let session = serde_json::from_slice::<Success<Registration>>(&body)?
            .data
            .lead_session_key;
        Ok(Self {
            client,
            endpoint,
            session,
            call,
        })
    }

    pub(super) async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &[u8],
    ) -> Result<T, ReplyError> {
        let response = native_client::bridge(&self.client, &self.endpoint, path, body.to_vec())
            .await
            .map_err(|_| ReplyError::ReplyLost)?;
        if !response.status().is_success() {
            return Err(ReplyError::Rejected(response.status()));
        }
        let bytes = native_client::bounded(response, 144 * 1024)
            .await
            .map_err(|_| ReplyError::ReplyLost)?;
        serde_json::from_slice::<Success<T>>(&bytes)
            .map(|result| result.data)
            .map_err(|_| ReplyError::ReplyLost)
    }

    pub(super) fn call(&self) -> Result<ResourceIdDto, Failure> {
        self.call
            .clone()
            .ok_or_else(|| "Missing original native call identity.".into())
    }
}

impl ReplyError {
    pub(super) fn report(self, operation: &str, session: &SessionKeyDto) -> Failure {
        match self {
            Self::Rejected(status) if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY =>
                reviewq_app::access::REPORT_CORRECTION_MESSAGE.into(),
            Self::Rejected(status) => format!("ReviewQ {operation} for lead {} returned HTTP {}.", session.as_str(), status.as_u16()).into(),
            Self::ReplyLost => format!("ReviewQ could not confirm {operation} for lead {}. Read the saved state before retrying.", session.as_str()).into(),
        }
    }
}

pub(super) fn regular_file(path: &Path) -> Result<File, Failure> {
    let file = File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err("Expected a regular file.".into());
    }
    Ok(file)
}

pub(super) fn json_file<T: DeserializeOwned>(path: &Path, max: u64) -> Result<T, Failure> {
    let mut content = Vec::new();
    regular_file(path)?
        .take(max + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > max {
        return Err("The metadata file exceeds its size limit.".into());
    }
    Ok(serde_json::from_slice(&content)
        .map_err(|_| "The metadata file does not match the expected JSON fields.")?)
}

pub(super) fn print(
    value: &impl Serialize,
    capture_redirected_output: bool,
) -> Result<(), Failure> {
    let encoded = serde_json::to_string(value)?;
    println!("{encoded}");
    if capture_redirected_output
        && rustix::fs::fstat(std::io::stdout())
            .ok()
            .is_some_and(|stat| rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file())
    {
        eprintln!("{encoded}");
    }
    Ok(())
}
