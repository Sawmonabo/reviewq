use std::process::Stdio;
use std::time::Duration;

use reviewq_app::access::AppError;
use reviewq_app::hosts::HostProof;
use reviewq_app::launch::{
    Invocation, LaunchInput, LaunchProgress, NativeLaunchResult, NativeLauncher, NativeModelReader,
};
use reviewq_domain::identity::NativeSessionId;
use reviewq_domain::session::{HostProvider, NativeBinding, NativeIdentity};
use tokio::io::AsyncReadExt;

use super::super::jobs::{JobKind, claude_job};
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
            let reservation = progress.id().to_string();
            let Ok(native): Result<NativeSessionId, _> = reservation.parse() else {
                return NativeLaunchResult::Rejected;
            };
            let cwd = input.checkout.root.as_path();
            let mut command = self.command(cwd);
            let title = format!("ReviewQ · !{}", input.source.identity.iid());
            command
                .args(["--bg", "--session-id", &reservation, "--name", &title])
                .stdin(Stdio::null());
            if let Some(model) = &input.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &input.effort {
                command.args(["--effort", effort]);
            }
            self.plugin_args(&mut command);
            command.arg("--").arg(input.prompt());

            if progress.check().await.is_err() {
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
            let mut observed_job = None;
            for delay in [0, 150, 300, 600] {
                if delay != 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                match claude_job(&self.executable, self.profile.as_path(), cwd, &reservation).await
                {
                    Ok(Some(job)) if matches!(job.kind, JobKind::Background) => {
                        if let Some(id) = job.id {
                            observed_job = Some(id);
                            break;
                        }
                    }
                    Err(AppError::Conflict) => return NativeLaunchResult::Uncertain,
                    _ => {}
                }
            }
            let Some(job) = observed_job else {
                return NativeLaunchResult::Uncertain;
            };
            let acknowledged = acknowledgement.is_some_and(|receipt| {
                receipt
                    .split(|value: char| !value.is_ascii_alphanumeric())
                    .any(|token| token == job)
            });
            let identity = NativeIdentity::new(HostProvider::Claude, self.profile.clone(), native);
            let binding = NativeBinding::new(identity, input.checkout.root);
            let probe = binding.clone();
            let source = tokio::task::spawn_blocking(move || {
                verified_claude_source(&probe, None, &std::sync::atomic::AtomicBool::new(false))
            })
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
            let proof = HostProof {
                binding,
                source,
                activity: None,
            };
            if progress
                .record_observed_native(proof.clone(), Some(job))
                .await
                .is_err()
            {
                return NativeLaunchResult::Uncertain;
            }
            if progress.register(proof, None).await.is_ok() && acknowledged {
                NativeLaunchResult::Confirmed
            } else {
                NativeLaunchResult::Uncertain
            }
        })
    }
}

impl ClaudeCli {
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
                .contains(skill)
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
