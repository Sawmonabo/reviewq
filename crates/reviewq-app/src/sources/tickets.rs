use reviewq_domain::identity::TicketKey;

use crate::queue::ReviewSource;

pub struct UnverifiedTrack {
    pub review: ReviewSource,
    pub candidate_tickets: Vec<TicketKey>,
}

pub struct TicketIdentity {
    pub key: TicketKey,
    pub title: String,
}

pub struct TicketDetail {
    pub identity: TicketIdentity,
    pub id: u64,
    pub url: String,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub assignee_id: Option<String>,
    pub description: Option<String>,
    pub estimate: Option<f64>,
    pub warning: Option<&'static str>,
    pub comments: Vec<TicketComment>,
    pub next_offset: Option<u32>,
}

pub struct TicketComment {
    pub id: u64,
    pub author: Option<String>,
    pub author_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: Option<i64>,
    pub body: String,
}
