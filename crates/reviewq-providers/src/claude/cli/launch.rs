use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::hosts::HostProof;
use reviewq_app::launch::{
    ClaudeLaunchLocator, Invocation, LaunchInput, LaunchProgress, NativeLaunchResult,
    NativeLauncher, NativeModelReader,
};
use reviewq_domain::session::{HostProvider, NativeBinding, NativeIdentity};
use tokio::io::AsyncReadExt;

use super::super::jobs::claude_launch_job;
use super::super::validate_id;
use super::ClaudeCli;
use crate::checkout::verify_checkout;
use crate::history::verified_claude_source;

const ACK_LIMIT: u64 = 32 * 1024;

impl NativeLauncher for ClaudeCli {
    fn launch(
        &self,
        input: LaunchInput,
        progress: LaunchProgress,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = NativeLaunchResult> + Send + '_>> {
        Box::pin(async move {
            if self.launch_ready(&input, &progress).await.is_err() {
                return NativeLaunchResult::Rejected;
            }
            let cwd = input.checkout.root.as_path();
            let mut command = self.command(cwd);
            let name = format!(
                "ReviewQ · !{} · {}",
                input.source.identity.iid(),
                progress.id()
            );
            command.args(["--bg", "--name", &name]).stdin(Stdio::null());
            if let Some(model) = &input.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &input.effort {
                command.args(["--effort", effort]);
            }
            self.plugin_args(&mut command);
            command.arg("--").arg(input.prompt());

            if progress
                .record_claude_locator(self.profile.clone(), name.clone(), None)
                .await
                .is_err()
                || progress.check().await.is_err()
            {
                return NativeLaunchResult::Rejected;
            }
            let Ok(mut child) = command.spawn() else {
                return NativeLaunchResult::Rejected;
            };
            let acknowledgement = if let Some(out) = child.stdout.take() {
                tokio::time::timeout(Duration::from_secs(10), async {
                    let mut bytes = Vec::new();
                    out.take(ACK_LIMIT + 1).read_to_end(&mut bytes).await.ok()?;
                    if bytes.len() as u64 > ACK_LIMIT || !child.wait().await.ok()?.success() {
                        return None;
                    }
                    String::from_utf8(bytes).ok()
                })
                .await
                .ok()
                .flatten()
            } else {
                None
            };
            drop(child);
            let acknowledged = acknowledgement.as_deref().and_then(acknowledged_job);
            let locator = ClaudeLaunchLocator {
                profile: self.profile.clone(),
                checkout: input.checkout.root,
                native: None,
                name: Some(name.clone()),
                job: acknowledged.map(str::to_owned),
            };
            if acknowledged.is_some()
                && progress
                    .record_claude_locator(self.profile.clone(), name, locator.job.clone())
                    .await
                    .is_err()
            {
                return NativeLaunchResult::Uncertain;
            }
            let mut observed_job = None;
            for delay in [0, 150, 300, 600] {
                if delay != 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                match self
                    .observe_launch_job(&locator, progress.created_at_ms())
                    .await
                {
                    Ok(Some(found)) => {
                        observed_job = Some(found);
                        break;
                    }
                    Err(AppError::Conflict) => return NativeLaunchResult::Uncertain,
                    _ => {}
                }
            }
            let Some((proof, job)) = observed_job else {
                return NativeLaunchResult::Uncertain;
            };
            if progress
                .record_observed_native(proof.clone(), Some(job))
                .await
                .is_err()
            {
                return NativeLaunchResult::Uncertain;
            }
            if progress.register(proof, None).await.is_ok() && acknowledged.is_some() {
                NativeLaunchResult::Confirmed
            } else {
                NativeLaunchResult::Uncertain
            }
        })
    }

    fn reconcile_job(
        &self,
        locator: ClaudeLaunchLocator,
        created_at_ms: i64,
    ) -> AppFuture<'_, Option<(HostProof, String)>> {
        Box::pin(async move { self.observe_launch_job(&locator, created_at_ms).await })
    }
}

impl ClaudeCli {
    async fn observe_launch_job(
        &self,
        locator: &ClaudeLaunchLocator,
        attempt_started_at_ms: i64,
    ) -> Result<Option<(HostProof, String)>, AppError> {
        if locator.profile != self.profile {
            return Err(AppError::Forbidden);
        }
        self.verify_checkout(&locator.checkout).await?;
        let cwd = locator.checkout.as_path();
        let Some(job) = claude_launch_job(
            self.command(cwd),
            cwd,
            locator.job.as_deref(),
            locator.native.as_ref().map(|id| id.as_str()),
            locator.name.as_deref(),
        )
        .await?
        else {
            return Ok(None);
        };
        let job_started_at_ms = job.started_at.filter(|time| *time > 0);
        if locator.job.is_none()
            && locator.native.is_none()
            && job_started_at_ms.is_none_or(|started| started < attempt_started_at_ms)
        {
            return Err(AppError::Conflict);
        }
        let (Some(job_id), Some(native)) = (job.id, job.session_id) else {
            return Ok(None);
        };
        let native = native.parse().map_err(|_| AppError::Conflict)?;
        validate_id(&native)?;
        let identity = NativeIdentity::new(HostProvider::Claude, self.profile.clone(), native);
        let binding = NativeBinding::new(identity, locator.checkout.clone());
        let probe = binding.clone();
        let source = tokio::task::spawn_blocking(move || {
            verified_claude_source(&probe, None, &std::sync::atomic::AtomicBool::new(false))
        })
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        let name = job.name.as_deref().and_then(|name| {
            let display = if Some(name) == locator.name.as_deref() {
                name.rsplit_once(" · ").map_or(name, |(title, _)| title)
            } else {
                name
            };
            crate::settings::safe_label(display, 512).ok()
        });
        Ok(Some((
            HostProof {
                binding,
                source,
                activity: None,
                metadata: reviewq_app::hosts::NativeSessionMetadata {
                    name,
                    started_at_ms: job_started_at_ms,
                    ..Default::default()
                },
            },
            job_id,
        )))
    }

    async fn launch_ready(
        &self,
        input: &LaunchInput,
        progress: &LaunchProgress,
    ) -> Result<(), AppError> {
        if input.provider != HostProvider::Claude {
            return Err(AppError::InvalidRequest);
        }
        let (checkout, _) = verify_checkout(&input.checkout.root).await?;
        if checkout != input.checkout {
            return Err(AppError::StaleRevision);
        }
        if let Invocation::Skill { skill, .. } = &input.invocation {
            use reviewq_app::settings::NativeSkillReader;
            if !self
                .skills(input.checkout.root.clone())
                .await?
                .iter()
                .any(|option| &option.skill == skill)
            {
                return Err(AppError::StaleRevision);
            }
        }
        if input.model.is_some() || input.effort.is_some() {
            let models = self.models(input.checkout.root.clone()).await?;
            let current = models
                .iter()
                .find(|model| {
                    input
                        .model
                        .as_ref()
                        .map_or(model.is_default, |value| value == &model.id)
                })
                .ok_or(AppError::StaleRevision)?;
            if input
                .effort
                .as_ref()
                .is_some_and(|value| !current.efforts.contains(value))
            {
                return Err(AppError::StaleRevision);
            }
        }
        progress.check().await
    }
}

fn acknowledged_job(output: &str) -> Option<&str> {
    let mut header = output.lines().next()?.split_whitespace();
    if header.next()? != "backgrounded" || header.next()? != "·" {
        return None;
    }
    let job = header.next()?;
    (job.len() <= 64 && job.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(job)
}
