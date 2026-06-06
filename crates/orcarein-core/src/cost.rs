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
///
/// The split matters: the cache only ever discounts **input** (prompt) tokens,
/// never **output** (completion) tokens. So `input_saved_pct` — not
/// `spent_usd` — is the honest signal of "is the cache working", because
/// output length (which the cache can't touch) otherwise swamps the total.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    /// USD spent on input (prompt) tokens — cache-hit + cache-miss.
    pub input_usd: f64,
    /// USD spent on output (completion) tokens — never cached.
    pub output_usd: f64,
    /// Total USD spent (`input_usd + output_usd`).
    pub spent_usd: f64,
    /// USD the cache saved on input versus paying the miss price for every
    /// prompt token.
    pub saved_usd: f64,
    /// Fraction of the *uncached* input bill the cache saved, in `0.0..=1.0`.
    /// This is output-length-independent — the clean "is the cache working"
    /// signal.
    pub input_saved_pct: f64,
    /// Fraction of prompt tokens served from cache, in `0.0..=1.0`.
    pub hit_rate: f64,
}

/// Estimates cost for `usage` at `model`'s prices, or `None` if the model's
/// prices are unknown.
///
/// When the provider reports cache hit/miss splits (DeepSeek), they drive the
/// estimate. When it doesn't (hit+miss == 0, e.g. OpenAI), all prompt tokens
/// are treated as misses and the cache-savings fields are 0.
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
    let input_usd = per_m(hit, p.cache_hit) + per_m(miss, p.cache_miss);
    let output_usd = per_m(usage.completion_tokens, p.output);
    let spent_usd = input_usd + output_usd;
    let saved_usd = per_m(hit, p.cache_miss - p.cache_hit);

    // What the input WOULD have cost with no cache = actual input + what we saved.
    let input_if_all_miss = input_usd + saved_usd;
    let input_saved_pct = if input_if_all_miss > 0.0 {
        saved_usd / input_if_all_miss
    } else {
        0.0
    };
    let hit_rate = if prompt > 0 {
        hit as f64 / prompt as f64
    } else {
        0.0
    };

    Some(CostEstimate {
        input_usd,
        output_usd,
        spent_usd,
        saved_usd,
        input_saved_pct,
        hit_rate,
    })
}

/// A one-line human meter, or `None` if the model's prices are unknown.
///
/// Splits input vs output so the cache effect stays legible regardless of how
/// long the model's answer was. Example:
/// `cache 68% hit | in $0.0002 (saved 93%) | out $0.0006 | total $0.0008`.
pub fn meter_line(usage: &TokenUsage, model: &str) -> Option<String> {
    let c = estimate(usage, model)?;
    Some(format!(
        "cache {:.0}% hit | in ${:.4} (saved {:.0}%) | out ${:.4} | total ${:.4}",
        c.hit_rate * 100.0,
        c.input_usd,
        c.input_saved_pct * 100.0,
        c.output_usd,
        c.spent_usd
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
        // input = 0.9*0.0028 + 0.1*0.14 ; output = 0.05*0.28
        assert!((c.input_usd - (0.00252 + 0.014)).abs() < 1e-9);
        assert!((c.output_usd - 0.014).abs() < 1e-9);
        assert!((c.spent_usd - (0.00252 + 0.014 + 0.014)).abs() < 1e-9);
        // saved = 0.9 * (0.14 - 0.0028) ; the full prompt would cost 1M*0.14 = 0.14.
        assert!((c.saved_usd - (0.9 * (0.14 - 0.0028))).abs() < 1e-9);
        assert!((c.input_saved_pct - (0.9 * (0.14 - 0.0028)) / 0.14).abs() < 1e-9);
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
        assert_eq!(c.input_saved_pct, 0.0);
        assert_eq!(c.output_usd, 0.0);
        assert!((c.input_usd - 0.14).abs() < 1e-9); // 1M @ miss price
        assert!((c.spent_usd - 0.14).abs() < 1e-9);
    }

    #[test]
    fn meter_line_none_for_unknown_model() {
        let usage = TokenUsage::default();
        assert!(meter_line(&usage, "gpt-4o-mini").is_none());
        assert!(meter_line(&usage, "deepseek-v4-flash").is_some());
    }

    #[test]
    fn meter_line_splits_input_and_output() {
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 50_000,
            total_tokens: 1_050_000,
            prompt_cache_hit_tokens: 900_000,
            prompt_cache_miss_tokens: 100_000,
        };
        let line = meter_line(&usage, "deepseek-v4-flash").unwrap();
        assert!(line.contains("in $"), "{line}");
        assert!(line.contains("out $"), "{line}");
        assert!(line.contains("saved 88%"), "{line}"); // 0.882 -> 88%
    }
}
