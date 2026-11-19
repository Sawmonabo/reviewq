use std::collections::HashSet;
use std::path::PathBuf;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::settings::NativeSkillReader;
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::NativeSkill;
use serde::Deserialize;
use serde_json::json;

use super::Codex;

const MAX_SKILLS: usize = 512;

#[derive(Deserialize)]
struct Catalog {
    data: Vec<CheckoutSkills>,
}

#[derive(Deserialize)]
struct CheckoutSkills {
    cwd: PathBuf,
    skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct Skill {
    name: String,
    path: PathBuf,
    enabled: bool,
}

impl NativeSkillReader for Codex {
    fn skills(&self, checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeSkill>> {
        Box::pin(async move {
            let reply: Catalog = self
                .connection
                .read(
                    "skills/list",
                    json!({"cwds":[checkout.as_path()],"forceReload":true}),
                )
                .await?;
            let [selected] = reply.data.as_slice() else {
                return Err(AppError::Unavailable);
            };
            if selected.cwd != checkout.as_path() {
                return Err(AppError::Conflict);
            }
            if selected.skills.len() > MAX_SKILLS {
                return Err(AppError::PayloadTooLarge);
            }
            let mut seen = HashSet::new();
            let mut result = Vec::new();
            for skill in reply
                .data
                .into_iter()
                .flat_map(|entry| entry.skills)
                .filter(|skill| skill.enabled)
            {
                if skill.name.is_empty()
                    || skill.name.len() > 256
                    || skill.name.chars().any(char::is_control)
                    || skill
                        .path
                        .to_str()
                        .is_none_or(|path| path.len() > 4096 || path.contains('\0'))
                {
                    return Err(AppError::Unavailable);
                }
                let path =
                    AbsolutePath::from_canonical(skill.path).map_err(|_| AppError::Unavailable)?;
                if seen.insert((skill.name.clone(), path.clone())) {
                    result.push(NativeSkill {
                        name: skill.name,
                        path,
                    });
                }
            }
            Ok(result)
        })
    }
}
