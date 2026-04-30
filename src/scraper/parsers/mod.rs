//! Parsers convert a transcript file's bytes into `TurnRecord`s.
//! One parser per agent. Parsers are pure: no I/O, no clock.
//! Caller hands them bytes (or a path + offset); parsers return
//! everything they could parse plus the offset of the last fully
//! parsed line so the caller can advance state safely.

use std::path::Path;

/// Token breakdown extracted from a single turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

/// One parsed turn from a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRecord {
    pub session_id: String,
    pub turn_id: String,
    pub model: String,
    pub tokens: Tokens,
    /// ms epoch of the turn-completion event in the transcript.
    pub ts_ms: i64,
    /// Number of API messages (assistant turns) covered by this record.
    /// Usually 1 per record; some parsers may aggregate.
    pub message_count: u32,
}

/// Result of a parse attempt. `last_complete_offset` is the offset of
/// the byte immediately following the last fully parsed record. The
/// dispatch loop advances `state.last_offset` to this value and retries
/// from there next tick if there's a partial trailing line.
pub struct ParseResult {
    pub records: Vec<TurnRecord>,
    pub last_complete_offset: u64,
}

/// Per-agent parser. Implementations live in sibling modules.
pub trait Parser: Send + Sync {
    /// The agent label this parser handles (matches `FileState.agent`).
    fn agent(&self) -> &'static str;

    /// Parse the file at `path` from byte offset `from_offset` to EOF.
    /// Returns the records and the offset of the last fully parsed line.
    /// Errors are reserved for I/O failures; per-line malformation is
    /// counted internally and skipped, never returned as an error.
    fn parse_from(
        &self,
        path: &Path,
        from_offset: u64,
    ) -> std::io::Result<ParseResult>;
}

pub mod claude;
pub mod codex;
