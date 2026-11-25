use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use reviewq_domain::identity::GitSha;
use reviewq_protocol::GitShaDto;
use reviewq_protocol::actions as dto;
use serde_json::Value;

use super::Failure;
use super::workflow::{self, Context, Profile};

#[derive(Subcommand)]
pub(super) enum Action {
    /// Record the current user's exact publication instruction.
    Declare {
        #[arg(long, value_enum)]
        intent: Intent,
        #[arg(long)]
        reviewed_sha: GitSha,
        #[arg(long, value_name = "JSON")]
        comments_file: Option<PathBuf>,
    },
    /// Recheck the current action against fresh GitLab evidence.
    Reconcile,
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Intent {
    Comments,
    Approval,
    Both,
    Cancel,
}

impl From<Intent> for dto::Intent {
    fn from(value: Intent) -> Self {
        match value {
            Intent::Comments => Self::Comments,
            Intent::Approval => Self::Approval,
            Intent::Both => Self::Both,
            Intent::Cancel => Self::Cancel,
        }
    }
}

pub(super) async fn execute(
    profile: Option<PathBuf>,
    scope: Profile,
    command: Action,
) -> Result<(), Failure> {
    let context = Context::connect(profile, scope, true).await?;
    let (route, body) = match command {
        Action::Declare {
            intent,
            reviewed_sha,
            comments_file,
        } => {
            let comments = comments_file
                .as_deref()
                .map(|path| workflow::json_file(path, 64 * 1024))
                .transpose()?
                .unwrap_or_default();
            let command = dto::DeclareCommand {
                session_key: context.session.clone(),
                call_id: context.call()?,
                turn_id: None,
                request_entry_id: None,
                declaration: dto::Declaration {
                    intent: intent.into(),
                    reviewed_sha: GitShaDto::new(reviewed_sha.as_str())
                        .ok_or("Invalid reviewed SHA.")?,
                    comments,
                },
            };
            (
                "/api/v1/native/actions/declare",
                serde_json::to_vec(&command)?,
            )
        }
        Action::Reconcile => (
            "/api/v1/native/actions/reconcile",
            serde_json::to_vec(&dto::ReconcileCommand {
                session_key: context.session.clone(),
                call_id: context.call()?,
            })?,
        ),
    };
    let reply: Value = context
        .post(route, &body)
        .await
        .map_err(|error| error.report("publication state", &context.session))?;
    workflow::print(&reply)
}
