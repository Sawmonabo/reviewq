use std::sync::Arc;

use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_domain::settings::{NativeSkill, WorkspaceScope};

use crate::access::{AppError, AppFuture};

pub struct NativeSkillOption {
    pub skill: NativeSkill,
    pub description: Option<String>,
}

impl NativeSkillOption {
    pub fn new(skill: NativeSkill, description: Option<String>) -> Self {
        Self {
            skill,
            description: description.filter(|text| {
                !text.trim().is_empty()
                    && text.len() <= 2048
                    && !text.chars().any(|character| {
                        character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                    })
            }),
        }
    }
}

pub trait NativeSkillReader: Send + Sync {
    fn skills(&self, checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeSkillOption>>;
}

#[derive(Default)]
pub struct NativeSkills {
    pub codex: Option<Arc<dyn NativeSkillReader>>,
    pub claude: Option<Arc<dyn NativeSkillReader>>,
}

impl NativeSkills {
    pub fn reader(&self, provider: HostProvider) -> Result<&dyn NativeSkillReader, AppError> {
        match provider {
            HostProvider::Codex => self.codex.as_deref(),
            HostProvider::Claude => self.claude.as_deref(),
        }
        .ok_or(AppError::Unavailable)
    }

    pub async fn verify(&self, scope: &WorkspaceScope) -> Result<(), AppError> {
        for repository in &scope.repositories {
            for (provider, skill) in [
                (
                    HostProvider::Codex,
                    repository.default_skills.codex.as_ref(),
                ),
                (
                    HostProvider::Claude,
                    repository.default_skills.claude.as_ref(),
                ),
            ] {
                if let Some(skill) = skill {
                    let checkout = repository
                        .checkout
                        .as_ref()
                        .ok_or(AppError::InvalidRequest)?;
                    let skills = self.reader(provider)?.skills(checkout.root.clone()).await?;
                    if !skills.iter().any(|option| &option.skill == skill) {
                        return Err(AppError::NotFound);
                    }
                }
            }
        }
        Ok(())
    }
}
