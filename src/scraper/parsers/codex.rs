//! Codex transcript parser. Real implementation in Task 8.

use super::{Parser, ParseResult};
use std::path::Path;

pub struct CodexParser;

impl Parser for CodexParser {
    fn agent(&self) -> &'static str {
        "codex"
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
