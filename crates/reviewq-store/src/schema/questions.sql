CREATE TABLE question_attempts (
    attempt_id TEXT PRIMARY KEY,
    session_key TEXT NOT NULL REFERENCES sessions(session_key) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    native_turn_id TEXT NOT NULL,
    native_item_id TEXT NOT NULL,
    source_generation TEXT NOT NULL,
    source_position INTEGER NOT NULL CHECK (source_position >= 0),
    blocking INTEGER NOT NULL CHECK (blocking IN (0, 1)),
    actor_key BLOB NOT NULL CHECK (length(actor_key) = 32),
    submission_id TEXT NOT NULL,
    answer_digest BLOB CHECK (answer_digest IS NULL OR length(answer_digest) = 32),
    state TEXT NOT NULL CHECK (state IN ('submitting', 'answered', 'closed', 'uncertain', 'unavailable')),
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (actor_key, submission_id)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX question_one_live_attempt
    ON question_attempts(session_key, native_turn_id, native_item_id)
    WHERE state<>'unavailable';

CREATE TABLE question_receipt_heads (
    session_key TEXT NOT NULL REFERENCES sessions(session_key) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL REFERENCES question_attempts(attempt_id) ON DELETE CASCADE,
    PRIMARY KEY (session_key,request_id)
) STRICT, WITHOUT ROWID;
