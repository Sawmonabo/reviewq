use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::queue::{
    RunCard, RunCards, RunCardsFilter, RunCardsPosition, StageCounts, WorkspaceQueue,
};
use reviewq_domain::review_stage::RunStage;
use rusqlite::{Connection, params};

use super::{
    ITEM_COLUMNS, ITEM_FROM, SCOPE_PREDICATE, check_limit, page_context, parse, queue_item, text,
};
use crate::events::snapshot;
use crate::workspace::{ReadScope, scope};
use crate::{SqliteStore, StoreError};

pub(super) const MR_SEARCH: &str = "(:search='' OR instr(reviewq_fold(r.title),:search)>0
      OR instr(reviewq_fold(r.project_path),:search)>0
      OR instr(reviewq_fold(coalesce(r.author_username,'')),:search)>0
      OR EXISTS (SELECT 1 FROM scoped_review_issues a JOIN scoped_issues i USING(board_id,ticket_key)
          WHERE a.review_id=r.review_id AND (instr(reviewq_fold(i.ticket_key),:search)>0 OR instr(reviewq_fold(i.title),:search)>0))
      OR EXISTS (SELECT 1 FROM retained_review_tickets i WHERE i.review_id=r.review_id
          AND (instr(reviewq_fold(i.ticket_key),:search)>0 OR instr(reviewq_fold(i.title),:search)>0)))";
pub(super) const RUN_IDS: &str = "(SELECT json_group_array(run_id) FROM (
    SELECT run_id FROM runs WHERE review_id=r.review_id ORDER BY started_at_ms DESC,run_id DESC LIMIT 25))";

impl WorkspaceQueue for SqliteStore {
    fn run_cards(
        &self,
        actor: Actor,
        filter: RunCardsFilter,
        after: Option<RunCardsPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<RunCards>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |db| snapshot(db, cards(db, actor, filter, after, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn cards(
    db: &Connection,
    actor: Actor,
    filter: RunCardsFilter,
    after: Option<RunCardsPosition>,
    limit: u16,
) -> Result<RunCards, StoreError> {
    if filter.search.chars().count() > 200
        || filter.reason.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
    {
        return Err(AppError::InvalidRequest.into());
    }
    let access = scope(db, &actor)?;
    let (code, owner) = read_scope(&access);
    let context = page_context(
        db,
        &actor,
        limit,
        after.as_ref().map(|position| &position.context),
    )?;
    let search = filter.search.to_lowercase();
    let source = format!("({SCOPE_PREDICATE}) AND f.run_id IS NOT NULL AND {MR_SEARCH}");
    let mut summary = db.prepare_cached(&format!(
        "SELECT substr(f.stage_reason,1,instr(f.stage_reason,'/')-1),count(*)
         {ITEM_FROM} WHERE {source} GROUP BY 1"
    ))?;
    let mut rows = summary.query(params![code, owner, search])?;
    let mut counts = StageCounts::default();
    while let Some(row) = rows.next()? {
        let count = positive(row.get(1)?)?;
        match text(row, 0)? {
            "needs_input" => counts.needs_input = count,
            "working" => counts.working = count,
            "waiting" => counts.waiting = count,
            "archived" => counts.archived = count,
            _ => return Err(AppError::Internal.into()),
        }
    }
    let stage = filter.stage.map(RunStage::as_str);
    let narrowed = format!(
        "{source} AND (?4 IS NULL OR f.stage_reason LIKE ?4||'/%')
        AND (?5 IS NULL OR substr(f.stage_reason,instr(f.stage_reason,'/')+1)=?5)
        AND (?6 IS NULL OR f.has_questions=?6)"
    );
    let total: i64 = db
        .prepare_cached(&format!("SELECT count(*) {ITEM_FROM} WHERE {narrowed}"))?
        .query_row(
            params![
                code,
                owner,
                search,
                stage,
                filter.reason,
                filter.has_questions
            ],
            |row| row.get(0),
        )?;
    let (activity, id) = after.as_ref().map_or((i64::MAX, ""), |position| {
        (position.activity, position.review.as_str())
    });
    let mut statement = db.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS},{RUN_IDS} {ITEM_FROM} WHERE {narrowed}
           AND (f.last_activity<?7 OR (f.last_activity=?7 AND r.review_id>?8))
         ORDER BY f.last_activity DESC,r.review_id LIMIT ?9"
    ))?;
    let mut rows = statement.query(params![
        code,
        owner,
        search,
        stage,
        filter.reason,
        filter.has_questions,
        activity,
        id,
        limit + 1
    ])?;
    let mut selected = Vec::new();
    while let Some(row) = rows.next()? {
        selected.push(RunCard {
            review: queue_item(row)?,
            runs: run_ids(text(row, 33)?)?,
        });
    }
    let next = if selected.len() > usize::from(limit) {
        selected.pop();
        selected.last().map(|item| RunCardsPosition {
            context,
            activity: item.review.signals.last_activity_ms,
            review: item.review.review_id.clone(),
        })
    } else {
        None
    };
    Ok(RunCards {
        items: selected,
        next,
        counts,
        total: positive(total)?,
    })
}

pub(super) fn run_ids(value: &str) -> Result<Vec<reviewq_domain::identity::RunId>, StoreError> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|_| AppError::Internal)?
        .iter()
        .map(|id| parse(id))
        .collect()
}

pub(super) fn read_scope(access: &ReadScope) -> (i64, Option<&str>) {
    match access {
        ReadScope::Local => (0, None),
        ReadScope::Grant => (1, None),
        ReadScope::Review(review) => (2, Some(review.as_str())),
    }
}

fn positive(value: i64) -> Result<u64, StoreError> {
    value.try_into().map_err(|_| AppError::Internal.into())
}
