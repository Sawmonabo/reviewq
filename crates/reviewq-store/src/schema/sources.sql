CREATE TABLE scoped_issues (
    board_id INTEGER NOT NULL CHECK (board_id > 0),
    ticket_key TEXT NOT NULL,
    title TEXT NOT NULL,
    project_key TEXT NOT NULL,
    status_id INTEGER NOT NULL CHECK (status_id > 0),
    status TEXT NOT NULL,
    assignee TEXT,
    priority TEXT,
    estimate REAL CHECK (estimate >= 0),
    estimate_warning TEXT,
    PRIMARY KEY (board_id, ticket_key)
) STRICT, WITHOUT ROWID;

CREATE INDEX scoped_issues_key ON scoped_issues(ticket_key, board_id);

CREATE TABLE scoped_sprints (
    board_id INTEGER NOT NULL CHECK (board_id > 0),
    sprint_id INTEGER NOT NULL CHECK (sprint_id > 0),
    label TEXT NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0,1)),
    PRIMARY KEY (board_id, sprint_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE scoped_issue_sprints (
    board_id INTEGER NOT NULL,
    ticket_key TEXT NOT NULL,
    sprint_id INTEGER NOT NULL,
    PRIMARY KEY (board_id, ticket_key, sprint_id),
    FOREIGN KEY (board_id, ticket_key) REFERENCES scoped_issues ON DELETE CASCADE,
    FOREIGN KEY (board_id, sprint_id) REFERENCES scoped_sprints ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE scoped_review_issues (
    review_id TEXT NOT NULL REFERENCES reviews ON DELETE CASCADE,
    board_id INTEGER NOT NULL,
    ticket_key TEXT NOT NULL,
    PRIMARY KEY (review_id, board_id, ticket_key),
    FOREIGN KEY (board_id, ticket_key) REFERENCES scoped_issues ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX scoped_review_issues_key ON scoped_review_issues(ticket_key, board_id, review_id);

CREATE TABLE loaded_sprints (
    board_id INTEGER NOT NULL REFERENCES settings_boards(board_id) ON DELETE CASCADE,
    sprint_id INTEGER NOT NULL CHECK (sprint_id>0),
    label TEXT NOT NULL,
    PRIMARY KEY (board_id,sprint_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE loaded_sprint_issues (
    board_id INTEGER NOT NULL,
    sprint_id INTEGER NOT NULL,
    ticket_key TEXT NOT NULL,
    PRIMARY KEY (board_id,sprint_id,ticket_key),
    FOREIGN KEY (board_id,sprint_id) REFERENCES loaded_sprints ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
CREATE TABLE source_refresh_receipts (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    state TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'superseded')),
    finished_at_ms INTEGER,
    total_rows INTEGER CHECK (total_rows >= 0),
    error TEXT CHECK (error IN ('unavailable', 'too_large', 'interrupted')),
    CHECK ((finished_at_ms IS NULL) = (state = 'running')),
    CHECK ((total_rows IS NOT NULL) = (state = 'succeeded')),
    CHECK ((error IS NOT NULL) = (state = 'failed'))
) STRICT;

CREATE UNIQUE INDEX source_refresh_one_running ON source_refresh_receipts(state) WHERE state='running';
