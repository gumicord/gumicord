//! Authentication token.
//!
//! A runtime self-check that scans output for the token is a last resort, not
//! a defence: it reports a leak after the fact. Wrapping the token in a type
//! whose `Debug` and `Display` are redacted means `{:?}` cannot leak it at
//! all, and reaching the value requires calling [`Token::expose`], which is
//! greppable.
//!
//! A token is the account. It is stored only in OS secure storage, never in a
//! plaintext file, and is never visible to plugins.

use core::fmt;

/// A Discord authentication token.
///
/// ```
/// # use gumicord_model::Token;
/// let t = Token::new("very-secret-token");
/// assert_eq!(format!("{t:?}"), "Token(<redacted>)");
/// assert_eq!(t.to_string(), "<redacted>");
/// assert_eq!(t.expose(), "very-secret-token");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(s: impl Into<String>) -> Self {
        Token(s.into())
    }

    /// Yields the secret. Named to be greppable; a bland name like `as_str`
    /// would slip through review.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Checks that output does not contain the token.
    ///
    /// The type guards formatting, but a string built from `expose()` passes
    /// straight through. Use this where response bodies are logged.
    ///
    /// Always true below 8 characters, where matches would be coincidental.
    pub fn is_absent_from(&self, haystack: &str) -> bool {
        self.0.len() < 8 || !haystack.contains(&self.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_prints_itself() {
        let t = Token::new("MTIzNDU2Nzg5.secret.abcdefg");

        for s in [format!("{t:?}"), format!("{t}"), format!("{:#?}", t)] {
            assert!(t.is_absent_from(&s), "token leaked into output: {s}");
        }
    }

    /// Formatting a whole struct is where leaks would happen.
    #[test]
    fn a_token_nested_in_a_struct_stays_hidden() {
        #[derive(Debug)]
        #[allow(dead_code, reason = "exists only to be formatted")]
        struct Session {
            token: Token,
            user: &'static str,
        }

        let s = Session {
            token: Token::new("MTIzNDU2Nzg5.secret.abcdefg"),
            user: "nennneko",
        };
        let printed = format!("{s:?}");

        assert!(s.token.is_absent_from(&printed));
        assert!(printed.contains("nennneko"), "other fields stay visible");
    }

    #[test]
    fn exposing_is_explicit() {
        let t = Token::new("秘密の値です");
        assert_eq!(t.expose(), "秘密の値です");
    }

    /// The self-check is an aid, not a guarantee.
    #[test]
    fn the_self_check_ignores_short_tokens() {
        let t = Token::new("ab");
        assert!(t.is_absent_from("ab appears here"));
    }
}
