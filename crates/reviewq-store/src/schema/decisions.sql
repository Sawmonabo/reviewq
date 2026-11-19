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
