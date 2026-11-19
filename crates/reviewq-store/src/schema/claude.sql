CREATE TABLE claude_companions (
    session_key TEXT PRIMARY KEY REFERENCES sessions(session_key),
    generation TEXT NOT NULL CHECK (length(generation)=32 AND generation NOT GLOB '*[^0-9a-f]*'),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    seen_at_ms INTEGER NOT NULL,
    turn_id TEXT,
    latest_prompt_id TEXT,
    model TEXT CHECK (length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
    effort TEXT CHECK (length(CAST(effort AS BLOB)) BETWEEN 1 AND 64)
) STRICT, WITHOUT ROWID;
