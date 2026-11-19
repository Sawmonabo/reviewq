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
    reviewer_id TEXT,
    binding_verified INTEGER NOT NULL CHECK (binding_verified IN (0, 1)),
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 1024),
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    CHECK (binding_verified=0 OR (run_id IS NOT NULL AND reviewer_id IS NOT NULL))
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
