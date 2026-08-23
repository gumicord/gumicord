//! CDN に置かれた絵 1 枚。
//!
//! # なぜ URL を組み立てた文字列で持たないのか
//!
//! アバターもアイコンも、**どこにあるか**と**どう頼むか**が別々である。
//! 置き場は Discord が決め、大きさと形式はこちらが決める。文字列を返す
//! 関数にすると、その 2 つが呼び出しのたびに混ざる:
//!
//! ```text
//!   avatar_url(size)              ← 大きさを知らないと呼べない
//!   guild_avatar_url(guild, size) ← 引数が増えるたびに全部の呼び出しが変わる
//! ```
//!
//! [`Asset`] は置き場だけを持ち、大きさと形式は後から重ねる。
//! discord.py の `Asset` と同じ考えである。
//!
//! ```
//! # use gumicord_model::User;
//! # let user: User = serde_json::from_str(r#"{"id":"7","username":"x"}"#).unwrap();
//! let url = user.display_avatar().with_size(128).url();
//! assert!(url.contains("/embed/avatars/"));
//! ```
//!
//! # ⚠️ 既定は必ず PNG である
//!
//! `a_` で始まる印は動く絵 (GIF) だが、**頼むのは静止画である**。R5 の
//! 読み込みは PNG しか解けないので、動く形を頼むと**動かないどころか
//! 何も出せない**。動かす仕組みができるまで、この既定は変えない。

use std::fmt;

use crate::{GuildId, UserId};

/// CDN の場所。**ここを変えるときは全部同時に変わる**
const BASE: &str = "https://cdn.discordapp.com";

/// 頼める一番小さい辺
const MIN_SIZE: u16 = 16;
/// 頼める一番大きい辺
const MAX_SIZE: u16 = 4096;

/// 絵の形式。
///
/// ⚠️ **`Gif` を選べるが、いま読めるのは `Png` だけである。**
/// 選べるようにしてあるのは、動く絵を扱う日に [`Asset`] 側を
/// 触らずに済ませるためである
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Webp => "webp",
            Format::Gif => "gif",
        }
    }
}

/// CDN に置かれた絵 1 枚。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// [`BASE`] から下の道筋。**拡張子も `?size=` も含まない**
    path: String,
    /// Discord が付けた印 (ハッシュ)。既定の絵には無い
    key: String,
    /// 大きさを選べるか。
    ///
    /// ⚠️ **既定のアバターは選べない。** 誰が使っても同じ絵なので、
    /// 大きさを固定しておけば**利用者をまたいで 1 枚を使い回せる**
    resizable: bool,
    format: Format,
    size: Option<u16>,
}

/// ⚠️ **CDN の道筋を組み立ててよいのはここだけである。**
///
/// 同じ形の文字列があちこちに散ると、Discord が置き場を変えた日に
/// **どこを直せば済むのかが分からなくなる**。
impl Asset {
    /// 本人が設定したアバター
    pub fn user_avatar(user: UserId, hash: &str) -> Self {
        Asset::from_key(format!("avatars/{user}/{hash}"), hash)
    }

    /// そのギルドだけのアバター
    pub fn member_avatar(guild: GuildId, user: UserId, hash: &str) -> Self {
        Asset::from_key(format!("guilds/{guild}/users/{user}/avatars/{hash}"), hash)
    }

    /// 誰にでも配られる既定のアバター。`index` は [`crate::User::default_avatar_index`]
    pub fn default_avatar(index: u64) -> Self {
        Asset::fixed(format!("embed/avatars/{index}"))
    }

    /// サーバアイコン
    pub fn guild_icon(guild: GuildId, hash: &str) -> Self {
        Asset::from_key(format!("icons/{guild}/{hash}"), hash)
    }

    /// 印から作る。`key` が `a_` で始まれば動く絵である
    fn from_key(path: impl Into<String>, key: &str) -> Self {
        Asset {
            path: path.into(),
            key: key.to_owned(),
            resizable: true,
            format: Format::default(),
            size: None,
        }
    }

    /// 誰にでも配られる絵。**大きさを選べない**
    fn fixed(path: impl Into<String>) -> Self {
        Asset {
            path: path.into(),
            key: String::new(),
            resizable: false,
            format: Format::default(),
            size: None,
        }
    }

    /// Discord が付けた印。既定の絵なら空である
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 動く絵か。
    ///
    /// ⚠️ **動く絵でも [`Asset::url`] が返すのは静止画である。**
    /// これは「動かせる素材がある」という事実であって、いま何を頼むかの
    /// 話ではない
    pub fn is_animated(&self) -> bool {
        self.key.starts_with("a_")
    }

    /// 辺の長さを決める。
    ///
    /// ⚠️ **2 の冪でないと Discord が丸める。** 丸めた結果が何になるかは
    /// 向こうの都合なので、こちらで 2 の冪へ上げてから頼む。
    /// 大きさを選べない絵では**何もしない**
    pub fn with_size(mut self, size: u16) -> Self {
        if self.resizable {
            self.size = Some(round_up_pow2(size));
        }
        self
    }

    /// 形式を決める。
    ///
    /// ⚠️ **`Png` 以外はまだ読めない** (R5)。それでも選べるのは、
    /// 読めるようになった日にここを触らずに済ませるためである
    pub fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// 頼む先の URL。
    pub fn url(&self) -> String {
        let mut url = format!("{BASE}/{}.{}", self.path, self.format.extension());
        if let Some(size) = self.size {
            url.push_str(&format!("?size={size}"));
        }
        url
    }
}

impl fmt::Display for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url())
    }
}

/// その値以上で一番小さい 2 の冪。範囲の外は端に寄せる
fn round_up_pow2(size: u16) -> u16 {
    let size = size.clamp(MIN_SIZE, MAX_SIZE);
    if size.is_power_of_two() {
        return size;
    }
    // MAX_SIZE 自体が 2 の冪なので、これが範囲を超えることはない
    size.next_power_of_two().min(MAX_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_rounded_up_to_a_power_of_two() {
        assert_eq!(round_up_pow2(128), 128);
        assert_eq!(round_up_pow2(100), 128);
        assert_eq!(round_up_pow2(1), MIN_SIZE, "小さすぎるものは端へ");
        assert_eq!(round_up_pow2(u16::MAX), MAX_SIZE, "大きすぎるものは端へ");
    }

    /// ⚠️ **動く印でも頼むのは png である。** R5 は png しか解けない
    #[test]
    fn an_animated_key_is_still_requested_as_png() {
        let a = Asset::user_avatar(UserId::from(7u64), "a_abc");
        assert!(a.is_animated(), "動かせる素材であることは分かる");
        assert!(a.url().ends_with("a_abc.png"), "頼むのは静止画");
    }

    /// ⚠️ **既定の絵は大きさを選べない。** 1 枚を使い回すためである
    #[test]
    fn a_fixed_asset_ignores_the_size() {
        let a = Asset::default_avatar(3).with_size(128);
        assert_eq!(a.url(), format!("{BASE}/embed/avatars/3.png"));
        assert_eq!(a.key(), "");
        assert!(!a.is_animated());
    }

    #[test]
    fn size_and_format_stack_onto_the_place() {
        let a = Asset::guild_icon(GuildId::from(1u64), "abc")
            .with_size(100)
            .with_format(Format::Webp);
        assert_eq!(a.url(), format!("{BASE}/icons/1/abc.webp?size=128"));
        assert_eq!(a.to_string(), a.url(), "Display は URL である");
    }
}
