CREATE TABLE actions (
    action_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    lead_session_key TEXT NOT NULL,
    native_turn_id TEXT,
    native_call_id TEXT,
    request_entry_id TEXT,
    source_generation TEXT,
    source_position INTEGER CHECK (source_position >= 0),
    settings_revision INTEGER CHECK (settings_revision >= 0),
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
        AND reviewed_sha IS NOT NULL AND authenticated_actor_id IS NOT NULL
        AND source_generation IS NOT NULL AND source_position IS NOT NULL AND settings_revision IS NOT NULL))
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
