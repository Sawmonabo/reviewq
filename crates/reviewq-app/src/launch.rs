//! Exact launch inputs prepared against one verified MR, checkout and catalogue.

use std::sync::Arc;

use reviewq_domain::identity::{ReviewId, Revision};
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_domain::settings::{CheckoutBinding, NativeSkill};

use crate::settings::NativeSkillOption;

use crate::access::{AppError, AppFuture};
use crate::queue::{MergeRequestState, ReviewSource};

mod effects;
pub use effects::{
    ClaudeLaunchLocator, LaunchClaim, LaunchProgress, LaunchReceipt, LaunchState, LaunchStore,
    NativeLaunchResult, NativeLauncher, NativeLaunchers, ObservedLaunchSettings, run_launch,
};

pub trait NativeModelReader: Send + Sync {
    fn models(&self, checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeModel>>;
}

#[derive(Default)]
pub struct NativeModels {
    pub codex: Option<Arc<dyn NativeModelReader>>,
    pub claude: Option<Arc<dyn NativeModelReader>>,
}

impl NativeModels {
    pub fn reader(&self, provider: HostProvider) -> Result<&dyn NativeModelReader, AppError> {
        match provider {
            HostProvider::Codex => self.codex.as_deref(),
            HostProvider::Claude => self.claude.as_deref(),
        }
        .ok_or(AppError::Unavailable)
    }
}

pub struct NativeModel {
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub default_effort: Option<String>,
    pub efforts: Vec<String>,
}

pub struct LaunchSelection {
    pub invocation: Option<InvocationDraft>,
    pub context: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub enum InvocationDraft {
    Skill {
        name: String,
        path: Option<AbsolutePath>,
        args: Option<String>,
    },
    Custom {
        prompt: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invocation {
    Skill {
        skill: NativeSkill,
        args: Option<String>,
    },
    Custom {
        prompt: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchDetails {
    pub source: reviewq_domain::identity::GitSha,
    pub state: LaunchState,
    pub invocation: Invocation,
    pub context: Option<String>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub observed_model: Option<String>,
    pub observed_effort: Option<String>,
}

/// Independently recorded request metadata scoped to the exact lead of a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestEvidence {
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub skill_name: String,
    pub input: Option<RequestInput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestInput {
    pub slot: RequestInputSlot,
    pub turn: reviewq_domain::identity::NativeTurnId,
    pub entry: reviewq_domain::identity::NativeInputId,
    pub generation: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestInputSlot {
    ReviewRequest,
    SkillArguments,
}

#[derive(Clone)]
pub struct LaunchInput {
    pub review: ReviewId,
    pub prior_source: ReviewSource,
    pub source: ReviewSource,
    pub settings: Revision,
    pub provider: HostProvider,
    pub checkout: CheckoutBinding,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub invocation: Invocation,
    pub context: Option<String>,
}

impl LaunchInput {
    pub fn prompt(&self) -> String {
        let mut text = match &self.invocation {
            Invocation::Custom { prompt } => prompt.clone(),
            Invocation::Skill { skill, args } => match self.provider {
                HostProvider::Codex => args.clone().unwrap_or_default(),
                HostProvider::Claude => match args {
                    Some(args) => format!("/{} {}", skill.name, args),
                    None => format!("/{}", skill.name),
                },
            },
        };
        if let Some(context) = &self.context {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(context);
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!(
            "Review {}!{} at {}. ",
            self.source.project_path,
            self.source.identity.iid(),
            self.source.head_sha,
        ));
        text.push_str(report_guidance());
        text
    }
}

pub struct LaunchPreparation {
    pub review: ReviewId,
    pub prior_source: ReviewSource,
    pub source: ReviewSource,
    pub settings: Revision,
    pub provider: HostProvider,
    pub checkout: CheckoutBinding,
    pub default_skill: Option<NativeSkill>,
    pub models: Vec<NativeModel>,
    pub skills: Vec<NativeSkillOption>,
}

impl LaunchPreparation {
    pub fn select(self, selection: LaunchSelection) -> Result<LaunchInput, AppError> {
        if self.source.state != MergeRequestState::Open {
            return Err(AppError::Conflict);
        }
        validate_text(selection.context.as_deref(), 16 * 1024, true)?;
        validate_text(selection.model.as_deref(), 256, false)?;
        validate_text(selection.effort.as_deref(), 64, false)?;
        select_model(
            &self.models,
            selection.model.as_deref(),
            selection.effort.as_deref(),
        )?;
        let invocation = match selection.invocation {
            Some(InvocationDraft::Custom { prompt }) => {
                validate_text(Some(&prompt), 48 * 1024, true)?;
                Invocation::Custom { prompt }
            }
            Some(InvocationDraft::Skill { name, path, args }) => {
                validate_text(Some(&name), 256, false)?;
                validate_text(args.as_deref(), 16 * 1024, true)?;
                let mut available = self.skills.iter().map(|option| &option.skill).filter(|skill| {
                    skill.name == name && path.as_ref().is_none_or(|path| *path == skill.path)
                });
                let skill = available.next().ok_or(AppError::NotFound)?.clone();
                if available.next().is_some() {
                    return Err(AppError::Conflict);
                }
                Invocation::Skill { skill, args }
            }
            None => match self.default_skill {
                Some(skill) if self.skills.iter().any(|option| option.skill == skill) => Invocation::Skill { skill, args: None },
                Some(_) => return Err(AppError::StaleRevision),
                None => Invocation::Custom { prompt: "Review the selected merge request and explain the evidence for your findings.".into() },
            },
        };
        Ok(LaunchInput {
            review: self.review,
            prior_source: self.prior_source,
            source: self.source,
            settings: self.settings,
            provider: self.provider,
            checkout: self.checkout,
            model: selection.model,
            effort: selection.effort,
            invocation,
            context: selection.context,
        })
    }
}

pub fn report_guidance() -> &'static str {
    concat!(
        "Follow the repository's review instructions and keep its requested output. ",
        "When finished, submit once with ReviewQ's report_handoff tool from this linked lead conversation. ",
        "Use the tool schema to map your own conclusions to its finding fields. ",
        "Use an empty findings list for a clean review. A pending receipt will be verified after the native turn completes. ",
        "GitLab publication requires separate explicit human authorization.\nExample handoff:\n",
        include_str!("../../../fixtures/contracts/report-handoff.json"),
    )
}

fn select_model(
    models: &[NativeModel],
    requested: Option<&str>,
    effort: Option<&str>,
) -> Result<(), AppError> {
    let matches = |model: &&NativeModel| requested.map_or(model.is_default, |id| model.id == id);
    if requested.is_none() && effort.is_none() {
        return Ok(());
    }
    let mut choices = models.iter().filter(matches);
    let model = choices.next().ok_or(AppError::NotFound)?;
    if choices.next().is_some()
        || effort.is_some_and(|wanted| !model.efforts.iter().any(|advertised| advertised == wanted))
    {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}

fn validate_text(text: Option<&str>, limit: usize, multiline: bool) -> Result<(), AppError> {
    if text.is_some_and(|text| {
        text.trim().is_empty()
            || text.len() > limit
            || text
                .chars()
                .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
    }) {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}
