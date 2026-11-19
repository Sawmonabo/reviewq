CREATE TABLE settings (
    provider TEXT PRIMARY KEY CHECK (provider IN ('gitlab', 'jira')),
    origin TEXT NOT NULL CHECK (length(CAST(origin AS BLOB)) BETWEEN 1 AND 512),
    email TEXT CHECK (length(CAST(email AS BLOB)) BETWEEN 1 AND 320),
    token_type TEXT CHECK (token_type IN ('scoped', 'unscoped')),
    cloud_id TEXT CHECK (length(cloud_id) = 36),
    identity_id TEXT NOT NULL CHECK (length(CAST(identity_id AS BLOB)) BETWEEN 1 AND 512),
    identity_label TEXT NOT NULL CHECK (length(CAST(identity_label AS BLOB)) BETWEEN 1 AND 512),
    CHECK ((provider = 'jira') = (email IS NOT NULL)),
    CHECK ((provider = 'jira') = (token_type IS NOT NULL)),
    CHECK ((provider = 'jira') = (cloud_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE credentials (
    provider TEXT PRIMARY KEY REFERENCES settings(provider),
    token TEXT NOT NULL CHECK (length(CAST(token AS BLOB)) BETWEEN 1 AND 8192)
) STRICT, WITHOUT ROWID;
