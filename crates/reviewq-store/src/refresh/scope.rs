use std::collections::HashSet;

use reviewq_app::access::AppError;
use reviewq_app::sources::{ScopedIssue, ScopedMatch, ScopedSprint};
use reviewq_domain::identity::ReviewId;
use rusqlite::{Connection, params};

use crate::StoreError;

pub(super) fn replace(
    db: &Connection,
    issues: Vec<ScopedIssue>,
    sprints: Vec<ScopedSprint>,
    associations: impl Iterator<Item = (ReviewId, HashSet<ScopedMatch>)>,
) -> Result<(), StoreError> {
    db.execute_batch("DELETE FROM scoped_issues; DELETE FROM scoped_sprints;")?;
    let mut sprint_insert = db.prepare_cached(
        "INSERT INTO scoped_sprints (board_id,sprint_id,label) VALUES (?1,?2,?3)",
    )?;
    for sprint in sprints {
        sprint_insert.execute(params![
            number(sprint.board)?,
            number(sprint.id)?,
            sprint.label
        ])?;
    }
    let mut issue_insert = db.prepare_cached(
        "INSERT INTO scoped_issues
         (board_id,ticket_key,title,project_key,status_id,status,assignee,priority,estimate,estimate_warning)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    let mut membership_insert = db.prepare_cached(
        "INSERT INTO scoped_issue_sprints (board_id,ticket_key,sprint_id) VALUES (?1,?2,?3)",
    )?;
    for issue in issues {
        let board = number(issue.board)?;
        let ticket = issue.ticket;
        issue_insert.execute(params![
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
        for id in issue.sprint_ids {
            membership_insert.execute(params![board, ticket.key.as_str(), number(id)?])?;
        }
    }
    let mut match_insert = db.prepare_cached(
        "INSERT INTO scoped_review_issues (review_id,board_id,ticket_key) VALUES (?1,?2,?3)",
    )?;
    for (review, matched) in associations {
        for item in matched {
            match_insert.execute(params![
                review.as_str(),
                number(item.board)?,
                item.key.as_str()
            ])?;
        }
    }
    Ok(())
}

fn number(id: u64) -> Result<i64, StoreError> {
    i64::try_from(id).map_err(|_| AppError::Internal.into())
}
