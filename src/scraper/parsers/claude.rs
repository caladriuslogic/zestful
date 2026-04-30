//! Claude Code transcript parser. Real implementation in Task 6.

use super::{Parser, ParseResult};
use std::path::Path;

pub struct ClaudeParser;

impl Parser for ClaudeParser {
    fn agent(&self) -> &'static str {
        "claude-code"
    }

    fn parse_from(
        &self,
        _path: &Path,
        from_offset: u64,
    ) -> std::io::Result<ParseResult> {
        Ok(ParseResult {
            records: vec![],
            last_complete_offset: from_offset,
        })
    }
}
