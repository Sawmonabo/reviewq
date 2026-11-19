use reviewq_app::access::AppError;
use reviewq_app::queue::ReviewSource;
use reviewq_app::sources::SourceObservation;
use rusqlite::{Connection, params};

use crate::StoreError;
use crate::queue::{author_number, state_label};

pub(super) fn initialize(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch(include_str!("stage.sql"))?;
    Ok(())
}

pub(super) fn load(db: &Connection, source: &SourceObservation) -> Result<(), StoreError> {
    let mut mrs = db
        .prepare_cached("INSERT INTO refresh_mrs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")?;
    let mut attach = db.prepare_cached("INSERT INTO refresh_links VALUES(?1,?2,?3,?4,?5)")?;
    let mut save_review = |row: &ReviewSource, discovered: bool| -> Result<(), StoreError> {
        mrs.execute(params![
            row.identity.origin().as_str(),
            row.identity.project_id().as_sqlite_integer(),
            row.identity.iid().as_sqlite_integer(),
            row.project_path,
            row.title,
            row.head_sha.as_str(),
            state_label(row.state),
            row.updated_at_ms,
            author_number(row)?,
            row.author.as_ref().map(|author| author.username.as_str()),
            row.draft,
            discovered,
        ])?;
        Ok(())
    };
    for (row, links) in source.reviews.values() {
        save_review(row, true)?;
        for link in links {
            attach.execute(params![
                row.identity.origin().as_str(),
                row.identity.project_id().as_sqlite_integer(),
                row.identity.iid().as_sqlite_integer(),
                number(link.board)?,
                link.key.as_str(),
            ])?;
        }
    }
    for row in &source.owned {
        save_review(row, false)?;
    }
    let mut sprint = db.prepare_cached("INSERT INTO refresh_sprints VALUES(?1,?2,?3,?4)")?;
    for row in &source.sprints {
        sprint.execute(params![
            number(row.board)?,
            number(row.id)?,
            row.label,
            row.active
        ])?;
    }
    let mut issue =
        db.prepare_cached("INSERT INTO refresh_issues VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?;
    let mut membership = db.prepare_cached("INSERT INTO refresh_membership VALUES(?1,?2,?3)")?;
    for row in &source.issues {
        let board = number(row.board)?;
        let ticket = &row.ticket;
        issue.execute(params![
            board,
            ticket.key.as_str(),
            ticket.title,
            ticket.project,
            number(ticket.status_id)?,
            ticket.status,
            ticket.assignee,
            ticket.priority,
            ticket.estimate,
            ticket.estimate_warning,
        ])?;
        for &id in &row.sprint_ids {
            membership.execute(params![board, ticket.key.as_str(), number(id)?])?;
        }
    }
    Ok(())
}

pub(super) fn discover(db: &Connection) -> Result<bool, StoreError> {
    let removed = db.execute(
        "DELETE FROM review_discoveries AS d
         WHERE NOT EXISTS (SELECT 1 FROM reviews r JOIN refresh_mrs q USING(origin,project_id,iid)
                           WHERE r.review_id=d.review_id AND q.discovered=1)",
        [],
    )?;
    let added = db.execute(
        "INSERT INTO review_discoveries(review_id)
         SELECT r.review_id FROM refresh_mrs q JOIN reviews r USING(origin,project_id,iid)
         WHERE q.discovered=1
         ON CONFLICT DO NOTHING",
        [],
    )?;
    Ok(removed != 0 || added != 0)
}

pub(super) fn replace_scope(db: &Connection) -> Result<bool, StoreError> {
    db.execute(
        "INSERT INTO refresh_review_issues
         SELECT r.review_id,s.board_id,s.ticket_key FROM refresh_links s JOIN reviews r USING(origin,project_id,iid)",
        [],
    )?;
    let statements = [
        "DELETE FROM scoped_sprints AS p WHERE NOT EXISTS (
           SELECT 1 FROM refresh_sprints s WHERE (s.board_id,s.sprint_id)=(p.board_id,p.sprint_id))",
        "INSERT INTO scoped_sprints SELECT * FROM refresh_sprints WHERE true
         ON CONFLICT(board_id,sprint_id) DO UPDATE SET label=excluded.label,active=excluded.active
         WHERE (label,active) IS DISTINCT FROM (excluded.label,excluded.active)",
        "DELETE FROM scoped_issues AS p WHERE NOT EXISTS (
           SELECT 1 FROM refresh_issues s WHERE (s.board_id,s.ticket_key)=(p.board_id,p.ticket_key))",
        "INSERT INTO scoped_issues SELECT * FROM refresh_issues WHERE true
         ON CONFLICT(board_id,ticket_key) DO UPDATE SET
           title=excluded.title,project_key=excluded.project_key,status_id=excluded.status_id,
           status=excluded.status,assignee=excluded.assignee,priority=excluded.priority,
           estimate=excluded.estimate,estimate_warning=excluded.estimate_warning
         WHERE (title,project_key,status_id,status,assignee,priority,estimate,estimate_warning)
              IS DISTINCT FROM (excluded.title,excluded.project_key,excluded.status_id,
                                excluded.status,excluded.assignee,excluded.priority,
                                excluded.estimate,excluded.estimate_warning)",
        "DELETE FROM scoped_issue_sprints AS p WHERE NOT EXISTS (
           SELECT 1 FROM refresh_membership s
           WHERE (s.board_id,s.ticket_key,s.sprint_id)=(p.board_id,p.ticket_key,p.sprint_id))",
        "INSERT INTO scoped_issue_sprints SELECT * FROM refresh_membership WHERE true ON CONFLICT DO NOTHING",
        "DELETE FROM scoped_review_issues AS p WHERE NOT EXISTS (
           SELECT 1 FROM refresh_review_issues s
           WHERE (s.review_id,s.board_id,s.ticket_key)=(p.review_id,p.board_id,p.ticket_key))",
        "INSERT INTO scoped_review_issues SELECT * FROM refresh_review_issues WHERE true ON CONFLICT DO NOTHING",
    ];
    let mut changed = false;
    for statement in statements {
        changed |= db.prepare_cached(statement)?.execute([])? != 0;
    }
    Ok(changed)
}

pub(in crate::refresh) fn clear(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch(
        "DELETE FROM refresh_mrs; DELETE FROM refresh_sprints; DELETE FROM refresh_issues;
         DELETE FROM refresh_membership; DELETE FROM refresh_links; DELETE FROM refresh_review_issues;",
    )?;
    Ok(())
}

fn number(id: u64) -> Result<i64, StoreError> {
    i64::try_from(id).map_err(|_| AppError::Internal.into())
}
