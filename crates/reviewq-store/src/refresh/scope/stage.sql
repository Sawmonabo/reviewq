CREATE TEMP TABLE IF NOT EXISTS refresh_mrs (
    origin TEXT NOT NULL, project_id INTEGER NOT NULL, iid INTEGER NOT NULL,
    project_path TEXT NOT NULL, title TEXT NOT NULL, head_sha TEXT NOT NULL,
    state TEXT NOT NULL, updated_at_ms INTEGER NOT NULL, author_id INTEGER,
    author_username TEXT, draft INTEGER NOT NULL, discovered INTEGER NOT NULL,
    PRIMARY KEY (origin, project_id, iid)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_sprints (
    board_id INTEGER NOT NULL, sprint_id INTEGER NOT NULL, label TEXT NOT NULL,
    active INTEGER NOT NULL,
    PRIMARY KEY (board_id, sprint_id)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_issues (
    board_id INTEGER NOT NULL, ticket_key TEXT NOT NULL, title TEXT NOT NULL,
    project_key TEXT NOT NULL, status_id INTEGER NOT NULL, status TEXT NOT NULL,
    assignee TEXT, priority TEXT, estimate REAL, estimate_warning TEXT,
    PRIMARY KEY (board_id, ticket_key)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_membership (
    board_id INTEGER NOT NULL, ticket_key TEXT NOT NULL, sprint_id INTEGER NOT NULL,
    PRIMARY KEY (board_id, ticket_key, sprint_id)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_links (
    origin TEXT NOT NULL, project_id INTEGER NOT NULL, iid INTEGER NOT NULL,
    board_id INTEGER NOT NULL, ticket_key TEXT NOT NULL,
    PRIMARY KEY (origin, project_id, iid, board_id, ticket_key)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_review_issues (
    review_id TEXT NOT NULL, board_id INTEGER NOT NULL, ticket_key TEXT NOT NULL,
    PRIMARY KEY (review_id, board_id, ticket_key)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_loaded_sprints (
    board_id INTEGER NOT NULL, sprint_id INTEGER NOT NULL,
    retired INTEGER NOT NULL,
    PRIMARY KEY (board_id,sprint_id)
) STRICT, WITHOUT ROWID;

CREATE TEMP TABLE IF NOT EXISTS refresh_loaded_sprint_issues (
    board_id INTEGER NOT NULL, sprint_id INTEGER NOT NULL,ticket_key TEXT NOT NULL,
    PRIMARY KEY (board_id,sprint_id,ticket_key)
) STRICT, WITHOUT ROWID;
