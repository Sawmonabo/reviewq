use std::collections::HashSet;
use std::path::PathBuf;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::launch::{NativeModel, NativeModelReader};
use reviewq_app::settings::{NativeSkillOption, NativeSkillReader};
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::NativeSkill;
use serde::Deserialize;
use serde_json::json;

use super::Codex;

const MAX_SKILLS: usize = 512;
const MODEL_PAGE: usize = 50;
const MAX_MODEL_PAGES: usize = 8;

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
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Models {
    data: Vec<Model>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    model: String,
    display_name: String,
    is_default: bool,
    hidden: bool,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<Effort>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Effort {
    reasoning_effort: String,
}

impl NativeModelReader for Codex {
    fn models(&self, _checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeModel>> {
        Box::pin(async move {
            let mut models = Vec::new();
            let mut seen = HashSet::new();
            let mut cursors = HashSet::new();
            let mut cursor = None;
            for _ in 0..MAX_MODEL_PAGES {
                let page: Models = self
                    .connection
                    .read(
                        "model/list",
                        json!({"cursor":cursor,"limit":MODEL_PAGE,"includeHidden":false}),
                    )
                    .await?;
                if page.data.len() > MODEL_PAGE
                    || (page.next_cursor.is_some() && page.data.is_empty())
                {
                    return Err(AppError::Unavailable);
                }
                for row in page.data.into_iter().filter(|row| !row.hidden) {
                    if !visible(&row.model, 256)
                        || !visible(&row.display_name, 512)
                        || !seen.insert(row.model.clone())
                        || row.supported_reasoning_efforts.len() > 8
                    {
                        return Err(AppError::Unavailable);
                    }
                    let efforts: Vec<_> = row
                        .supported_reasoning_efforts
                        .into_iter()
                        .map(|effort| effort.reasoning_effort)
                        .collect();
                    if efforts.iter().any(|effort| !visible(effort, 64))
                        || !visible(&row.default_reasoning_effort, 64)
                    {
                        return Err(AppError::Unavailable);
                    }
                    models.push(NativeModel {
                        id: row.model,
                        label: row.display_name,
                        is_default: row.is_default,
                        default_effort: Some(row.default_reasoning_effort),
                        efforts,
                    });
                }
                let Some(next) = page.next_cursor else {
                    return Ok(models);
                };
                if !visible(&next, 4096) || !cursors.insert(next.clone()) {
                    return Err(AppError::Conflict);
                }
                cursor = Some(next);
            }
            Err(AppError::PayloadTooLarge)
        })
    }
}

fn visible(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
}

impl NativeSkillReader for Codex {
    fn skills(&self, checkout: AbsolutePath) -> AppFuture<'_, Vec<NativeSkillOption>> {
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
                    result.push(NativeSkillOption::new(
                        NativeSkill {
                            name: skill.name,
                            path,
                        },
                        skill.description,
                    ));
                }
            }
            Ok(result)
        })
    }
}
