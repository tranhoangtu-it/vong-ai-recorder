-- Vọng AI Recorder MVP 0.1 — Initial schema
-- Plan v2 Section 4.4 — sessions / transcript_segments / summaries
-- Plan v2 Section 13.3.4 — FTS5 unicode61 remove_diacritics=2 for Vietnamese tone-insensitive search

-- ============================================================================
-- sessions: one row per recording session (user clicks Start → Stop)
-- ============================================================================
CREATE TABLE sessions (
    id              INTEGER PRIMARY KEY,
    started_at      INTEGER NOT NULL,
    ended_at        INTEGER,
    duration_ms     INTEGER,
    audio_source    TEXT NOT NULL,
    provider        TEXT NOT NULL,
    detected_language TEXT,
    audio_path      TEXT,
    meta_json       TEXT
);

CREATE INDEX idx_sessions_started ON sessions(started_at DESC);

-- ============================================================================
-- transcript_segments: one row per Final token from STT provider
-- ============================================================================
CREATE TABLE transcript_segments (
    id              INTEGER PRIMARY KEY,
    session_id      INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    start_ms        INTEGER NOT NULL,
    end_ms          INTEGER NOT NULL,
    speaker_label   TEXT,
    text            TEXT NOT NULL,
    language        TEXT,
    confidence      REAL,
    is_final        INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX idx_segments_session ON transcript_segments(session_id, seq);

-- ============================================================================
-- FTS5 full-text search index
-- remove_diacritics=2 makes Vietnamese tone-insensitive
-- ============================================================================
CREATE VIRTUAL TABLE transcript_fts USING fts5(
    text,
    content='transcript_segments',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2"
);

CREATE TRIGGER transcript_segments_ai AFTER INSERT ON transcript_segments BEGIN
    INSERT INTO transcript_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER transcript_segments_ad AFTER DELETE ON transcript_segments BEGIN
    INSERT INTO transcript_fts(transcript_fts, rowid, text) VALUES('delete', old.id, old.text);
END;

CREATE TRIGGER transcript_segments_au AFTER UPDATE ON transcript_segments BEGIN
    INSERT INTO transcript_fts(transcript_fts, rowid, text) VALUES('delete', old.id, old.text);
    INSERT INTO transcript_fts(rowid, text) VALUES (new.id, new.text);
END;

-- ============================================================================
-- summaries: placeholder for future LLM post-process (MVP 0.2+)
-- ============================================================================
CREATE TABLE summaries (
    id              INTEGER PRIMARY KEY,
    session_id      INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    llm_provider    TEXT,
    llm_model       TEXT,
    content         TEXT NOT NULL,
    rendered_svg    BLOB,
    created_at      INTEGER NOT NULL
);

CREATE INDEX idx_summaries_session ON summaries(session_id);
