use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::recovery::{
    InterruptClaim, InterruptGuard, InterruptState, NativeJobInterrupt,
};
use reviewq_domain::session::HostProvider;

use super::ClaudeCli;
use crate::claude::jobs::{Job, JobKind, claude_job};

impl NativeJobInterrupt for ClaudeCli {
    fn interrupt_job(&self, guard: InterruptGuard) -> AppFuture<'_, InterruptState> {
        Box::pin(async move {
            use InterruptState::{Deferred, Interrupted, Rejected, Uncertain};
            let claim = guard.claim();
            let Some(job_id) = claim.claude_job.as_deref() else {
                return Ok(Rejected);
            };
            let Ok(Some(job)) = self.managed_job(claim).await else {
                return Ok(Deferred);
            };
            if job.is_stopped() {
                return Ok(Interrupted);
            }
            if matches!(job.state.as_deref(), Some("done" | "failed")) {
                return Ok(Rejected);
            }
            if job.pid.is_none() || !matches!(job.status.as_deref(), Some("busy" | "waiting")) {
                return Ok(Deferred);
            }
            if guard.check().await.is_err() {
                return Ok(Deferred);
            }
            let Ok(mut child) = self
                .command(claim.target.binding().cwd().as_path())
                .args(["stop", job_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
            else {
                return Ok(Deferred);
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            drop(child);
            if self
                .managed_job(claim)
                .await
                .ok()
                .flatten()
                .is_some_and(|job| job.is_stopped())
            {
                Ok(Interrupted)
            } else {
                Ok(Uncertain)
            }
        })
    }

    fn observe_stopped_job(&self, claim: InterruptClaim) -> AppFuture<'_, bool> {
        Box::pin(async move {
            Ok(self
                .managed_job(&claim)
                .await?
                .is_some_and(|job| job.is_stopped()))
        })
    }
}

impl ClaudeCli {
    async fn managed_job(&self, claim: &InterruptClaim) -> Result<Option<Job>, AppError> {
        let binding = claim.target.binding();
        let native = binding.identity();
        if native.provider() != HostProvider::Claude || native.profile() != &self.profile {
            return Err(AppError::Forbidden);
        }
        let expected = claim
            .claude_job
            .as_deref()
            .ok_or(AppError::InvalidRequest)?;
        if expected.is_empty()
            || expected.len() > 64
            || !expected.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(AppError::Forbidden);
        }
        let cwd = binding.cwd().as_path();
        let job = claude_job(self.command(cwd), cwd, native.native_id().as_str()).await?;
        if job.as_ref().is_some_and(|job| {
            !matches!(job.kind, JobKind::Background) || job.id.as_deref() != Some(expected)
        }) {
            return Err(AppError::Conflict);
        }
        Ok(job)
    }
}

impl Job {
    fn is_stopped(&self) -> bool {
        self.state.as_deref() == Some("stopped") && self.pid.is_none()
    }
}
