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
    recovery_id TEXT REFERENCES recovery_episodes(episode_id),
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    authority_revision INTEGER NOT NULL CHECK (authority_revision > 0),
    head_sha TEXT NOT NULL CHECK (length(head_sha) IN (40, 64)),
    model TEXT NOT NULL CHECK (length(model) BETWEEN 1 AND 128),
    effort TEXT NOT NULL CHECK (length(effort) BETWEEN 1 AND 64),
    latest_user_entry_id TEXT,
    latest_user_turn_id TEXT,
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_batches(review_id, batch_id),
    CHECK ((action_id IS NOT NULL)+(batch_id IS NOT NULL)+(recovery_id IS NOT NULL)=1),
    CHECK (action_id IS NULL OR (latest_user_entry_id IS NOT NULL AND latest_user_turn_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_dispatch_action ON workflow_dispatch(action_id);
CREATE INDEX workflow_dispatch_batch ON workflow_dispatch(review_id, batch_id);

CREATE UNIQUE INDEX workflow_dispatch_one_batch ON workflow_dispatch(batch_id) WHERE batch_id IS NOT NULL;

CREATE UNIQUE INDEX workflow_dispatch_recovery ON workflow_dispatch(recovery_id) WHERE recovery_id IS NOT NULL;
