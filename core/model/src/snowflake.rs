//! Discord identifiers.
//!
//! Distinct types per kind so that passing a channel id where a guild id
//! belongs fails to compile.
//!
//! Discord sends 64-bit ids as JSON *strings*, because JavaScript numbers are
//! only exact to 53 bits. The conversion is confined here.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A Discord 64-bit identifier.
///
/// The high 42 bits hold milliseconds since the Discord epoch, so id order is
/// creation order and can be used for sorting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Snowflake(pub u64);

/// 2015-01-01T00:00:00Z in UNIX milliseconds.
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

impl Snowflake {
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Creation time in UNIX milliseconds, carried by the id itself.
    pub const fn created_at_ms(self) -> u64 {
        (self.0 >> 22) + DISCORD_EPOCH_MS
    }
}

impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for Snowflake {
    fn from(v: u64) -> Self {
        Snowflake(v)
    }
}

impl FromStr for Snowflake {
    type Err = core::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>().map(Snowflake)
    }
}

impl Serialize for Snowflake {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // As a string: a number loses precision on the JavaScript side.
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, Unexpected};

        // Strings are the norm, but accept numbers too rather than failing on
        // the sender's choice of encoding.
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => s
                .parse()
                .map(Snowflake)
                .map_err(|_| D::Error::invalid_value(Unexpected::Str(&s), &"a 64-bit integer")),
            serde_json::Value::Number(n) => n.as_u64().map(Snowflake).ok_or_else(|| {
                D::Error::invalid_type(Unexpected::Other("number"), &"a 64-bit integer")
            }),
            other => Err(D::Error::invalid_type(
                Unexpected::Other(match other {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "boolean",
                    serde_json::Value::Array(_) => "array",
                    _ => "object",
                }),
                &"a string or a number",
            )),
        }
    }
}

/// Kind-tagged ids, so mixing them up fails to compile.
///
/// ```
/// # use gumicord_model::{ChannelId, GuildId};
/// fn open(_: ChannelId) {}
/// let g = GuildId::from(1u64);
/// // open(g);  // does not compile
/// ```
macro_rules! define_ids {
    ($($(#[$doc:meta])* $name:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Snowflake);

        impl $name {
            pub const fn get(self) -> u64 {
                self.0.get()
            }

            pub const fn created_at_ms(self) -> u64 {
                self.0.created_at_ms()
            }
        }

        impl From<u64> for $name {
            fn from(v: u64) -> Self {
                $name(Snowflake(v))
            }
        }

        impl From<Snowflake> for $name {
            fn from(v: Snowflake) -> Self {
                $name(v)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )*};
}

define_ids! {
    /// A guild (server).
    GuildId;
    /// A channel, including DMs.
    ChannelId;
    MessageId;
    UserId;
    AttachmentId;
    RoleId;
    EmojiId;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snowflakes_round_trip_as_strings() {
        let id = Snowflake(1_234_567_890_123_456_789);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"1234567890123456789\"");
        assert_eq!(serde_json::from_str::<Snowflake>(&json).unwrap(), id);
    }

    #[test]
    fn a_numeric_snowflake_is_accepted_too() {
        assert_eq!(
            serde_json::from_str::<Snowflake>("123").unwrap(),
            Snowflake(123)
        );
    }

    #[test]
    fn a_bad_snowflake_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Snowflake>("\"あ\"").is_err());
        assert!(serde_json::from_str::<Snowflake>("null").is_err());
        assert!(serde_json::from_str::<Snowflake>("[]").is_err());
    }

    #[test]
    fn a_snowflake_carries_its_own_timestamp() {
        assert_eq!(Snowflake(0).created_at_ms(), DISCORD_EPOCH_MS);
        // The low 22 bits are a sequence counter.
        assert_eq!(Snowflake(1 << 22).created_at_ms(), DISCORD_EPOCH_MS + 1);
    }

    #[test]
    fn snowflakes_sort_by_creation_time() {
        let older = Snowflake(1 << 22);
        let newer = Snowflake(9 << 22);
        assert!(older < newer);
        assert!(older.created_at_ms() < newer.created_at_ms());
    }

    #[test]
    fn typed_ids_serialise_like_snowflakes() {
        let c = ChannelId::from(42u64);
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"42\"");
        assert_eq!(serde_json::from_str::<ChannelId>("\"42\"").unwrap(), c);
        assert_eq!(c.to_string(), "42");
    }
}
