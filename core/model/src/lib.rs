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

pub mod asset;
pub mod snowflake;
pub mod token;

pub mod de;

pub use asset::{Asset, Format};
pub use snowflake::{
    AttachmentId, ChannelId, EmojiId, GuildId, MessageId, RoleId, Snowflake, UserId,
};
pub use token::Token;

use serde::{Deserialize, Serialize};

/// 利用者。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    /// 単一の利用者名。**画面に出すのはこれではない** ([`User::display_name`])
    pub username: String,
    /// 自分で決めた表示名。設定していなければ `None`
    #[serde(default)]
    pub global_name: Option<String>,
    /// 旧来の 4 桁。
    ///
    /// ⚠️ **新方式へ移った利用者は `"0"` になるが、ボットは持ったままである。**
    /// 既定のアバターの選び方が旧方式と新方式で違うので、
    /// **捨てるとボットの顔が変わる**。だから残して保存もする
    #[serde(default)]
    pub discriminator: String,
    /// アバターの印。**URL ではない** ([`User::avatar`])
    #[serde(default, rename = "avatar")]
    pub avatar_hash: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl User {
    /// 画面に出す名前。**`global_name` があればそちら**
    pub fn display_name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }

    /// 旧来の 4 桁。**新方式へ移った利用者には無い**。
    ///
    /// `"0"` も `"0000"` も「無い」の意味で、区別に意味はない。
    /// ボットは移行の対象外なので、ここが `Some` なら概ねボットである
    pub fn tag(&self) -> Option<&str> {
        let d = self.discriminator.trim();
        (!d.is_empty() && !d.chars().all(|c| c == '0')).then_some(d)
    }

    /// 本人が設定したアバター。設定していなければ `None`
    pub fn avatar(&self) -> Option<Asset> {
        let hash = self.avatar_hash.as_ref()?;
        Some(Asset::user_avatar(self.id, hash))
    }

    /// 既定のアバターの通し番号。
    ///
    /// # ⚠️ 選び方が 2 通りある
    ///
    /// ```text
    ///   旧方式 (4 桁を持つ)   4 桁 % 5     → 0〜4
    ///   新方式 (4 桁が "0")   (id >> 22) % 6 → 0〜5
    /// ```
    ///
    /// **旧方式は同じ 4 桁の全員が同じ顔になる。** 新方式が識別子から引く
    /// ようになったのはそのためである。ボットは旧方式のままなので、
    /// [`User::discriminator`] を捨てると顔が変わってしまう
    pub fn default_avatar_index(&self) -> u64 {
        match self.tag().and_then(|d| d.parse::<u64>().ok()) {
            Some(d) => d % 5,
            None => (self.id.get() >> 22) % 6,
        }
    }

    /// 誰にでも配られる既定のアバター。**必ずある**。
    ///
    /// ⚠️ **大きさを選べない。** 同じ番号の全員が同じ絵なので、
    /// 揃えておけば**利用者をまたいで 1 枚を使い回せる**
    pub fn default_avatar(&self) -> Asset {
        Asset::default_avatar(self.default_avatar_index())
    }

    /// 出すべきアバター。**必ずある**。
    ///
    /// 設定していれば本人のもの、していなければ既定のものである。
    ///
    /// ⚠️ **ギルドごとのアバターはここでは見えない。** それを知っているのは
    /// [`Member`] だけなので、ギルドの中では [`Member::display_avatar`] を使う
    pub fn display_avatar(&self) -> Asset {
        self.avatar().unwrap_or_else(|| self.default_avatar())
    }
}

/// ギルドの一員。
///
/// # ⚠️ 同じ人でも、ギルドごとに名前も顔も違う
///
/// ```text
///   User    ねんねこ           全体の名前と顔
///   Member  ねこ (このサーバでは)  そのギルドだけの名前と顔
/// ```
///
/// 上書きされているところだけが入っていて、**入っていなければ [`User`] の
/// ものを使う**。[`Member::display_name`] と [`Member::display_avatar`] が
/// その順序を持っている。
///
/// # ⚠️ どのギルドの一員かを、自分では知らない
///
/// `MESSAGE_CREATE` に添えて来る `member` には `guild_id` が入っていない。
/// 入っているのは**メッセージのほう**である。ここに嘘の `guild_id` を
/// 持たせるより、**知っている側から渡してもらう**ほうが正しい。
///
/// だから [`Member::guild_avatar`] はギルドを引数に取る。
/// 役職 1 つ。
///
/// # ⚠️ 色はまだ持たない
///
/// Discord は役職に色を持たせ、公式クライアントは名前をその色で出す。
/// だが**データが見た目を決める**のはこの設計にまだ場所が無い
/// (サーバフォルダの色と同じ宿題である)。テーマが決めた色を、データが
/// 上書きしてよいかを決めていないうちは持ち込まない。
///
/// ここにあるのは**並べ方と名前**だけである。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    #[serde(default)]
    pub name: String,
    /// 大きいほど上。**メンバー一覧の見出しの順である**
    #[serde(default)]
    pub position: i64,
    /// メンバー一覧で**別の見出しとして立てるか**
    #[serde(default)]
    pub hoist: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// このギルドだけの呼び名
    #[serde(default)]
    pub nick: Option<String>,
    /// このギルドだけのアバターの印。**URL ではない**
    #[serde(default, rename = "avatar")]
    pub avatar_hash: Option<String>,
    #[serde(default)]
    pub roles: Vec<RoleId>,
    /// ISO 8601。**表示のための整形は上の層の仕事**
    #[serde(default)]
    pub joined_at: Option<String>,
    /// ⚠️ **無いことがある。** `MESSAGE_CREATE` の `member` には
    /// 送信者が誰かが書かれていない。それはメッセージ側の `author` である
    #[serde(default)]
    pub user: Option<User>,
}

impl Member {
    /// このギルドで画面に出す名前。
    ///
    /// ```text
    ///   nick → global_name → username
    /// ```
    ///
    /// `user` を持たない `member` もあるので、そのときは
    /// **[`User::display_name`] を呼ぶ側で重ねる**
    pub fn display_name<'a>(&'a self, user: &'a User) -> &'a str {
        self.nick
            .as_deref()
            .unwrap_or_else(|| self.user.as_ref().unwrap_or(user).display_name())
    }

    /// このギルドだけのアバター。設定していなければ `None`
    pub fn guild_avatar(&self, guild: GuildId, user: UserId) -> Option<Asset> {
        let hash = self.avatar_hash.as_ref()?;
        Some(Asset::member_avatar(guild, user, hash))
    }

    /// このギルドで出すべきアバター。**必ずある**。
    ///
    /// ```text
    ///   ギルドのアバター → 本人のアバター → 既定のアバター
    /// ```
    pub fn display_avatar(&self, guild: GuildId, user: &User) -> Asset {
        self.guild_avatar(guild, user.id)
            .unwrap_or_else(|| user.display_avatar())
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
    /// アイコンの印。**URL ではない** ([`Guild::icon`])
    #[serde(default, rename = "icon")]
    pub icon_hash: Option<String>,
    /// いま落ちている。**名前もチャンネルも入っていない**
    #[serde(default)]
    pub unavailable: bool,
    /// READY では入っていないことがある。あとから GUILD_CREATE で埋まる。
    ///
    /// ⚠️ **読めないチャンネルが 1 つあってもギルドごと落とさない**
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    pub channels: Vec<Channel>,
    /// 役職。**メンバー一覧の見出しに名前が要る**。
    ///
    /// ⚠️ 落ちているギルドには入っていない
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    pub roles: Vec<Role>,
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
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    roles: Vec<Role>,
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
            icon_hash: raw.icon.or(p_icon),
            unavailable: raw.unavailable,
            channels: raw.channels,
            roles: raw.roles,
        }
    }
}

impl Guild {
    /// サーバアイコン。設定していなければ `None`。
    ///
    /// ⚠️ **サーバには既定のアイコンが無い。** 人と違って Discord は絵を
    /// 配っていないので、無いときは頭文字を出すしかない
    pub fn icon(&self) -> Option<Asset> {
        let hash = self.icon_hash.as_ref()?;
        Some(Asset::guild_icon(self.id, hash))
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
            .map(User::display_name)
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
    /// 送信者の、このギルドでの姿。
    ///
    /// ⚠️ **ここに `user` は入っていない。** 送信者は `author` である。
    /// DM には最初から無い
    #[serde(default)]
    pub member: Option<Member>,
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
        assert_eq!(u.display_name(), "ねんねこ");

        let json = r#"{"id":"1","username":"nenneko"}"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.display_name(), "nenneko");
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
        assert!(u.avatar().is_none(), "設定していなければ絵は無い");

        u.avatar_hash = Some("a_abc".into());
        let a = u.avatar().unwrap();
        assert!(a.is_animated(), "動かせる素材であることは分かる");
        assert!(
            a.with_size(64).url().ends_with("a_abc.png?size=64"),
            "それでも頼むのは静止画"
        );

        u.avatar_hash = Some("abc".into());
        let a = u.avatar().unwrap();
        assert!(!a.is_animated());
        assert!(a.with_size(64).url().ends_with("abc.png?size=64"));
    }

    /// ⚠️ **4 桁を持っているかで既定のアバターの選び方が変わる。**
    ///
    /// ボットは移行の対象外なので 4 桁を持ったままである。捨てると
    /// 新方式として扱われ、**顔が変わる**
    #[test]
    fn the_default_avatar_depends_on_the_old_four_digits() {
        // 新方式。`"0"` は「無い」の意味である
        let u: User =
            serde_json::from_str(r#"{"id":"4194304","username":"x","discriminator":"0"}"#).unwrap();
        assert_eq!(u.tag(), None);
        assert_eq!(u.default_avatar_index(), 1, "(id >> 22) % 6");

        // 旧方式。4 桁をそのまま 5 で割る
        let u: User =
            serde_json::from_str(r#"{"id":"4194304","username":"x","discriminator":"0007"}"#)
                .unwrap();
        assert_eq!(u.tag(), Some("0007"));
        assert_eq!(u.default_avatar_index(), 2, "7 % 5");

        // `"0000"` も「無い」である。`"0"` と区別しない
        let u: User =
            serde_json::from_str(r#"{"id":"4194304","username":"x","discriminator":"0000"}"#)
                .unwrap();
        assert_eq!(u.tag(), None);
        assert_eq!(u.default_avatar_index(), 1);
    }

    /// ⚠️ **出す顔は必ずある。** 設定していない人にも Discord が配っている
    #[test]
    fn everyone_has_a_face() {
        let mut u: User = serde_json::from_str(r#"{"id":"7","username":"x"}"#).unwrap();
        assert!(u.display_avatar().url().contains("/embed/avatars/"));

        u.avatar_hash = Some("abc".into());
        assert!(
            u.display_avatar()
                .with_size(64)
                .url()
                .ends_with("abc.png?size=64")
        );
    }

    /// ⚠️ **ギルドの中では、名前も顔もそのギルドのものが勝つ。**
    ///
    /// 同じ人でもサーバごとに呼び名を変えている。全体の名前で出すと、
    /// **そのサーバでは誰なのか分からない**
    #[test]
    fn a_guild_overrides_both_the_name_and_the_face() {
        let user: User =
            serde_json::from_str(r#"{"id":"7","username":"nenneko","global_name":"ねんねこ"}"#)
                .unwrap();
        let guild = GuildId::from(1u64);

        // 何も上書きしていない一員
        let m: Member = serde_json::from_str("{}").unwrap();
        assert_eq!(m.display_name(&user), "ねんねこ");
        assert!(m.guild_avatar(guild, user.id).is_none());
        assert!(
            m.display_avatar(guild, &user)
                .url()
                .contains("/embed/avatars/"),
            "本人も設定していないなら既定の絵"
        );

        // このサーバだけの呼び名と顔
        let m: Member = serde_json::from_str(r#"{"nick":"ねこ","avatar":"xyz"}"#).unwrap();
        assert_eq!(m.display_name(&user), "ねこ");
        assert_eq!(
            m.display_avatar(guild, &user).url(),
            "https://cdn.discordapp.com/guilds/1/users/7/avatars/xyz.png"
        );
    }

    /// `MESSAGE_CREATE` は送信者のギルドでの姿を添えて来る
    #[test]
    fn a_message_carries_the_senders_guild_face() {
        let json = r#"{
            "id":"2","channel_id":"9","guild_id":"1","content":"はい",
            "author":{"id":"7","username":"nenneko"},
            "member":{"nick":"ねこ","roles":["3"],"joined_at":"2020-01-01T00:00:00+00:00"}
        }"#;
        let m: Message = serde_json::from_str(json).unwrap();
        let member = m.member.as_ref().expect("member が落ちている");

        assert_eq!(member.display_name(&m.author), "ねこ");
        assert_eq!(member.roles, vec![RoleId::from(3u64)]);
        assert!(member.user.is_none(), "送信者は author のほうである");
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
        assert_eq!(g.icon_hash.as_deref(), Some("abc"));
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
        assert_eq!(g.icon_hash.as_deref(), Some("xyz"));
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
