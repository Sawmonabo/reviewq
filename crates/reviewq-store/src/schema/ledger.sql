CREATE TABLE ledger_runs (
    run_id TEXT PRIMARY KEY REFERENCES runs(run_id),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    published_sequence INTEGER NOT NULL DEFAULT 0 CHECK (published_sequence BETWEEN 0 AND sequence)
) STRICT, WITHOUT ROWID;

CREATE INDEX ledger_runs_unpublished ON ledger_runs(run_id) WHERE sequence > published_sequence;

CREATE TABLE ledger_changes (
    run_id TEXT NOT NULL REFERENCES ledger_runs(run_id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    finding INTEGER CHECK (finding BETWEEN 1 AND 999999999),
    note_section TEXT CHECK (note_section IN ('title', 'summary', 'rationale', 'flow', 'changes',
        'threads', 'questions', 'verification', 'publication', 'extra')),
    note_title TEXT CHECK (length(note_title) <= 120),
    revision INTEGER NOT NULL CHECK (revision > 0),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude')),
    native_session TEXT NOT NULL CHECK (length(CAST(native_session AS BLOB)) BETWEEN 1 AND 512),
    native_turn_id TEXT CHECK (length(CAST(native_turn_id AS BLOB)) BETWEEN 1 AND 512),
    call_id TEXT NOT NULL CHECK (length(CAST(call_id AS BLOB)) BETWEEN 1 AND 512),
    author TEXT CHECK (length(CAST(author AS BLOB)) BETWEEN 1 AND 512),
    changed_at_ms INTEGER NOT NULL,
    severity TEXT CHECK (severity IN ('P0', 'P1', 'P2', 'P3')),
    blocking INTEGER CHECK (blocking IN (0, 1)),
    section TEXT CHECK (section IN ('functional', 'quality')),
    title TEXT CHECK (length(title) BETWEEN 1 AND 200),
    category TEXT CHECK (length(category) BETWEEN 1 AND 48),
    evidence_kind TEXT CHECK (evidence_kind IN ('observed', 'gap')),
    conclusion TEXT CHECK (conclusion IN (
        'fixed', 'clarified_withdrawn', 'superseded', 'partly_addressed', 'still_present', 'unverified')),
    checked_sha TEXT CHECK (length(checked_sha) IN (40, 64)),
    outcome_reason TEXT CHECK (length(outcome_reason) BETWEEN 1 AND 1024),
    discussion_ids TEXT CHECK (json_valid(discussion_ids)),
    locations TEXT CHECK (json_valid(locations)),
    issue TEXT,
    consequence TEXT,
    evidence TEXT,
    proposed_comment TEXT CHECK (length(proposed_comment) > 0),
    details TEXT CHECK (length(details) > 0),
    body TEXT,
    PRIMARY KEY (run_id, sequence),
    CHECK ((finding IS NULL) <> (note_section IS NULL)),
    CHECK (note_section IS NULL OR (note_title IS NOT NULL AND body IS NOT NULL
        AND (note_section = 'extra') = (note_title <> ''))),
    CHECK (finding IS NULL OR (severity IS NOT NULL AND blocking IS NOT NULL AND section IS NOT NULL
        AND title IS NOT NULL AND evidence_kind IS NOT NULL AND locations IS NOT NULL
        AND issue IS NOT NULL AND consequence IS NOT NULL AND evidence IS NOT NULL AND body IS NULL)),
    CHECK ((conclusion IS NULL) = (outcome_reason IS NULL)
        AND (conclusion IS NULL) = (discussion_ids IS NULL)
        AND (conclusion IS NOT NULL OR checked_sha IS NULL))
) STRICT;

CREATE UNIQUE INDEX ledger_change_finding ON ledger_changes(run_id, finding, revision)
    WHERE finding IS NOT NULL;
CREATE UNIQUE INDEX ledger_change_note ON ledger_changes(run_id, note_section, note_title, revision)
    WHERE note_section IS NOT NULL;

CREATE TABLE ledger_findings (
    run_id TEXT NOT NULL REFERENCES ledger_runs(run_id),
    finding INTEGER NOT NULL CHECK (finding BETWEEN 1 AND 999999999),
    revision INTEGER NOT NULL CHECK (revision > 0),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    outcome_sequence INTEGER CHECK (outcome_sequence BETWEEN 1 AND sequence),
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    first_path TEXT CHECK (length(first_path) BETWEEN 1 AND 512),
    first_line INTEGER CHECK (first_line IS NULL OR (first_line > 0 AND first_path IS NOT NULL)),
    PRIMARY KEY (run_id, finding),
    FOREIGN KEY (run_id, sequence) REFERENCES ledger_changes(run_id, sequence)
) STRICT, WITHOUT ROWID;

CREATE INDEX ledger_finding_changed ON ledger_findings(run_id, sequence);
CREATE INDEX ledger_finding_active ON ledger_findings(run_id, sequence) WHERE active = 1;
CREATE INDEX ledger_finding_match ON ledger_findings(run_id, title, first_path, first_line)
    WHERE active = 1;

CREATE TABLE ledger_notes (
    run_id TEXT NOT NULL REFERENCES ledger_runs(run_id),
    section TEXT NOT NULL CHECK (section IN ('title', 'summary', 'rationale', 'flow', 'changes',
        'threads', 'questions', 'verification', 'publication', 'extra')),
    title TEXT NOT NULL CHECK ((section = 'extra') = (title <> '') AND length(title) <= 120),
    revision INTEGER NOT NULL CHECK (revision > 0),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    first_sequence INTEGER NOT NULL CHECK (first_sequence BETWEEN 1 AND sequence),
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    PRIMARY KEY (run_id, section, title),
    FOREIGN KEY (run_id, sequence) REFERENCES ledger_changes(run_id, sequence)
) STRICT, WITHOUT ROWID;

CREATE INDEX ledger_note_changed ON ledger_notes(run_id, sequence);

CREATE TABLE review_passes (
    pass_id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    kind TEXT NOT NULL CHECK (kind IN ('full_review', 'selected_threads')),
    state TEXT NOT NULL CHECK (state IN ('open', 'waiting', 'closed')),
    opened_by TEXT NOT NULL CHECK (opened_by IN ('launch', 'batch', 'record')),
    launch_id TEXT REFERENCES launch_attempts(id),
    review_id TEXT,
    batch_id TEXT,
    call_id TEXT CHECK (length(CAST(call_id AS BLOB)) BETWEEN 1 AND 512),
    opened_sequence INTEGER NOT NULL CHECK (opened_sequence >= 0),
    opened_at_ms INTEGER NOT NULL,
    -- When the lead was first seen starting a turn for this pass. Settling waits for it.
    lead_started_at_ms INTEGER,
    held_turn_id TEXT CHECK (length(CAST(held_turn_id AS BLOB)) BETWEEN 1 AND 512),
    held_at_ms INTEGER,
    hold_reason TEXT CHECK (length(hold_reason) BETWEEN 1 AND 1024),
    gate_turn_id TEXT CHECK (length(CAST(gate_turn_id AS BLOB)) BETWEEN 1 AND 512),
    gate_blocks INTEGER NOT NULL DEFAULT 0 CHECK (gate_blocks >= 0),
    closed_by TEXT CHECK (closed_by IN ('publish', 'successor', 'archive')),
    closed_receipt_id TEXT,
    closed_at_ms INTEGER,
    FOREIGN KEY (review_id, batch_id) REFERENCES followup_batches(review_id, batch_id),
    FOREIGN KEY (run_id, closed_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    CHECK ((opened_by = 'launch') = (launch_id IS NOT NULL)),
    CHECK ((opened_by = 'batch') = (batch_id IS NOT NULL) AND (batch_id IS NULL) = (review_id IS NULL)),
    CHECK ((opened_by = 'record') = (call_id IS NOT NULL)),
    CHECK (opened_by <> 'record' OR lead_started_at_ms IS NOT NULL),
    CHECK ((kind = 'selected_threads') = (opened_by = 'batch')),
    CHECK (state <> 'waiting' OR held_at_ms IS NOT NULL),
    CHECK ((held_at_ms IS NULL) = (hold_reason IS NULL) AND (held_turn_id IS NULL OR held_at_ms IS NOT NULL)),
    CHECK ((state = 'closed') = (closed_by IS NOT NULL) AND (closed_by IS NULL) = (closed_at_ms IS NULL)),
    CHECK (closed_receipt_id IS NULL OR closed_by = 'publish')
) STRICT;

CREATE UNIQUE INDEX review_pass_current ON review_passes(run_id) WHERE state <> 'closed';

CREATE TABLE ledger_calls (
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude')),
    native_session TEXT NOT NULL CHECK (length(CAST(native_session AS BLOB)) BETWEEN 1 AND 512),
    call_id TEXT NOT NULL CHECK (length(CAST(call_id AS BLOB)) BETWEEN 1 AND 512),
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    tool TEXT NOT NULL CHECK (tool IN ('record_finding', 'assess_finding', 'record_note',
        'finish_review', 'needs_input')),
    target TEXT NOT NULL CHECK (length(CAST(target AS BLOB)) BETWEEN 1 AND 512),
    native_turn_id TEXT CHECK (length(CAST(native_turn_id AS BLOB)) BETWEEN 1 AND 512),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    result TEXT NOT NULL CHECK (json_valid(result)),
    accepted_at_ms INTEGER NOT NULL,
    PRIMARY KEY (provider, native_session, call_id)
) STRICT;

CREATE INDEX ledger_call_closing ON ledger_calls(run_id, native_turn_id) WHERE tool = 'finish_review';
CREATE INDEX ledger_call_findings ON ledger_calls(run_id, accepted_at_ms)
    WHERE tool IN ('record_finding', 'assess_finding');

-- One row once Codex's stop hook has reported on this installation.
CREATE TABLE codex_stop_hook_reported (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1)
) STRICT;
