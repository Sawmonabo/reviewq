use reviewq_app::access::{Actor, AppError};
use reviewq_app::queue::{
    CompactTicket, QueueFacet, QueueFacets, QueueFilter, QueueListItem, QueueOrder, QueueOwnership,
    QueuePage, QueuePhase, QueuePosition, QueueSortKey, SprintFilter, SprintPair, UnmatchedTicket,
};
use rusqlite::{Connection, ToSql, named_params};

use super::workspace::{MR_SEARCH, read_scope};
use super::{ITEM_COLUMNS, SCOPE_PREDICATE, page_context, parse, queue_item, text};
use crate::StoreError;
use crate::workspace::{scope, settings_revision};

pub(super) fn queue(
    db: &Connection,
    actor: Actor,
    filter: QueueFilter,
    after: Option<QueuePosition>,
    limit: u16,
) -> Result<QueuePage, StoreError> {
    if filter.boards.len() > 128
        || filter.projects.len() > 128
        || filter.pairs.len() > 32
        || filter.search.chars().count() > 200
    {
        return Err(AppError::InvalidRequest.into());
    }
    let access = scope(db, &actor)?;
    let (scope_code, scope_id) = read_scope(&access);
    let context = page_context(db, &actor, limit, after.as_ref().map(|row| &row.context))?;
    let boards = serde_json::to_string(&filter.boards).map_err(|_| AppError::Internal)?;
    let projects = serde_json::to_string(&filter.projects).map_err(|_| AppError::Internal)?;
    let sprint_pairs = filter
        .pairs
        .iter()
        .map(|pair| [pair.board, pair.sprint])
        .collect::<Vec<_>>();
    let pairs = serde_json::to_string(&sprint_pairs).map_err(|_| AppError::Internal)?;
    let default_scope = filter.boards.is_empty();
    let author = filter
        .author
        .map(i64::try_from)
        .transpose()
        .map_err(|_| AppError::InvalidRequest)?;
    let search = filter.search.to_lowercase();
    let sprint = match filter.sprint {
        SprintFilter::Active => "active",
        SprintFilter::Any => "any",
        SprintFilter::None => "none",
        SprintFilter::Selected => "selected",
    };
    let owner = match filter.ownership {
        QueueOwnership::All => "all",
        QueueOwnership::Mine => "mine",
        QueueOwnership::Others => "others",
    };
    let phase = match filter.phase {
        QueuePhase::AllOpen => "all_open",
        QueuePhase::NeedsReview => "needs_review",
        QueuePhase::Followup => "followup",
        QueuePhase::WaitingAuthor => "waiting_author",
        QueuePhase::Reviewed => "reviewed",
        QueuePhase::Archive => "archive",
    };
    let order = match filter.order {
        QueueOrder::Recent => "recent",
        QueueOrder::Oldest => "oldest",
        QueueOrder::Author => "author",
        QueueOrder::Ticket => "ticket",
    };
    let shape = include_str!("filters.sql")
        .replace(
            "__SCOPE__",
            &SCOPE_PREDICATE
                .replace("?1", ":scope_code")
                .replace("?2", ":scope_id"),
        )
        .replace("__MR_SEARCH__", MR_SEARCH);
    let shared = named_params! {
        ":scope_code":scope_code,":scope_id":scope_id,":boards":boards,":projects":projects,
        ":pairs":pairs,":default_scope":default_scope,":empty_scope":filter.empty_scope,
        ":sprint":sprint,":search":search,":owner":owner,":author":author,":phase":phase,
        ":order":order,":hide_reviewed":filter.hide_reviewed,":hide_in_progress":filter.hide_in_progress,
    };
    let (total, total_mrs, hide_reviewed, hide_in_progress) = db
        .prepare_cached(&format!(
        "{shape} SELECT (SELECT count(*) FROM visible),(SELECT count(*) FROM visible WHERE kind=0),
           (SELECT count(*) FROM candidates WHERE hide_reviewed=1),
           (SELECT count(*) FROM candidates WHERE hide_in_progress=1)"
    ))?
        .query_row(shared, |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
    let facets = read_facets(
        db,
        &shape,
        shared,
        positive(hide_reviewed)?,
        positive(hide_in_progress)?,
    )?;
    let first = QueueSortKey {
        group: -1,
        time: 0,
        label: String::new(),
        digits: 0,
        number: String::new(),
        id: String::new(),
    };
    let key = after.as_ref().map_or(&first, |row| &row.key);
    let wanted = limit + 1;
    let page = named_params! {
        ":group":key.group,":time":key.time,":label":key.label,":digits":key.digits,
        ":number":key.number,":id":key.id,":limit":wanted,
    };
    let mut args = shared.to_vec();
    args.extend_from_slice(page);
    let mut statement=db.prepare_cached(&format!(
        "{shape}, page AS (
           SELECT * FROM visible WHERE (sort_group,sort_time,sort_label,sort_digits,sort_number,id)
                                    > (:group,:time,:label,:digits,:number,:id)
           ORDER BY sort_group,sort_time,sort_label,sort_digits,sort_number,id LIMIT :limit)
         SELECT {ITEM_COLUMNS},p.kind,p.sort_group,p.sort_time,p.sort_label,p.sort_digits,p.sort_number,p.id,
                i.title,i.status,i.assignee,i.priority
         FROM page p LEFT JOIN reviews r ON p.kind=0 AND r.review_id=p.id
         LEFT JOIN review_decisions d ON d.review_id=r.review_id
         LEFT JOIN review_activity f ON f.review_id=r.review_id
         LEFT JOIN scoped_issues i ON p.kind=1 AND (i.board_id,i.ticket_key)=(p.board_id,p.id)
         ORDER BY p.sort_group,p.sort_time,p.sort_label,p.sort_digits,p.sort_number,p.id"
    ))?;
    let mut rows = statement.query(args.as_slice())?;
    let mut items = Vec::new();
    let mut next_key = None;
    let mut more = false;
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            more = true;
            break;
        }
        let id: String = row.get(39)?;
        let kind: i64 = row.get(33)?;
        let item = if kind == 0 {
            QueueListItem::Review(Box::new(queue_item(row)?))
        } else {
            QueueListItem::Unmatched(UnmatchedTicket {
                ticket: CompactTicket {
                    key: parse(&id)?,
                    title: row.get(40)?,
                },
                status: row.get(41)?,
                assignee: row.get(42)?,
                priority: row.get(43)?,
            })
        };
        next_key = Some(QueueSortKey {
            group: row.get(34)?,
            time: row.get(35)?,
            label: row.get(36)?,
            digits: row.get(37)?,
            number: row.get(38)?,
            id,
        });
        items.push(item);
    }
    let revision = settings_revision(db)?;
    Ok(QueuePage {
        items,
        next: next_key
            .filter(|_| more)
            .map(|key| QueuePosition { context, key }),
        total: positive(total)?,
        total_mrs: positive(total_mrs)?,
        facets,
        settings_revision: revision,
        sync: crate::refresh::read_sync(db, revision)?,
    })
}

fn read_facets(
    db: &Connection,
    shape: &str,
    args: &[(&str, &dyn ToSql)],
    hide_reviewed: u64,
    hide_in_progress: u64,
) -> Result<QueueFacets, StoreError> {
    let boards = facets(
        db,
        "SELECT board_id,label FROM settings_boards WHERE enabled=1 ORDER BY label,board_id",
        &[],
    )?;
    let projects=facets(db,"SELECT p.project_id,coalesce(s.path,
        (SELECT r.project_path FROM reviews r WHERE (r.origin,r.project_id)=(p.origin,p.project_id) LIMIT 1),
        CAST(p.project_id AS TEXT))
        FROM configured_projects p LEFT JOIN settings_repositories s USING(project_id)
        ORDER BY 2,p.project_id",&[])?;
    let authors=facets(db,&format!("{shape} SELECT author_id,min(author_username)
        FROM base WHERE author_id IS NOT NULL GROUP BY author_id ORDER BY reviewq_fold(min(author_username)),author_id"),args)?;
    let mut query = db.prepare_cached(
        "SELECT s.board_id,s.sprint_id,s.label FROM (
           SELECT board_id,sprint_id,label FROM scoped_sprints
           UNION ALL SELECT l.board_id,l.sprint_id,l.label FROM loaded_sprints l WHERE NOT EXISTS (
             SELECT 1 FROM scoped_sprints s WHERE (s.board_id,s.sprint_id)=(l.board_id,l.sprint_id))
         ) s JOIN settings_boards b USING(board_id)
         WHERE b.enabled=1 ORDER BY b.label,s.label,s.board_id,s.sprint_id",
    )?;
    let mut rows = query.query([])?;
    let mut sprints = Vec::new();
    while let Some(row) = rows.next()? {
        let pair = SprintPair {
            board: positive(row.get(0)?)?,
            sprint: positive(row.get(1)?)?,
        };
        sprints.push((pair, row.get(2)?));
    }
    Ok(QueueFacets {
        boards,
        projects,
        sprints,
        authors,
        hide_reviewed,
        hide_in_progress,
    })
}

fn facets(
    db: &Connection,
    sql: &str,
    args: &[(&str, &dyn ToSql)],
) -> Result<Vec<QueueFacet>, StoreError> {
    let mut query = db.prepare_cached(sql)?;
    let mut rows = query.query(args)?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        values.push(QueueFacet {
            id: positive(row.get(0)?)?,
            label: text(row, 1)?.to_owned(),
        });
    }
    Ok(values)
}

fn positive(value: i64) -> Result<u64, StoreError> {
    value.try_into().map_err(|_| AppError::Internal.into())
}
