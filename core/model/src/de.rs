//! Deserialisation helpers that keep going when Discord sends something
//! unexpected.
//!
//! Discord adds fields without notice and mixes in empty shells. Failing a
//! whole list because one element is unreadable blanks the screen behind it:
//! a single unavailable-guild shell in READY once made the entire payload
//! unreadable and the Gateway reconnected forever.
//!
//! Nothing is dropped silently — the count is always logged.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

/// Reads a list, skipping elements that fail to deserialise.
///
/// ```ignore
/// #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
/// pub guilds: Vec<Guild>,
/// ```
///
/// A non-list where a list belongs is still an error; only elements are
/// skipped.
pub fn lenient_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(raw.len());
    let mut dropped = 0usize;

    for value in raw {
        match serde_json::from_value(value) {
            Ok(item) => out.push(item),
            Err(e) => {
                dropped += 1;
                tracing::debug!(error = %e, "could not read a list element");
            }
        }
    }

    if dropped > 0 {
        tracing::warn!(
            dropped,
            kept = out.len(),
            type_name = std::any::type_name::<T>(),
            "skipped unreadable list elements"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Guild;

    #[derive(Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "lenient_vec")]
        guilds: Vec<Guild>,
    }

    #[test]
    fn an_unreadable_element_does_not_take_the_list_with_it() {
        let h: Holder = serde_json::from_str(
            r#"{"guilds":[
                {"id":"1","name":"ふつう"},
                {"no id at all":true},
                {"id":"3","name":"これも読める"}
            ]}"#,
        )
        .expect("the whole list failed");

        let names: Vec<_> = h.guilds.iter().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["ふつう", "これも読める"]);
    }

    #[test]
    fn a_non_list_is_still_an_error() {
        assert!(serde_json::from_str::<Holder>(r#"{"guilds":42}"#).is_err());
    }

    /// The shell that caused the original reconnect loop.
    #[test]
    fn an_unavailable_guild_is_a_shell_but_still_readable() {
        let h: Holder =
            serde_json::from_str(r#"{"guilds":[{"id":"1","unavailable":true}]}"#).unwrap();

        assert_eq!(h.guilds.len(), 1, "the shell was dropped");
        assert!(h.guilds[0].unavailable);
        assert!(h.guilds[0].name.is_empty());
    }
}
