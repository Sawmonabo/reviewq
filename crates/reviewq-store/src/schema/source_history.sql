CREATE TABLE source_history_projects (
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL,
    settings_revision INTEGER NOT NULL,
    watermark_ms INTEGER NOT NULL,
    valid INTEGER NOT NULL CHECK(valid IN (0,1)),
    PRIMARY KEY(origin, project_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE source_history_rows (
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL,
    iid INTEGER NOT NULL CHECK(iid>0),
    title TEXT NOT NULL,
    branch TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(origin, project_id, iid),
    FOREIGN KEY(origin, project_id) REFERENCES source_history_projects ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE source_history_keys (
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL,
    iid INTEGER NOT NULL,
    ticket_key TEXT NOT NULL,
    PRIMARY KEY(origin, project_id, ticket_key, iid),
    FOREIGN KEY(origin, project_id, iid) REFERENCES source_history_rows ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX source_history_key_parent ON source_history_keys(origin,project_id,iid);

CREATE TABLE source_history_scan (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    attempt INTEGER NOT NULL,
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL,
    bytes INTEGER NOT NULL
) STRICT;

CREATE TABLE source_history_staging (
    iid INTEGER PRIMARY KEY CHECK(iid>0),
    title TEXT NOT NULL,
    branch TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    closed INTEGER NOT NULL CHECK(closed IN (0,1)),
    keys TEXT NOT NULL CHECK(json_valid(keys))
) STRICT;
