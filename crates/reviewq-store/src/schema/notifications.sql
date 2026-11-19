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
    desktop_eligible INTEGER NOT NULL DEFAULT 0 CHECK (desktop_eligible IN (0, 1)),
    desktop_episode TEXT CHECK (length(CAST(desktop_episode AS BLOB)) BETWEEN 1 AND 512),
    UNIQUE (source, occurrence_id),
    FOREIGN KEY (review_id, run_id) REFERENCES runs(review_id, run_id),
    FOREIGN KEY (run_id, session_key) REFERENCES sessions(run_id, session_key),
    CHECK (session_key IS NULL OR run_id IS NOT NULL)
) STRICT;

CREATE INDEX notifications_order ON notifications(occurred_at_ms DESC, source, notification_id DESC)
    WHERE dismissed_at_ms IS NULL;
CREATE INDEX notifications_unread ON notifications(review_id, ordinal)
    WHERE dismissed_at_ms IS NULL AND read_at_ms IS NULL;
CREATE INDEX notifications_desktop ON notifications(ordinal)
    WHERE desktop_eligible=1 AND read_at_ms IS NULL AND dismissed_at_ms IS NULL AND notified_at_ms IS NULL;
CREATE INDEX notifications_review ON notifications(review_id, occurred_at_ms DESC);
CREATE INDEX notifications_run_owner ON notifications(review_id, run_id);
CREATE INDEX notifications_session_owner ON notifications(run_id, session_key);

CREATE TABLE desktop_receipts (
    receipt_id TEXT PRIMARY KEY,
    episode_key TEXT NOT NULL UNIQUE CHECK (length(CAST(episode_key AS BLOB)) BETWEEN 1 AND 512),
    notification_id TEXT NOT NULL REFERENCES notifications(notification_id),
    state TEXT NOT NULL CHECK (state IN ('claimed', 'delivered', 'failed', 'uncertain')),
    fence INTEGER NOT NULL CHECK (fence > 0),
    attempts INTEGER NOT NULL DEFAULT 1 CHECK (attempts BETWEEN 1 AND 3),
    attempted_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    due_at_ms INTEGER,
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 128),
    CHECK ((state='claimed') = (finished_at_ms IS NULL)),
    CHECK (due_at_ms IS NULL OR state='failed')
) STRICT;

CREATE INDEX desktop_receipt_notification ON desktop_receipts(notification_id);
CREATE INDEX desktop_receipt_due ON desktop_receipts(due_at_ms, receipt_id) WHERE state='failed';

CREATE TABLE desktop_members (
    receipt_id TEXT NOT NULL REFERENCES desktop_receipts(receipt_id),
    notification_id TEXT NOT NULL REFERENCES notifications(notification_id),
    PRIMARY KEY (receipt_id, notification_id),
    UNIQUE (notification_id)
) STRICT, WITHOUT ROWID;
