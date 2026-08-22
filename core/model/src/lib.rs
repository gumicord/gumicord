//! Discord のドメイン型。
//!
//! Snowflake / Guild / Channel / Message などの型とシリアライズを持つ。
//! **ここには振る舞いを置かない。** 型と変換だけの層である。
//!
//! # 生のペイロードをそのまま持たない
//!
//! Discord の JSON には我々が使わないフィールドが大量にある。全部持つと、
//! **それ自体が拡張 ABI になってしまい、Discord の変更に追随できなくなる**
//! ([`spec/03-uitree.md`] 2.4)。
//!
//! したがって**使うものだけを宣言し、知らないフィールドは捨てる**。
//! 足りなくなったら足す。捨てたことで壊れるのは我々だけであり、
//! テーマやプラグインには波及しない。
//!
//! # 知らない値で落ちない
//!
//! Discord は予告なく新しいチャンネル種別やメッセージ種別を足す。
//! **列挙は必ず「知らないもの」を持つ** ([`ChannelKind::Unknown`] など)。
//! 落ちるより、知らないものとして描かないほうがよい。
//!
//! 仕様: [`spec/09-discord-protocol.md`]

pub mod snowflake;
pub mod token;

pub mod de;

pub use snowflake::{
    AttachmentId, ChannelId, EmojiId, GuildId, MessageId, RoleId, Snowflake, UserId,
};
pub use token::Token;

use serde::{Deserialize, Serialize};

/// 利用者。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: String,
    /// 表示名。設定していなければ `None` で、`username` を出す
    #[serde(default, rename = "global_name")]
    pub display_name: Option<String>,
    /// 旧来の 4 桁。新方式の利用者は `"0"` になる
    #[serde(default)]
    pub discriminator: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl User {
    /// 画面に出す名前。**`display_name` があればそちら**
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }

    /// アバター画像の URL。設定していなければ `None`。
    ///
    /// `size` は 2 の冪 (16〜4096)。Discord がその大きさで返す。
    ///
    /// ⚠️ **動くアバターも静止画として頼む。** `a_` で始まる印は GIF だが、
    /// `.png` を頼めば 1 コマ目が PNG で返る。動かす仕組みは別の話であり、
    /// **読めない形を頼んで何も出せないほうが悪い**
    pub fn avatar_url(&self, size: u16) -> Option<String> {
        let hash = self.avatar.as_ref()?;
        Some(format!(
            "https://cdn.discordapp.com/avatars/{}/{hash}.png?size={size}",
            self.id
        ))
    }
}

/// ギルド (サーバー)。
///
/// # ⚠️ 名前の在処が 2 つある
///
/// **利用者トークンの READY では、名前もアイコンも `properties` の中にある。**
///
/// ```text
///   ボット            {"id": …, "name": "…", "icon": …}
///   利用者 (READY)    {"id": …, "properties": {"name": "…", "icon": …}, …}
///   落ちている        {"id": …, "unavailable": true}
/// ```
///
/// 実際にこれで**サーバ一覧が空になった**。READY は届き、11 件のギルドも
/// 入っていたのに、名前が空だったので 1 つも出せなかった。
///
/// 上の 3 つを [`RawGuild`] が吸収し、ここには平らな形だけが残る。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawGuild")]
pub struct Guild {
    pub id: GuildId,
    /// ⚠️ **無いことがある。** 落ちているギルドは識別子だけで来る
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    /// いま落ちている。**名前もチャンネルも入っていない**
    #[serde(default)]
    pub unavailable: bool,
    /// READY では入っていないことがある。あとから GUILD_CREATE で埋まる。
    ///
    /// ⚠️ **読めないチャンネルが 1 つあってもギルドごと落とさない**
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    pub channels: Vec<Channel>,
}

/// Discord がよこす**3 つの形**をそのまま受ける入れ物。
///
/// ⚠️ ここに直接触らないこと。[`Guild`] に変換された後の平らな形だけを使う
#[derive(Deserialize)]
struct RawGuild {
    id: GuildId,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    unavailable: bool,
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    channels: Vec<Channel>,
    /// 利用者トークンの READY はここに入れてくる
    #[serde(default)]
    properties: Option<GuildProperties>,
}

#[derive(Deserialize)]
struct GuildProperties {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

impl From<RawGuild> for Guild {
    fn from(raw: RawGuild) -> Self {
        // ⚠️ **上の階層を優先する。** `properties` は「そこにしか無いとき」の
        // 置き場であって、食い違ったときに勝ってよいものではない
        let (p_name, p_icon) = match raw.properties {
            Some(p) => (p.name, p.icon),
            None => (None, None),
        };

        Guild {
            id: raw.id,
            name: raw.name.or(p_name).unwrap_or_default(),
            icon: raw.icon.or(p_icon),
            unavailable: raw.unavailable,
            channels: raw.channels,
        }
    }
}

impl Guild {
    pub fn icon_url(&self, size: u16) -> Option<String> {
        let hash = self.icon.as_ref()?;
        // ⚠️ **動くアイコンも静止画として頼む** (User::avatar_url と同じ)
        Some(format!(
            "https://cdn.discordapp.com/icons/{}/{hash}.png?size={size}",
            self.id
        ))
    }
}

/// チャンネルの種別。
///
/// ⚠️ **`Unknown` を消さないこと。** Discord は予告なく種別を足す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
pub enum ChannelKind {
    GuildText,
    Dm,
    GuildVoice,
    GroupDm,
    GuildCategory,
    GuildAnnouncement,
    AnnouncementThread,
    PublicThread,
    PrivateThread,
    GuildStageVoice,
    GuildForum,
    /// 我々がまだ知らない種別。**描かないが落ちもしない**
    Unknown(u8),
}

impl From<u8> for ChannelKind {
    fn from(v: u8) -> Self {
        match v {
            0 => ChannelKind::GuildText,
            1 => ChannelKind::Dm,
            2 => ChannelKind::GuildVoice,
            3 => ChannelKind::GroupDm,
            4 => ChannelKind::GuildCategory,
            5 => ChannelKind::GuildAnnouncement,
            10 => ChannelKind::AnnouncementThread,
            11 => ChannelKind::PublicThread,
            12 => ChannelKind::PrivateThread,
            13 => ChannelKind::GuildStageVoice,
            15 => ChannelKind::GuildForum,
            other => ChannelKind::Unknown(other),
        }
    }
}

impl From<ChannelKind> for u8 {
    fn from(k: ChannelKind) -> Self {
        match k {
            ChannelKind::GuildText => 0,
            ChannelKind::Dm => 1,
            ChannelKind::GuildVoice => 2,
            ChannelKind::GroupDm => 3,
            ChannelKind::GuildCategory => 4,
            ChannelKind::GuildAnnouncement => 5,
            ChannelKind::AnnouncementThread => 10,
            ChannelKind::PublicThread => 11,
            ChannelKind::PrivateThread => 12,
            ChannelKind::GuildStageVoice => 13,
            ChannelKind::GuildForum => 15,
            ChannelKind::Unknown(v) => v,
        }
    }
}

impl ChannelKind {
    /// 文字を読み書きできるか。M1 で表示するのはここだけ
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            ChannelKind::GuildText
                | ChannelKind::Dm
                | ChannelKind::GroupDm
                | ChannelKind::GuildAnnouncement
        )
    }

    /// 見出しであって、開けるものではない
    pub const fn is_category(self) -> bool {
        matches!(self, ChannelKind::GuildCategory)
    }

    /// レンダラのアイコン名 (`gumicord_render::icon`)
    pub const fn icon(self) -> &'static str {
        match self {
            ChannelKind::GuildVoice | ChannelKind::GuildStageVoice => "channel.voice",
            _ => "channel.text",
        }
    }
}

/// チャンネル。DM も同じ型で表す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    /// DM には無い
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    /// カテゴリへの所属
    #[serde(default)]
    pub parent_id: Option<ChannelId>,
    /// 並び順。同じ値なら ID 順
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub nsfw: bool,
    /// DM と GroupDM の相手
    #[serde(default)]
    pub recipients: Vec<User>,
}

impl Channel {
    /// 画面に出す名前。
    ///
    /// **DM には名前が無い**ので、相手の名前を並べて作る。
    pub fn display_name(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        if self.recipients.is_empty() {
            return String::new();
        }
        self.recipients
            .iter()
            .map(User::name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// 添付ファイル (`FR-025`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub filename: String,
    pub size: u64,
    pub url: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl Attachment {
    /// 画像として表示できるか
    pub fn is_image(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|t| t.starts_with("image/"))
    }
}

/// メッセージ (`FR-020`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub channel_id: ChannelId,
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    pub author: User,
    #[serde(default)]
    pub content: String,
    /// ISO 8601。**表示のための整形は上の層の仕事**
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// 返信元 (`FR-028`)
    #[serde(default)]
    pub referenced_message: Option<Box<Message>>,
}

/// 自分自身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentUser {
    #[serde(flatten)]
    pub user: User,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub verified: bool,
    /// 2 要素認証を設定しているか (`FR-002`)
    #[serde(default, rename = "mfa_enabled")]
    pub mfa_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **知らないフィールドで落ちない。** Discord は予告なく足す
    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{
            "id": "1", "username": "ねんねこ",
            "まだ知らないフィールド": {"入れ子": [1,2,3]}
        }"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.username, "ねんねこ");
    }

    /// **知らないチャンネル種別でも落ちない。** 描かないだけ
    #[test]
    fn an_unknown_channel_kind_survives() {
        let c: Channel = serde_json::from_str(r#"{"id":"1","type":99}"#).unwrap();
        assert_eq!(c.kind, ChannelKind::Unknown(99));
        assert!(!c.kind.is_text());
        // 往復しても値が変わらない
        assert_eq!(serde_json::to_value(c.kind).unwrap(), 99);
    }

    #[test]
    fn known_channel_kinds_round_trip() {
        for k in [
            ChannelKind::GuildText,
            ChannelKind::Dm,
            ChannelKind::GuildVoice,
            ChannelKind::GuildForum,
        ] {
            let v = serde_json::to_value(k).unwrap();
            assert_eq!(serde_json::from_value::<ChannelKind>(v).unwrap(), k);
        }
    }

    /// 表示名があればそちらを使う
    #[test]
    fn the_display_name_wins_over_the_username() {
        let json = r#"{"id":"1","username":"nenneko","global_name":"ねんねこ"}"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.name(), "ねんねこ");

        let json = r#"{"id":"1","username":"nenneko"}"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.name(), "nenneko");
    }

    /// **DM には名前が無い。** 相手の名前で作る
    #[test]
    fn a_dm_is_named_after_its_recipients() {
        let json = r#"{
            "id":"1","type":1,
            "recipients":[{"id":"2","username":"みどり"},{"id":"3","username":"sururu"}]
        }"#;
        let c: Channel = serde_json::from_str(json).unwrap();
        assert_eq!(c.display_name(), "みどり, sururu");
        assert!(c.kind.is_text());
    }

    /// ⚠️ **動くアバターも png で頼む。**
    ///
    /// `a_` で始まる印は GIF だが、`.png` を頼めば 1 コマ目が PNG で返る。
    /// 読める形を頼まないと、**動かないどころか何も出せない** (R5 が読むのは
    /// PNG だけである)
    #[test]
    fn even_animated_avatars_are_requested_as_png() {
        let mut u: User = serde_json::from_str(r#"{"id":"7","username":"x"}"#).unwrap();
        assert_eq!(u.avatar_url(64), None, "設定していなければ URL は無い");

        u.avatar = Some("a_abc".into());
        assert!(u.avatar_url(64).unwrap().ends_with("a_abc.png?size=64"));

        u.avatar = Some("abc".into());
        assert!(u.avatar_url(64).unwrap().ends_with("abc.png?size=64"));
    }

    /// 返信元が入れ子で入る (`FR-028`)
    #[test]
    fn a_reply_carries_the_message_it_answers() {
        let json = r#"{
            "id":"2","channel_id":"9","content":"はい",
            "author":{"id":"1","username":"a"},
            "referenced_message":{
                "id":"1","channel_id":"9","content":"ですか？",
                "author":{"id":"2","username":"b"}
            }
        }"#;
        let m: Message = serde_json::from_str(json).unwrap();
        assert_eq!(m.referenced_message.unwrap().content, "ですか？");
    }
}

#[cfg(test)]
mod guild_shape_tests {
    use super::*;

    /// ボットの形。名前は上の階層にある
    #[test]
    fn a_bot_style_guild_reads_its_name_from_the_top_level() {
        let g: Guild = serde_json::from_str(r#"{"id":"1","name":"ふつう","icon":"abc"}"#).unwrap();
        assert_eq!(g.name, "ふつう");
        assert_eq!(g.icon.as_deref(), Some("abc"));
        assert!(!g.unavailable);
    }

    /// ⚠️ **利用者トークンの READY は properties の中に入れてくる。**
    /// これで実際にサーバ一覧が空になった
    #[test]
    fn a_user_style_guild_reads_its_name_from_properties() {
        let g: Guild = serde_json::from_str(
            r#"{
                "id":"1",
                "properties":{"name":"本物","icon":"xyz"},
                "channels":[{"id":"2","type":0,"name":"general"}],
                "lazy":true,
                "member_count":3
            }"#,
        )
        .expect("READY の形を読めない");

        assert_eq!(g.name, "本物");
        assert_eq!(g.icon.as_deref(), Some("xyz"));
        assert_eq!(g.channels.len(), 1);
    }

    /// 落ちているギルドは識別子だけ。**それでも読める**
    #[test]
    fn an_unavailable_guild_is_just_an_id() {
        let g: Guild = serde_json::from_str(r#"{"id":"1","unavailable":true}"#).unwrap();
        assert!(g.unavailable);
        assert!(g.name.is_empty());
        assert!(g.channels.is_empty());
    }

    /// 両方にあれば**上の階層が勝つ**。
    /// properties は「そこにしか無いとき」の置き場である
    #[test]
    fn the_top_level_wins_when_both_are_present() {
        let g: Guild =
            serde_json::from_str(r#"{"id":"1","name":"うえ","properties":{"name":"した"}}"#)
                .unwrap();
        assert_eq!(g.name, "うえ");
    }

    /// 知らないフィールドで落ちない。**READY には 18 個ほど入っている**
    #[test]
    fn unknown_fields_do_not_break_it() {
        let g: Guild = serde_json::from_str(
            r#"{"id":"1","properties":{"name":"x"},
                "application_command_counts":{},"data_mode":"full","emojis":[],
                "guild_scheduled_events":[],"joined_at":"…","large":false,
                "premium_subscription_count":0,"roles":[],"stage_instances":[],
                "stickers":[],"threads":[],"version":12345}"#,
        )
        .unwrap();
        assert_eq!(g.name, "x");
    }
}
