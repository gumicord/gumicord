//! Discord domain types: types and serialisation only, no behaviour.
//!
//! Only fields this client uses are declared; the rest are dropped. Carrying
//! the raw payload would make it an extension ABI of its own and freeze us to
//! Discord's shape. Enums always keep an unknown variant, because Discord adds
//! channel and message kinds without notice and refusing to draw beats
//! failing to parse.
//!
//! See `spec/09-discord-protocol.md`.

pub mod asset;
pub mod identity;
pub mod snowflake;
pub mod token;

pub mod de;

pub use asset::{Asset, Format};
pub use snowflake::{
    AttachmentId, ChannelId, EmojiId, GuildId, MessageId, RoleId, Snowflake, UserId,
};
pub use token::Token;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    /// The unique handle. Not what is shown — see [`User::display_name`].
    pub username: String,
    pub global_name: Option<String>,
    /// The legacy four digits.
    ///
    /// Migrated users report `"0"`, but bots keep theirs, and the default
    /// avatar is chosen differently in each scheme. Dropping this changes a
    /// bot's face, so it is kept and persisted.
    #[serde(default)]
    pub discriminator: String,
    /// The avatar hash, not a URL — see [`User::avatar`].
    #[serde(default, rename = "avatar")]
    pub avatar_hash: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl User {
    pub fn display_name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }

    /// The legacy four digits, absent for migrated users.
    ///
    /// `"0"` and `"0000"` both mean absent. `Some` is broadly a bot, since
    /// bots were not migrated.
    pub fn tag(&self) -> Option<&str> {
        let d = self.discriminator.trim();
        (!d.is_empty() && !d.chars().all(|c| c == '0')).then_some(d)
    }

    /// The avatar the user set, if any.
    pub fn avatar(&self) -> Option<Asset> {
        let hash = self.avatar_hash.as_ref()?;
        Some(Asset::user_avatar(self.id, hash))
    }

    /// Which built-in avatar this user gets.
    ///
    /// ```text
    ///   legacy (has four digits)   digits % 5      -> 0..4
    ///   current (digits are "0")   (id >> 22) % 6  -> 0..5
    /// ```
    ///
    /// The legacy scheme gave everyone sharing four digits the same face,
    /// which is why the current one derives it from the id. Bots stayed on
    /// the legacy scheme, so discarding [`User::discriminator`] would change
    /// their faces.
    pub fn default_avatar_index(&self) -> u64 {
        match self.tag().and_then(|d| d.parse::<u64>().ok()) {
            Some(d) => d % 5,
            None => (self.id.get() >> 22) % 6,
        }
    }

    /// The built-in avatar. Always exists, and cannot be resized so that one
    /// copy is shared across users.
    pub fn default_avatar(&self) -> Asset {
        Asset::default_avatar(self.default_avatar_index())
    }

    /// The avatar to show. Always exists.
    ///
    /// Per-guild avatars are invisible here; inside a guild use
    /// [`Member::display_avatar`].
    pub fn display_avatar(&self) -> Asset {
        self.avatar().unwrap_or_else(|| self.default_avatar())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    #[serde(default)]
    pub name: String,
    /// Higher is above. The topmost coloured role decides a name's colour.
    #[serde(default)]
    pub position: i64,
    /// Whether the member list gives this role its own heading.
    #[serde(default)]
    pub hoist: bool,
    /// `0xRRGGBB`. Zero means uncoloured, not black.
    ///
    /// `Option` because `#[serde(default)]` only covers a missing field, and
    /// `"color": null` would otherwise fail. A failed role is dropped by
    /// [`crate::de::lenient_vec`], taking its *name* with it — the heading
    /// disappears, not just the colour.
    #[serde(default)]
    pub color: Option<u32>,
}

impl Role {
    /// The colour, if one is set.
    ///
    /// Zero means uncoloured; painting it black would make every name
    /// unreadable. Where the colour is applied is the theme's decision.
    pub fn tint(&self) -> Option<u32> {
        self.color.filter(|c| *c != 0)
    }
}

/// A member of a guild.
///
/// The same person has a different name and face per guild. Only the
/// overrides are stored; where absent, the [`User`] values apply, and
/// [`Member::display_name`] / [`Member::display_avatar`] encode that order.
///
/// A member does not know which guild it belongs to: the `member` attached to
/// `MESSAGE_CREATE` carries no `guild_id` — the message does. So
/// [`Member::guild_avatar`] takes the guild as an argument rather than storing
/// a made-up one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub nick: Option<String>,
    /// The per-guild avatar hash, not a URL.
    #[serde(default, rename = "avatar")]
    pub avatar_hash: Option<String>,
    #[serde(default)]
    pub roles: Vec<RoleId>,
    /// ISO 8601. Formatting belongs to the layer above.
    #[serde(default)]
    pub joined_at: Option<String>,
    /// Often absent: the `member` on `MESSAGE_CREATE` does not say who sent
    /// the message — that is the message's `author`.
    #[serde(default)]
    pub user: Option<User>,
}

impl Member {
    /// `nick` -> `global_name` -> `username`.
    ///
    /// Some `member` payloads carry no `user`, so the caller supplies one.
    pub fn display_name<'a>(&'a self, user: &'a User) -> &'a str {
        self.nick
            .as_deref()
            .unwrap_or_else(|| self.user.as_ref().unwrap_or(user).display_name())
    }

    pub fn guild_avatar(&self, guild: GuildId, user: UserId) -> Option<Asset> {
        let hash = self.avatar_hash.as_ref()?;
        Some(Asset::member_avatar(guild, user, hash))
    }

    /// Guild avatar -> user avatar -> built-in avatar. Always exists.
    pub fn display_avatar(&self, guild: GuildId, user: &User) -> Asset {
        self.guild_avatar(guild, user.id)
            .unwrap_or_else(|| user.display_avatar())
    }
}

/// A guild (server).
///
/// The name arrives in one of three shapes:
///
/// ```text
///   bot token       {"id": …, "name": "…", "icon": …}
///   user token      {"id": …, "properties": {"name": "…", "icon": …}, …}
///   unavailable     {"id": …, "unavailable": true}
/// ```
///
/// Missing the second one once left the guild list empty: READY arrived with
/// all eleven guilds and every name was blank. [`RawGuild`] absorbs all three
/// so only the flat shape survives here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawGuild")]
pub struct Guild {
    pub id: GuildId,
    /// Empty for an unavailable guild, which arrives as an id alone.
    #[serde(default)]
    pub name: String,
    /// The icon hash, not a URL — see [`Guild::icon`].
    #[serde(default, rename = "icon")]
    pub icon_hash: Option<String>,
    #[serde(default)]
    pub unavailable: bool,
    /// Sometimes absent in READY and filled in later by GUILD_CREATE. One
    /// unreadable channel must not take the guild with it.
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    pub channels: Vec<Channel>,
    /// Needed for member list headings. Absent for unavailable guilds.
    #[serde(default, deserialize_with = "crate::de::lenient_vec")]
    pub roles: Vec<Role>,
}

/// Accepts all three shapes Discord sends. Use [`Guild`], not this.
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
    /// Where a user token's READY puts the name and icon.
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
        // The top level wins: `properties` is a fallback location, not an
        // authority when the two disagree.
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
    /// The server icon, if one is set.
    ///
    /// Unlike users, guilds have no built-in icon; callers fall back to the
    /// initials.
    pub fn icon(&self) -> Option<Asset> {
        let hash = self.icon_hash.as_ref()?;
        Some(Asset::guild_icon(self.id, hash))
    }
}

/// Channel kind. `Unknown` must stay: Discord adds kinds without notice.
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
    /// A kind this client does not know. Not drawn, but not fatal.
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
    /// Whether text can be read and written here.
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            ChannelKind::GuildText
                | ChannelKind::Dm
                | ChannelKind::GroupDm
                | ChannelKind::GuildAnnouncement
        )
    }

    /// A heading, not something that opens.
    pub const fn is_category(self) -> bool {
        matches!(self, ChannelKind::GuildCategory)
    }

    /// Renderer icon name.
    pub const fn icon(self) -> &'static str {
        match self {
            ChannelKind::GuildVoice | ChannelKind::GuildStageVoice => "channel.voice",
            _ => "channel.text",
        }
    }
}

/// A channel. DMs use the same type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    /// Absent for DMs.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    /// The category this belongs to.
    #[serde(default)]
    pub parent_id: Option<ChannelId>,
    /// Sort order; ties break by id.
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub nsfw: bool,
    /// The other side of a DM or group DM.
    #[serde(default)]
    pub recipients: Vec<User>,
    /// The newest message here, used to decide unread state.
    ///
    /// May point at a deleted message; only the comparison against the read
    /// marker matters, so it need not exist.
    #[serde(default)]
    pub last_message_id: Option<MessageId>,
}

impl Channel {
    /// The name to show. DMs have none, so recipients are joined instead.
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
    pub fn is_image(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|t| t.starts_with("image/"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub channel_id: ChannelId,
    #[serde(default)]
    pub guild_id: Option<GuildId>,
    pub author: User,
    #[serde(default)]
    pub content: String,
    /// ISO 8601. Formatting belongs to the layer above.
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// The sender's guild-specific face. Carries no `user` — the sender is
    /// `author`. Absent in DMs.
    #[serde(default)]
    pub member: Option<Member>,
    #[serde(default)]
    pub referenced_message: Option<Box<Message>>,
    /// People named directly. Role mentions do not appear here; detecting
    /// those needs our own roles in this guild, which we do not hold.
    #[serde(default)]
    pub mentions: Vec<User>,
    /// `@everyone` or `@here`.
    #[serde(default)]
    pub mention_everyone: bool,
}

impl Message {
    /// Whether this message names us.
    ///
    /// Our own messages never count: replies routinely include the sender,
    /// and marking every one of those would be noise. Role mentions are not
    /// detected, for the reason on [`Message::mentions`].
    pub fn mentions_me(&self, me: UserId) -> bool {
        if self.author.id == me {
            return false;
        }
        self.mention_everyone || self.mentions.iter().any(|u| u.id == me)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentUser {
    #[serde(flatten)]
    pub user: User,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default, rename = "mfa_enabled")]
    pub mfa_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{
            "id": "1", "username": "ねんねこ",
            "a field we do not know": {"nested": [1,2,3]}
        }"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.username, "ねんねこ");
    }

    #[test]
    fn an_unknown_channel_kind_survives() {
        let c: Channel = serde_json::from_str(r#"{"id":"1","type":99}"#).unwrap();
        assert_eq!(c.kind, ChannelKind::Unknown(99));
        assert!(!c.kind.is_text());
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

    #[test]
    fn the_display_name_wins_over_the_username() {
        let json = r#"{"id":"1","username":"nenneko","global_name":"ねんねこ"}"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.display_name(), "ねんねこ");

        let json = r#"{"id":"1","username":"nenneko"}"#;
        let u: User = serde_json::from_str(json).unwrap();
        assert_eq!(u.display_name(), "nenneko");
    }

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

    /// An `a_` hash is a GIF, but asking for `.png` returns the first frame.
    /// The decoder only reads PNG, so requesting the animated form would
    /// render nothing at all.
    #[test]
    fn even_animated_avatars_are_requested_as_png() {
        let mut u: User = serde_json::from_str(r#"{"id":"7","username":"x"}"#).unwrap();
        assert!(u.avatar().is_none());

        u.avatar_hash = Some("a_abc".into());
        let a = u.avatar().unwrap();
        assert!(a.is_animated());
        assert!(a.with_size(64).url().ends_with("a_abc.png?size=64"));

        u.avatar_hash = Some("abc".into());
        let a = u.avatar().unwrap();
        assert!(!a.is_animated());
        assert!(a.with_size(64).url().ends_with("abc.png?size=64"));
    }

    /// Bots were never migrated, so dropping the four digits would move them
    /// onto the current scheme and change their faces.
    #[test]
    fn the_default_avatar_depends_on_the_old_four_digits() {
        // Current scheme; "0" means absent.
        let u: User =
            serde_json::from_str(r#"{"id":"4194304","username":"x","discriminator":"0"}"#).unwrap();
        assert_eq!(u.tag(), None);
        assert_eq!(u.default_avatar_index(), 1, "(id >> 22) % 6");

        // Legacy scheme.
        let u: User =
            serde_json::from_str(r#"{"id":"4194304","username":"x","discriminator":"0007"}"#)
                .unwrap();
        assert_eq!(u.tag(), Some("0007"));
        assert_eq!(u.default_avatar_index(), 2, "7 % 5");

        // "0000" also means absent.
        let u: User =
            serde_json::from_str(r#"{"id":"4194304","username":"x","discriminator":"0000"}"#)
                .unwrap();
        assert_eq!(u.tag(), None);
        assert_eq!(u.default_avatar_index(), 1);
    }

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

    /// People rename themselves per server; showing the global name would
    /// leave the reader unable to tell who it is.
    #[test]
    fn a_guild_overrides_both_the_name_and_the_face() {
        let user: User =
            serde_json::from_str(r#"{"id":"7","username":"nenneko","global_name":"ねんねこ"}"#)
                .unwrap();
        let guild = GuildId::from(1u64);

        let m: Member = serde_json::from_str("{}").unwrap();
        assert_eq!(m.display_name(&user), "ねんねこ");
        assert!(m.guild_avatar(guild, user.id).is_none());
        assert!(
            m.display_avatar(guild, &user)
                .url()
                .contains("/embed/avatars/")
        );

        let m: Member = serde_json::from_str(r#"{"nick":"ねこ","avatar":"xyz"}"#).unwrap();
        assert_eq!(m.display_name(&user), "ねこ");
        assert_eq!(
            m.display_avatar(guild, &user).url(),
            "https://cdn.discordapp.com/guilds/1/users/7/avatars/xyz.png"
        );
    }

    #[test]
    fn a_message_carries_the_senders_guild_face() {
        let json = r#"{
            "id":"2","channel_id":"9","guild_id":"1","content":"はい",
            "author":{"id":"7","username":"nenneko"},
            "member":{"nick":"ねこ","roles":["3"],"joined_at":"2020-01-01T00:00:00+00:00"}
        }"#;
        let m: Message = serde_json::from_str(json).unwrap();
        let member = m.member.as_ref().expect("member was dropped");

        assert_eq!(member.display_name(&m.author), "ねこ");
        assert_eq!(member.roles, vec![RoleId::from(3u64)]);
        assert!(
            member.user.is_none(),
            "the sender is author, not member.user"
        );
    }

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

    #[test]
    fn a_bot_style_guild_reads_its_name_from_the_top_level() {
        let g: Guild = serde_json::from_str(r#"{"id":"1","name":"ふつう","icon":"abc"}"#).unwrap();
        assert_eq!(g.name, "ふつう");
        assert_eq!(g.icon_hash.as_deref(), Some("abc"));
        assert!(!g.unavailable);
    }

    /// The shape that once left the guild list empty.
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
        .expect("cannot read the READY shape");

        assert_eq!(g.name, "本物");
        assert_eq!(g.icon_hash.as_deref(), Some("xyz"));
        assert_eq!(g.channels.len(), 1);
    }

    #[test]
    fn an_unavailable_guild_is_just_an_id() {
        let g: Guild = serde_json::from_str(r#"{"id":"1","unavailable":true}"#).unwrap();
        assert!(g.unavailable);
        assert!(g.name.is_empty());
        assert!(g.channels.is_empty());
    }

    #[test]
    fn the_top_level_wins_when_both_are_present() {
        let g: Guild =
            serde_json::from_str(r#"{"id":"1","name":"うえ","properties":{"name":"した"}}"#)
                .unwrap();
        assert_eq!(g.name, "うえ");
    }

    /// READY carries around eighteen fields this client ignores.
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

#[cfg(test)]
mod role_tests {
    use super::*;

    /// `#[serde(default)]` only covers a missing field. Failing here would
    /// drop the whole role and take its name with it.
    #[test]
    fn a_null_colour_does_not_take_the_role_with_it() {
        let raw = r#"{ "id": "55", "name": "管理者", "position": 3, "color": null }"#;
        let role: Role = serde_json::from_str(raw).expect("unreadable");

        assert_eq!(role.name, "管理者");
        assert_eq!(role.tint(), None);
    }

    #[test]
    fn zero_is_not_black() {
        let raw = r#"{ "id": "55", "name": "みんな", "color": 0 }"#;
        let role: Role = serde_json::from_str(raw).expect("unreadable");
        assert_eq!(role.tint(), None);
    }

    #[test]
    fn a_colour_comes_through() {
        let raw = r#"{ "id": "55", "name": "管理者", "color": 14688352 }"#;
        let role: Role = serde_json::from_str(raw).expect("unreadable");
        assert_eq!(role.tint(), Some(14_688_352));
    }

    #[test]
    fn one_unreadable_role_does_not_take_the_guild_with_it() {
        let raw = r#"{
            "id": "1", "name": "テスト",
            "roles": [
                { "id": "55", "name": "管理者", "color": 100 },
                { "id": "not an id at all", "name": "壊れている" },
                { "id": "56", "name": "みんな", "color": null }
            ]
        }"#;
        let guild: Guild = serde_json::from_str(raw).expect("unreadable");
        assert_eq!(guild.roles.len(), 2);
    }
}
