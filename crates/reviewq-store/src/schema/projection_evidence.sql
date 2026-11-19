CREATE TABLE review_reviewer_activity (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id) ON DELETE CASCADE,
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    source_sha TEXT NOT NULL CHECK (length(source_sha) IN (40,64)),
    latest_comment_at_ms INTEGER,
    observed_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE report_delivery_failures (
    notification_id TEXT PRIMARY KEY REFERENCES notifications(notification_id),
    run_id TEXT NOT NULL,
    lead_session_key TEXT NOT NULL,
    native_turn_id TEXT NOT NULL,
    native_call_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('report_delivery_failure', 'report_delivery_rejection')),
    identity_verified INTEGER NOT NULL CHECK (identity_verified IN (0,1)),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 2048),
    occurred_at_ms INTEGER NOT NULL,
    resolved_by_receipt_id TEXT,
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key)
) STRICT, WITHOUT ROWID;

CREATE INDEX report_delivery_failure_pending ON report_delivery_failures(run_id, occurred_at_ms DESC, notification_id)
    WHERE resolved_by_receipt_id IS NULL;

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
