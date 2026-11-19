CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY,
    sha256 TEXT CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    original_sha256 TEXT CHECK (original_sha256 IS NULL OR (
        length(original_sha256) = 64 AND original_sha256 NOT GLOB '*[^0-9a-f]*')),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    total_code_points INTEGER NOT NULL CHECK (total_code_points BETWEEN 0 AND byte_length),
    line_count INTEGER NOT NULL CHECK (line_count BETWEEN 0 AND total_code_points),
    max_line_bytes INTEGER NOT NULL CHECK (max_line_bytes BETWEEN 0 AND byte_length),
    complete INTEGER NOT NULL CHECK (complete IN (0, 1)),
    sealed INTEGER NOT NULL CHECK (sealed IN (0, 1) AND (sealed=0 OR sha256 IS NOT NULL)),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX artifacts_staging ON artifacts(created_at_ms, artifact_id) WHERE sealed=0;

CREATE TABLE artifact_chunks (
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    byte_start INTEGER NOT NULL CHECK (byte_start >= 0),
    scalar_start INTEGER NOT NULL CHECK (scalar_start >= 0),
    raw BLOB NOT NULL CHECK (length(raw) BETWEEN 1 AND 65536),
    PRIMARY KEY (artifact_id, ordinal),
    UNIQUE (artifact_id, byte_start),
    UNIQUE (artifact_id, scalar_start)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_handoffs (
    receipt_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    lead_session_key TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    root_turn_id TEXT,
    call_id TEXT NOT NULL,
    source_generation TEXT NOT NULL CHECK (length(CAST(source_generation AS BLOB)) BETWEEN 1 AND 512),
    source_position INTEGER NOT NULL CHECK (source_position >= 0),
    request_entry_id TEXT,
    result_entry_id TEXT,
    request_sha256 TEXT NOT NULL CHECK (length(request_sha256) = 64),
    declaration_sha256 TEXT NOT NULL CHECK (length(declaration_sha256) = 64),
    form TEXT NOT NULL CHECK (form IN ('final', 'unchanged', 'needs_input')),
    state TEXT NOT NULL CHECK (state IN ('pending_completion', 'recorded', 'failed')),
    source_kind TEXT CHECK (source_kind IN ('file', 'response')),
    source_path BLOB,
    source_hash TEXT CHECK (length(source_hash) = 64),
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    revision_reason TEXT CHECK (length(CAST(revision_reason AS BLOB)) <= 1024),
    verdict TEXT CHECK (verdict IN ('approve', 'request_changes', 'blocked', 'not_specified')),
    expected_receipt_id TEXT,
    expected_sha256 TEXT CHECK (length(expected_sha256) = 64),
    base_receipt_id TEXT,
    disposition_version INTEGER NOT NULL CHECK (disposition_version >= 0),
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    due_at_ms INTEGER NOT NULL,
    artifact_id TEXT REFERENCES artifacts(artifact_id),
    reason TEXT CHECK (length(CAST(reason AS BLOB)) <= 2048),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    batch_id TEXT REFERENCES followup_batches(batch_id),
    assessments TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(assessments) AND length(CAST(assessments AS BLOB)) <= 131072),
    edits TEXT CHECK (json_valid(edits) AND length(CAST(edits AS BLOB)) <= 131072),
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    CHECK ((expected_receipt_id IS NULL) = (expected_sha256 IS NULL)),
    CHECK ((source_kind='file') = (source_path IS NOT NULL)),
    CHECK ((form='final') = (verdict IS NOT NULL)),
    CHECK (state<>'recorded' OR result_entry_id IS NOT NULL),
    FOREIGN KEY (run_id, lead_session_key) REFERENCES runs(run_id, lead_session_key),
    UNIQUE (run_id, receipt_id),
    UNIQUE (run_id, lead_session_key, turn_id, call_id)
) STRICT;

CREATE UNIQUE INDEX report_handoff_native_order ON report_handoffs(run_id, source_generation, source_position);
CREATE INDEX report_handoff_latest ON report_handoffs(run_id, created_at_ms DESC, receipt_id DESC);
CREATE INDEX report_handoff_batch ON report_handoffs(batch_id);
CREATE INDEX report_handoff_artifact ON report_handoffs(artifact_id);
CREATE INDEX report_handoff_pending ON report_handoffs(due_at_ms, receipt_id)
    WHERE state='pending_completion';

CREATE TABLE report_versions (
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    receipt_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    handoff_receipt_id TEXT CHECK (handoff_receipt_id = receipt_id),
    parent_receipt_id TEXT,
    attention_receipt_id TEXT NOT NULL,
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    revision_reason TEXT CHECK (length(CAST(revision_reason AS BLOB)) BETWEEN 1 AND 1024),
    full_mr_reviewed_sha TEXT CHECK (length(full_mr_reviewed_sha) IN (40, 64)),
    coverage TEXT NOT NULL CHECK (coverage IN ('full_mr', 'selected_threads')),
    coverage_source_sha TEXT CHECK (length(coverage_source_sha) IN (40, 64)),
    coverage_target_sha TEXT CHECK (length(coverage_target_sha) IN (40, 64)),
    verdict TEXT NOT NULL CHECK (verdict IN ('approve', 'request_changes', 'blocked', 'not_specified')),
    semantic_index TEXT NOT NULL CHECK (semantic_index IN ('ready', 'source_only')),
    finding_count INTEGER CHECK (finding_count >= 0),
    change_count INTEGER CHECK (change_count >= 0),
    captured_at_ms INTEGER NOT NULL,
    starting_disposition_version INTEGER NOT NULL CHECK (starting_disposition_version >= 0),
    closing_disposition_version INTEGER CHECK (closing_disposition_version >= starting_disposition_version),
    CHECK ((reviewed_sha IS NULL) <> (revision_reason IS NULL)),
    CHECK ((semantic_index = 'ready') = (finding_count IS NOT NULL AND change_count IS NOT NULL)),
    CHECK (parent_receipt_id IS NULL OR parent_receipt_id <> receipt_id),
    PRIMARY KEY (run_id, receipt_id),
    FOREIGN KEY (run_id, handoff_receipt_id) REFERENCES report_handoffs(run_id, receipt_id),
    FOREIGN KEY (run_id, parent_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    FOREIGN KEY (run_id, attention_receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX report_version_history ON report_versions(run_id, captured_at_ms DESC, receipt_id DESC);
CREATE INDEX report_version_artifact ON report_versions(artifact_id);
CREATE INDEX report_version_parent ON report_versions(run_id, parent_receipt_id);
CREATE INDEX report_version_attention ON report_versions(run_id, attention_receipt_id);

CREATE TABLE report_heads (
    run_id TEXT PRIMARY KEY REFERENCES runs(run_id),
    receipt_id TEXT NOT NULL,
    disposition_version INTEGER NOT NULL CHECK (disposition_version >= 0),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_findings (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) BETWEEN 1 AND 384),
    severity TEXT CHECK (length(CAST(severity AS BLOB)) <= 64),
    blocking INTEGER CHECK (blocking IN (0, 1)),
    scalar_offset INTEGER CHECK (scalar_offset >= 0),
    scalar_length INTEGER CHECK (scalar_length > 0),
    evidence_sha256 TEXT CHECK (length(evidence_sha256) = 64),
    CHECK ((scalar_offset IS NULL) = (scalar_length IS NULL)),
    CHECK (scalar_offset IS NULL OR evidence_sha256 IS NOT NULL),
    PRIMARY KEY (run_id, receipt_id, finding_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_sections (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    scalar_offset INTEGER NOT NULL CHECK (scalar_offset >= 0),
    scalar_length INTEGER NOT NULL CHECK (scalar_length > 0),
    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) BETWEEN 1 AND 384),
    evidence_sha256 TEXT NOT NULL CHECK (length(evidence_sha256) = 64),
    PRIMARY KEY (run_id, receipt_id, scalar_offset),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_changes (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    conclusion TEXT NOT NULL CHECK (conclusion IN (
        'fixed', 'clarified_withdrawn', 'superseded', 'partly_addressed', 'still_present', 'unverified')),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 1024),
    checked_sha TEXT CHECK (length(checked_sha) IN (40, 64)),
    PRIMARY KEY (run_id, receipt_id, finding_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE report_change_discussions (
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    discussion_id TEXT NOT NULL CHECK (length(CAST(discussion_id AS BLOB)) BETWEEN 1 AND 256),
    PRIMARY KEY (run_id, receipt_id, finding_id, discussion_id),
    FOREIGN KEY (run_id, receipt_id, finding_id)
        REFERENCES report_changes(run_id, receipt_id, finding_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE finding_dispositions (
    run_id TEXT NOT NULL,
    disposition_version INTEGER NOT NULL CHECK (disposition_version > 0),
    receipt_id TEXT NOT NULL,
    finding_id TEXT NOT NULL CHECK (length(finding_id) BETWEEN 1 AND 64),
    blocking INTEGER CHECK (blocking IN (0, 1)),
    publication TEXT CHECK (publication IN ('withheld', 'authorized', 'published')),
    reason TEXT NOT NULL CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 384),
    removed INTEGER NOT NULL CHECK (removed IN (0, 1)),
    PRIMARY KEY (run_id, disposition_version, finding_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id),
    CHECK (removed=0 OR (blocking IS NULL AND publication IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX finding_disposition_at ON finding_dispositions(run_id, finding_id, disposition_version DESC);
CREATE INDEX finding_disposition_receipt ON finding_dispositions(run_id, receipt_id);

CREATE TABLE report_reads (
    viewer_key BLOB NOT NULL CHECK (length(viewer_key) = 32),
    run_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    viewed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (viewer_key, run_id, receipt_id),
    FOREIGN KEY (run_id, receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX report_read_version ON report_reads(run_id, receipt_id);
