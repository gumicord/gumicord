//! 正規化された状態のインメモリ保持と SQLite への永続化。
//!
//! 責務: Gateway イベントの状態への反映 / ローカルキャッシュ / 全文検索 (FTS5) / 暗号化。
//!
//! ⚠️ **起動時はここから先に描画する。** S4 の実測で Gateway の READY 到達に
//! 672〜1120 ms かかることが分かっており、`NFR-001` (コールドスタート 500ms) の
//! 「操作可能」の定義に READY を含めることはできない。
//!
//! # まだ無いもの
//!
//! | | いつ |
//! |---|---|
//! | 全文検索 (FTS5) | M2 (`FR-035`) |
//! | キャッシュの暗号化 | M2 (`SEC-020`)。⚠️ **本文が平文でディスクに残る** |
//! | 送信キューのオフライン退避 | M2 (`NFR-012`) |
//! | 未読・既読位置 | READY の `read_state` を読んでいない |
//!
//! 要件: `FR-035`, `NFR-011`, `NFR-012`, `SEC-020`, `SEC-021`
//! 仕様: [`spec/02-architecture.md`]

pub mod db;

pub use db::{Db, DbError, Snapshot, default_path};

use std::collections::HashMap;

use gumicord_model::{Channel, ChannelId, Guild, GuildId, Message};

/// 正規化された状態。
///
/// # 「正規化」が意味すること
///
/// **同じものを 2 箇所に持たない。** チャンネルはギルドの中の配列ではなく
/// 識別子で引ける表にあり、ギルドは所属するチャンネルの識別子だけを持つ。
///
/// ```text
///   guilds:         GuildId   → 名前・アイコン
///   guild_channels: GuildId   → [ChannelId]      並び順つき
///   channels:       ChannelId → 種別・名前・話題
///   messages:       ChannelId → [Message]        古い順
/// ```
///
/// ⚠️ **これをしないと同じチャンネルが 2 つの形で存在する。** READY が
/// 持ってきたものと GUILD_UPDATE が持ってきたものが食い違ったとき、
/// どちらが正しいかを決める場所が無くなる。
///
/// # 画面より先にここが埋まる (`NFR-011`)
///
/// 起動時、Gateway に繋ぐより先に [`Db`] から読み込む。S4 の実測では
/// READY まで 672〜1120 ms かかり、**`NFR-001` (500 ms) に入らない**。
/// キャッシュから先に描いて、READY は後から差し替える。
#[derive(Debug, Default)]
pub struct Store {
    guilds: HashMap<GuildId, GuildRow>,
    /// ギルドごとのチャンネルの識別子。**並び順つき**
    guild_channels: HashMap<GuildId, Vec<ChannelId>>,
    channels: HashMap<ChannelId, Channel>,
    /// チャンネルごとのメッセージ。**古い順**
    messages: HashMap<ChannelId, Vec<Message>>,
    /// 名前順に並べたギルドの識別子。**毎フレーム並べ替えないため**
    order: Vec<GuildId>,
}

/// ギルドのうち、チャンネル以外の部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildRow {
    pub id: GuildId,
    pub name: String,
    pub icon: Option<String>,
}

impl Store {
    pub fn new() -> Self {
        Store::default()
    }

    /// 画面に出す順のギルド。**名前順**
    pub fn guilds(&self) -> impl Iterator<Item = &GuildRow> {
        self.order.iter().filter_map(|id| self.guilds.get(id))
    }

    pub fn guild(&self, id: GuildId) -> Option<&GuildRow> {
        self.guilds.get(&id)
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.get(&id)
    }

    /// そのギルドの、文字を読み書きできるチャンネル。**並び順つき**
    pub fn channels_of(&self, guild: GuildId) -> impl Iterator<Item = &Channel> {
        self.guild_channels
            .get(&guild)
            .into_iter()
            .flatten()
            .filter_map(|id| self.channels.get(id))
    }

    /// そのチャンネルのメッセージ。**まだ読んでいなければ空**
    pub fn messages(&self, channel: ChannelId) -> &[Message] {
        self.messages.get(&channel).map_or(&[], Vec::as_slice)
    }

    /// 中身を持っているか。**「空」と「まだ読んでいない」の区別である**
    pub fn has_messages(&self, channel: ChannelId) -> bool {
        self.messages.contains_key(&channel)
    }

    pub fn is_empty(&self) -> bool {
        self.guilds.is_empty()
    }

    /// ギルドを丸ごと入れ替える。READY と、キャッシュからの読み込みで使う。
    pub fn replace_guilds(&mut self, guilds: Vec<Guild>) {
        self.guilds.clear();
        self.guild_channels.clear();
        self.channels.clear();
        for g in guilds {
            self.upsert_guild(g);
        }
    }

    /// ギルドを 1 つ入れる・更新する。
    ///
    /// ⚠️ **殻だけのギルドは採らない。** 名前もチャンネルも無いので、
    /// 一覧に中身の無い丸が並ぶだけになる。GUILD_CREATE で届いたら入る
    pub fn upsert_guild(&mut self, guild: Guild) {
        if guild.unavailable || guild.name.is_empty() {
            return;
        }
        let id = guild.id;

        // ⚠️ **並び順は position、同じなら識別子順。** position が同じ
        // チャンネルは実在するので、そこで崩れると並びが毎フレーム変わる
        let mut channels: Vec<Channel> = guild
            .channels
            .into_iter()
            .filter(|c| c.kind.is_text())
            .collect();
        channels.sort_by_key(|c| (c.position, c.id.get()));

        // ⚠️ 更新のときにチャンネルが空なら、**前のものを残す**。
        // GUILD_UPDATE は名前だけを持ってくることがある
        if !channels.is_empty() || !self.guild_channels.contains_key(&id) {
            let ids: Vec<ChannelId> = channels.iter().map(|c| c.id).collect();
            for c in channels {
                self.channels.insert(c.id, c);
            }
            self.guild_channels.insert(id, ids);
        }

        self.guilds.insert(
            id,
            GuildRow {
                id,
                name: guild.name,
                icon: guild.icon,
            },
        );
        self.resort();
    }

    /// 過去分を丸ごと置く。**REST から取ってきたもので上書きする**
    pub fn set_backlog(&mut self, channel: ChannelId, list: Vec<Message>) {
        self.messages.insert(channel, list);
    }

    /// 新着を末尾に足す。**足したら真。**
    ///
    /// ⚠️ 開いていないチャンネルの分は溜めない。開いたときに取り直すので、
    /// ここで持つと二重になる。
    ///
    /// ⚠️ 同じものを二度足さない。送った直後は REST の応答と
    /// `MESSAGE_CREATE` が競る
    pub fn push_message(&mut self, message: Message) -> bool {
        let Some(list) = self.messages.get_mut(&message.channel_id) else {
            return false;
        };
        if list.iter().any(|m| m.id == message.id) {
            return false;
        }
        list.push(message);
        true
    }

    /// 名前順に並べ直す。同名のギルドは実在するので識別子順に倒す
    fn resort(&mut self) {
        self.order = self.guilds.keys().copied().collect();
        let mut keyed: Vec<(String, GuildId)> = self
            .order
            .iter()
            .map(|id| {
                let name = self
                    .guilds
                    .get(id)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                (name, *id)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.get().cmp(&b.1.get())));
        self.order = keyed.into_iter().map(|(_, id)| id).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_model::{ChannelKind, MessageId, User, UserId};

    fn guild(id: u64, name: &str, channels: &[(u64, &str, i32)]) -> Guild {
        Guild {
            id: id.into(),
            name: name.to_owned(),
            icon: None,
            unavailable: false,
            channels: channels
                .iter()
                .map(|(cid, cname, pos)| Channel {
                    id: (*cid).into(),
                    kind: ChannelKind::GuildText,
                    name: Some((*cname).to_owned()),
                    guild_id: Some(id.into()),
                    parent_id: None,
                    position: *pos,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                })
                .collect(),
        }
    }

    fn message(id: u64, channel: u64) -> Message {
        Message {
            id: MessageId::from(id),
            channel_id: ChannelId::from(channel),
            guild_id: None,
            author: User {
                id: UserId::from(1u64),
                username: "ねんねこ".to_owned(),
                display_name: None,
                discriminator: "0".to_owned(),
                avatar: None,
                bot: false,
            },
            content: format!("その {id}"),
            timestamp: "2026-08-22T00:00:00+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            referenced_message: None,
        }
    }

    /// ギルドは名前順、チャンネルは position 順
    #[test]
    fn everything_comes_out_in_a_stable_order() {
        let mut s = Store::new();
        s.replace_guilds(vec![
            guild(2, "ばなな", &[(20, "ろ", 1), (21, "い", 0)]),
            guild(1, "あんず", &[]),
        ]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["あんず", "ばなな"]);

        let chans: Vec<_> = s
            .channels_of(GuildId::from(2u64))
            .map(|c| c.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(chans, vec!["い", "ろ"], "position 順になっていない");
    }

    /// **チャンネルは 1 箇所にしかない。** 同じものが 2 つの形で存在すると、
    /// 食い違ったときにどちらが正しいか決められなくなる
    #[test]
    fn a_channel_lives_in_exactly_one_place() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "あ", &[(10, "い", 0)])]);

        let c = s.channel(ChannelId::from(10u64)).expect("引けない");
        assert_eq!(c.name.as_deref(), Some("い"));
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 1);
    }

    /// GUILD_UPDATE がチャンネルを持ってこなくても、前のものを失わない
    #[test]
    fn an_update_without_channels_keeps_the_old_ones() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "まえ", &[(10, "い", 0)])]);
        s.upsert_guild(guild(1, "あと", &[]));

        assert_eq!(s.guild(GuildId::from(1u64)).unwrap().name, "あと");
        assert_eq!(
            s.channels_of(GuildId::from(1u64)).count(),
            1,
            "チャンネルが消えた"
        );
    }

    /// 殻だけのギルドは採らない
    #[test]
    fn a_shell_guild_is_not_shown() {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: String::new(),
            icon: None,
            unavailable: true,
            channels: Vec::new(),
        });
        assert_eq!(s.guilds().count(), 0);
    }

    /// 「空」と「まだ読んでいない」は別のものである
    #[test]
    fn empty_and_unread_are_different() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        assert!(!s.has_messages(ch));

        s.set_backlog(ch, Vec::new());
        assert!(s.has_messages(ch), "取り終えたことが分からない");
        assert!(s.messages(ch).is_empty());
    }

    /// 開いていないチャンネルの新着は溜めない
    #[test]
    fn a_message_for_an_unopened_channel_is_dropped() {
        let mut s = Store::new();
        assert!(!s.push_message(message(1, 99)));
        assert!(s.messages(ChannelId::from(99u64)).is_empty());
    }

    /// 同じものを二度足さない
    #[test]
    fn the_same_message_is_not_added_twice() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, Vec::new());

        assert!(s.push_message(message(1, 10)));
        assert!(!s.push_message(message(1, 10)));
        assert_eq!(s.messages(ch).len(), 1);
    }
}
