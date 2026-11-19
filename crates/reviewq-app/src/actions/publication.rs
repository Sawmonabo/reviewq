use std::collections::HashSet;

use reviewq_domain::identity::{GitSha, MrIdentity, ReportHash, Revision};
use reviewq_domain::session::LeadTarget;

use crate::access::{AppError, AppFuture};
use crate::events::Snapshot;
use crate::queue::MergeRequestState;
use crate::sources::{ReviewSourceReader, WorkflowSnapshot};
use crate::workflow::{NativeCall, NativeWorkflowProof, WorkflowTool};

use super::{Action, Approval, Evidence, Intent};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentTarget {
    pub hash: ReportHash,
    pub discussion: Option<String>,
}

#[derive(Clone)]
pub struct ActionDeclaration {
    pub intent: Intent,
    pub reviewed_sha: GitSha,
    pub targets: Vec<CommentTarget>,
}

impl ActionDeclaration {
    pub fn validate(&self) -> Result<(), AppError> {
        let requires_comments = matches!(self.intent, Intent::Comments | Intent::Both);
        if requires_comments == self.targets.is_empty() || self.targets.len() > 50 {
            return Err(AppError::InvalidRequest);
        }
        let mut distinct = HashSet::new();
        for target in &self.targets {
            if target.discussion.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
            }) || !distinct.insert((&target.hash, target.discussion.as_deref()))
            {
                return Err(AppError::InvalidRequest);
            }
        }
        Ok(())
    }
}

pub struct ActionWork {
    pub target: LeadTarget,
    pub identity: MrIdentity,
    pub action: Action,
    pub authenticated_actor: u64,
    pub settings: Revision,
    pub declared_at_ms: i64,
}

pub struct TargetObservation {
    pub state: Evidence,
    pub note_id: Option<String>,
}

pub struct PublicationEvidence {
    pub actor: u64,
    pub auth_head: GitSha,
    pub checked_at_ms: i64,
    pub approval: Approval,
    pub open: bool,
    pub targets: Vec<TargetObservation>,
}

pub trait ActionCommandStore: Send + Sync {
    fn action_source(&self, lead: LeadTarget) -> AppFuture<'_, (MrIdentity, Revision)>;

    fn declared_action(
        &self,
        call: NativeCall,
        digest: ReportHash,
    ) -> AppFuture<'_, Option<Snapshot<Action>>>;

    fn declare_action(
        &self,
        call: NativeCall,
        input: ActionDeclaration,
        digest: ReportHash,
        observation: WorkflowSnapshot,
        settings: Revision,
    ) -> AppFuture<'_, Snapshot<Action>>;

    fn current_action_work(&self, lead: LeadTarget) -> AppFuture<'_, Option<ActionWork>>;

    fn record_action_evidence(
        &self,
        work: ActionWork,
        evidence: PublicationEvidence,
        superseded_request: bool,
    ) -> AppFuture<'_, Snapshot<Action>>;
}

pub async fn declare_publication(
    store: &(impl ActionCommandStore + ?Sized),
    proof: &dyn NativeWorkflowProof,
    source: &dyn ReviewSourceReader,
    call: NativeCall,
    input: ActionDeclaration,
    digest: ReportHash,
) -> Result<Snapshot<Action>, AppError> {
    input.validate()?;
    let request_entry = call.request_entry_id.as_ref().ok_or(AppError::Conflict)?;
    if request_entry.as_str().is_empty() || call.tool != WorkflowTool::Action {
        return Err(AppError::Forbidden);
    }
    if let Some(previous) = store.declared_action(call.clone(), digest.clone()).await? {
        return Ok(previous);
    }
    let (identity, settings) = store.action_source(call.lead.clone()).await?;
    let observed = source.workflow_snapshot(identity.clone()).await?;
    if observed.review.identity != identity || observed.review.head_sha != input.reviewed_sha {
        return Err(AppError::StaleRevision);
    }
    if observed.review.state != MergeRequestState::Open {
        return Err(AppError::Conflict);
    }
    for target in &input.targets {
        if let Some(discussion) = &target.discussion
            && !observed.discussions.iter().any(|row| row.id == *discussion)
        {
            return Err(AppError::NotFound);
        }
    }
    let current = proof
        .verify_active_call(
            call.lead.clone(),
            Some(call.turn_id.clone()),
            call.call_id.clone(),
            WorkflowTool::Action,
        )
        .await?;
    if current != call {
        return Err(AppError::Conflict);
    }
    store
        .declare_action(call, input, digest, observed, settings)
        .await
}

pub async fn reconcile_publication(
    store: &(impl ActionCommandStore + ?Sized),
    proof: &dyn NativeWorkflowProof,
    source: &dyn ReviewSourceReader,
    call: NativeCall,
) -> Result<Option<Snapshot<Action>>, AppError> {
    let Some(work) = store.current_action_work(call.lead.clone()).await? else {
        return Ok(None);
    };
    let snapshot = source.workflow_snapshot(work.identity.clone()).await?;
    if snapshot.review.identity != work.identity
        || snapshot.authenticated_actor != work.authenticated_actor
    {
        return Err(AppError::Conflict);
    }
    let current = proof
        .verify_active_call(
            call.lead.clone(),
            Some(call.turn_id.clone()),
            call.call_id.clone(),
            WorkflowTool::Action,
        )
        .await?;
    if current != call {
        return Err(AppError::Conflict);
    }
    let changed_request = call
        .request_entry_id
        .as_ref()
        .is_some_and(|entry| Some(entry) != work.action.request_entry.as_ref());
    let evidence = publication_observation(&work, snapshot);
    store
        .record_action_evidence(work, evidence, changed_request)
        .await
        .map(Some)
}

pub(super) fn publication_observation(
    work: &ActionWork,
    snapshot: WorkflowSnapshot,
) -> PublicationEvidence {
    let approved = if !matches!(work.action.intent, Intent::Approval | Intent::Both) {
        Approval::NotRequested
    } else {
        match snapshot.personal_approval {
            Some(true) => Approval::Confirmed,
            Some(false) => Approval::Absent,
            None => Approval::Unknown,
        }
    };
    let mut used: HashSet<String> = work
        .action
        .targets
        .iter()
        .filter_map(|target| target.note_id.clone())
        .collect();
    let mut targets = Vec::with_capacity(work.action.targets.len());
    let mut all_notes: Vec<_> = snapshot
        .discussions
        .iter()
        .flat_map(|thread| thread.notes.iter().map(move |note| (thread, note)))
        .collect();
    all_notes.sort_unstable_by_key(|(_, note)| note.id);
    for target in &work.action.targets {
        if target.evidence == Evidence::Confirmed {
            targets.push(TargetObservation {
                state: Evidence::Confirmed,
                note_id: target.note_id.clone(),
            });
            continue;
        }
        let mut matches = all_notes.iter().filter_map(|(discussion, note)| {
            let scope = match &target.discussion_id {
                Some(id) => &discussion.id == id,
                None => discussion.individual_note == Some(true),
            };
            let head = match &note.positioned_head {
                Some(sha) => work.action.reviewed_sha.as_ref() == Some(sha),
                None => note.created_at_ms >= work.declared_at_ms,
            };
            (scope
                && head
                && note.author == snapshot.authenticated_actor
                && note.body_hash == target.body_sha256
                && !used.contains(&note.id.to_string()))
            .then_some(note.id)
        });
        let value = match (matches.next(), matches.next()) {
            (Some(note), None) => {
                let note = note.to_string();
                used.insert(note.clone());
                TargetObservation {
                    state: Evidence::Confirmed,
                    note_id: Some(note),
                }
            }
            (None, _) => TargetObservation {
                state: Evidence::Absent,
                note_id: None,
            },
            _ => TargetObservation {
                state: Evidence::Unknown,
                note_id: None,
            },
        };
        targets.push(value);
    }
    PublicationEvidence {
        actor: snapshot.authenticated_actor,
        auth_head: snapshot.review.head_sha,
        approval: approved,
        checked_at_ms: snapshot.observed_at_ms,
        open: snapshot.review.state == MergeRequestState::Open,
        targets,
    }
}
