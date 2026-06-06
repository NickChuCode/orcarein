//! Cost estimation + the cache-savings meter.
//!
//! DeepSeek's automatic byte-prefix cache makes cache-hit prompt tokens ~50×
//! cheaper than misses (V4-Flash: $0.0028/M hit vs $0.14/M miss). This module
//! turns a [`TokenUsage`] into a human-readable "what did this cost, and how
//! much did the cache save" line — the credibility signal reasonix made its
//! headline (see vision §2.1).
//!
//! Prices are a small hard-coded table (USD per 1M tokens) keyed loosely by
//! model name. Unknown models (e.g. arbitrary OpenAI-compatible endpoints)
//! return `None` rather than guessing — the meter then simply isn't shown.

use crate::TokenUsage;

/// Per-1M-token prices in USD for one model tier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prices {
    /// Price per 1M cache-hit prompt tokens.
    pub cache_hit: f64,
    /// Price per 1M cache-miss prompt tokens.
    pub cache_miss: f64,
    /// Price per 1M output (completion) tokens.
    pub output: f64,
}

// DeepSeek V4 list prices (post 2026-04-26 cut), USD per 1M tokens.
const FLASH: Prices = Prices {
    cache_hit: 0.0028,
    cache_miss: 0.14,
    output: 0.28,
};
const PRO: Prices = Prices {
    cache_hit: 0.0145,
    cache_miss: 1.74,
    output: 3.48,
};

/// Looks up prices by model name. DeepSeek V4 Flash/Pro are known; anything
/// else (OpenAI, custom endpoints) returns `None`.
pub fn prices_for(model: &str) -> Option<Prices> {
    let m = model.to_lowercase();
    if !m.contains("deepseek") {
        return None;
    }
    if m.contains("pro") {
        Some(PRO)
    } else {
        Some(FLASH)
    }
}

/// A cost breakdown for one [`TokenUsage`] at a model's prices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    /// Total USD actually spent (cache-hit + cache-miss + output).
    pub spent_usd: f64,
    /// USD the cache saved versus paying the miss price for every hit token.
    pub saved_usd: f64,
    /// Fraction of prompt tokens served from cache, in `0.0..=1.0`.
    pub hit_rate: f64,
}

/// Estimates cost for `usage` at `model`'s prices, or `None` if the model's
/// prices are unknown.
///
/// When the provider reports cache hit/miss splits (DeepSeek), they drive the
/// estimate. When it doesn't (hit+miss == 0, e.g. OpenAI), all prompt tokens
/// are treated as misses and `saved_usd`/`hit_rate` are 0.
pub fn estimate(usage: &TokenUsage, model: &str) -> Option<CostEstimate> {
    let p = prices_for(model)?;

    let hit = usage.prompt_cache_hit_tokens;
    let split = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens;
    let (hit, miss, prompt) = if split > 0 {
        (hit, usage.prompt_cache_miss_tokens, split)
    } else {
        // No cache split reported: treat the whole prompt as a miss.
        (0, usage.prompt_tokens, usage.prompt_tokens)
    };

    let per_m = |tokens: u64, price: f64| (tokens as f64) * price / 1_000_000.0;
    let spent_usd = per_m(hit, p.cache_hit)
        + per_m(miss, p.cache_miss)
        + per_m(usage.completion_tokens, p.output);
    let saved_usd = per_m(hit, p.cache_miss - p.cache_hit);
    let hit_rate = if prompt > 0 {
        hit as f64 / prompt as f64
    } else {
        0.0
    };

    Some(CostEstimate {
        spent_usd,
        saved_usd,
        hit_rate,
    })
}

/// A one-line human meter, or `None` if the model's prices are unknown.
///
/// Example: `cache 85% hit | spent $0.0123 | saved $0.0410`.
pub fn meter_line(usage: &TokenUsage, model: &str) -> Option<String> {
    let c = estimate(usage, model)?;
    Some(format!(
        "cache {:.0}% hit | spent ${:.4} | saved ${:.4}",
        c.hit_rate * 100.0,
        c.spent_usd,
        c.saved_usd
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_known_for_deepseek_flash_and_pro() {
        assert_eq!(prices_for("deepseek-v4-flash"), Some(FLASH));
        assert_eq!(prices_for("deepseek-v4-pro"), Some(PRO));
        assert_eq!(prices_for("DeepSeek-V4-Pro"), Some(PRO)); // case-insensitive
    }

    #[test]
    fn prices_unknown_for_non_deepseek() {
        assert_eq!(prices_for("gpt-4o-mini"), None);
        assert_eq!(prices_for("mock-model"), None);
    }

    #[test]
    fn estimate_uses_cache_split_when_present() {
        // 900k hit + 100k miss prompt, 50k output, on Flash.
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 50_000,
            total_tokens: 1_050_000,
            prompt_cache_hit_tokens: 900_000,
            prompt_cache_miss_tokens: 100_000,
        };
        let c = estimate(&usage, "deepseek-v4-flash").unwrap();
        assert!((c.hit_rate - 0.9).abs() < 1e-9);
        // spent = 0.9*0.0028 + 0.1*0.14 + 0.05*0.28
        assert!((c.spent_usd - (0.00252 + 0.014 + 0.014)).abs() < 1e-9);
        // saved = 0.9 * (0.14 - 0.0028)
        assert!((c.saved_usd - (0.9 * (0.14 - 0.0028))).abs() < 1e-9);
    }

    #[test]
    fn estimate_treats_no_split_as_all_miss() {
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 0,
            total_tokens: 1_000_000,
            ..Default::default()
        };
        let c = estimate(&usage, "deepseek-v4-flash").unwrap();
        assert_eq!(c.hit_rate, 0.0);
        assert_eq!(c.saved_usd, 0.0);
        assert!((c.spent_usd - 0.14).abs() < 1e-9); // 1M @ miss price
    }

    #[test]
    fn meter_line_none_for_unknown_model() {
        let usage = TokenUsage::default();
        assert!(meter_line(&usage, "gpt-4o-mini").is_none());
        assert!(meter_line(&usage, "deepseek-v4-flash").is_some());
    }
}
