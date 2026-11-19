use std::collections::BTreeSet;

use reviewq_app::access::AppError;
use reviewq_app::changes::{ApprovalFacts, ApprovalRule, ApprovalView, Approver};
use reviewq_domain::identity::{GitSha, MrIdentity};
use serde::Deserialize;

use super::GitLab;
use crate::settings::safe_label;

const MAX_APPROVAL_BYTES: usize = 512 * 1024;
const MAX_ITEMS: usize = 1_000;

#[derive(Deserialize)]
struct User {
    id: u64,
    username: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ApprovalState {
    project_id: Option<u64>,
    iid: Option<u64>,
    approved: Option<bool>,
    approvals_required: Option<u32>,
    approvals_left: Option<u32>,
    approved_by: Vec<ApprovalActor>,
}

#[derive(Deserialize)]
struct ApprovalActor {
    user: User,
    approved_at: Option<String>,
}

#[derive(Deserialize)]
struct RuleState {
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
struct Rule {
    id: u64,
    name: String,
    approved: bool,
    approvals_required: u32,
    approved_by: Option<Vec<User>>,
}

impl GitLab {
    pub(super) async fn approval_view(
        &self,
        source: MrIdentity,
        expected: Option<GitSha>,
        additional_reviewers: Vec<u64>,
    ) -> Result<ApprovalView, AppError> {
        if source.origin() != &self.identity {
            return Err(AppError::NotFound);
        }
        let before = self
            .merge_request(source.project_id(), source.iid())
            .await?;
        let head = before.head_sha()?;
        if expected.as_ref().is_some_and(|expected| expected != &head) {
            return Err(AppError::Conflict);
        }
        let (approvals, rules, account) = tokio::join!(
            self.read_approval_facts(&source),
            self.read_approval_rules(&source),
            self.approval_account(),
        );
        let approvals = tolerate_unavailable(approvals)?.flatten();
        let rules = tolerate_unavailable(rules)?.flatten();
        let account = tolerate_unavailable(account)?;
        let after = self
            .merge_request(source.project_id(), source.iid())
            .await?;
        if head != after.head_sha()? || before.updated_at != after.updated_at {
            return Err(AppError::StaleRevision);
        }
        let personal_approval = approvals.as_ref().and_then(|facts| {
            if facts.approvers.iter().any(|person| {
                account == Some(person.id) || additional_reviewers.contains(&person.id)
            }) {
                Some(true)
            } else if account.is_some() {
                Some(false)
            } else {
                None
            }
        });
        Ok(ApprovalView {
            head,
            observed_at_ms: jiff::Timestamp::now().as_millisecond(),
            personal_approval,
            approvals,
            rules,
        })
    }

    pub(super) async fn approval_account(&self) -> Result<u64, AppError> {
        if let Some(account) = self.account {
            return Ok(account);
        }
        self.observe_authenticated_actor().await
    }

    pub(in crate::gitlab) async fn observe_authenticated_actor(&self) -> Result<u64, AppError> {
        let (_, body) = self.get(self.url("api/v4/user")?, 32 * 1024).await?;
        let user: User = serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        checked_id(user.id)
    }

    pub(super) async fn read_approval_facts(
        &self,
        source: &MrIdentity,
    ) -> Result<Option<ApprovalFacts>, AppError> {
        let Some(body) = self.approval_response(source, "approvals").await? else {
            return Ok(None);
        };
        let state: ApprovalState =
            serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        if state
            .project_id
            .is_some_and(|id| id != source.project_id().get())
            || state.iid.is_some_and(|id| id != source.iid().get())
        {
            return Err(AppError::Conflict);
        }
        if state.approved_by.len() > MAX_ITEMS {
            return Err(AppError::PayloadTooLarge);
        }
        let mut ids = BTreeSet::new();
        let mut approvers = Vec::with_capacity(state.approved_by.len());
        for row in state.approved_by {
            let id = checked_id(row.user.id)?;
            if !ids.insert(id) {
                return Err(AppError::Unavailable);
            }
            approvers.push(Approver {
                id,
                username: row
                    .user
                    .username
                    .as_deref()
                    .map(|name| safe_label(name, 256))
                    .transpose()?,
                name: row
                    .user
                    .name
                    .as_deref()
                    .map(|name| safe_label(name, 512))
                    .transpose()?,
                approved_at_ms: row
                    .approved_at
                    .as_deref()
                    .map(|time| {
                        time.parse::<jiff::Timestamp>()
                            .map(|time| time.as_millisecond())
                            .map_err(|_| AppError::Unavailable)
                    })
                    .transpose()?,
            });
        }
        Ok(Some(ApprovalFacts {
            approved: state.approved,
            required: state.approvals_required,
            left: state.approvals_left,
            approvers,
        }))
    }

    async fn read_approval_rules(
        &self,
        source: &MrIdentity,
    ) -> Result<Option<Vec<ApprovalRule>>, AppError> {
        let Some(body) = self.approval_response(source, "approval_state").await? else {
            return Ok(None);
        };
        let state: RuleState = serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        if state.rules.len() > MAX_ITEMS {
            return Err(AppError::PayloadTooLarge);
        }
        let mut ids = BTreeSet::new();
        let mut rules = Vec::with_capacity(state.rules.len());
        for rule in state.rules {
            let id = checked_id(rule.id)?;
            if !ids.insert(id) {
                return Err(AppError::Unavailable);
            }
            rules.push(ApprovalRule {
                id,
                name: safe_label(&rule.name, 512)?,
                approved: rule.approved,
                required: rule.approvals_required,
                approved_count: rule.approved_by.map(approved_count).transpose()?,
            });
        }
        Ok(Some(rules))
    }

    async fn approval_response(
        &self,
        source: &MrIdentity,
        leaf: &str,
    ) -> Result<Option<Vec<u8>>, AppError> {
        let url = self.url(&format!(
            "api/v4/projects/{}/merge_requests/{}/{leaf}",
            source.project_id(),
            source.iid()
        ))?;
        tolerate_unavailable(
            self.get(url, MAX_APPROVAL_BYTES)
                .await
                .map(|(_, body)| body),
        )
    }
}

fn approved_count(users: Vec<User>) -> Result<usize, AppError> {
    if users.len() > MAX_ITEMS {
        return Err(AppError::PayloadTooLarge);
    }
    let distinct = users
        .into_iter()
        .map(|user| checked_id(user.id))
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(distinct.len())
}

fn tolerate_unavailable<T>(result: Result<T, AppError>) -> Result<Option<T>, AppError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(AppError::Unavailable | AppError::NotFound | AppError::Forbidden) => Ok(None),
        Err(error) => Err(error),
    }
}

fn checked_id(id: u64) -> Result<u64, AppError> {
    (1..=i64::MAX as u64)
        .contains(&id)
        .then_some(id)
        .ok_or(AppError::Unavailable)
}
