//! Normalised in-memory state, persisted to SQLite.
//!
//! Startup draws from here before the gateway is even connected: READY takes
//! closer to a second, which does not fit the cold-start budget.
//!
//! Not here yet: full-text search, encryption of the cache (message bodies sit
//! on disk in the clear), an offline send queue, and muted-channel handling —
//! notification settings are not read, so muted channels still light up.
//! Read markers are not persisted, since READY brings them every time.

pub mod db;

pub use db::{Db, DbError, Snapshot, default_path};

use std::collections::{HashMap, HashSet};

use gumicord_model::{
    Asset, Channel, ChannelId, Guild, GuildId, Member, Message, MessageId, Role, RoleId, UserId,
};

/// The normalised state.
///
/// Nothing is stored twice: channels live in a table keyed by id, and a guild
/// holds only the ids of the channels in it.
///
/// ```text
///   guilds:         GuildId   -> name, icon
///   guild_channels: GuildId   -> [ChannelId]   in order
///   channels:       ChannelId -> kind, name, topic
///   messages:       ChannelId -> [Message]     oldest first
/// ```
///
/// Otherwise the same channel exists in two shapes, and when READY and
/// GUILD_UPDATE disagree there is nowhere to decide which is right.
///
/// Filled from the cache before connecting, and replaced when READY arrives.
#[derive(Debug, Default)]
pub struct Store {
    guilds: HashMap<GuildId, GuildRow>,
    /// Channel ids per guild, in order.
    guild_channels: HashMap<GuildId, Vec<ChannelId>>,
    channels: HashMap<ChannelId, Channel>,
    /// Messages per channel, oldest first.
    messages: HashMap<ChannelId, Vec<Message>>,
    /// Display order, kept here so nothing sorts per frame.
    order: Vec<GuildId>,
    /// The order the user arranged in Discord.
    preferred: Vec<GuildId>,
    /// Arrival order; anything unplaced follows in it.
    arrival: Vec<GuildId>,
    /// The sidebar order; folders and lone guilds share the list.
    sidebar: Vec<FolderRow>,
    /// Folded folders, persisted: refolding them is not the user's job.
    collapsed: std::collections::HashSet<u64>,
    /// Roles per guild, needed to name member list headings.
    roles: HashMap<GuildId, Vec<Role>>,
    /// Read markers, per channel.
    read: HashMap<ChannelId, ReadMark>,
    /// Members seen, per guild.
    ///
    /// Discord attaches `member` to gateway messages only, never to history
    /// from REST. Reading it off the message left a freshly opened channel
    /// with no nicknames, avatars or colours until one new message arrived.
    ///
    /// A member belongs to a (guild, person), not to a message, so it is
    /// remembered where it is seen and looked up from the message.
    ///
    /// Only people actually seen are here; filling the rest needs an explicit
    /// request.
    members: HashMap<(GuildId, UserId), Member>,
}

/// One channel's read marker.
///
/// Unread is a comparison, not a count: snowflakes carry time, so anything
/// above the marker is unread. Counting would need every message, which is
/// not held for channels that were never opened. Mention counts come from the
/// server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadMark {
    /// Read up to here; `None` means never read.
    pub seen: Option<MessageId>,
    /// Unread mentions.
    pub mentions: u32,
}

/// One sidebar row: a folder or a guild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildEntry<'a> {
    /// A folder header; pressing it folds.
    Folder { id: u64, row: &'a FolderRow },
    Guild {
        row: &'a GuildRow,
        /// Which folder it is in, if any.
        folder: Option<u64>,
    },
}

/// One sidebar folder.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FolderRow {
    /// Absent means this is not really a folder: unfoldered guilds arrive in
    /// the same list as containers without an id.
    pub id: Option<u64>,
    /// Unnamed folders borrow their contents' names.
    pub name: Option<String>,
    pub guilds: Vec<GuildId>,
    /// The colour the user chose, if any.
    #[serde(default)]
    pub color: Option<u32>,
}

/// One channel row: a heading or something openable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEntry<'a> {
    /// A category heading; nothing opens.
    Category(&'a Channel),
    Channel(&'a Channel),
}

/// A guild, minus its channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildRow {
    pub id: GuildId,
    pub name: String,
    /// The icon hash, not a URL.
    pub icon_hash: Option<String>,
}

impl Store {
    pub fn new() -> Self {
        Store::default()
    }

    /// Guilds in display order: the user's arrangement first, the rest as
    /// they arrived.
    pub fn guilds(&self) -> impl Iterator<Item = &GuildRow> {
        self.order.iter().filter_map(|id| self.guilds.get(id))
    }

    /// Folders and guilds in sidebar order.
    ///
    /// ```text
    ///   guild             outside a folder, positioned here
    ///   [folder]          presses fold it
    ///     guild           only while open
    ///   guild
    /// ```
    ///
    /// Unfoldered guilds are not collected at the end: Discord puts folders
    /// and lone guilds in one list, and pulling folders out first loses the
    /// order the user arranged.
    ///
    /// A folded folder emits no contents.
    pub fn guild_entries(&self) -> Vec<GuildEntry<'_>> {
        // With no known order, emit arrival order, flat.
        if self.sidebar.is_empty() {
            return self
                .guilds()
                .map(|row| GuildEntry::Guild { row, folder: None })
                .collect();
        }

        let mut out = Vec::new();
        let mut placed = std::collections::HashSet::new();

        for row in &self.sidebar {
            match row.id {
                // A real folder.
                Some(id) => {
                    out.push(GuildEntry::Folder { id, row });
                    placed.extend(row.guilds.iter().copied());
                    if self.collapsed.contains(&id) {
                        continue;
                    }
                    out.extend(row.guilds.iter().filter_map(|g| {
                        self.guilds.get(g).map(|row| GuildEntry::Guild {
                            row,
                            folder: Some(id),
                        })
                    }));
                }
                // Not a folder; its contents go straight into the list.
                None => {
                    placed.extend(row.guilds.iter().copied());
                    out.extend(row.guilds.iter().filter_map(|g| {
                        self.guilds
                            .get(g)
                            .map(|row| GuildEntry::Guild { row, folder: None })
                    }));
                }
            }
        }

        // Anything unplaced, which is where a newly joined guild lands.
        out.extend(
            self.guilds()
                .filter(|row| !placed.contains(&row.id))
                .map(|row| GuildEntry::Guild { row, folder: None }),
        );
        out
    }

    /// Sets the sidebar order. Pass unfoldered guilds too, as containers
    /// without an id, in order.
    pub fn set_sidebar(&mut self, rows: Vec<FolderRow>) {
        self.sidebar = rows;
    }

    pub fn sidebar(&self) -> &[FolderRow] {
        &self.sidebar
    }

    /// Folded folders, persisted across restarts.
    pub fn collapsed(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.collapsed.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn is_collapsed(&self, id: u64) -> bool {
        self.collapsed.contains(&id)
    }

    pub fn set_collapsed(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.collapsed = ids.into_iter().collect();
    }

    /// Toggles a folder and reports the new state.
    pub fn toggle_folder(&mut self, id: u64) -> bool {
        if !self.collapsed.insert(id) {
            self.collapsed.remove(&id);
            return true;
        }
        false
    }

    /// A guild's icon, if set. Unlike users, guilds have no built-in one, so
    /// callers fall back to initials.
    pub fn guild_icon(&self, id: GuildId) -> Option<Asset> {
        let hash = self.guilds.get(&id)?.icon_hash.as_ref()?;
        Some(Asset::guild_icon(id, hash))
    }

    pub fn guild(&self, id: GuildId) -> Option<&GuildRow> {
        self.guilds.get(&id)
    }

    /// A role's name, if known. Never the id: an 18-digit number as a heading
    /// tells the reader nothing.
    pub fn role_name(&self, guild: GuildId, role: RoleId) -> Option<&str> {
        self.roles
            .get(&guild)?
            .iter()
            .find(|r| r.id == role)
            .map(|r| &*r.name)
    }

    /// The colour for a person's name.
    ///
    /// The topmost *coloured* role wins, not the topmost role: an uncoloured
    /// role above it does not take the colour away. Unknown roles are skipped
    /// rather than guessed at while the guild is still arriving.
    pub fn member_tint(&self, guild: GuildId, roles: &[RoleId]) -> Option<u32> {
        let table = self.roles.get(&guild)?;
        table
            .iter()
            .filter(|r| roles.contains(&r.id))
            .filter_map(|r| Some((r.position, r.tint()?)))
            .max_by_key(|(position, _)| *position)
            .map(|(_, tint)| tint)
    }

    // ─────────────────────────────────────────── Unread

    /// Stores read markers from READY verbatim.
    ///
    /// Markers for things that are not channels are kept too; filtering
    /// happens on lookup instead.
    pub fn set_read_marks(&mut self, marks: impl IntoIterator<Item = (ChannelId, ReadMark)>) {
        self.read = marks.into_iter().collect();
    }

    /// Whether a channel has unread messages.
    ///
    /// A never-read channel is not unread: a freshly joined guild arrives with
    /// no markers at all, and treating that as unread lights up every channel
    /// the moment someone joins. Discord ignores anything from before joining.
    ///
    /// Mutes are not read yet, so muted channels still light up.
    pub fn is_unread(&self, channel: ChannelId) -> bool {
        let Some(last) = self.channels.get(&channel).and_then(|c| c.last_message_id) else {
            return false;
        };
        match self.read.get(&channel).and_then(|m| m.seen) {
            Some(seen) => last > seen,
            None => false,
        }
    }

    /// Unread mentions in a channel.
    pub fn mentions(&self, channel: ChannelId) -> u32 {
        self.read.get(&channel).map_or(0, |m| m.mentions)
    }

    /// A guild's unread state, rolled up from its channels.
    pub fn guild_unread(&self, guild: GuildId) -> (bool, u32) {
        let mut unread = false;
        let mut mentions = 0;
        for c in self.channels_of(guild) {
            unread |= self.is_unread(c.id);
            mentions += self.mentions(c.id);
        }
        (unread, mentions)
    }

    /// Marks a channel read locally. Telling the server is the caller's job.
    pub fn mark_read(&mut self, channel: ChannelId) -> bool {
        let last = self.channels.get(&channel).and_then(|c| c.last_message_id);
        let mark = self.read.entry(channel).or_default();
        // A never-read channel gets a marker too, or opening it leaves it
        // unread.
        let changed = mark.seen != last || mark.mentions != 0;
        mark.seen = last;
        mark.mentions = 0;
        changed
    }

    // ─────────────────────────────── Guild members

    /// Remembers a member; later sightings win.
    pub fn remember_member(&mut self, guild: GuildId, user: UserId, member: Member) {
        self.members.insert((guild, user), member);
    }

    /// A member, if one has been seen.
    pub fn member(&self, guild: GuildId, user: UserId) -> Option<&Member> {
        self.members.get(&(guild, user))
    }

    /// Remembers the member attached to a message.
    ///
    /// Skipped when the message carries no guild, which REST history does not:
    /// there would be no way to say which guild the member belongs to.
    pub fn remember_from_message(&mut self, message: &Message) {
        let (Some(guild), Some(member)) = (message.guild_id, message.member.as_ref()) else {
            return;
        };
        self.remember_member(guild, message.author.id, member.clone());
    }

    /// Records a new message and advances the channel's newest id, counting
    /// a mention when it names us.
    pub fn note_arrival(&mut self, message: &Message, me: Option<UserId>) -> bool {
        // A member belongs to a (guild, person), so remember it here.
        self.remember_from_message(message);

        let Some(channel) = self.channels.get_mut(&message.channel_id) else {
            return false;
        };
        // Never moves backwards; older messages can arrive late.
        if channel
            .last_message_id
            .is_some_and(|last| last >= message.id)
        {
            return false;
        }
        channel.last_message_id = Some(message.id);

        if me.is_some_and(|me| message.mentions_me(me)) {
            self.read.entry(message.channel_id).or_default().mentions += 1;
        }
        true
    }

    /// A folder's colour, if the user set one.
    pub fn folder_tint(&self, folder: u64) -> Option<u32> {
        self.sidebar
            .iter()
            .find(|f| f.id == Some(folder))
            .and_then(|f| f.color)
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.get(&id)
    }

    /// A guild's openable channels, in order. Categories are excluded.
    pub fn channels_of(&self, guild: GuildId) -> impl Iterator<Item = &Channel> {
        self.entries_of(guild).filter_map(|e| match e {
            ChannelEntry::Channel(c) => Some(c),
            ChannelEntry::Category(_) => None,
        })
    }

    /// Categories and channels in display order, following Discord's rules.
    ///
    /// ```text
    ///   channels with no category    by position
    ///   category A                   by position
    ///     └ its channels             by position
    ///   category B
    ///     └ …
    /// ```
    ///
    /// Positions collide, and ties break by id, which is creation order.
    /// Without that the list reorders every frame.
    pub fn entries_of(&self, guild: GuildId) -> impl Iterator<Item = ChannelEntry<'_>> {
        let ids = self.guild_channels.get(&guild).cloned().unwrap_or_default();
        let all: Vec<&Channel> = ids.iter().filter_map(|id| self.channels.get(id)).collect();

        fn sorted(mut v: Vec<&Channel>) -> Vec<&Channel> {
            v.sort_by_key(|c| (c.position, c.id.get()));
            v
        }

        let mut out: Vec<ChannelEntry<'_>> = Vec::with_capacity(all.len());

        // Channels outside any category.
        out.extend(
            sorted(
                all.iter()
                    .copied()
                    .filter(|c| !c.kind.is_category() && c.parent_id.is_none())
                    .collect(),
            )
            .into_iter()
            .map(ChannelEntry::Channel),
        );

        // Categories and their contents.
        for cat in sorted(
            all.iter()
                .copied()
                .filter(|c| c.kind.is_category())
                .collect(),
        ) {
            let children = sorted(
                all.iter()
                    .copied()
                    .filter(|c| !c.kind.is_category() && c.parent_id == Some(cat.id))
                    .collect(),
            );
            // Empty categories still appear, as in Discord: one visible there
            // but missing here reads as a misconfiguration.
            out.push(ChannelEntry::Category(cat));
            out.extend(children.into_iter().map(ChannelEntry::Channel));
        }
        out.into_iter()
    }

    /// A channel's messages, empty until fetched.
    pub fn messages(&self, channel: ChannelId) -> &[Message] {
        self.messages.get(&channel).map_or(&[], Vec::as_slice)
    }

    /// Distinguishes "empty" from "not fetched".
    pub fn has_messages(&self, channel: ChannelId) -> bool {
        self.messages.contains_key(&channel)
    }

    pub fn is_empty(&self) -> bool {
        self.guilds.is_empty()
    }

    /// Replaces every guild; used by READY and by the cache.
    pub fn replace_guilds(&mut self, guilds: Vec<Guild>) {
        self.guilds.clear();
        self.guild_channels.clear();
        self.channels.clear();
        self.arrival.clear();
        for g in guilds {
            self.upsert_guild(g);
        }
    }

    /// Inserts or updates one guild.
    ///
    /// Shells are rejected: with no name and no channels they would just be
    /// empty circles in the list. They arrive properly in GUILD_CREATE.
    pub fn upsert_guild(&mut self, guild: Guild) {
        if guild.unavailable || guild.name.is_empty() {
            return;
        }
        let id = guild.id;

        // By position, ties by id. Equal positions do occur, and without the
        // tiebreak the list reorders every frame.
        let channels: Vec<Channel> = guild
            .channels
            .into_iter()
            // Categories are kept; they are the headings.
            .filter(|c| c.kind.is_text() || c.kind.is_category())
            .collect();

        // An update with no channels keeps the previous ones: GUILD_UPDATE
        // sometimes carries only the name.
        if !channels.is_empty() || !self.guild_channels.contains_key(&id) {
            let ids: Vec<ChannelId> = channels.iter().map(|c| c.id).collect();
            for c in channels {
                self.channels.insert(c.id, c);
            }
            self.guild_channels.insert(id, ids);
        }

        // Same for roles, or the member list headings fall back to ids.
        if !guild.roles.is_empty() {
            // Counts only: ids and names would reveal which guilds the user
            // is in.
            tracing::debug!(
                roles = guild.roles.len(),
                colored = guild.roles.iter().filter(|r| r.tint().is_some()).count(),
                "役職を受け取った"
            );
            self.roles.insert(id, guild.roles);
        }

        // Arrival order, which anything unplaced falls back to.
        if !self.arrival.contains(&id) {
            self.arrival.push(id);
        }
        self.guilds.insert(
            id,
            GuildRow {
                id,
                name: guild.name,
                icon_hash: guild.icon_hash,
            },
        );
        self.resort();
    }

    /// Replaces the history with what REST returned.
    pub fn set_backlog(&mut self, channel: ChannelId, list: Vec<Message>) {
        self.messages.insert(channel, list);
    }

    /// The oldest message, which is where paging starts.
    pub fn oldest_message(&self, channel: ChannelId) -> Option<MessageId> {
        self.messages.get(&channel)?.first().map(|m| m.id)
    }

    /// Prepends older messages, returning how many were added.
    ///
    /// `list` must be oldest first; Discord returns newest first and reversing
    /// is the caller's job.
    ///
    /// Duplicates are skipped, since paging can request the same range twice.
    /// Nothing is added to a channel that was never opened, which would leave
    /// a history with a hole in it.
    pub fn prepend_messages(&mut self, channel: ChannelId, list: Vec<Message>) -> usize {
        let Some(existing) = self.messages.get_mut(&channel) else {
            return 0;
        };
        let known: HashSet<MessageId> = existing.iter().map(|m| m.id).collect();
        let mut older: Vec<Message> = list
            .into_iter()
            .filter(|m| !known.contains(&m.id))
            .collect();
        if older.is_empty() {
            return 0;
        }
        let added = older.len();
        older.append(existing);
        *existing = older;
        added
    }

    /// Appends a new message.
    ///
    /// Nothing accumulates for unopened channels, which are refetched on open.
    /// Duplicates are skipped: after sending, the REST reply races the gateway
    /// echo.
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

    /// Replaces an edited message.
    ///
    /// Unknown messages are never added: updates arrive for unopened channels
    /// and for history that was never paged in, and adding one would leave a
    /// lone row with a gap around it.
    ///
    /// Replaced wholesale. An update from a late-resolving embed carries no
    /// body, but Discord normally sends the complete message, and anything
    /// missing fills in on the next fetch.
    pub fn update_message(&mut self, message: Message) -> bool {
        let Some(list) = self.messages.get_mut(&message.channel_id) else {
            return false;
        };
        let Some(slot) = list.iter_mut().find(|m| m.id == message.id) else {
            return false;
        };
        *slot = message;
        true
    }

    /// Removes a deleted message.
    pub fn remove_message(&mut self, channel: ChannelId, id: MessageId) -> bool {
        let Some(list) = self.messages.get_mut(&channel) else {
            return false;
        };
        let before = list.len();
        list.retain(|m| m.id != id);
        before != list.len()
    }

    /// Sets the order the user arranged in Discord.
    ///
    /// Never by name: any other order stops being their guild list. Guilds not
    /// listed follow in arrival order.
    pub fn set_preferred_order(&mut self, order: Vec<GuildId>) {
        self.preferred = order;
        self.resort();
    }

    /// The current order, ready to persist.
    pub fn order(&self) -> &[GuildId] {
        &self.order
    }

    /// Reorders: the user's arrangement first, arrival order after. Names are
    /// never involved.
    fn resort(&mut self) {
        let rank: HashMap<GuildId, usize> = self
            .preferred
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();

        let mut ids: Vec<GuildId> = self.arrival.to_vec();
        ids.retain(|id| self.guilds.contains_key(id));

        // Stable, so anything unordered keeps its arrival order.
        ids.sort_by_key(|id| rank.get(id).copied().unwrap_or(usize::MAX));
        self.order = ids;
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
            icon_hash: None,
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
                    last_message_id: None,
                })
                .collect(),
            roles: Vec::new(),
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
                global_name: None,
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: format!("その {id}"),
            timestamp: "2026-08-22T00:00:00+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: None,
            referenced_message: None,
            mentions: Vec::new(),
            mention_everyone: false,
        }
    }

    /// An edit never adds an unknown message, which would leave a lone row
    /// with a gap around it.
    #[test]
    fn an_edit_never_adds_an_unknown_message() {
        let mut s = Store::new();
        s.set_backlog(ChannelId::from(9u64), vec![message(1, 9)]);

        // An unknown message in the same channel.
        let mut other = message(2, 9);
        other.content = "後から来た".to_owned();
        assert!(!s.update_message(other));
        assert_eq!(s.messages(ChannelId::from(9u64)).len(), 1);

        // A channel that was never opened.
        assert!(!s.update_message(message(3, 8)));
        assert!(s.messages(ChannelId::from(8u64)).is_empty());
    }

    #[test]
    fn an_edit_replaces_the_body() {
        let mut s = Store::new();
        s.set_backlog(ChannelId::from(9u64), vec![message(1, 9), message(2, 9)]);

        let mut edited = message(1, 9);
        edited.content = "直した".to_owned();
        assert!(s.update_message(edited));

        let list = s.messages(ChannelId::from(9u64));
        assert_eq!(list.len(), 2, "the count changed");
        assert_eq!(list[0].content, "直した");
        assert_eq!(list[1].content, "その 2", "the neighbour was edited too");
    }

    #[test]
    fn a_delete_removes_it_from_the_list() {
        let mut s = Store::new();
        let ch = ChannelId::from(9u64);
        s.set_backlog(ch, vec![message(1, 9), message(2, 9)]);

        assert!(s.remove_message(ch, MessageId::from(1u64)));
        assert_eq!(s.messages(ch).len(), 1);
        assert_eq!(s.messages(ch)[0].id, MessageId::from(2u64));

        // Deleting twice just reports no change.
        assert!(!s.remove_message(ch, MessageId::from(1u64)));
    }

    /// Guilds keep arrival order; channels sort by position.
    #[test]
    fn guilds_keep_their_arrival_order_and_channels_sort_by_position() {
        let mut s = Store::new();
        s.replace_guilds(vec![
            guild(2, "ばなな", &[(20, "ろ", 1), (21, "い", 0)]),
            guild(1, "あんず", &[]),
        ]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["ばなな", "あんず"], "sorted by name");

        let chans: Vec<_> = s
            .channels_of(GuildId::from(2u64))
            .map(|c| c.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(chans, vec!["い", "ろ"], "not in position order");
    }

    /// The user's order wins; the rest follow in arrival order.
    #[test]
    fn the_users_own_order_wins() {
        let mut s = Store::new();
        s.replace_guilds(vec![
            guild(1, "いち", &[]),
            guild(2, "に", &[]),
            guild(3, "さん", &[]),
        ]);

        // Only 3 and 1 are placed; 2 is unspecified.
        s.set_preferred_order(vec![GuildId::from(3u64), GuildId::from(1u64)]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["さん", "いち", "に"]);
    }

    /// A saved order can name guilds that have since been left.
    #[test]
    fn a_stale_order_does_not_resurrect_guilds() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "いち", &[])]);
        s.set_preferred_order(vec![GuildId::from(9u64), GuildId::from(1u64)]);

        let names: Vec<_> = s.guilds().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["いち"]);
        assert_eq!(s.order().len(), 1);
    }

    /// A category is a heading, followed by its contents.
    #[test]
    fn categories_come_out_as_headings_with_their_channels() {
        use gumicord_model::ChannelKind;

        let mut g = guild(1, "テスト", &[(10, "そとがわ", 0)]);
        g.channels.push(Channel {
            id: 20u64.into(),
            kind: ChannelKind::GuildCategory,
            name: Some("category".to_owned()),
            guild_id: Some(1u64.into()),
            parent_id: None,
            position: 1,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: None,
        });
        g.channels.push(Channel {
            id: 21u64.into(),
            kind: ChannelKind::GuildText,
            name: Some("なかみ".to_owned()),
            guild_id: Some(1u64.into()),
            parent_id: Some(20u64.into()),
            position: 0,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: None,
        });

        let mut s = Store::new();
        s.replace_guilds(vec![g]);

        let got: Vec<String> = s
            .entries_of(GuildId::from(1u64))
            .map(|e| match e {
                ChannelEntry::Category(c) => format!("[{}]", c.display_name()),
                ChannelEntry::Channel(c) => c.display_name(),
            })
            .collect();
        assert_eq!(got, vec!["そとがわ", "[category]", "なかみ"]);

        // A heading is not openable.
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 2);
    }

    /// Empty categories still appear, as in Discord.
    #[test]
    fn an_empty_category_is_still_shown() {
        use gumicord_model::ChannelKind;

        let mut g = guild(1, "テスト", &[]);
        g.channels.push(Channel {
            id: 20u64.into(),
            kind: ChannelKind::GuildCategory,
            name: Some("からっぽ".to_owned()),
            guild_id: Some(1u64.into()),
            parent_id: None,
            position: 0,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: None,
        });

        let mut s = Store::new();
        s.replace_guilds(vec![g]);

        let got: Vec<_> = s.entries_of(GuildId::from(1u64)).collect();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], ChannelEntry::Category(_)));
        // A heading is not openable.
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 0);
    }

    /// A channel exists once; two copies leave no way to resolve a
    /// disagreement.
    #[test]
    fn a_channel_lives_in_exactly_one_place() {
        let mut s = Store::new();
        s.replace_guilds(vec![guild(1, "あ", &[(10, "い", 0)])]);

        let c = s.channel(ChannelId::from(10u64)).expect("not found");
        assert_eq!(c.name.as_deref(), Some("い"));
        assert_eq!(s.channels_of(GuildId::from(1u64)).count(), 1);
    }

    /// A GUILD_UPDATE without channels keeps the previous ones.
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

    /// Shells are rejected.
    #[test]
    fn a_shell_guild_is_not_shown() {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: String::new(),
            icon_hash: None,
            unavailable: true,
            channels: Vec::new(),
            roles: Vec::new(),
        });
        assert_eq!(s.guilds().count(), 0);
    }

    /// "Empty" is not "not fetched".
    #[test]
    fn empty_and_unread_are_different() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        assert!(!s.has_messages(ch));

        s.set_backlog(ch, Vec::new());
        assert!(s.has_messages(ch), "cannot tell it was fetched");
        assert!(s.messages(ch).is_empty());
    }

    /// Nothing accumulates for unopened channels.
    #[test]
    fn a_message_for_an_unopened_channel_is_dropped() {
        let mut s = Store::new();
        assert!(!s.push_message(message(1, 99)));
        assert!(s.messages(ChannelId::from(99u64)).is_empty());
    }

    /// Duplicates are skipped.
    #[test]
    fn the_same_message_is_not_added_twice() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, Vec::new());

        assert!(s.push_message(message(1, 10)));
        assert!(!s.push_message(message(1, 10)));
        assert_eq!(s.messages(ch).len(), 1);
    }

    /// An older page prepends and keeps its order.
    #[test]
    fn older_messages_go_in_front() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, vec![message(5, 10), message(6, 10)]);

        assert_eq!(s.oldest_message(ch), Some(MessageId::from(5u64)));
        assert_eq!(
            s.prepend_messages(ch, vec![message(3, 10), message(4, 10)]),
            2
        );

        let ids: Vec<u64> = s.messages(ch).iter().map(|m| m.id.get()).collect();
        assert_eq!(ids, vec![3, 4, 5, 6]);
        assert_eq!(s.oldest_message(ch), Some(MessageId::from(3u64)));
    }

    /// Overlap is dropped: the same range can be requested twice.
    #[test]
    fn an_overlapping_page_does_not_duplicate_rows() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        s.set_backlog(ch, vec![message(4, 10), message(5, 10)]);

        assert_eq!(
            s.prepend_messages(ch, vec![message(3, 10), message(4, 10)]),
            1
        );
        let ids: Vec<u64> = s.messages(ch).iter().map(|m| m.id.get()).collect();
        assert_eq!(ids, vec![3, 4, 5]);
    }

    /// Nothing is added to a channel that was never opened.
    #[test]
    fn a_channel_that_was_never_opened_gets_nothing() {
        let mut s = Store::new();
        let ch = ChannelId::from(10u64);
        assert_eq!(s.prepend_messages(ch, vec![message(1, 10)]), 0);
        assert!(!s.has_messages(ch));
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    fn store() -> Store {
        let mut s = Store::new();
        s.replace_guilds(vec![
            Guild {
                id: 1u64.into(),
                name: "いち".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: Vec::new(),
                roles: Vec::new(),
            },
            Guild {
                id: 2u64.into(),
                name: "に".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: Vec::new(),
                roles: Vec::new(),
            },
            Guild {
                id: 3u64.into(),
                name: "さん".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: Vec::new(),
                roles: Vec::new(),
            },
        ]);
        s
    }

    fn folder(id: u64, guilds: &[u64]) -> FolderRow {
        FolderRow {
            id: Some(id),
            name: Some(format!("folder{id}")),
            color: None,
            guilds: guilds.iter().map(|g| GuildId::from(*g)).collect(),
        }
    }

    /// An unfoldered guild, in the same list.
    fn bare(guild: u64) -> FolderRow {
        FolderRow {
            id: None,
            name: None,
            color: None,
            guilds: vec![GuildId::from(guild)],
        }
    }

    fn shape(s: &Store) -> Vec<String> {
        s.guild_entries()
            .into_iter()
            .map(|e| match e {
                GuildEntry::Folder { row, .. } => {
                    format!("[{}]", row.name.as_deref().unwrap_or(""))
                }
                GuildEntry::Guild {
                    row,
                    folder: Some(_),
                } => format!("  {}", row.name),
                GuildEntry::Guild { row, folder: None } => row.name.clone(),
            })
            .collect()
    }

    /// With no known order, arrival order, flat.
    #[test]
    fn without_a_sidebar_it_is_a_flat_list() {
        let s = store();
        assert_eq!(shape(&s), vec!["いち", "に", "さん"]);
    }

    /// Folder positions survive. Collecting lone guilds at the end used to
    /// lose the order the user arranged.
    #[test]
    fn a_folder_keeps_its_place_in_the_list() {
        let mut s = store();
        // Ordered: first guild, folder, third guild.
        s.set_sidebar(vec![bare(1), folder(10, &[2]), bare(3)]);

        assert_eq!(shape(&s), vec!["いち", "[folder10]", "  に", "さん"]);
    }

    /// A folded folder emits no contents.
    #[test]
    fn a_collapsed_folder_hides_its_guilds() {
        let mut s = store();
        s.set_sidebar(vec![bare(1), folder(10, &[2, 3])]);

        assert!(!s.toggle_folder(10), "should have folded");
        assert_eq!(shape(&s), vec!["いち", "[folder10]"]);

        assert!(s.toggle_folder(10), "should have unfolded");
        assert_eq!(shape(&s).len(), 4);
    }

    /// A guild left behind in a saved order does not appear.
    #[test]
    fn a_sidebar_referring_to_a_missing_guild_skips_it() {
        let mut s = store();
        s.set_sidebar(vec![folder(10, &[2, 999]), bare(1), bare(3)]);
        assert_eq!(shape(&s), vec!["[folder10]", "  に", "いち", "さん"]);
    }

    /// A newly joined guild lands at the end, being unplaced.
    #[test]
    fn a_guild_missing_from_the_sidebar_still_appears() {
        let mut s = store();
        s.set_sidebar(vec![bare(1), bare(2)]);
        assert_eq!(shape(&s), vec!["いち", "に", "さん"]);
    }

    /// The folded state round-trips.
    #[test]
    fn the_collapsed_set_round_trips() {
        let mut s = store();
        s.set_sidebar(vec![folder(10, &[2]), folder(20, &[3]), bare(1)]);
        s.toggle_folder(20);

        let saved = s.collapsed();
        assert_eq!(saved, vec![20]);

        let mut again = store();
        again.set_sidebar(vec![folder(10, &[2]), folder(20, &[3]), bare(1)]);
        again.set_collapsed(saved);
        assert_eq!(shape(&again), shape(&s));
    }
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    fn role(id: u64, position: i64, color: u32) -> Role {
        Role {
            id: RoleId::from(id),
            name: format!("役職{id}"),
            position,
            hoist: false,
            color: Some(color),
        }
    }

    fn store(roles: Vec<Role>) -> Store {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles,
        });
        s
    }

    /// The topmost coloured role wins; an uncoloured one above it does not
    /// take the colour away.
    #[test]
    fn the_highest_coloured_role_wins_not_the_highest_role() {
        let s = store(vec![
            role(10, 1, 0x0000_ff00),
            // Above, but uncoloured.
            role(20, 5, 0),
        ]);
        let mine = [RoleId::from(10u64), RoleId::from(20u64)];
        assert_eq!(s.member_tint(1u64.into(), &mine), Some(0x0000_ff00));
    }

    /// Zero is not black; painting it black makes every name unreadable.
    #[test]
    fn a_role_without_a_colour_gives_nothing() {
        let s = store(vec![role(10, 1, 0)]);
        assert_eq!(s.member_tint(1u64.into(), &[RoleId::from(10u64)]), None);
    }

    /// Unknown roles are skipped rather than guessed at.
    #[test]
    fn an_unknown_role_is_skipped() {
        let s = store(vec![role(10, 1, 0x0000_ff00)]);
        assert_eq!(s.member_tint(1u64.into(), &[RoleId::from(99u64)]), None);
        assert_eq!(s.member_tint(2u64.into(), &[RoleId::from(10u64)]), None);
    }

    /// An update with no roles keeps the previous ones, or colours and
    /// headings vanish.
    #[test]
    fn an_update_without_roles_keeps_the_old_ones() {
        let mut s = store(vec![role(10, 1, 0x0000_ff00)]);
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: "名前だけ変えた".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: Vec::new(),
        });
        assert_eq!(
            s.member_tint(1u64.into(), &[RoleId::from(10u64)]),
            Some(0x0000_ff00)
        );
    }

    /// A folder's colour is looked up by id.
    #[test]
    fn a_folder_carries_its_colour() {
        let mut s = Store::new();
        s.set_sidebar(vec![
            FolderRow {
                id: Some(100),
                name: None,
                guilds: Vec::new(),
                color: Some(0x007c_6cf0),
            },
            FolderRow {
                id: Some(200),
                name: None,
                guilds: Vec::new(),
                color: None,
            },
        ]);
        assert_eq!(s.folder_tint(100), Some(0x007c_6cf0));
        assert_eq!(s.folder_tint(200), None);
        assert_eq!(s.folder_tint(999), None);
    }
}

#[cfg(test)]
mod unread_tests {
    use super::*;
    use gumicord_model::{ChannelKind, User};

    fn channel(id: u64, last: Option<u64>) -> Channel {
        Channel {
            id: ChannelId::from(id),
            kind: ChannelKind::GuildText,
            name: Some(format!("ch{id}")),
            guild_id: Some(GuildId::from(1u64)),
            parent_id: None,
            position: 0,
            topic: None,
            nsfw: false,
            recipients: Vec::new(),
            last_message_id: last.map(MessageId::from),
        }
    }

    fn store(channels: Vec<Channel>) -> Store {
        let mut s = Store::new();
        s.upsert_guild(Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels,
            roles: Vec::new(),
        });
        s
    }

    fn user(id: u64) -> User {
        User {
            id: gumicord_model::UserId::from(id),
            username: format!("u{id}"),
            global_name: None,
            discriminator: "0".to_owned(),
            avatar_hash: None,
            bot: false,
        }
    }

    fn message(id: u64, channel: u64, from: u64, to: &[u64]) -> Message {
        Message {
            id: MessageId::from(id),
            channel_id: ChannelId::from(channel),
            guild_id: Some(GuildId::from(1u64)),
            author: user(from),
            content: String::new(),
            timestamp: String::new(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: None,
            referenced_message: None,
            mentions: to.iter().map(|u| user(*u)).collect(),
            mention_everyone: false,
        }
    }

    /// Anything above the marker is unread.
    #[test]
    fn newer_than_the_mark_is_unread() {
        let mut s = store(vec![channel(10, Some(100))]);
        let ch = ChannelId::from(10u64);

        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(99u64)),
                mentions: 0,
            },
        )]);
        assert!(s.is_unread(ch));

        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(100u64)),
                mentions: 0,
            },
        )]);
        assert!(!s.is_unread(ch), "read up to the same point");
    }

    /// A never-read channel is not unread, or joining a guild lights up every
    /// channel in it.
    #[test]
    fn a_channel_we_never_read_is_not_unread() {
        let s = store(vec![channel(10, Some(100))]);
        assert!(!s.is_unread(ChannelId::from(10u64)));
    }

    /// A guild's state rolls up from its channels.
    #[test]
    fn a_guild_folds_up_its_channels() {
        let mut s = store(vec![channel(10, Some(100)), channel(11, Some(200))]);
        s.set_read_marks([
            (
                ChannelId::from(10u64),
                ReadMark {
                    seen: Some(MessageId::from(100u64)),
                    mentions: 0,
                },
            ),
            (
                ChannelId::from(11u64),
                ReadMark {
                    seen: Some(MessageId::from(150u64)),
                    mentions: 3,
                },
            ),
        ]);

        assert_eq!(s.guild_unread(GuildId::from(1u64)), (true, 3));
    }

    /// Opening marks it read and clears the mention count.
    #[test]
    fn opening_a_channel_clears_it() {
        let mut s = store(vec![channel(10, Some(100))]);
        let ch = ChannelId::from(10u64);
        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(50u64)),
                mentions: 2,
            },
        )]);

        assert!(s.mark_read(ch));
        assert!(!s.is_unread(ch));
        assert_eq!(s.mentions(ch), 0);
        assert!(!s.mark_read(ch), "2 度目は何も変わらない");
    }

    /// A new message advances the newest id, and counts a mention when it
    /// names us.
    #[test]
    fn an_arrival_moves_the_head_and_counts_mentions() {
        let mut s = store(vec![channel(10, Some(100))]);
        let ch = ChannelId::from(10u64);
        let me = gumicord_model::UserId::from(7u64);
        s.set_read_marks([(
            ch,
            ReadMark {
                seen: Some(MessageId::from(100u64)),
                mentions: 0,
            },
        )]);
        assert!(!s.is_unread(ch));

        assert!(s.note_arrival(&message(101, 10, 8, &[]), Some(me)));
        assert!(s.is_unread(ch));
        assert_eq!(s.mentions(ch), 0, "名指しではない");

        assert!(s.note_arrival(&message(102, 10, 8, &[7]), Some(me)));
        assert_eq!(s.mentions(ch), 1);
    }

    /// Our own messages never count; replies routinely include the sender.
    #[test]
    fn my_own_message_never_mentions_me() {
        let mut s = store(vec![channel(10, Some(100))]);
        let me = gumicord_model::UserId::from(7u64);

        s.note_arrival(&message(101, 10, 7, &[7]), Some(me));
        assert_eq!(s.mentions(ChannelId::from(10u64)), 0);
    }

    /// The newest id never moves backwards; older messages arrive late.
    #[test]
    fn a_late_old_message_does_not_move_the_head_back() {
        let mut s = store(vec![channel(10, Some(100))]);
        assert!(!s.note_arrival(&message(50, 10, 8, &[]), None));
        assert!(!s.note_arrival(&message(100, 10, 8, &[]), None));
    }
}
