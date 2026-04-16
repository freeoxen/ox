//! Model pricing for cost estimation.
//!
//! Prices are per million tokens. Returns `(input_cost, output_cost)` in USD.
//! Unknown models return `None` — the caller decides how to handle that.

/// Per-million-token pricing for a model.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Cost per million input tokens in USD.
    pub input_per_mtok: f64,
    /// Cost per million output tokens in USD.
    pub output_per_mtok: f64,
    /// Multiplier for cache creation tokens (e.g. 1.25 for Anthropic).
    pub cache_creation_multiplier: f64,
    /// Multiplier for cache read tokens (e.g. 0.1 for Anthropic).
    pub cache_read_multiplier: f64,
}

/// Return the pricing source URL for a model's provider.
pub fn pricing_url(model: &str) -> &'static str {
    if model.starts_with("claude") {
        "https://docs.anthropic.com/en/docs/about-claude/models"
    } else if model.starts_with("gpt") || model.starts_with("o1") || model.starts_with("o3") {
        "https://openai.com/api/pricing"
    } else {
        ""
    }
}

/// Look up pricing for a model by its ID.
///
/// Matches by prefix so versioned IDs (e.g. `claude-sonnet-4-20250514`)
/// resolve to the base model's pricing.
pub fn model_pricing(model: &str) -> Option<ModelPricing> {
    // Anthropic cache multipliers: creation = 1.25x, read = 0.1x
    const ANTHROPIC_CACHE: (f64, f64) = (1.25, 0.1);
    // OpenAI: no separate cache pricing
    const NO_CACHE: (f64, f64) = (1.0, 1.0);

    let (input, output, cache) = match () {
        // Anthropic — https://docs.anthropic.com/en/docs/about-claude/models (2025-05)
        _ if model.starts_with("claude-opus-4") => (15.0, 75.0, ANTHROPIC_CACHE),
        _ if model.starts_with("claude-sonnet-4") => (3.0, 15.0, ANTHROPIC_CACHE),
        _ if model.starts_with("claude-haiku-4") => (0.80, 4.0, ANTHROPIC_CACHE),
        _ if model.starts_with("claude-3-5-sonnet") || model.starts_with("claude-3.5-sonnet") => {
            (3.0, 15.0, ANTHROPIC_CACHE)
        }
        _ if model.starts_with("claude-3-5-haiku") || model.starts_with("claude-3.5-haiku") => {
            (0.80, 4.0, ANTHROPIC_CACHE)
        }
        _ if model.starts_with("claude-3-opus") => (15.0, 75.0, ANTHROPIC_CACHE),
        _ if model.starts_with("claude-3-sonnet") => (3.0, 15.0, ANTHROPIC_CACHE),
        _ if model.starts_with("claude-3-haiku") => (0.25, 1.25, ANTHROPIC_CACHE),

        // OpenAI — https://openai.com/api/pricing
        _ if model.starts_with("gpt-4o-mini") => (0.15, 0.60, NO_CACHE),
        _ if model.starts_with("gpt-4o") => (2.50, 10.0, NO_CACHE),
        _ if model.starts_with("gpt-4-turbo") => (10.0, 30.0, NO_CACHE),
        _ if model.starts_with("gpt-4") => (30.0, 60.0, NO_CACHE),
        _ if model.starts_with("o3-mini") => (1.10, 4.40, NO_CACHE),
        _ if model.starts_with("o3") => (10.0, 40.0, NO_CACHE),
        _ if model.starts_with("o1-mini") => (1.10, 4.40, NO_CACHE),
        _ if model.starts_with("o1") => (15.0, 60.0, NO_CACHE),

        _ => return None,
    };

    Some(ModelPricing {
        input_per_mtok: input,
        output_per_mtok: output,
        cache_creation_multiplier: cache.0,
        cache_read_multiplier: cache.1,
    })
}

/// Look up pricing with user overrides checked first.
///
/// `overrides` maps model prefix → `ModelPricing`. Checked before the
/// hardcoded table using the same prefix-match logic.
pub fn model_pricing_with_overrides(
    model: &str,
    overrides: &std::collections::BTreeMap<String, ModelPricing>,
) -> Option<ModelPricing> {
    // Check overrides first (longest prefix match)
    for (prefix, pricing) in overrides.iter().rev() {
        if model.starts_with(prefix.as_str()) {
            return Some(*pricing);
        }
    }
    model_pricing(model)
}

/// Estimate cost from input/output token counts only (ignores cache).
pub fn estimate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> Option<f64> {
    let pricing = model_pricing(model)?;
    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_mtok;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_mtok;
    Some(input_cost + output_cost)
}

/// Compute cost from a known `ModelPricing` and token counts.
pub fn compute_cost(
    p: &ModelPricing,
    input: u32,
    output: u32,
    cache_creation: u32,
    cache_read: u32,
) -> f64 {
    let base_input = input.saturating_sub(cache_creation + cache_read);
    let base_cost = (base_input as f64 / 1_000_000.0) * p.input_per_mtok;
    let creation_cost =
        (cache_creation as f64 / 1_000_000.0) * p.input_per_mtok * p.cache_creation_multiplier;
    let read_cost = (cache_read as f64 / 1_000_000.0) * p.input_per_mtok * p.cache_read_multiplier;
    let output_cost = (output as f64 / 1_000_000.0) * p.output_per_mtok;
    base_cost + creation_cost + read_cost + output_cost
}

/// Estimate cost with full cache-aware breakdown.
///
/// Cache creation tokens are billed at `input_rate * cache_creation_multiplier`.
/// Cache read tokens are billed at `input_rate * cache_read_multiplier`.
/// Base input tokens = `input_tokens - cache_creation - cache_read`.
pub fn estimate_cost_full(
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
) -> Option<f64> {
    let p = model_pricing(model)?;
    Some(compute_cost(
        &p,
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
    ))
}

/// Like [`estimate_cost_full`] but checks user overrides first.
pub fn estimate_cost_full_with_overrides(
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
    overrides: &std::collections::BTreeMap<String, ModelPricing>,
) -> Option<f64> {
    let p = model_pricing_with_overrides(model, overrides)?;
    Some(compute_cost(
        &p,
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet_pricing() {
        let p = model_pricing("claude-sonnet-4-20250514").unwrap();
        assert!((p.input_per_mtok - 3.0).abs() < f64::EPSILON);
        assert!((p.output_per_mtok - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn opus_pricing() {
        let p = model_pricing("claude-opus-4-20250514").unwrap();
        assert!((p.input_per_mtok - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gpt4o_pricing() {
        let p = model_pricing("gpt-4o").unwrap();
        assert!((p.input_per_mtok - 2.50).abs() < f64::EPSILON);
    }

    #[test]
    fn gpt4o_mini_matched_before_gpt4o() {
        let p = model_pricing("gpt-4o-mini").unwrap();
        assert!((p.input_per_mtok - 0.15).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(model_pricing("unknown-model").is_none());
    }

    #[test]
    fn estimate_cost_sonnet() {
        let cost = estimate_cost("claude-sonnet-4-20250514", 1000, 500).unwrap();
        // 1000 in * 3.0/1M + 500 out * 15.0/1M = 0.003 + 0.0075 = 0.0105
        assert!((cost - 0.0105).abs() < 1e-10);
    }

    #[test]
    fn estimate_cost_unknown() {
        assert!(estimate_cost("mystery-model", 1000, 500).is_none());
    }

    #[test]
    fn anthropic_has_cache_multipliers() {
        let p = model_pricing("claude-sonnet-4-20250514").unwrap();
        assert!((p.cache_creation_multiplier - 1.25).abs() < f64::EPSILON);
        assert!((p.cache_read_multiplier - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn openai_has_no_cache_discount() {
        let p = model_pricing("gpt-4o").unwrap();
        assert!((p.cache_creation_multiplier - 1.0).abs() < f64::EPSILON);
        assert!((p.cache_read_multiplier - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_cost_full_with_cache() {
        // 1000 total input, 600 cache_creation, 300 cache_read, 100 base
        // Sonnet: input $3/Mtok, output $15/Mtok
        // base: 100 * 3/1M = 0.0003
        // creation: 600 * 3/1M * 1.25 = 0.00225
        // read: 300 * 3/1M * 0.1 = 0.00009
        // output: 500 * 15/1M = 0.0075
        // total = 0.01014
        let cost = estimate_cost_full("claude-sonnet-4-20250514", 1000, 500, 600, 300).unwrap();
        assert!((cost - 0.01014).abs() < 1e-10);
    }

    #[test]
    fn estimate_cost_full_no_cache_equals_estimate_cost() {
        let full = estimate_cost_full("claude-sonnet-4-20250514", 1000, 500, 0, 0).unwrap();
        let simple = estimate_cost("claude-sonnet-4-20250514", 1000, 500).unwrap();
        assert!((full - simple).abs() < 1e-10);
    }
}
