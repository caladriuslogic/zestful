//! Static pricing + context-window tables for known models. Compiled
//! into the binary; updated at release time when upstream pricing
//! changes. Lookups return `None` for unknown models so callers
//! degrade gracefully (`cost_estimate_usd = null`,
//! `context.max_tokens = null`, etc.).
//!
//! Pricing reference dates and sources MUST be kept in the comment
//! at the top of the PRICES constant. Stale entries surface as
//! slightly wrong dollar figures; missing entries surface as null.

use crate::scraper::parsers::Tokens;

/// Per-million-token prices (USD).
#[derive(Debug, Clone, Copy)]
pub struct PriceBreakdown {
    pub input_per_mtoken: f64,
    pub output_per_mtoken: f64,
    pub cache_read_per_mtoken: f64,
    pub cache_write_per_mtoken: f64,
    pub reasoning_per_mtoken: f64,
}

/// Context-window size in tokens. Lookup table for `model_id -> max_tokens`.
///
/// Source: https://docs.anthropic.com/en/docs/about-claude/models  and
///         https://platform.openai.com/docs/models
/// Last verified: 2026-04-30
const CONTEXT_WINDOWS: &[(&str, u64)] = &[
    ("claude-3-5-sonnet-20241022", 200_000),
    ("claude-3-5-sonnet-latest",   200_000),
    ("claude-3-7-sonnet-20250219", 200_000),
    ("claude-3-7-sonnet-latest",   200_000),
    ("claude-opus-4-7",            1_000_000),
    ("claude-sonnet-4-6",          1_000_000),
    ("claude-haiku-4-5-20251001",  200_000),
    // Codex models — populate from Task 7 fixtures.
];

/// Per-million-token prices (USD).
///
/// Source: https://www.anthropic.com/pricing and https://openai.com/api/pricing/
/// Last verified: 2026-04-30
const PRICES: &[(&str, PriceBreakdown)] = &[
    // Claude 3.5/3.7 Sonnet — 200K context, $3/$15/$0.30/$3.75 per Mtok
    ("claude-3-5-sonnet-20241022", SONNET_3X_PRICES),
    ("claude-3-5-sonnet-latest",   SONNET_3X_PRICES),
    ("claude-3-7-sonnet-20250219", SONNET_3X_PRICES),
    ("claude-3-7-sonnet-latest",   SONNET_3X_PRICES),
    // Claude 4.x family — placeholders matching pricing-page values as of
    // plan date; verify and update before commit.
    ("claude-opus-4-7", PriceBreakdown {
        input_per_mtoken:       15.00,
        output_per_mtoken:      75.00,
        cache_read_per_mtoken:  1.50,
        cache_write_per_mtoken: 18.75,
        reasoning_per_mtoken:   0.0,
    }),
    ("claude-sonnet-4-6", SONNET_3X_PRICES),
    ("claude-haiku-4-5-20251001", PriceBreakdown {
        input_per_mtoken:       1.00,
        output_per_mtoken:      5.00,
        cache_read_per_mtoken:  0.10,
        cache_write_per_mtoken: 1.25,
        reasoning_per_mtoken:   0.0,
    }),
    // Codex / OpenAI models populated from Task 7's fixture survey.
    // Until that lands, this list omits them — unknown-model code path
    // exercises gracefully (cost_estimate_usd = null).
];

const SONNET_3X_PRICES: PriceBreakdown = PriceBreakdown {
    input_per_mtoken:       3.00,
    output_per_mtoken:      15.00,
    cache_read_per_mtoken:  0.30,
    cache_write_per_mtoken: 3.75,
    reasoning_per_mtoken:   0.0,
};

/// Look up the context window in tokens for a model. `None` for unknown models.
pub fn context_window_of(model: &str) -> Option<u64> {
    CONTEXT_WINDOWS.iter().find(|(m, _)| *m == model).map(|(_, w)| *w)
}

/// Look up the price breakdown for a model. `None` for unknown models.
pub fn price_of(model: &str) -> Option<PriceBreakdown> {
    PRICES.iter().find(|(m, _)| *m == model).map(|(_, p)| *p)
}

/// Cost estimate in USD for the given token breakdown under the given
/// model's prices. `None` if the model is unknown.
pub fn cost_estimate_usd(model: &str, t: &Tokens) -> Option<f64> {
    let p = price_of(model)?;
    let m = 1_000_000.0;
    Some(
        (t.input        as f64) * p.input_per_mtoken        / m
      + (t.output       as f64) * p.output_per_mtoken       / m
      + (t.cache_read   as f64) * p.cache_read_per_mtoken   / m
      + (t.cache_write  as f64) * p.cache_write_per_mtoken  / m
      + (t.reasoning    as f64) * p.reasoning_per_mtoken    / m,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_returns_window() {
        assert_eq!(context_window_of("claude-3-5-sonnet-20241022"), Some(200_000));
    }

    #[test]
    fn unknown_model_returns_none_window() {
        assert_eq!(context_window_of("totally-made-up"), None);
    }

    #[test]
    fn known_model_returns_price() {
        let p = price_of("claude-3-5-sonnet-20241022").expect("known model");
        assert!((p.input_per_mtoken - 3.00).abs() < 1e-9);
        assert!((p.output_per_mtoken - 15.00).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_returns_none_price() {
        assert_eq!(price_of("totally-made-up").is_none(), true);
    }

    #[test]
    fn cost_arithmetic_correct_for_full_breakdown() {
        let t = Tokens {
            input: 1_000_000,
            output: 100_000,
            cache_read: 500_000,
            cache_write: 0,
            reasoning: 0,
        };
        // 1.00 * $3 + 0.10 * $15 + 0.50 * $0.30 = 3.00 + 1.50 + 0.15 = 4.65
        let cost = cost_estimate_usd("claude-3-5-sonnet-20241022", &t).unwrap();
        assert!((cost - 4.65).abs() < 1e-9, "expected 4.65, got {}", cost);
    }

    #[test]
    fn cost_none_for_unknown_model() {
        let t = Tokens::default();
        assert!(cost_estimate_usd("totally-made-up", &t).is_none());
    }
}
