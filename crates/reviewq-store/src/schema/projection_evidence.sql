CREATE TABLE review_reviewer_activity (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id) ON DELETE CASCADE,
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    source_sha TEXT NOT NULL CHECK (length(source_sha) IN (40,64)),
    latest_comment_at_ms INTEGER,
    observed_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE report_delivery_failures (
    failure_id INTEGER PRIMARY KEY,
    notification_id TEXT UNIQUE REFERENCES notifications(notification_id),
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    pass_id INTEGER REFERENCES review_passes(pass_id),
    kind TEXT NOT NULL CHECK (kind IN ('unfinished_review', 'closing_refused', 'update_failed')),
    native_turn_id TEXT CHECK (length(CAST(native_turn_id AS BLOB)) BETWEEN 1 AND 512),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 2048),
    ledger_sequence INTEGER NOT NULL CHECK (ledger_sequence >= 0),
    occurred_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= occurred_at_ms),
    resolved_at_ms INTEGER,
    resolved_by_receipt_id TEXT,
    FOREIGN KEY (run_id, resolved_by_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    CHECK (resolved_by_receipt_id IS NULL OR resolved_at_ms IS NOT NULL),
    CHECK (notification_id IS NOT NULL OR resolved_at_ms IS NOT NULL)
) STRICT;

CREATE UNIQUE INDEX report_delivery_failure_key
    ON report_delivery_failures(run_id, coalesce(pass_id, 0), kind) WHERE resolved_at_ms IS NULL;
CREATE INDEX report_delivery_failure_pending ON report_delivery_failures(run_id, occurred_at_ms DESC, failure_id DESC)
    WHERE resolved_at_ms IS NULL;

CREATE TABLE native_async_questions (
    run_id TEXT NOT NULL,
    session_key TEXT NOT NULL,
    request_id TEXT NOT NULL,
    native_turn_id TEXT NOT NULL,
    native_item_id TEXT NOT NULL,
    source_generation TEXT NOT NULL,
    source_position INTEGER NOT NULL CHECK (source_position >= 0),
    source_line_sha256 TEXT NOT NULL CHECK (length(source_line_sha256) = 64),
    occurred_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'submitting', 'answered', 'closed', 'unavailable', 'uncertain')),
    answer_submission_id TEXT,
    PRIMARY KEY (session_key, native_turn_id, native_item_id),
    UNIQUE (session_key, request_id),
    FOREIGN KEY (run_id, session_key) REFERENCES sessions(run_id, session_key)
) STRICT, WITHOUT ROWID;

CREATE INDEX native_async_question_pending ON native_async_questions(run_id, occurred_at_ms DESC, session_key)
    WHERE state='pending';

CREATE TABLE run_request_evidence (
    run_id TEXT PRIMARY KEY,
    lead_session_key TEXT NOT NULL,
    requested_model TEXT CHECK (length(CAST(requested_model AS BLOB)) BETWEEN 1 AND 256),
    requested_effort TEXT CHECK (length(CAST(requested_effort AS BLOB)) BETWEEN 1 AND 64),
    skill_name TEXT NOT NULL CHECK (length(CAST(skill_name AS BLOB)) BETWEEN 1 AND 256),
    input_slot TEXT CHECK (input_slot IN ('review_request','skill_arguments')),
    native_turn_id TEXT CHECK (length(CAST(native_turn_id AS BLOB)) BETWEEN 1 AND 256),
    native_entry_id TEXT CHECK (length(CAST(native_entry_id AS BLOB)) BETWEEN 1 AND 256),
    source_generation TEXT CHECK (length(source_generation)=64 AND source_generation NOT GLOB '*[^0-9a-f]*'),
    CHECK ((input_slot IS NULL) = (native_turn_id IS NULL)
       AND (input_slot IS NULL) = (native_entry_id IS NULL)
       AND (input_slot IS NULL) = (source_generation IS NULL)),
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key)
) STRICT, WITHOUT ROWID;
