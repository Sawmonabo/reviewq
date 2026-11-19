#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SprintPair {
    pub board: u64,
    pub sprint: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SprintFilter {
    #[default]
    Active,
    Any,
    None,
    Selected,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueOwnership {
    #[default]
    Others,
    Mine,
    All,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueuePhase {
    AllOpen,
    #[default]
    NeedsReview,
    Followup,
    WaitingAuthor,
    Reviewed,
    Archive,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueOrder {
    #[default]
    Recent,
    Oldest,
    Author,
    Ticket,
}

#[derive(Default)]
pub struct QueueFilter {
    pub empty_scope: bool,
    pub boards: Vec<u64>,
    pub projects: Vec<u64>,
    pub sprint: SprintFilter,
    pub pairs: Vec<SprintPair>,
    pub ownership: QueueOwnership,
    pub author: Option<u64>,
    pub phase: QueuePhase,
    pub hide_reviewed: bool,
    pub hide_in_progress: bool,
    pub search: String,
    pub order: QueueOrder,
}

impl QueueFilter {
    pub fn all_open() -> Self {
        Self {
            sprint: SprintFilter::Any,
            ownership: QueueOwnership::All,
            phase: QueuePhase::AllOpen,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSortKey {
    pub group: i64,
    pub time: i64,
    pub label: String,
    pub digits: i64,
    pub number: String,
    pub id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueueFacets {
    pub boards: Vec<QueueFacet>,
    pub projects: Vec<QueueFacet>,
    pub sprints: Vec<(SprintPair, String)>,
    pub authors: Vec<QueueFacet>,
    pub hide_reviewed: u64,
    pub hide_in_progress: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueFacet {
    pub id: u64,
    pub label: String,
}
