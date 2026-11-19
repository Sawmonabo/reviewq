//! Non-secret settings identities shared by provider and persistence adapters.

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Provider {
    GitLab,
    Jira,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JiraTokenType {
    Scoped,
    Unscoped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Connection {
    GitLab {
        origin: String,
    },
    Jira {
        site: String,
        email: String,
        token_type: JiraTokenType,
    },
}

impl Connection {
    pub fn provider(&self) -> Provider {
        match self {
            Self::GitLab { .. } => Provider::GitLab,
            Self::Jira { .. } => Provider::Jira,
        }
    }

    pub fn origin(&self) -> &str {
        match self {
            Self::GitLab { origin } => origin,
            Self::Jira { site, .. } => site,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAccount {
    pub connection: Connection,
    pub identity: Identity,
    pub cloud_id: Option<String>,
}

impl VerifiedAccount {
    pub fn same_authority(&self, other: &Self) -> bool {
        self.connection.provider() == other.connection.provider()
            && self.connection.origin() == other.connection.origin()
            && self.identity.id == other.identity.id
            && self.cloud_id == other.cloud_id
    }
}
