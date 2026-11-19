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
    verifier BLOB CHECK (verifier IS NULL OR length(verifier) = 32),
    active INTEGER NOT NULL CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX repo_grants_active_checkout ON repo_grants(common_dir) WHERE active = 1;
CREATE UNIQUE INDEX repo_grants_active_verifier ON repo_grants(verifier) WHERE active = 1 AND verifier IS NOT NULL;

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
CREATE UNIQUE INDEX runs_lead_scope ON runs(run_id, lead_session_key);

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

CREATE TABLE messages (
    message_id TEXT PRIMARY KEY,
    actor_key BLOB,
    operation TEXT,
    submission_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_key TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('user', 'report_feedback', 'codex_recovery', 'thread_followup', 'publication')),
    body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) BETWEEN 1 AND 48000),
    receipt_id TEXT,
    report_hash TEXT CHECK (length(report_hash) = 64),
    current_receipt_id TEXT,
    quoted_text TEXT CHECK (length(CAST(quoted_text AS BLOB)) BETWEEN 1 AND 48000),
    quoted_offset INTEGER CHECK (quoted_offset >= 0),
    state TEXT NOT NULL CHECK (state IN ('queued', 'dispatching', 'started', 'rejected', 'uncertain')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    native_turn_id TEXT,
    native_input_id TEXT,
    CHECK ((actor_key IS NULL) = (operation IS NULL)),
    CHECK (actor_key IS NULL OR (length(actor_key) = 32 AND length(message_id) = 36)),
    CHECK (quoted_offset IS NULL OR quoted_text IS NOT NULL),
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
CREATE UNIQUE INDEX messages_observed_input ON messages(session_key, native_turn_id, native_input_id)
    WHERE actor_key IS NOT NULL AND native_input_id IS NOT NULL;

CREATE TABLE outbox (
    delivery_id TEXT PRIMARY KEY REFERENCES messages(message_id),
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    claim_kind TEXT CHECK (claim_kind IN ('dispatch', 'reconcile')),
    due_at_ms INTEGER NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count BETWEEN 0 AND 7)
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

CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    original_sha256 TEXT CHECK (original_sha256 IS NULL OR (
        length(original_sha256) = 64 AND original_sha256 NOT GLOB '*[^0-9a-f]*')),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    total_code_points INTEGER NOT NULL CHECK (total_code_points BETWEEN 0 AND byte_length),
    line_count INTEGER NOT NULL CHECK (line_count BETWEEN 0 AND total_code_points),
    max_line_bytes INTEGER NOT NULL CHECK (max_line_bytes BETWEEN 0 AND byte_length),
    complete INTEGER NOT NULL CHECK (complete IN (0, 1)),
    sealed INTEGER NOT NULL CHECK (sealed IN (0, 1)),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX artifacts_staging ON artifacts(created_at_ms, artifact_id) WHERE sealed=0;

CREATE TABLE artifact_chunks (
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    byte_start INTEGER NOT NULL CHECK (byte_start >= 0),
    scalar_start INTEGER NOT NULL CHECK (scalar_start >= 0),
    raw BLOB NOT NULL CHECK (length(raw) BETWEEN 1 AND 65536),
    PRIMARY KEY (artifact_id, ordinal),
    UNIQUE (artifact_id, byte_start),
    UNIQUE (artifact_id, scalar_start)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_handoffs (
    receipt_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    lead_session_key TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    declaration_sha256 TEXT NOT NULL CHECK (length(declaration_sha256) = 64),
    form TEXT NOT NULL CHECK (form IN ('final', 'unchanged', 'needs_input')),
    state TEXT NOT NULL CHECK (state IN ('pending_completion', 'recorded', 'failed')),
    source_kind TEXT CHECK (source_kind IN ('file', 'response')),
    artifact_id TEXT REFERENCES artifacts(artifact_id),
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 2048),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key),
    UNIQUE (run_id, receipt_id),
    UNIQUE (run_id, lead_session_key, turn_id, call_id)
) STRICT;

CREATE INDEX report_handoff_latest ON report_handoffs(run_id, created_at_ms DESC, receipt_id DESC);
CREATE INDEX report_handoff_artifact ON report_handoffs(artifact_id);
CREATE INDEX report_handoff_pending ON report_handoffs(updated_at_ms, receipt_id)
    WHERE state='pending_completion';

CREATE TABLE report_versions (
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    receipt_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    handoff_receipt_id TEXT CHECK (handoff_receipt_id = receipt_id),
    parent_receipt_id TEXT,
    attention_receipt_id TEXT NOT NULL,
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    revision_reason TEXT CHECK (length(CAST(revision_reason AS BLOB)) BETWEEN 1 AND 1024),
    full_mr_reviewed_sha TEXT CHECK (length(full_mr_reviewed_sha) IN (40, 64)),
    coverage TEXT NOT NULL CHECK (coverage IN ('full_mr', 'selected_threads')),
    verdict TEXT NOT NULL CHECK (verdict IN ('approve', 'request_changes', 'blocked', 'not_specified')),
    semantic_index TEXT NOT NULL CHECK (semantic_index IN ('ready', 'source_only')),
    finding_count INTEGER CHECK (finding_count >= 0),
    change_count INTEGER CHECK (change_count >= 0),
    captured_at_ms INTEGER NOT NULL,
    starting_disposition_version INTEGER NOT NULL CHECK (starting_disposition_version >= 0),
    closing_disposition_version INTEGER CHECK (closing_disposition_version >= starting_disposition_version),
    CHECK ((reviewed_sha IS NULL) <> (revision_reason IS NULL)),
    CHECK ((semantic_index = 'ready') = (finding_count IS NOT NULL AND change_count IS NOT NULL)),
    CHECK (parent_receipt_id IS NULL OR parent_receipt_id <> receipt_id),
    PRIMARY KEY (run_id, receipt_id),
    FOREIGN KEY (run_id, handoff_receipt_id) REFERENCES report_handoffs(run_id, receipt_id),
    FOREIGN KEY (run_id, parent_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    FOREIGN KEY (run_id, attention_receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX report_version_history ON report_versions(run_id, captured_at_ms DESC, receipt_id DESC);
CREATE INDEX report_version_artifact ON report_versions(artifact_id);
CREATE INDEX report_version_parent ON report_versions(run_id, parent_receipt_id);
CREATE INDEX report_version_attention ON report_versions(run_id, attention_receipt_id);

CREATE TABLE report_heads (
    run_id TEXT PRIMARY KEY REFERENCES runs(run_id),
    receipt_id TEXT NOT NULL,
    disposition_version INTEGER NOT NULL CHECK (disposition_version >= 0),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_findings (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) BETWEEN 1 AND 384),
    severity TEXT CHECK (length(CAST(severity AS BLOB)) <= 64),
    blocking INTEGER CHECK (blocking IN (0, 1)),
    scalar_offset INTEGER CHECK (scalar_offset >= 0),
    scalar_length INTEGER CHECK (scalar_length > 0),
    evidence_sha256 TEXT CHECK (length(evidence_sha256) = 64),
    CHECK ((scalar_offset IS NULL) = (scalar_length IS NULL)),
    CHECK (scalar_offset IS NULL OR evidence_sha256 IS NOT NULL),
    PRIMARY KEY (run_id, receipt_id, finding_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_sections (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    scalar_offset INTEGER NOT NULL CHECK (scalar_offset >= 0),
    scalar_length INTEGER NOT NULL CHECK (scalar_length > 0),
    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) BETWEEN 1 AND 384),
    evidence_sha256 TEXT NOT NULL CHECK (length(evidence_sha256) = 64),
    PRIMARY KEY (run_id, receipt_id, scalar_offset),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_changes (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    conclusion TEXT NOT NULL CHECK (conclusion IN (
        'fixed', 'clarified_withdrawn', 'superseded', 'partly_addressed', 'still_present', 'unverified')),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 384),
    checked_sha TEXT NOT NULL CHECK (length(checked_sha) IN (40, 64)),
    PRIMARY KEY (run_id, receipt_id, finding_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE finding_dispositions (
    run_id TEXT NOT NULL,
    disposition_version INTEGER NOT NULL CHECK (disposition_version > 0),
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    blocking INTEGER CHECK (blocking IN (0, 1)),
    publication TEXT CHECK (publication IN ('withheld', 'authorized', 'published')),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 384),
    removed INTEGER NOT NULL CHECK (removed IN (0, 1)),
    PRIMARY KEY (run_id, disposition_version, finding_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id),
    CHECK (removed=0 OR (blocking IS NULL AND publication IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX finding_disposition_at ON finding_dispositions(run_id, finding_id, disposition_version DESC);
CREATE INDEX finding_disposition_receipt ON finding_dispositions(run_id, receipt_id);

CREATE TABLE report_reads (
    viewer_key BLOB NOT NULL CHECK (length(viewer_key) = 32),
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    viewed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (viewer_key, run_id, receipt_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX report_read_version ON report_reads(run_id, receipt_id);
