CREATE TABLE claude_companions (
    session_key TEXT PRIMARY KEY REFERENCES sessions(session_key),
    generation TEXT NOT NULL CHECK (length(generation)=32 AND generation NOT GLOB '*[^0-9a-f]*'),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    seen_at_ms INTEGER NOT NULL,
    turn_id TEXT,
    permission_turn_id TEXT,
    turn_step INTEGER CHECK(turn_step>=0),
    permission_step INTEGER,
    -- A permission prompt that arrived while the companion was neither live nor gone, which its
    -- next report binds to the turn and step that report names.
    permission_unbound INTEGER NOT NULL DEFAULT 0 CHECK (permission_unbound IN (0,1)),
    latest_prompt_id TEXT,
    latest_prompt_turn_id TEXT,
    latest_prompt_manual INTEGER NOT NULL CHECK (latest_prompt_manual IN (0,1)),
    model TEXT CHECK (length(CAST(model AS BLOB)) BETWEEN 1 AND 128),
    effort TEXT CHECK (length(CAST(effort AS BLOB)) BETWEEN 1 AND 64),
    -- The process hosting this generation, as its heartbeats name it, since the first report
    -- that named it; trusted once a report at least 5 seconds later named the same process.
    host_pid INTEGER CHECK (host_pid > 0),
    host_start INTEGER CHECK (host_start >= 0),
    host_boot TEXT CHECK (length(CAST(host_boot AS BLOB)) BETWEEN 1 AND 64),
    host_namespace TEXT CHECK (length(CAST(host_namespace AS BLOB)) BETWEEN 1 AND 64),
    host_since_ms INTEGER,
    host_trusted INTEGER NOT NULL DEFAULT 0 CHECK (host_trusted IN (0,1)),
    CHECK (latest_prompt_turn_id IS NULL OR latest_prompt_id IS NOT NULL),
    CHECK ((host_pid IS NULL) = (host_start IS NULL) AND (host_pid IS NULL) = (host_boot IS NULL)
       AND (host_pid IS NULL) = (host_namespace IS NULL) AND (host_pid IS NULL) = (host_since_ms IS NULL)),
    CHECK (host_trusted = 0 OR host_pid IS NOT NULL),
    CHECK (latest_prompt_manual=0 OR latest_prompt_turn_id IS NOT NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE claude_deliveries (
    delivery_id TEXT NOT NULL REFERENCES messages(message_id),
    session_key TEXT NOT NULL REFERENCES claude_companions(session_key),
    fence INTEGER NOT NULL CHECK (fence > 0),
    generation TEXT NOT NULL CHECK (length(generation)=32 AND generation NOT GLOB '*[^0-9a-f]*'),
    authorized_at_ms INTEGER,
    anchor_generation TEXT CHECK (length(anchor_generation)=64),
    anchor_offset INTEGER CHECK (anchor_offset >= 0),
    anchor_digest BLOB CHECK (length(anchor_digest)=32),
    finished_kind TEXT CHECK (finished_kind IN ('started','deferred','dropped')),
    PRIMARY KEY(delivery_id,fence),
    CHECK ((anchor_generation IS NULL) = (anchor_offset IS NULL)
       AND (anchor_offset IS NULL) = (anchor_digest IS NULL)
       AND (anchor_digest IS NULL) = (authorized_at_ms IS NULL))
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX claude_one_delivery ON claude_deliveries(session_key) WHERE finished_kind IS NULL;

CREATE TABLE claude_turns (
    session_key TEXT NOT NULL REFERENCES claude_companions(session_key),
    turn_id TEXT NOT NULL,
    companion_generation TEXT NOT NULL CHECK (length(companion_generation)=32),
    normal INTEGER NOT NULL CHECK (normal IN (0,1)),
    PRIMARY KEY(session_key,turn_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE claude_children (
    session_key TEXT PRIMARY KEY REFERENCES sessions(session_key),
    companion_generation TEXT NOT NULL CHECK (length(companion_generation)=32),
    sequence INTEGER NOT NULL CHECK (sequence>0)
) STRICT, WITHOUT ROWID;

CREATE TABLE claude_display_events (
    session_key TEXT NOT NULL REFERENCES claude_companions(session_key),
    turn_id TEXT NOT NULL,
    display_id TEXT NOT NULL,
    first_seen INTEGER NOT NULL DEFAULT 0 CHECK (first_seen IN (0,1)),
    final_seen INTEGER NOT NULL DEFAULT 0 CHECK (final_seen IN (0,1)),
    PRIMARY KEY (session_key,turn_id,display_id)
) STRICT, WITHOUT ROWID;
