//! Discord の識別子。
//!
//! # なぜ専用の型なのか
//!
//! すべての ID が `u64` だと、ギルド ID を取るところへチャンネル ID を渡しても
//! 通ってしまう。**型を分けると、その取り違えがコンパイルで止まる。**
//!
//! # JSON では文字列である
//!
//! Discord は 64 ビットの ID を**文字列**で送ってくる。JavaScript の数値が
//! 53 ビットしか正確に表せないためである。`serde` の既定では `u64` として
//! 読めないので、変換をここに閉じ込める。
//!
//! 仕様: [`spec/09-discord-protocol.md`]

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Discord の 64 ビット識別子。
///
/// 上位 42 ビットに Discord 紀元 (2015-01-01) からのミリ秒が入っている。
/// **したがって ID の順序は生成の順序である。** メッセージの並べ替えに使える。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Snowflake(pub u64);

/// Discord 紀元 (2015-01-01T00:00:00Z) の UNIX ミリ秒
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

impl Snowflake {
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 生成された時刻 (UNIX ミリ秒)。
    ///
    /// **別途タイムスタンプを持たなくてよい。** ID そのものに入っている。
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
        // ⚠️ **文字列で書く。** 数値で書くと、受け取る側 (JS) で桁が落ちる
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, Unexpected};

        // 文字列で来るのが常だが、数値で来ても受ける。
        // **相手の都合で落ちるより、広く受けるほうがよい**
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => s
                .parse()
                .map(Snowflake)
                .map_err(|_| D::Error::invalid_value(Unexpected::Str(&s), &"64 ビットの整数")),
            serde_json::Value::Number(n) => n.as_u64().map(Snowflake).ok_or_else(|| {
                D::Error::invalid_type(Unexpected::Other("数値"), &"64 ビットの整数")
            }),
            other => Err(D::Error::invalid_type(
                Unexpected::Other(match other {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "真偽値",
                    serde_json::Value::Array(_) => "配列",
                    _ => "オブジェクト",
                }),
                &"文字列または数値",
            )),
        }
    }
}

/// 取り違えをコンパイルで止めるための、種類つきの ID。
///
/// ```
/// # use gumicord_model::{ChannelId, GuildId};
/// fn open(_: ChannelId) {}
/// let g = GuildId::from(1u64);
/// // open(g);  ← 通らない
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
    /// ギルド (サーバー)
    GuildId;
    /// チャンネル。DM も含む
    ChannelId;
    /// メッセージ
    MessageId;
    /// 利用者
    UserId;
    /// 添付
    AttachmentId;
    /// ロール
    RoleId;
    /// 絵文字
    EmojiId;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JSON では文字列で来る。**数値として読もうとすると桁が落ちる**
    #[test]
    fn snowflakes_round_trip_as_strings() {
        let id = Snowflake(1_234_567_890_123_456_789);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"1234567890123456789\"");
        assert_eq!(serde_json::from_str::<Snowflake>(&json).unwrap(), id);
    }

    /// 数値で来ても受ける。相手の都合で落ちない
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

    /// ID に時刻が入っている。別途持たなくてよい
    #[test]
    fn a_snowflake_carries_its_own_timestamp() {
        // Discord 紀元ちょうど
        assert_eq!(Snowflake(0).created_at_ms(), DISCORD_EPOCH_MS);
        // 1 ミリ秒後 (下位 22 ビットは通し番号)
        assert_eq!(Snowflake(1 << 22).created_at_ms(), DISCORD_EPOCH_MS + 1);
    }

    /// ID の順序は生成の順序である。メッセージの並べ替えに使える
    #[test]
    fn snowflakes_sort_by_creation_time() {
        let older = Snowflake(1 << 22);
        let newer = Snowflake(9 << 22);
        assert!(older < newer);
        assert!(older.created_at_ms() < newer.created_at_ms());
    }

    /// 種類つきの ID も文字列として往復する
    #[test]
    fn typed_ids_serialise_like_snowflakes() {
        let c = ChannelId::from(42u64);
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"42\"");
        assert_eq!(serde_json::from_str::<ChannelId>("\"42\"").unwrap(), c);
        assert_eq!(c.to_string(), "42");
    }
}
