CREATE TABLE history_prompt_overviews (
    session_key TEXT PRIMARY KEY REFERENCES sessions(session_key) ON DELETE CASCADE,
    generation TEXT NOT NULL CHECK (length(generation) BETWEEN 1 AND 512),
    pinned_end INTEGER NOT NULL CHECK (pinned_end>=0),
    source_witness BLOB NOT NULL CHECK (length(source_witness)=32),
    total INTEGER NOT NULL CHECK (total>=0),
    next_offset INTEGER CHECK (next_offset>=0 AND next_offset<pinned_end),
    next_witness BLOB CHECK (length(next_witness)=32),
    CHECK ((next_offset IS NULL)=(next_witness IS NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE history_prompt_groups (
    session_key TEXT NOT NULL REFERENCES history_prompt_overviews(session_key) ON DELETE CASCADE,
    slot INTEGER NOT NULL CHECK (slot BETWEEN 0 AND 31),
    first_ordinal INTEGER NOT NULL CHECK (first_ordinal>0),
    last_ordinal INTEGER NOT NULL CHECK (last_ordinal>=first_ordinal),
    start_ordinal INTEGER NOT NULL CHECK (start_ordinal BETWEEN 1 AND first_ordinal),
    byte_offset INTEGER NOT NULL CHECK (byte_offset>=0),
    high_water INTEGER NOT NULL CHECK (high_water>byte_offset),
    witness BLOB NOT NULL CHECK (length(witness)=32),
    PRIMARY KEY (session_key,slot)
) STRICT, WITHOUT ROWID;
