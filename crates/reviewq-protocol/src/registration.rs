//! Exact operator attachment to a native lead session.

use serde::{Deserialize, Serialize};

use crate::{HostProviderDto, ResourceIdDto, ReviewIdDto, RunIdDto, SessionKeyDto, Success};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RegisterSessionRequest {
    pub review_id: ReviewIdDto,
    pub provider: HostProviderDto,
    pub native_id: ResourceIdDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RegisteredSession {
    pub review_id: ReviewIdDto,
    pub run_id: RunIdDto,
    pub lead_session_key: SessionKeyDto,
    pub provider: HostProviderDto,
    pub native_id: ResourceIdDto,
    pub cwd: String,
    pub review_checkout: Option<String>,
}

pub type RegisteredSessionResponse = Success<RegisteredSession>;
