//! Built-in fallback metadata for known model families.
//!
//! When a provider's models endpoint doesn't return `max_context_size` or
//! `max_output_tokens` (Anthropic's `/v1/models` is the canonical example
//! — it returns ids and display names only), the catalog-refresh path
//! consults this table to fill in operationally meaningful defaults so
//! the agent harness can decide on context compaction and output capping
//! without hardcoding numbers in the kernel.
//!
//! Resolution is **first-match wins** by linear scan. Within each
//! dialect, entries are hand-ordered longest-prefix first so a more
//! specific rule (e.g. `claude-haiku-4-5`) wins over a less specific one
//! (e.g. `claude-`). The structural test
//! `family_table_is_longest_prefix_first` guards that ordering.
//!
//! Sources for the values below: Anthropic and OpenAI public docs and
//! Meta Llama model cards as of 2025-10. Update both the values and this
//! reference comment when the next round of family entries lands.

use serde::{Deserialize, Serialize};

/// Fallback context/output limits for a model family.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownFamilyEntry {
    /// Maximum input context window in tokens, if known for the family.
    pub max_context_size: Option<u32>,
    /// Wire-required output cap (`max_tokens`), if known for the family.
    pub max_output_tokens: Option<u32>,
}

/// One row in the family lookup table. Private — callers go through
/// [`known_family_metadata`].
struct FamilyRule {
    dialect: &'static str,
    prefix: &'static str,
    entry: KnownFamilyEntry,
}

/// Fallback metadata for known model families.
///
/// Hand-ordered longest-prefix-first within each dialect: the lookup
/// short-circuits on first match, so ordering *is* the disambiguation.
/// The `family_table_is_longest_prefix_first` test guards this invariant.
const FAMILY_TABLE: &[FamilyRule] = &[
    // -- Anthropic --------------------------------------------------------
    // Hand-ordered longest-prefix first within the dialect.
    // Claude 3.7 Sonnet — 200K context, 8K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-3-7-sonnet",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
        },
    },
    // Claude 3.5 Sonnet — 200K context, 8K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-3-5-sonnet",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
        },
    },
    // Claude 4.5 (Haiku) — 200K context, 8K output (per Anthropic 2025-10).
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-haiku-4-5",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
        },
    },
    // Claude 3.5 Haiku — 200K context, 8K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-3-5-haiku",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
        },
    },
    // Claude Sonnet 4.x — 200K context, 64K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-sonnet-4",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(64_000),
        },
    },
    // Claude 3 Sonnet (legacy 3.0) — 200K context, 4K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-3-sonnet",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Claude 3 Haiku (legacy 3.0) — 200K context, 4K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-3-haiku",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Claude Opus 4.x — 200K context, 32K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-opus-4",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(32_000),
        },
    },
    // Claude 3 Opus (legacy 3.0) — 200K context, 4K output.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-3-opus",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Generic Claude fallback — last in the dialect block, longest-prefix
    // wins, so this only fires for ids we haven't pinned above.
    FamilyRule {
        dialect: "anthropic",
        prefix: "claude-",
        entry: KnownFamilyEntry {
            max_context_size: Some(200_000),
            max_output_tokens: Some(4_096),
        },
    },
    // -- OpenAI -----------------------------------------------------------
    // Hand-ordered longest-prefix first within the dialect.
    // GPT-4o mini — 128K context, 16K output.
    FamilyRule {
        dialect: "openai",
        prefix: "gpt-4o-mini",
        entry: KnownFamilyEntry {
            max_context_size: Some(128_000),
            max_output_tokens: Some(16_384),
        },
    },
    // GPT-4 Turbo — 128K context, 4K output.
    FamilyRule {
        dialect: "openai",
        prefix: "gpt-4-turbo",
        entry: KnownFamilyEntry {
            max_context_size: Some(128_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Llama 3.3 — 128K context, 4K output (typical).
    FamilyRule {
        dialect: "openai",
        prefix: "llama-3.3",
        entry: KnownFamilyEntry {
            max_context_size: Some(128_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Llama 3.2 — 128K context, 4K output.
    FamilyRule {
        dialect: "openai",
        prefix: "llama-3.2",
        entry: KnownFamilyEntry {
            max_context_size: Some(128_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Llama 3.1 — 128K context, 4K output.
    FamilyRule {
        dialect: "openai",
        prefix: "llama-3.1",
        entry: KnownFamilyEntry {
            max_context_size: Some(128_000),
            max_output_tokens: Some(4_096),
        },
    },
    // Llama 3 — 8K context, 4K output (the original 3.0 release).
    FamilyRule {
        dialect: "openai",
        prefix: "llama-3",
        entry: KnownFamilyEntry {
            max_context_size: Some(8_192),
            max_output_tokens: Some(4_096),
        },
    },
    // GPT-4o — 128K context, 16K output. Comes after the longer
    // `gpt-4o-mini` and `gpt-4-turbo` rows so longest-prefix wins.
    FamilyRule {
        dialect: "openai",
        prefix: "gpt-4o",
        entry: KnownFamilyEntry {
            max_context_size: Some(128_000),
            max_output_tokens: Some(16_384),
        },
    },
];

/// Look up fallback metadata for a model id under a given dialect.
///
/// Returns the first rule in [`FAMILY_TABLE`] whose dialect matches and
/// whose prefix is a prefix of `model_id`. The table is hand-ordered
/// longest-prefix-first within each dialect, so the linear scan
/// short-circuits to the most specific match.
///
/// Returns `None` for unknown model ids or unknown dialects.
pub fn known_family_metadata(model_id: &str, dialect: &str) -> Option<KnownFamilyEntry> {
    for rule in FAMILY_TABLE {
        if rule.dialect == dialect && model_id.starts_with(rule.prefix) {
            return Some(rule.entry.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_sonnet_4_anthropic_returns_200k_and_at_least_32k_output() {
        let entry = known_family_metadata("claude-sonnet-4-20250514", "anthropic")
            .expect("known family for claude-sonnet-4-*");
        assert_eq!(entry.max_context_size, Some(200_000));
        let out = entry
            .max_output_tokens
            .expect("max_output_tokens populated for claude-sonnet-4-*");
        assert!(
            out >= 32_000,
            "claude-sonnet-4-* output cap should be at least 32K; got {out}"
        );
    }

    #[test]
    fn claude_haiku_4_5_anthropic_returns_200k_and_8192_output() {
        let entry = known_family_metadata("claude-haiku-4-5-20251001", "anthropic")
            .expect("known family for claude-haiku-4-5-*");
        assert_eq!(entry.max_context_size, Some(200_000));
        assert_eq!(entry.max_output_tokens, Some(8_192));
    }

    #[test]
    fn gpt_4o_openai_returns_128k_context() {
        let entry = known_family_metadata("gpt-4o-2024-11-20", "openai")
            .expect("known family for gpt-4o*");
        assert_eq!(entry.max_context_size, Some(128_000));
        assert!(
            entry.max_output_tokens.is_some(),
            "gpt-4o output cap should be set"
        );
    }

    #[test]
    fn unknown_model_id_returns_none() {
        assert!(known_family_metadata("totally-made-up-model", "anthropic").is_none());
        assert!(known_family_metadata("something-else", "openai").is_none());
    }

    #[test]
    fn dialect_disambiguates_overlapping_prefixes() {
        // A bare `claude-` id under the OpenAI dialect must not pick up
        // the Anthropic generic-claude row. Likewise a `gpt-4o` id under
        // the Anthropic dialect doesn't see the OpenAI rows.
        assert!(
            known_family_metadata("claude-3-opus-20240229", "openai").is_none(),
            "Anthropic claude-* row must not leak into OpenAI dialect"
        );
        assert!(
            known_family_metadata("gpt-4o-2024-08-06", "anthropic").is_none(),
            "OpenAI gpt-4o row must not leak into Anthropic dialect"
        );
    }

    #[test]
    fn longest_prefix_wins_for_overlapping_anthropic_rules() {
        // Both `claude-` (generic, 4096 output) and `claude-haiku-4-5`
        // (specific, 8192 output) are in the table. An id that matches
        // both must resolve to the haiku-specific rule.
        let entry = known_family_metadata("claude-haiku-4-5-20251001", "anthropic")
            .expect("haiku family resolves");
        assert_eq!(
            entry.max_output_tokens,
            Some(8_192),
            "longest-prefix-wins: claude-haiku-4-5 should beat claude-"
        );
    }

    #[test]
    fn family_table_is_longest_prefix_first() {
        // Within each dialect, FAMILY_TABLE entries must be sorted by
        // prefix length descending. Since lookup is first-match-wins, a
        // shorter prefix above a longer one would silently swallow more
        // specific rows. This guard runs the structural check so future
        // contributors get a test failure instead of a subtle regression.
        use std::collections::HashMap;
        let mut last_len_by_dialect: HashMap<&'static str, usize> = HashMap::new();
        for rule in FAMILY_TABLE {
            let prev = last_len_by_dialect
                .get(rule.dialect)
                .copied()
                .unwrap_or(usize::MAX);
            assert!(
                rule.prefix.len() <= prev,
                "FAMILY_TABLE not longest-prefix-first within dialect {:?}: \
                 prefix {:?} (len {}) appears after a shorter prefix (len {})",
                rule.dialect,
                rule.prefix,
                rule.prefix.len(),
                prev
            );
            last_len_by_dialect.insert(rule.dialect, rule.prefix.len());
        }
    }
}
