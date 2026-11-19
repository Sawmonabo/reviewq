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
CREATE UNIQUE INDEX runs_lead_scope ON runs(run_id, lead_session_key);

CREATE TABLE sessions (
    session_key TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude')),
    profile BLOB NOT NULL,
    native_id TEXT NOT NULL,
    cwd BLOB NOT NULL,
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
    sha256 TEXT CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    original_sha256 TEXT CHECK (original_sha256 IS NULL OR (
        length(original_sha256) = 64 AND original_sha256 NOT GLOB '*[^0-9a-f]*')),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    total_code_points INTEGER NOT NULL CHECK (total_code_points BETWEEN 0 AND byte_length),
    line_count INTEGER NOT NULL CHECK (line_count BETWEEN 0 AND total_code_points),
    max_line_bytes INTEGER NOT NULL CHECK (max_line_bytes BETWEEN 0 AND byte_length),
    complete INTEGER NOT NULL CHECK (complete IN (0, 1)),
    sealed INTEGER NOT NULL CHECK (sealed IN (0, 1) AND (sealed=0 OR sha256 IS NOT NULL)),
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
    source_generation TEXT NOT NULL CHECK (length(CAST(source_generation AS BLOB)) BETWEEN 1 AND 512),
    request_entry_id TEXT,
    result_entry_id TEXT,
    declaration_sha256 TEXT NOT NULL CHECK (length(declaration_sha256) = 64),
    form TEXT NOT NULL CHECK (form IN ('final', 'unchanged', 'needs_input')),
    state TEXT NOT NULL CHECK (state IN ('pending_completion', 'recorded', 'failed')),
    source_kind TEXT CHECK (source_kind IN ('file', 'response')),
    source_path BLOB,
    source_hash TEXT CHECK (length(source_hash) = 64),
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    revision_reason TEXT CHECK (length(CAST(revision_reason AS BLOB)) <= 1024),
    verdict TEXT CHECK (verdict IN ('approve', 'request_changes', 'blocked', 'not_specified')),
    expected_receipt_id TEXT,
    expected_sha256 TEXT CHECK (length(expected_sha256) = 64),
    base_receipt_id TEXT,
    disposition_version INTEGER NOT NULL CHECK (disposition_version >= 0),
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    due_at_ms INTEGER NOT NULL,
    artifact_id TEXT REFERENCES artifacts(artifact_id),
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 2048),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CHECK ((expected_receipt_id IS NULL) = (expected_sha256 IS NULL)),
    CHECK ((source_kind='file') = (source_path IS NOT NULL)),
    CHECK ((form='final') = (verdict IS NOT NULL)),
    CHECK (state<>'recorded' OR result_entry_id IS NOT NULL),
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key),
    UNIQUE (run_id, receipt_id),
    UNIQUE (run_id, lead_session_key, turn_id, call_id)
) STRICT;

CREATE INDEX report_handoff_latest ON report_handoffs(run_id, created_at_ms DESC, receipt_id DESC);
CREATE INDEX report_handoff_artifact ON report_handoffs(artifact_id);
CREATE INDEX report_handoff_pending ON report_handoffs(due_at_ms, receipt_id)
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

CREATE TABLE notifications (
    ordinal INTEGER PRIMARY KEY,
    notification_id TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL CHECK (source IN ('gitlab', 'codex', 'claude', 'report', 'action', 'followup', 'service')),
    occurrence_id TEXT NOT NULL CHECK (length(CAST(occurrence_id AS BLOB)) BETWEEN 1 AND 512),
    kind TEXT NOT NULL CHECK (kind IN (
        'reply', 'edited_reply', 'resolved', 'reopened', 'new_head', 'approval_withdrawn',
        'agent_question', 'agent_permission', 'agent_failure', 'quota', 'report_ready', 'report_failed',
        'action_needs_input', 'followup_ready', 'followup_needs_input')),
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    run_id TEXT,
    session_key TEXT,
    event_ref TEXT CHECK (length(CAST(event_ref AS BLOB)) <= 512),
    label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 384),
    occurred_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    dismissed_at_ms INTEGER,
    notified_at_ms INTEGER,
    UNIQUE (source, occurrence_id),
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    FOREIGN KEY (run_id, session_key) REFERENCES sessions(run_id, session_key),
    CHECK (session_key IS NULL OR run_id IS NOT NULL)
) STRICT;

CREATE INDEX notifications_order ON notifications(occurred_at_ms DESC, source, notification_id DESC)
    WHERE dismissed_at_ms IS NULL;
CREATE INDEX notifications_unread ON notifications(review_id, ordinal)
    WHERE dismissed_at_ms IS NULL AND read_at_ms IS NULL;
CREATE INDEX notifications_review ON notifications(review_id, occurred_at_ms DESC);
CREATE INDEX notifications_run_owner ON notifications(review_id, run_id);
CREATE INDEX notifications_session_owner ON notifications(run_id, session_key);

CREATE TABLE desktop_receipts (
    receipt_id TEXT PRIMARY KEY,
    episode_key TEXT NOT NULL UNIQUE CHECK (length(CAST(episode_key AS BLOB)) BETWEEN 1 AND 512),
    notification_id TEXT NOT NULL REFERENCES notifications(notification_id),
    state TEXT NOT NULL CHECK (state IN ('claimed', 'delivered', 'failed', 'uncertain')),
    fence INTEGER NOT NULL CHECK (fence > 0),
    attempted_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    due_at_ms INTEGER,
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 128),
    CHECK ((state='claimed') = (finished_at_ms IS NULL)),
    CHECK (due_at_ms IS NULL OR state='failed')
) STRICT;

CREATE INDEX desktop_receipt_notification ON desktop_receipts(notification_id);
CREATE INDEX desktop_receipt_due ON desktop_receipts(due_at_ms, receipt_id) WHERE state='failed';

CREATE TABLE review_decisions (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    disposition TEXT CHECK (disposition IN ('reviewed', 'waiting_author', 'needs_review')),
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    reviewed_at_ms INTEGER,
    CHECK ((reviewed_sha IS NULL) = (reviewed_at_ms IS NULL)),
    CHECK (reviewed_sha IS NULL OR disposition IN ('reviewed', 'waiting_author'))
) STRICT, WITHOUT ROWID;

CREATE TABLE review_decision_receipts (
    actor_key BLOB NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('review_decision', 'review_checkpoint')),
    submission_id TEXT NOT NULL,
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    disposition TEXT CHECK (disposition IN ('reviewed', 'waiting_author', 'needs_review')),
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    reviewed_at_ms INTEGER,
    checkpoint_id TEXT,
    PRIMARY KEY (actor_key, operation, submission_id),
    UNIQUE (review_id, revision),
    FOREIGN KEY (actor_key, operation, submission_id)
        REFERENCES mutation_receipts(actor_key, operation, submission_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE review_handled_frontiers (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id),
    source_ordinal INTEGER CHECK (source_ordinal > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE review_handled_reports (
    review_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attention_receipt_id TEXT NOT NULL,
    PRIMARY KEY (review_id, run_id, attention_receipt_id),
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    FOREIGN KEY (run_id, attention_receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX review_handled_report_version ON review_handled_reports(run_id, attention_receipt_id);

CREATE TABLE review_handled_sessions (
    review_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_key TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    PRIMARY KEY (review_id, run_id, session_key),
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    FOREIGN KEY (run_id, session_key) REFERENCES sessions(run_id, session_key)
) STRICT, WITHOUT ROWID;

CREATE INDEX review_handled_session_owner ON review_handled_sessions(run_id, session_key);

CREATE TABLE actions (
    action_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    lead_session_key TEXT NOT NULL,
    native_turn_id TEXT,
    native_call_id TEXT,
    request_entry_id TEXT,
    declaration_sha256 TEXT CHECK (length(declaration_sha256) = 64),
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    authenticated_actor_id TEXT,
    attention_receipt_id TEXT,
    intent TEXT NOT NULL CHECK (intent IN ('comments', 'approval', 'both', 'cancel')),
    state TEXT NOT NULL CHECK (state IN ('working', 'checking', 'completed', 'retry_wait', 'needs_input', 'cancelled')),
    approval_state TEXT NOT NULL CHECK (approval_state IN ('not_requested', 'unknown', 'absent', 'confirmed')),
    approval_observed_at_ms INTEGER,
    attempts INTEGER NOT NULL CHECK (attempts BETWEEN 0 AND 3),
    revision INTEGER NOT NULL CHECK (revision > 0),
    authority_verified INTEGER NOT NULL CHECK (authority_verified IN (0, 1)),
    due_at_ms INTEGER,
    checked_at_ms INTEGER,
    pending_submission_id TEXT,
    last_executed_submission_id TEXT,
    last_executed_turn_id TEXT,
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 1024),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (run_id, action_id),
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key),
    FOREIGN KEY (run_id, attention_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    CHECK (authority_verified=0 OR (native_turn_id IS NOT NULL AND native_call_id IS NOT NULL
        AND request_entry_id IS NOT NULL AND declaration_sha256 IS NOT NULL
        AND reviewed_sha IS NOT NULL AND authenticated_actor_id IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX action_native_call ON actions(run_id, lead_session_key, native_turn_id, native_call_id)
    WHERE authority_verified=1;
CREATE INDEX action_run_latest ON actions(run_id, created_at_ms DESC, action_id DESC);
CREATE INDEX action_report_owner ON actions(run_id, attention_receipt_id);
CREATE INDEX action_due ON actions(due_at_ms, action_id)
    WHERE authority_verified=1 AND state IN ('working', 'checking', 'retry_wait', 'needs_input');

CREATE TABLE action_heads (
    run_id TEXT PRIMARY KEY REFERENCES runs(run_id),
    action_id TEXT NOT NULL,
    FOREIGN KEY (run_id, action_id) REFERENCES actions(run_id, action_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE action_targets (
    action_id TEXT NOT NULL REFERENCES actions(action_id),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 50),
    body_sha256 TEXT NOT NULL CHECK (length(body_sha256) = 64),
    discussion_id TEXT,
    evidence_state TEXT NOT NULL CHECK (evidence_state IN ('unknown', 'absent', 'confirmed')),
    note_id TEXT,
    observed_at_ms INTEGER,
    CHECK (note_id IS NULL OR evidence_state='confirmed'),
    PRIMARY KEY (action_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX action_target_identity ON action_targets(action_id, body_sha256, coalesce(discussion_id, ''));
CREATE UNIQUE INDEX action_target_note ON action_targets(action_id, note_id) WHERE note_id IS NOT NULL;

CREATE TABLE action_evidence (
    action_id TEXT NOT NULL REFERENCES actions(action_id),
    check_revision INTEGER NOT NULL CHECK (check_revision > 0),
    target_ordinal INTEGER,
    state TEXT NOT NULL CHECK (state IN ('unknown', 'absent', 'confirmed')),
    note_id TEXT,
    actor_id TEXT,
    head_sha TEXT CHECK (length(head_sha) IN (40, 64)),
    observed_at_ms INTEGER NOT NULL,
    FOREIGN KEY (action_id, target_ordinal) REFERENCES action_targets(action_id, ordinal),
    UNIQUE (action_id, check_revision, target_ordinal)
) STRICT;

CREATE UNIQUE INDEX action_approval_evidence ON action_evidence(action_id, check_revision)
    WHERE target_ordinal IS NULL;
CREATE INDEX action_evidence_target ON action_evidence(action_id, target_ordinal);

CREATE TABLE followup_preferences (
    singleton INTEGER PRIMARY KEY CHECK (singleton=1),
    revision INTEGER NOT NULL CHECK (revision > 0),
    automatic INTEGER NOT NULL CHECK (automatic IN (0, 1)),
    active_limit INTEGER NOT NULL CHECK (active_limit BETWEEN 1 AND 8)
) STRICT;

CREATE TABLE followup_monitors (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id),
    run_id TEXT,
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN ('enabled', 'paused', 'archived')),
    policy_override INTEGER CHECK (policy_override IN (0, 1)),
    binding_verified INTEGER NOT NULL CHECK (binding_verified IN (0, 1)),
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 1024),
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    CHECK (binding_verified=0 OR run_id IS NOT NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE followup_threads (
    review_id TEXT NOT NULL REFERENCES followup_monitors(review_id),
    generation INTEGER NOT NULL CHECK (generation > 0),
    original_note_id TEXT NOT NULL,
    discussion_id TEXT,
    reviewer_id TEXT,
    selected INTEGER NOT NULL CHECK (selected IN (0, 1)),
    discussion_version TEXT,
    baseline_sha256 TEXT CHECK (length(baseline_sha256) = 64),
    resolved INTEGER CHECK (resolved IN (0, 1)),
    PRIMARY KEY (review_id, generation, original_note_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX followup_discussion_lookup ON followup_threads(review_id, generation, discussion_id);

CREATE TABLE followup_thread_findings (
    review_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    original_note_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    PRIMARY KEY (review_id, generation, original_note_id, finding_id),
    FOREIGN KEY (review_id, generation, original_note_id)
        REFERENCES followup_threads(review_id, generation, original_note_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE followup_observations (
    observation_id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    original_note_id TEXT NOT NULL,
    discussion_id TEXT NOT NULL,
    discussion_version TEXT NOT NULL,
    occurrence_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('reply', 'edit', 'resolved', 'reopened', 'requested')),
    actor_id TEXT,
    note_id TEXT,
    observed_at_ms INTEGER NOT NULL,
    FOREIGN KEY (review_id, generation, original_note_id)
        REFERENCES followup_threads(review_id, generation, original_note_id),
    UNIQUE (review_id, generation, original_note_id, occurrence_key),
    UNIQUE (review_id, observation_id)
) STRICT;

CREATE TABLE followup_batches (
    batch_id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL REFERENCES followup_monitors(review_id),
    run_id TEXT,
    lead_session_key TEXT,
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'reserved', 'executing', 'completed', 'cancelled', 'reconciliation')),
    source_sha TEXT CHECK (length(source_sha) IN (40, 64)),
    target_sha TEXT CHECK (length(target_sha) IN (40, 64)),
    base_receipt_id TEXT,
    base_sha256 TEXT CHECK (length(base_sha256) = 64),
    disposition_version INTEGER CHECK (disposition_version >= 0),
    private_manifest TEXT CHECK (json_valid(private_manifest) AND length(CAST(private_manifest AS BLOB)) <= 131072),
    authority_verified INTEGER NOT NULL CHECK (authority_verified IN (0, 1)),
    holds_slot INTEGER NOT NULL CHECK (holds_slot IN (0, 1)),
    manual_requested INTEGER NOT NULL DEFAULT 0 CHECK (manual_requested IN (0, 1)),
    hold_reason TEXT CHECK (length(CAST(hold_reason AS BLOB)) <= 1024),
    submission_id TEXT,
    native_turn_id TEXT,
    lease_fence INTEGER NOT NULL DEFAULT 0 CHECK (lease_fence >= 0),
    lease_until_ms INTEGER,
    due_at_ms INTEGER,
    first_observed_at_ms INTEGER NOT NULL,
    latest_observed_at_ms INTEGER NOT NULL CHECK (latest_observed_at_ms >= first_observed_at_ms),
    completed_at_ms INTEGER,
    UNIQUE (review_id, batch_id),
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key),
    FOREIGN KEY (run_id, base_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    CHECK (holds_slot=0 OR (authority_verified=1 AND state IN ('reserved','executing'))),
    CHECK (lead_session_key IS NULL OR run_id IS NOT NULL),
    CHECK (authority_verified=0 OR (run_id IS NOT NULL AND lead_session_key IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX followup_one_pending ON followup_batches(review_id)
    WHERE state='pending' AND authority_verified=1;
CREATE UNIQUE INDEX followup_one_active ON followup_batches(review_id)
    WHERE state IN ('reserved', 'executing') AND authority_verified=1;
CREATE INDEX followup_due ON followup_batches(due_at_ms, batch_id)
    WHERE state='pending' AND authority_verified=1;
CREATE INDEX followup_review_order ON followup_batches(review_id, latest_observed_at_ms DESC, batch_id DESC);
CREATE INDEX followup_run_owner ON followup_batches(run_id, base_receipt_id);

CREATE TABLE followup_members (
    review_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    observation_id TEXT NOT NULL,
    reconciles_batch_id TEXT,
    PRIMARY KEY (review_id, batch_id, observation_id),
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_batches(review_id, batch_id),
    FOREIGN KEY (review_id, observation_id) REFERENCES followup_observations(review_id, observation_id),
    FOREIGN KEY (review_id, reconciles_batch_id) REFERENCES followup_batches(review_id, batch_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX followup_member_observation ON followup_members(review_id, observation_id);
CREATE INDEX followup_member_lineage ON followup_members(review_id, reconciles_batch_id);

CREATE TABLE followup_results (
    ordinal INTEGER PRIMARY KEY,
    review_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    receipt_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('final', 'unchanged')),
    checked_sha TEXT CHECK (length(checked_sha) IN (40, 64)),
    completed_at_ms INTEGER NOT NULL,
    UNIQUE (review_id, batch_id),
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_batches(review_id, batch_id),
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT;

CREATE INDEX followup_result_history ON followup_results(review_id, completed_at_ms DESC, batch_id DESC);
CREATE INDEX followup_result_receipt ON followup_results(run_id, receipt_id);

CREATE TABLE followup_assessments (
    review_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    discussion_id TEXT NOT NULL,
    conclusion TEXT NOT NULL CHECK (conclusion IN (
        'fixed', 'clarified_withdrawn', 'superseded', 'partly_addressed', 'still_present', 'unverified')),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 1024),
    next_action TEXT NOT NULL CHECK (next_action IN ('review', 'waiting_author')),
    PRIMARY KEY (review_id, batch_id, discussion_id),
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_results(review_id, batch_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE followup_assessment_findings (
    review_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    discussion_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    PRIMARY KEY (review_id, batch_id, discussion_id, finding_id),
    FOREIGN KEY (review_id, batch_id, discussion_id)
        REFERENCES followup_assessments(review_id, batch_id, discussion_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE followup_result_reads (
    viewer_key BLOB NOT NULL CHECK (length(viewer_key)=32),
    review_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    viewed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (viewer_key, review_id, batch_id),
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_results(review_id, batch_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX followup_result_read_owner ON followup_result_reads(review_id, batch_id);

CREATE TABLE report_mutation_receipts (
    actor_key BLOB NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('report_disposition', 'report_viewed')),
    submission_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    disposition_version INTEGER,
    finding_id TEXT,
    previous_receipt_id TEXT,
    rendered_batch_id TEXT,
    PRIMARY KEY (actor_key, operation, submission_id),
    FOREIGN KEY (actor_key, operation, submission_id)
        REFERENCES mutation_receipts(actor_key, operation, submission_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id),
    FOREIGN KEY (run_id, previous_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    FOREIGN KEY (run_id, disposition_version, finding_id)
        REFERENCES finding_dispositions(run_id, disposition_version, finding_id),
    CHECK ((operation='report_disposition') = (disposition_version IS NOT NULL AND finding_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX report_mutation_version ON report_mutation_receipts(run_id, receipt_id);
CREATE INDEX report_mutation_previous ON report_mutation_receipts(run_id, previous_receipt_id);
CREATE INDEX report_mutation_disposition
    ON report_mutation_receipts(run_id, disposition_version, finding_id);

CREATE TABLE followup_command_receipts (
    actor_key BLOB NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('followup_monitor','followup_preferences')),
    submission_id TEXT NOT NULL,
    review_id TEXT REFERENCES reviews(review_id),
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT CHECK (state IN ('enabled','paused','archived')),
    batch_id TEXT,
    automatic INTEGER CHECK (automatic IN (0, 1)),
    active_limit INTEGER CHECK (active_limit BETWEEN 1 AND 8),
    PRIMARY KEY (actor_key, operation, submission_id),
    FOREIGN KEY (actor_key, operation, submission_id)
        REFERENCES mutation_receipts(actor_key, operation, submission_id),
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_batches(review_id, batch_id),
    CHECK ((operation='followup_monitor') = (review_id IS NOT NULL AND state IS NOT NULL)),
    CHECK ((operation='followup_preferences') = (automatic IS NOT NULL AND active_limit IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX followup_command_batch ON followup_command_receipts(review_id, batch_id);

CREATE TABLE followup_note_baselines (
    review_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    original_note_id TEXT NOT NULL,
    note_id TEXT NOT NULL,
    reviewer_id TEXT NOT NULL,
    body_sha256 TEXT NOT NULL CHECK (length(body_sha256)=64),
    PRIMARY KEY (review_id, generation, original_note_id, note_id),
    FOREIGN KEY (review_id, generation, original_note_id)
        REFERENCES followup_threads(review_id, generation, original_note_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_dispatch (
    delivery_id TEXT PRIMARY KEY REFERENCES messages(message_id),
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    action_id TEXT REFERENCES actions(action_id),
    batch_id TEXT,
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    head_sha TEXT NOT NULL CHECK (length(head_sha) IN (40, 64)),
    model TEXT NOT NULL CHECK (length(model) BETWEEN 1 AND 128),
    effort TEXT NOT NULL CHECK (length(effort) BETWEEN 1 AND 64),
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_batches(review_id, batch_id),
    CHECK ((action_id IS NULL) <> (batch_id IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_dispatch_action ON workflow_dispatch(action_id);
CREATE INDEX workflow_dispatch_batch ON workflow_dispatch(review_id, batch_id);
