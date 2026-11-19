CREATE TABLE launch_attempts (
    id TEXT PRIMARY KEY,
    actor BLOB NOT NULL CHECK (length(actor) = 32),
    submission_id TEXT NOT NULL,
    review_id TEXT NOT NULL REFERENCES reviews,
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    source_sha TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude')),
    checkout BLOB NOT NULL,
    common_dir BLOB NOT NULL,
    requested_model TEXT,
    requested_effort TEXT,
    invocation TEXT NOT NULL CHECK (json_valid(invocation) AND length(CAST(invocation AS BLOB)) <= 153600),
    observed_model TEXT,
    observed_effort TEXT,
    profile BLOB,
    native_id TEXT,
    native_job_id TEXT,
    native_turn_id TEXT,
    native_entry_id TEXT,
    run_id TEXT REFERENCES runs,
    session_key TEXT REFERENCES sessions,
    state TEXT NOT NULL CHECK (state IN ('pending', 'confirmed', 'rejected', 'uncertain')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    UNIQUE (actor, submission_id),
    CHECK ((native_id IS NULL) = (profile IS NULL)),
    CHECK ((native_turn_id IS NULL) = (native_entry_id IS NULL)),
    CHECK (native_job_id IS NULL OR (provider = 'claude' AND native_id IS NOT NULL)),
    CHECK (state <> 'confirmed' OR provider <> 'codex' OR native_turn_id IS NOT NULL),
    CHECK ((run_id IS NULL) = (session_key IS NULL)),
    CHECK (state <> 'confirmed' OR (native_id IS NOT NULL AND run_id IS NOT NULL)),
    CHECK (state <> 'rejected' OR native_id IS NULL)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX launch_attempts_native
    ON launch_attempts(provider, profile, native_id) WHERE native_id IS NOT NULL;
CREATE INDEX launch_attempts_pending ON launch_attempts(id) WHERE state='pending';
