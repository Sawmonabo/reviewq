CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY,
    sha256 TEXT CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    total_code_points INTEGER NOT NULL CHECK (total_code_points BETWEEN 0 AND byte_length),
    line_count INTEGER NOT NULL CHECK (line_count BETWEEN 0 AND total_code_points),
    max_line_bytes INTEGER NOT NULL CHECK (max_line_bytes BETWEEN 0 AND byte_length),
    sealed INTEGER NOT NULL CHECK (sealed IN (0, 1) AND (sealed=0 OR sha256 IS NOT NULL)),
    created_at_ms INTEGER NOT NULL
) STRICT;

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

CREATE TABLE report_versions (
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    receipt_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    kind TEXT NOT NULL CHECK (kind IN ('full_review', 'selected_threads', 'update')),
    ledger_sequence INTEGER NOT NULL CHECK (ledger_sequence >= 0),
    parent_receipt_id TEXT,
    attention_receipt_id TEXT NOT NULL,
    reviewed_sha TEXT CHECK (length(reviewed_sha) IN (40, 64)),
    revision_reason TEXT CHECK (length(CAST(revision_reason AS BLOB)) BETWEEN 1 AND 1024),
    full_mr_reviewed_sha TEXT CHECK (length(full_mr_reviewed_sha) IN (40, 64)),
    coverage TEXT NOT NULL CHECK (coverage IN ('full_mr', 'selected_threads')),
    coverage_source_sha TEXT CHECK (length(coverage_source_sha) IN (40, 64)),
    coverage_target_sha TEXT CHECK (length(coverage_target_sha) IN (40, 64)),
    verdict TEXT NOT NULL CHECK (verdict IN ('approve', 'request_changes', 'blocked', 'not_specified')),
    finding_count INTEGER NOT NULL CHECK (finding_count >= 0),
    change_count INTEGER NOT NULL CHECK (change_count >= 0),
    published_at_ms INTEGER NOT NULL,
    starting_disposition_version INTEGER NOT NULL CHECK (starting_disposition_version >= 0),
    closing_disposition_version INTEGER CHECK (closing_disposition_version >= starting_disposition_version),
    CHECK ((reviewed_sha IS NULL) <> (revision_reason IS NULL)),
    CHECK (parent_receipt_id IS NULL OR parent_receipt_id <> receipt_id),
    PRIMARY KEY (run_id, receipt_id),
    FOREIGN KEY (run_id, parent_receipt_id) REFERENCES report_versions(run_id, receipt_id),
    FOREIGN KEY (run_id, attention_receipt_id) REFERENCES report_versions(run_id, receipt_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX report_version_history ON report_versions(run_id, published_at_ms DESC, receipt_id DESC);
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
    record_revision INTEGER NOT NULL CHECK (record_revision > 0),
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    severity TEXT NOT NULL CHECK (severity IN ('P0', 'P1', 'P2', 'P3')),
    blocking INTEGER NOT NULL CHECK (blocking IN (0, 1)),
    section TEXT NOT NULL CHECK (section IN ('functional', 'quality')),
    category TEXT NOT NULL CHECK (length(category) BETWEEN 1 AND 48),
    scalar_offset INTEGER NOT NULL CHECK (scalar_offset >= 0),
    scalar_length INTEGER NOT NULL CHECK (scalar_length > 0),
    evidence_sha256 TEXT NOT NULL CHECK (length(evidence_sha256) = 64),
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
