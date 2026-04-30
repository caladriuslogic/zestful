-- Migration 002: scraper_file_state
--
-- Per-file state for the agent-scraper subsystem. Tracks parse offsets
-- and fingerprints so the scraper can:
--   * resume incremental parsing of append-only JSONL transcripts
--   * detect file replacement / truncation (fingerprint mismatch)
--   * survive daemon restarts without re-emitting historical turns
--
-- Spec: docs/superpowers/specs/2026-04-30-agent-scraper-design.md

CREATE TABLE IF NOT EXISTS scraper_file_state (
    path         TEXT PRIMARY KEY,
    agent        TEXT NOT NULL,    -- 'claude-code' | 'codex'
    fingerprint  TEXT NOT NULL,    -- "<size>:<mtime_ms>"
    last_offset  INTEGER NOT NULL DEFAULT 0,
    last_emit_ts INTEGER NOT NULL  -- ms epoch
);

CREATE INDEX IF NOT EXISTS idx_scraper_file_state_agent
    ON scraper_file_state(agent);
