//! The managing tools' lookups over published reports and transcripts: `get_findings`, the
//! version list `get_review` reads with `report.kind` `versions`, and the transcript search of
//! `read_agent_transcript`. Each reads through the store or history read the browser uses, so
//! the actor's scope and the session's history checks are the browser's.

use reviewq_app::access::{Actor, AppError};
use reviewq_app::reports::{
    self as app, Collection, ContextKey, ContextPosition, ContextRequest, Descriptor,
    FindingSelection, ReportPosition,
};
use reviewq_domain::identity::{ReceiptId, Revision, RunId, SessionKey};
use reviewq_protocol::history::Direction;
use reviewq_protocol::mcp::{FindingState, GetFindings};
use reviewq_protocol::reports::FindingKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::operations::{ResolvedReview, alternatives, fit, resolve, select};
use crate::entry::Entry;
use crate::entry::features::reports::{
    project_basis, project_conclusion, project_finding_kind, project_location, project_severity,
    summary,
};
use crate::entry::history;
use crate::entry::pages::parse;
use crate::entry::targets;

/// Findings per page when the call names no limit; a finding with its full text can run to
/// about 40 KiB, so a default page stays near the size of a transcript page.
const DEFAULT_FINDINGS: u16 = 10;
const DEFAULT_VERSIONS: u16 = 10;
const MAX_PAGE: u16 = 25;

/// Where the next page of findings starts: the version the first page read, so a newer
/// version published meanwhile does not shift the list, and the last finding returned.
#[derive(Deserialize, Serialize)]
struct FindingsCursor {
    receipt: String,
    hash: String,
    revision: u64,
    after: String,
}

pub(super) async fn findings(
    entry: &Entry,
    actor: Actor,
    args: GetFindings,
) -> Result<Value, AppError> {
    let (review, target_session) = match resolve(entry, &actor, &args.target).await? {
        ResolvedReview::Review(id, session) => (id, session),
        ResolvedReview::Alternatives(urls) => return Ok(alternatives(urls)),
    };
    let session = targets::session(
        target_session,
        args.session_key.as_ref().map(|key| key.as_str()),
    )?;
    // The run `get_review` would return a report for: the session's, or the lead's.
    let (run, session) = select(entry, &actor, review, session).await?;
    let limit = page_limit(args.limit, DEFAULT_FINDINGS)?;
    let state = args.state.unwrap_or(FindingState::All);
    let selection = FindingSelection {
        state: match state {
            FindingState::Open => app::FindingState::Open,
            FindingState::Retired => app::FindingState::Retired,
            FindingState::All => app::FindingState::All,
        },
        kind: args.kind.map(|kind| match kind {
            FindingKind::Functional => app::FindingKind::Functional,
            FindingKind::Quality => app::FindingKind::Quality,
        }),
        full_text: true,
    };
    let purpose = format!("mcp-findings:{run}:{selection:?}:{limit}");
    let endpoint = &entry.auth.endpoint;
    let after = args
        .cursor
        .as_deref()
        .map(|token| {
            let page: FindingsCursor = endpoint.decode_cursor(purpose.as_bytes(), token)?;
            Ok::<_, AppError>(ContextPosition {
                report: app::ReportReference {
                    receipt_id: parse(&page.receipt)?,
                    hash: parse(&page.hash)?,
                },
                disposition_version: Revision::try_from(page.revision)
                    .map_err(|_| AppError::CursorExpired)?,
                key: ContextKey::Identity(page.after),
            })
        })
        .transpose()?;
    let receipt: Option<ReceiptId> = args.receipt_id.map(|id| parse(id.as_str())).transpose()?;
    let context = entry
        .store
        .read_report_context(
            actor,
            run.clone(),
            ContextRequest {
                receipt,
                collection: Collection::Findings,
                findings: selection,
                after,
                limit,
            },
        )
        .await?
        .value;
    let page = |report: Value, findings: Vec<&Value>, next: Option<String>| {
        json!({
            "runId": run.as_str(),
            "sessionKey": session.as_str(),
            "report": report,
            "findings": findings,
            "nextCursor": next,
        })
    };
    // A run with nothing published has no findings, which is not an error: `get_review` reads
    // its summary as `report: null` too. A receipt the call names that does not exist was
    // refused by the read.
    let Some(report) = context.report else {
        return Ok(page(Value::Null, Vec::new(), None));
    };
    let mut items = Vec::with_capacity(context.items.len());
    for item in context.items {
        let key = item.key();
        let ContextKey::Identity(id) = key else {
            return Err(AppError::Internal);
        };
        items.push((finding(item)?, id));
    }
    let cursor = |id: &String| {
        endpoint.encode_cursor(
            purpose.as_bytes(),
            &FindingsCursor {
                receipt: report.receipt_id.to_string(),
                hash: report.hash.to_string(),
                revision: context.disposition_version.get(),
                after: id.clone(),
            },
        )
    };
    let version = json!({"receiptId": report.receipt_id.as_str(), "sha256": report.hash.as_str()});
    fit_items(&items, context.next.is_some(), cursor, |findings, next| {
        page(version.clone(), findings, next)
    })
}

/// One finding as `get_findings` returns it. `card` is the range of its card in this
/// version's text, in code points, which `get_review` reads with `report.kind` `section`; a
/// retired finding's card is in the parent version.
fn finding(item: Descriptor) -> Result<Value, AppError> {
    let Descriptor::Finding {
        id,
        title,
        severity,
        blocking,
        source,
        details,
        disposition,
        retired,
        outcome,
    } = item
    else {
        return Err(AppError::Internal);
    };
    let details = details.ok_or(AppError::Internal)?;
    Ok(json!({
        "findingId": id,
        "title": title,
        "severity": project_severity(severity),
        "blocking": blocking,
        "kind": project_finding_kind(details.kind),
        "state": if retired { "retired" } else { "open" },
        "outcome": outcome.map(|outcome| json!({
            "conclusion": project_conclusion(outcome.conclusion),
            "reason": outcome.reason,
            "checkedSha": outcome.checked_sha.as_ref().map(|sha| sha.as_str()),
        })),
        "userDecision": disposition.map(|decision| json!({
            "blocking": decision.blocking,
            "publication": decision.publication.map(|value| value.to_string()),
            "reason": decision.reason,
        })),
        "locations": details.locations.into_iter().map(project_location).collect::<Vec<_>>(),
        "issue": details.issue,
        "consequence": details.consequence,
        "evidence": project_basis(details.evidence),
        "proposedComment": details.proposed_comment,
        "replyToNote": details.reply_to_note,
        "card": source.map(|range| json!({"offset": range.offset, "length": range.length})),
    }))
}

/// The run's published versions, newest first, a page at a time, through the history read the
/// browser's version list uses. The run is the one `get_review` selected.
pub(super) async fn versions(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    cursor: Option<String>,
    limit: Option<u16>,
) -> Result<Value, AppError> {
    let limit = page_limit(limit, DEFAULT_VERSIONS)?;
    let purpose = format!("mcp-report-versions:{limit}:{run}");
    let endpoint = &entry.auth.endpoint;
    let after = cursor
        .as_deref()
        .map(|token| {
            let (time, receipt): (i64, String) =
                endpoint.decode_cursor(purpose.as_bytes(), token)?;
            Ok::<_, AppError>(ReportPosition {
                published_at_ms: time,
                receipt_id: parse(&receipt)?,
            })
        })
        .transpose()?;
    let history = entry
        .store
        .read_report_history(actor, run, after, limit)
        .await?
        .value;
    let current = history.current_receipt_id.map(|id| id.to_string());
    let items = history
        .items
        .into_iter()
        .map(|item| {
            let position = (item.published_at_ms, item.reference.receipt_id.to_string());
            let value = serde_json::to_value(summary(item)?).map_err(|_| AppError::Internal)?;
            Ok::<_, AppError>((value, position))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor = |position: &(i64, String)| endpoint.encode_cursor(purpose.as_bytes(), position);
    fit_items(&items, history.next.is_some(), cursor, |versions, next| {
        json!({
            "kind": "versions",
            "value": {
                "currentReceiptId": current,
                "versions": versions,
                "nextCursor": next,
            }
        })
    })
}

/// One page of the transcript search for the selected session, through the search the
/// browser's transcript uses. Each match keeps its `entryId`, which a page read takes as
/// `aroundId`, and the text around it.
pub(super) async fn search(
    entry: &Entry,
    actor: Actor,
    target: (RunId, SessionKey),
    query: &str,
    cursor: Option<&str>,
    direction: Option<Direction>,
) -> Result<Value, AppError> {
    let (run, session) = target;
    let reverse = matches!(direction, Some(Direction::Older));
    let (page, _) = history::search(entry, actor, run, session, query, cursor, reverse).await?;
    let hits: Vec<Value> = page
        .hits
        .iter()
        .map(|hit| {
            json!({
                "entryId": hit.entry_id,
                "before": hit.before,
                "matched": hit.matched,
                "after": hit.after,
            })
        })
        .collect();
    Ok(json!({
        "runId": page.run_id,
        "sessionKey": page.session_key,
        "generation": page.generation,
        "availability": page.availability,
        "hits": hits,
        "nextCursor": page.next_cursor,
    }))
}

fn page_limit(limit: Option<u16>, default: u16) -> Result<u16, AppError> {
    let limit = limit.unwrap_or(default);
    (1..=MAX_PAGE)
        .contains(&limit)
        .then_some(limit)
        .ok_or(AppError::InvalidRequest)
}

/// A cursor page through `fit`: a shortened page continues after its last item, so the
/// continuation is always the position of the last item returned. Each item is
/// `(value, position)`.
fn fit_items<P>(
    items: &[(Value, P)],
    more: bool,
    cursor: impl Fn(&P) -> Result<String, AppError>,
    project: impl Fn(Vec<&Value>, Option<String>) -> Value,
) -> Result<Value, AppError> {
    fit(items.len(), |count| {
        let kept = &items[..count];
        let next = match kept.last() {
            Some((_, position)) if more || count < items.len() => Some(cursor(position)?),
            _ => None,
        };
        Ok(project(kept.iter().map(|(value, _)| value).collect(), next))
    })
}
