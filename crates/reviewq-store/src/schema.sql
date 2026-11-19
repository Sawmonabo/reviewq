CREATE TABLE service_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    store_id TEXT NOT NULL,
    event_head INTEGER NOT NULL DEFAULT 0 CHECK (event_head >= 0),
    event_floor INTEGER NOT NULL DEFAULT 0 CHECK (event_floor BETWEEN 0 AND event_head),
    queue_revision INTEGER NOT NULL DEFAULT 0 CHECK (queue_revision >= 0),
    settings_revision INTEGER NOT NULL DEFAULT 0 CHECK (settings_revision >= 0),
    inbox_revision INTEGER NOT NULL DEFAULT 0 CHECK (inbox_revision >= 0)
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
    verifier BLOB CHECK (verifier IS NULL OR length(verifier) = 32),
    active INTEGER NOT NULL CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX repo_grants_active_checkout ON repo_grants(common_dir) WHERE active = 1;
CREATE UNIQUE INDEX repo_grants_active_verifier ON repo_grants(verifier) WHERE active = 1 AND verifier IS NOT NULL;
CREATE INDEX repo_grants_retired_verifier ON repo_grants(verifier) WHERE active = 0 AND verifier IS NOT NULL;

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
    observation_cursor INTEGER NOT NULL DEFAULT 0 CHECK (observation_cursor >= 0),
    UNIQUE (origin, project_id, iid)
) STRICT;

CREATE INDEX reviews_queue_order ON reviews(updated_at_ms DESC, review_id DESC);
CREATE INDEX reviews_project_order ON reviews(origin, project_id, updated_at_ms DESC, review_id DESC);

CREATE TABLE review_discoveries (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE review_tracking (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE retained_review_tickets (
    review_id TEXT NOT NULL REFERENCES reviews(review_id) ON DELETE CASCADE,
    ticket_key TEXT NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY (review_id, ticket_key)
) STRICT, WITHOUT ROWID;

CREATE INDEX retained_review_tickets_key ON retained_review_tickets(ticket_key, review_id);

CREATE VIEW retained_reviews AS
    SELECT review_id FROM review_tracking
    UNION ALL SELECT review_id FROM runs
    UNION ALL SELECT review_id FROM review_decisions
    UNION ALL SELECT review_id FROM notifications;

CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    lead_session_key TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    FOREIGN KEY (run_id, lead_session_key)
        REFERENCES sessions(run_id, session_key) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX runs_review_order ON runs(review_id, started_at_ms DESC, run_id DESC);
CREATE UNIQUE INDEX runs_lead_scope ON runs(run_id, lead_session_key);

CREATE TABLE sessions (
    session_key TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude')),
    profile BLOB NOT NULL,
    native_id TEXT NOT NULL,
    cwd BLOB NOT NULL,
    source_path BLOB,
    host_activity TEXT CHECK (host_activity IS NULL OR host_activity IN ('idle','working','blocked','failed')),
    host_questions INTEGER NOT NULL DEFAULT 0 CHECK (host_questions IN (0,1)),
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

CREATE TABLE messages (
    message_id TEXT PRIMARY KEY,
    actor_key BLOB,
    operation TEXT,
    submission_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_key TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('user', 'report_feedback', 'codex_recovery', 'thread_followup', 'publication')),
    body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) <= 48000),
    receipt_id TEXT,
    report_hash TEXT CHECK (length(report_hash) = 64),
    current_receipt_id TEXT,
    quoted_text TEXT CHECK (length(CAST(quoted_text AS BLOB)) BETWEEN 1 AND 48000),
    quoted_offset INTEGER CHECK (quoted_offset >= 0),
    state TEXT NOT NULL CHECK (state IN ('queued', 'dispatching', 'started', 'rejected', 'uncertain')),
    outcome_detail TEXT CHECK (length(CAST(outcome_detail AS BLOB)) <= 2048),
    followup_batch_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    native_turn_id TEXT,
    native_input_id TEXT,
    CHECK ((actor_key IS NULL) = (operation IS NULL)),
    CHECK (actor_key IS NULL OR (length(actor_key) = 32 AND length(message_id) = 36)),
    CHECK (quoted_offset IS NULL OR quoted_text IS NOT NULL),
    CHECK (length(CAST(body AS BLOB)) > 0 OR quoted_text IS NOT NULL),
    CHECK (actor_key IS NULL OR (
        ((purpose = 'report_feedback') = (receipt_id IS NOT NULL)) AND
        ((receipt_id IS NULL) = (report_hash IS NULL)) AND
        ((quoted_text IS NULL) = (quoted_offset IS NULL)) AND
        (quoted_text IS NULL OR receipt_id IS NOT NULL) AND
        ((state = 'started') = (native_turn_id IS NOT NULL)) AND
        ((native_turn_id IS NULL) = (native_input_id IS NULL)))),
    UNIQUE (actor_key, operation, submission_id),
    FOREIGN KEY (actor_key, operation, submission_id)
        REFERENCES mutation_receipts(actor_key, operation, submission_id),
    FOREIGN KEY (run_id, session_key) REFERENCES sessions(run_id, session_key)
) STRICT;

CREATE INDEX messages_session_history ON messages(run_id, session_key, created_at_ms DESC, message_id DESC);
CREATE INDEX messages_session_pending ON messages(run_id, session_key, created_at_ms)
    WHERE state IN ('queued', 'dispatching', 'uncertain');
CREATE UNIQUE INDEX messages_observed_input ON messages(session_key, native_turn_id, native_input_id)
    WHERE actor_key IS NOT NULL AND native_input_id IS NOT NULL;

CREATE INDEX messages_native_history ON messages(run_id, session_key, native_input_id)
    WHERE state='started' AND actor_key IS NOT NULL;

CREATE TABLE outbox (
    delivery_id TEXT PRIMARY KEY REFERENCES messages(message_id),
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    claim_kind TEXT CHECK (claim_kind IN ('dispatch', 'reconcile')),
    due_at_ms INTEGER NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count BETWEEN 0 AND 7),
    reconcile_cursor TEXT CHECK (length(CAST(reconcile_cursor AS BLOB)) BETWEEN 1 AND 2048),
    uncertain_turn_id TEXT,
    uncertain_input_id TEXT,
    CHECK ((uncertain_turn_id IS NULL) = (uncertain_input_id IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX outbox_due ON outbox(due_at_ms, delivery_id);

CREATE TRIGGER outbox_requires_authority BEFORE INSERT ON outbox
WHEN NOT EXISTS (
    SELECT 1 FROM messages WHERE message_id=NEW.delivery_id AND actor_key IS NOT NULL
       AND state IN ('queued', 'dispatching', 'uncertain')
)
BEGIN
    SELECT RAISE(ABORT, 'dispatch requires an admitted message');
END;

CREATE UNIQUE INDEX runs_review_identity ON runs(review_id, run_id);
