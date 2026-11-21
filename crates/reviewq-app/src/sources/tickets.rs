use reviewq_domain::identity::TicketKey;

use crate::queue::ReviewSource;

pub struct UnverifiedTrack {
    pub review: ReviewSource,
    pub candidate_tickets: Vec<TicketKey>,
}

#[derive(Clone)]
pub struct TicketIdentity {
    pub key: TicketKey,
    pub title: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum CommentOrder {
    Newest,
    Oldest,
}

#[derive(Clone)]
pub struct TicketOpening {
    pub detail: TicketDetail,
    pub comments: TicketCommentPage,
}

#[derive(Clone)]
pub struct TicketDetail {
    pub identity: TicketIdentity,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub assignee_id: Option<String>,
    pub reporter: Option<String>,
    pub reporter_id: Option<String>,
    pub issue_type: Option<String>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    pub description: Option<String>,
    pub estimate: Option<f64>,
    pub updated_at_ms: Option<i64>,
    pub warning: Option<&'static str>,
}

#[derive(Clone)]
pub struct TicketCommentPage {
    pub issue: u64,
    pub url: String,
    pub fetched_at_ms: i64,
    pub items: Vec<TicketComment>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

#[derive(Clone)]
pub struct TicketComment {
    pub id: u64,
    pub author: Option<String>,
    pub author_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: Option<i64>,
    pub body: String,
}
