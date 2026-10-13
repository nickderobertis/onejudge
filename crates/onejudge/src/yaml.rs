//! The one way this crate reads YAML: the CLI config and SKILL.md front matter,
//! each deserialized typed, once, through `serde-saphyr` (safe Rust, in place of
//! the libyaml translations `deny.toml` bans).
//!
//! The options are the fleet's, the same for every document read here: no source
//! snippet in an error, because a config's prompt text would otherwise be echoed
//! into stderr and the `FailureReport`; only `true`/`false` are booleans; and `<<`
//! is an ordinary key rather than a merge. Its errors carry the line and column of
//! the offending node, which is what a caller's message names.

use serde::de::DeserializeOwned;
use serde_saphyr::MergeKeyPolicy;

pub(crate) fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(
        text,
        serde_saphyr::options! {
            with_snippet: false,
            strict_booleans: true,
            merge_keys: MergeKeyPolicy::AsOrdinary,
        },
    )
}
