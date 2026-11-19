CREATE TABLE recovery_preferences (
    singleton INTEGER PRIMARY KEY CHECK (singleton=1),
    revision INTEGER NOT NULL CHECK (revision>0),
    automatic INTEGER NOT NULL CHECK (automatic IN (0,1))
) STRICT;

CREATE TABLE recovery_episodes (
    episode_id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    run_id TEXT NOT NULL,
    lead_session_key TEXT NOT NULL,
    failed_turn_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('usage_limit','model_capacity','access')),
    state TEXT NOT NULL CHECK (state IN ('disabled','waiting','queued','started','uncertain','cancelled','complete')),
    revision INTEGER NOT NULL CHECK (revision>0),
    head_sha TEXT NOT NULL CHECK (length(head_sha) IN (40,64)),
    settings_revision INTEGER NOT NULL CHECK (settings_revision>=0),
    attempted_count INTEGER NOT NULL DEFAULT 0 CHECK (attempted_count>=0),
    observed_at_ms INTEGER NOT NULL,
    due_at_ms INTEGER,
    last_attempt_at_ms INTEGER,
    reason TEXT,
    account_hash TEXT CHECK (length(account_hash)=64),
    capacity_hash TEXT CHECK (length(capacity_hash)=64),
    model_provider TEXT,
    root_turn_id TEXT,
    model TEXT,
    effort TEXT,
    submitted_entry_id TEXT,
    reviewer_id TEXT,
    submission_id TEXT,
    delivery_id TEXT REFERENCES messages(message_id),
    resumed_turn_id TEXT,
    FOREIGN KEY (review_id,run_id) REFERENCES runs(review_id,run_id),
    FOREIGN KEY (run_id,lead_session_key) REFERENCES runs(run_id,lead_session_key),
    UNIQUE (run_id,lead_session_key,failed_turn_id),
    CHECK (state<>'queued' OR (submission_id IS NOT NULL AND root_turn_id IS NOT NULL AND account_hash IS NOT NULL
      AND capacity_hash IS NOT NULL AND model_provider IS NOT NULL AND model IS NOT NULL AND effort IS NOT NULL))
) STRICT;

CREATE INDEX recovery_due ON recovery_episodes(due_at_ms,episode_id) WHERE state='waiting';
CREATE INDEX recovery_owner ON recovery_episodes(run_id,lead_session_key,observed_at_ms DESC,episode_id DESC);
CREATE INDEX recovery_dispatch ON recovery_episodes(delivery_id);

CREATE TABLE managed_interrupts (
    run_id TEXT NOT NULL,
    lead_session_key TEXT NOT NULL,
    native_turn_id TEXT NOT NULL,
    native_job_id TEXT CHECK (native_job_id IS NULL OR length(native_job_id) BETWEEN 1 AND 64),
    state TEXT NOT NULL CHECK (state IN ('reserved','interrupted','deferred','rejected','uncertain')),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (run_id,lead_session_key,native_turn_id),
    FOREIGN KEY (run_id,lead_session_key) REFERENCES runs(run_id,lead_session_key)
) STRICT, WITHOUT ROWID;
