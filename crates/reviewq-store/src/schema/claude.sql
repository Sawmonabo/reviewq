CREATE TABLE claude_companions (
    session_key TEXT PRIMARY KEY REFERENCES sessions(session_key),
    generation TEXT NOT NULL CHECK (length(generation)=32 AND generation NOT GLOB '*[^0-9a-f]*'),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    seen_at_ms INTEGER NOT NULL,
    turn_id TEXT,
    latest_prompt_id TEXT,
    latest_prompt_turn_id TEXT,
    model TEXT CHECK (length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
    effort TEXT CHECK (length(CAST(effort AS BLOB)) BETWEEN 1 AND 64)
) STRICT, WITHOUT ROWID;

CREATE TABLE claude_deliveries (
    delivery_id TEXT PRIMARY KEY REFERENCES messages(message_id),
    session_key TEXT NOT NULL REFERENCES claude_companions(session_key),
    fence INTEGER NOT NULL CHECK (fence > 0),
    generation TEXT NOT NULL CHECK (length(generation)=32 AND generation NOT GLOB '*[^0-9a-f]*'),
    authorized_at_ms INTEGER,
    anchor_generation TEXT CHECK (length(anchor_generation)=64),
    anchor_offset INTEGER CHECK (anchor_offset >= 0),
    anchor_digest BLOB CHECK (length(anchor_digest)=32),
    finished INTEGER NOT NULL DEFAULT 0 CHECK (finished IN (0,1)),
    CHECK ((anchor_generation IS NULL) = (anchor_offset IS NULL)
       AND (anchor_offset IS NULL) = (anchor_digest IS NULL)
       AND (anchor_digest IS NULL) = (authorized_at_ms IS NULL))
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX claude_one_delivery ON claude_deliveries(session_key) WHERE finished=0;
