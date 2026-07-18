//! Token / cost usage aggregated across the calls in one run.

use serde::{Deserialize, Serialize};

/// Token / cost usage for one provider call, or a running total.
///
/// Each field is independently optional because not every harness reports every
/// signal (cost is commonly absent on subscription auth, and cache counts only
/// when the harness surfaces prompt-cache reads/writes). `None` means "no
/// signal", never "zero" — so a total stays `None` until something reports a real
/// number, then accumulates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk-schema", derive(schemars::JsonSchema))]
pub struct Usage {
    /// Prompt/input tokens billed, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Completion/output tokens billed, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Prompt tokens served from the provider's prompt cache — a cheap read of a
    /// previously-written prefix — when the harness reports them. `None` when the
    /// harness does not surface cache counts, never `0` as a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Prompt tokens written to the provider's prompt cache (a.k.a. cache
    /// creation), when the harness reports them. `None` when not surfaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Total cost in USD, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// True iff every field is `None` (no signal at all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_write_tokens.is_none()
            && self.cost_usd.is_none()
    }

    /// Fold another sample into this total. A `None` field stays `None` until
    /// something reports a real number, at which point values accumulate.
    pub fn add(&mut self, other: &Usage) {
        if let Some(v) = other.input_tokens {
            self.input_tokens = Some(self.input_tokens.unwrap_or(0) + v);
        }
        if let Some(v) = other.output_tokens {
            self.output_tokens = Some(self.output_tokens.unwrap_or(0) + v);
        }
        if let Some(v) = other.cache_read_tokens {
            self.cache_read_tokens = Some(self.cache_read_tokens.unwrap_or(0) + v);
        }
        if let Some(v) = other.cache_write_tokens {
            self.cache_write_tokens = Some(self.cache_write_tokens.unwrap_or(0) + v);
        }
        if let Some(v) = other.cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_none_everywhere() {
        assert!(Usage::default().is_empty());
        assert!(!Usage {
            input_tokens: Some(1),
            ..Usage::default()
        }
        .is_empty());
        // Cache-only usage is still a real reading, never "empty".
        assert!(!Usage {
            cache_read_tokens: Some(42),
            ..Usage::default()
        }
        .is_empty());
    }

    #[test]
    fn add_accumulates_only_present_fields() {
        let mut total = Usage::default();
        total.add(&Usage {
            input_tokens: Some(10),
            cache_read_tokens: Some(7),
            cost_usd: Some(0.5),
            ..Usage::default()
        });
        assert_eq!(total.input_tokens, Some(10));
        assert_eq!(total.output_tokens, None);
        assert_eq!(total.cache_read_tokens, Some(7));
        assert_eq!(total.cache_write_tokens, None);
        assert_eq!(total.cost_usd, Some(0.5));

        total.add(&Usage {
            input_tokens: Some(5),
            output_tokens: Some(3),
            cache_read_tokens: Some(2),
            cache_write_tokens: Some(4),
            cost_usd: None,
        });
        assert_eq!(total.input_tokens, Some(15));
        assert_eq!(total.output_tokens, Some(3));
        assert_eq!(total.cache_read_tokens, Some(9));
        assert_eq!(total.cache_write_tokens, Some(4));
        assert_eq!(total.cost_usd, Some(0.5));
    }

    #[test]
    fn usage_round_trips_skipping_none() {
        let json = serde_json::to_string(&Usage {
            input_tokens: Some(7),
            cache_read_tokens: Some(3),
            ..Usage::default()
        })
        .unwrap();
        assert_eq!(json, r#"{"input_tokens":7,"cache_read_tokens":3}"#);
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input_tokens, Some(7));
        assert_eq!(back.cache_read_tokens, Some(3));
        assert_eq!(back.cache_write_tokens, None);
    }
}
