CREATE TABLE settings (
    provider TEXT PRIMARY KEY CHECK (provider IN ('gitlab', 'jira')),
    origin TEXT NOT NULL CHECK (length(CAST(origin AS BLOB)) BETWEEN 1 AND 512),
    email TEXT CHECK (length(CAST(email AS BLOB)) BETWEEN 1 AND 320),
    token_type TEXT CHECK (token_type IN ('scoped', 'unscoped')),
    cloud_id TEXT CHECK (length(cloud_id) = 36),
    identity_id TEXT NOT NULL CHECK (length(CAST(identity_id AS BLOB)) BETWEEN 1 AND 512),
    identity_label TEXT NOT NULL CHECK (length(CAST(identity_label AS BLOB)) BETWEEN 1 AND 512),
    CHECK ((provider = 'jira') = (email IS NOT NULL)),
    CHECK ((provider = 'jira') = (token_type IS NOT NULL)),
    CHECK ((provider = 'jira') = (cloud_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE credentials (
    provider TEXT PRIMARY KEY REFERENCES settings(provider),
    token TEXT NOT NULL CHECK (length(CAST(token AS BLOB)) BETWEEN 1 AND 8192)
) STRICT, WITHOUT ROWID;

CREATE TABLE settings_repositories (
    project_id INTEGER PRIMARY KEY CHECK (project_id > 0),
    path TEXT NOT NULL,
    checkout BLOB,
    common_dir BLOB,
    board_selection TEXT NOT NULL CHECK (board_selection IN ('selected', 'any')),
    codex_skill TEXT,
    codex_skill_path BLOB,
    claude_skill TEXT,
    claude_skill_path BLOB,
    CHECK ((checkout IS NULL) = (common_dir IS NULL)),
    CHECK ((codex_skill IS NULL) = (codex_skill_path IS NULL)),
    CHECK ((claude_skill IS NULL) = (claude_skill_path IS NULL)),
    CHECK (checkout IS NOT NULL OR (codex_skill IS NULL AND claude_skill IS NULL))
) STRICT;

CREATE UNIQUE INDEX settings_repositories_checkout
    ON settings_repositories(common_dir) WHERE common_dir IS NOT NULL;

CREATE TABLE settings_boards (
    scope_id TEXT PRIMARY KEY,
    board_id INTEGER NOT NULL UNIQUE CHECK (board_id > 0),
    label TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('scrum', 'kanban')),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    projects TEXT NOT NULL CHECK (json_valid(projects)),
    statuses TEXT NOT NULL CHECK (json_valid(statuses)),
    jira_projects TEXT NOT NULL CHECK (json_valid(jira_projects)),
    filter_id INTEGER CHECK (filter_id > 0),
    estimate_field TEXT
) STRICT, WITHOUT ROWID;

CREATE TABLE settings_reviewers (
    reviewer_id INTEGER PRIMARY KEY CHECK (reviewer_id > 0)
) STRICT;
