CREATE TABLE service_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    store_id TEXT NOT NULL,
    event_head INTEGER NOT NULL DEFAULT 0 CHECK (event_head >= 0),
    event_floor INTEGER NOT NULL DEFAULT 0 CHECK (event_floor BETWEEN 0 AND event_head),
    queue_revision INTEGER NOT NULL DEFAULT 0 CHECK (queue_revision >= 0),
    settings_revision INTEGER NOT NULL DEFAULT 0 CHECK (settings_revision >= 0)
) STRICT;

CREATE TABLE events (
    cursor INTEGER PRIMARY KEY CHECK (cursor > 0),
    resources TEXT NOT NULL CHECK (json_valid(resources)),
    committed_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE source_refresh (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    attempt INTEGER NOT NULL CHECK (attempt > 0),
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    started_at_ms INTEGER NOT NULL,
    last_success_at_ms INTEGER,
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    error TEXT CHECK (error IN ('unavailable', 'too_large', 'interrupted'))
) STRICT;

CREATE TABLE configured_projects (
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL CHECK (project_id > 0),
    PRIMARY KEY (origin, project_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE repo_grants (
    grant_id TEXT PRIMARY KEY,
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL CHECK (project_id > 0),
    checkout BLOB NOT NULL,
    common_dir BLOB NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch > 0),
    active INTEGER NOT NULL CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX repo_grants_active_checkout ON repo_grants(common_dir) WHERE active = 1;

CREATE TABLE reviews (
    review_id TEXT PRIMARY KEY,
    origin TEXT NOT NULL,
    project_id INTEGER NOT NULL CHECK (project_id > 0),
    iid INTEGER NOT NULL CHECK (iid > 0),
    project_path TEXT NOT NULL,
    title TEXT NOT NULL,
    head_sha TEXT NOT NULL CHECK (length(head_sha) IN (40, 64)),
    state TEXT NOT NULL CHECK (state IN ('open', 'closed', 'merged')),
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (origin, project_id, iid)
) STRICT;

CREATE INDEX reviews_queue_order ON reviews(updated_at_ms DESC, review_id DESC);
CREATE INDEX reviews_project_order ON reviews(origin, project_id, updated_at_ms DESC, review_id DESC);

CREATE TABLE review_discoveries (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    lead_session_key TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    FOREIGN KEY (run_id, lead_session_key)
        REFERENCES sessions(run_id, session_key) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX runs_review_order ON runs(review_id, started_at_ms DESC, run_id DESC);
CREATE INDEX runs_lead_scope ON runs(run_id, lead_session_key);

CREATE TABLE sessions (
    session_key TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude')),
    profile BLOB NOT NULL,
    native_id TEXT NOT NULL,
    cwd BLOB NOT NULL,
    source_path BLOB,
    parent_session_key TEXT CHECK (parent_session_key <> session_key),
    UNIQUE (run_id, session_key),
    UNIQUE (provider, profile, native_id),
    FOREIGN KEY (run_id, parent_session_key) REFERENCES sessions(run_id, session_key)
) STRICT;

CREATE UNIQUE INDEX sessions_one_lead ON sessions(run_id) WHERE parent_session_key IS NULL;
CREATE INDEX sessions_parent_scope ON sessions(run_id, parent_session_key, session_key);

CREATE TABLE mutation_receipts (
    actor_key BLOB NOT NULL CHECK (length(actor_key) = 32),
    operation TEXT NOT NULL CHECK (operation <> ''),
    submission_id TEXT NOT NULL,
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    grant_id TEXT REFERENCES repo_grants(grant_id),
    grant_epoch INTEGER CHECK (grant_epoch > 0),
    CHECK ((grant_id IS NULL) = (grant_epoch IS NULL)),
    PRIMARY KEY (actor_key, operation, submission_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX mutation_grant_idx ON mutation_receipts(grant_id);

CREATE TABLE outbox (
    delivery_id TEXT PRIMARY KEY,
    actor_key BLOB NOT NULL,
    operation TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_key TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('user', 'report_feedback')),
    body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) BETWEEN 1 AND 48000),
    receipt_id TEXT,
    report_hash TEXT,
    quoted_text TEXT,
    quoted_offset INTEGER,
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    claim_kind TEXT CHECK (claim_kind IN ('dispatch', 'reconcile')),
    state TEXT NOT NULL CHECK (state IN ('queued', 'dispatching', 'started', 'rejected', 'uncertain')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    native_turn_id TEXT,
    native_input_id TEXT,
    CHECK ((purpose = 'report_feedback') = (receipt_id IS NOT NULL)),
    CHECK ((receipt_id IS NULL) = (report_hash IS NULL)),
    CHECK ((quoted_text IS NULL) = (quoted_offset IS NULL)),
    CHECK (quoted_text IS NULL OR (receipt_id IS NOT NULL AND quoted_offset >= 0)),
    CHECK ((state = 'started') = (native_turn_id IS NOT NULL)),
    CHECK ((native_turn_id IS NULL) = (native_input_id IS NULL)),
    CHECK (claim_kind IS NULL OR state IN ('dispatching', 'uncertain')),
    UNIQUE (actor_key, operation, submission_id),
    FOREIGN KEY (actor_key, operation, submission_id)
        REFERENCES mutation_receipts(actor_key, operation, submission_id),
    FOREIGN KEY (run_id, session_key) REFERENCES sessions(run_id, session_key)
) STRICT;

CREATE INDEX outbox_run_session ON outbox(run_id, session_key);
CREATE INDEX outbox_recipient_queue ON outbox(session_key, state, created_at_ms, delivery_id);
CREATE INDEX outbox_recovery ON outbox(created_at_ms, delivery_id)
    WHERE state IN ('queued', 'dispatching', 'uncertain');
CREATE UNIQUE INDEX outbox_observed_input ON outbox(session_key, native_turn_id, native_input_id)
    WHERE native_input_id IS NOT NULL;
