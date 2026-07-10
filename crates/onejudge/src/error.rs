//! The error type for the engine. A [`Provider`](crate::Provider) failure is
//! classified with a [`ProviderErrorKind`] so a consumer can branch on the
//! category (a broken environment vs. a broken skill) without matching substrings
//! in the human message.

use serde::{Deserialize, Serialize};

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Machine-readable classification of a provider failure.
///
/// The category a consumer branches on instead of matching substrings in the
/// human message (the reason this exists: a timeout is
/// [`ProviderErrorKind::Timeout`], not `message.contains("timed out")`). It maps
/// directly onto `oneharness`'s normalized `failure_kind`. The set is closed on
/// purpose; a label this version does not recognize becomes
/// [`ProviderErrorKind::Other`] rather than a new, unhandled string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// Authentication/authorization failed (missing or rejected credentials).
    Auth,
    /// The provider rate-limited the call; a backoff-and-retry may succeed.
    RateLimit,
    /// The harness/vendor does not recognize the requested model.
    ModelNotFound,
    /// The account's quota or billing limit is exhausted.
    Quota,
    /// A transient server-side overload.
    Overloaded,
    /// The call exceeded its deadline.
    Timeout,
    /// The provider process could not be started (binary missing, not runnable).
    Spawn,
    /// The provider ran but produced output that violated the protocol
    /// (unparseable envelope, missing results, malformed verdict).
    Protocol,
    /// A classified failure that does not map to a more specific category.
    Other,
}

impl ProviderErrorKind {
    /// Map a raw failure label (an `oneharness` `failure_kind`, or a category
    /// name already normalized to snake_case) to a kind. Unrecognized labels
    /// become [`ProviderErrorKind::Other`].
    #[must_use]
    pub fn classify(raw: &str) -> Self {
        match raw {
            "auth" => Self::Auth,
            "rate_limit" => Self::RateLimit,
            "model_not_found" => Self::ModelNotFound,
            "quota" => Self::Quota,
            "overloaded" => Self::Overloaded,
            "timeout" => Self::Timeout,
            "spawn" => Self::Spawn,
            "protocol" => Self::Protocol,
            _ => Self::Other,
        }
    }

    /// The stable snake_case wire string for this kind (matches serde).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::RateLimit => "rate_limit",
            Self::ModelNotFound => "model_not_found",
            Self::Quota => "quota",
            Self::Overloaded => "overloaded",
            Self::Timeout => "timeout",
            Self::Spawn => "spawn",
            Self::Protocol => "protocol",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything that can go wrong driving a conversation or judging a transcript.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A caller passed a semantically invalid argument (e.g. an empty provider
    /// command, or a numeric scale with `min > max`).
    #[error("invalid input: {0}")]
    Invalid(String),

    /// The provider command could not be spawned or did not behave. `kind`, when
    /// set, classifies the failure so consumers can distinguish a broken
    /// environment from a broken skill without parsing `message`.
    #[error("provider error ({context}): {message}")]
    Provider {
        /// Which provider operation failed (e.g. `respond`, `judge`).
        context: String,
        /// The human-readable specifics.
        message: String,
        /// The classified category, when known.
        kind: Option<ProviderErrorKind>,
    },
}

impl Error {
    /// Construct an unclassified [`Error::Provider`].
    pub fn provider(context: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Error::Provider {
            context: context.into(),
            message: message.to_string(),
            kind: None,
        }
    }

    /// Construct a classified [`Error::Provider`].
    pub fn provider_classified(
        context: impl Into<String>,
        message: impl std::fmt::Display,
        kind: ProviderErrorKind,
    ) -> Self {
        Error::Provider {
            context: context.into(),
            message: message.to_string(),
            kind: Some(kind),
        }
    }

    /// The classified kind of a provider error, if any.
    #[must_use]
    pub fn kind(&self) -> Option<ProviderErrorKind> {
        match self {
            Error::Provider { kind, .. } => *kind,
            Error::Invalid(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_string_forms_agree() {
        let all = [
            ProviderErrorKind::Auth,
            ProviderErrorKind::RateLimit,
            ProviderErrorKind::ModelNotFound,
            ProviderErrorKind::Quota,
            ProviderErrorKind::Overloaded,
            ProviderErrorKind::Timeout,
            ProviderErrorKind::Spawn,
            ProviderErrorKind::Protocol,
            ProviderErrorKind::Other,
        ];
        for kind in all {
            let wire = kind.as_str();
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(wire));
            assert_eq!(kind.to_string(), wire);
            assert_eq!(ProviderErrorKind::classify(wire), kind);
        }
    }

    #[test]
    fn classify_maps_unknown_labels_to_other() {
        assert_eq!(
            ProviderErrorKind::classify("brand_new_upstream_kind"),
            ProviderErrorKind::Other
        );
    }

    #[test]
    fn provider_helpers_set_context_and_kind() {
        let plain = Error::provider("respond", "boom");
        assert_eq!(plain.kind(), None);
        assert!(plain.to_string().contains("respond"));

        let classified = Error::provider_classified("judge", "denied", ProviderErrorKind::Auth);
        assert_eq!(classified.kind(), Some(ProviderErrorKind::Auth));

        assert_eq!(Error::Invalid("x".into()).kind(), None);
    }
}
