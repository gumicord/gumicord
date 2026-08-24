//! Real Discord data: the wiring between the store, the gateway and REST.
//!
//! Three sources collapse into one state. The cache is instant but stale,
//! REST is correct but costs a round trip, and the gateway only carries what
//! happens after connecting. Opening a channel shows the cache, replaces it
//! when REST returns, and follows the gateway from there.
//!
//! If REST returns first, a late cache result must not overwrite it.
//!
//! Startup draws from cache because READY takes closer to a second, which
//! does not fit the cold-start budget; waiting for it was never an option.
//!
//! Work runs on tokio and a writer thread, and results come back over a
//! channel that wakes the main thread.
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use gumicord_gateway::{Event, Fatal, Gateway, Ready, Subscriptions, status::Status};
use gumicord_model::{ChannelId, Guild, GuildId, Message, MessageId, Token, UserId};
use gumicord_platform::Waker;
use gumicord_rest::RestClient;
use gumicord_store::{Db, GuildRow, Store};

/// Messages fetched when a channel opens. The API allows 100; 50 is a little
/// more than fits on screen, which keeps the round trip short.
const BACKLOG: u8 = 50;

/// How long a typing indicator lives.
///
/// Discord resends `TYPING_START` while someone keeps typing; expiring sooner
/// than it does makes the indicator flicker mid-sentence.
const TYPING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// One person typing.
#[derive(Debug, Clone)]
struct Typist {
    user: UserId,
    name: String,
    at: std::time::Instant,
}

/// Connection state, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Not connected yet.
    Idle,
    Connecting,
    Up,
    /// Dropped; the gateway is still retrying internally.
    Reconnecting(String),
    /// Given up.
    Down(String),
}

impl Link {
    /// The line to show, and nothing while connected: announcing the normal
    /// case buries the abnormal one.
    pub fn hint(&self) -> Option<String> {
        match self {
            Link::Up | Link::Idle => None,
            Link::Connecting => Some("接続しています…".to_owned()),
            Link::Reconnecting(why) => Some(format!("再接続しています… ({why})")),
            Link::Down(why) => Some(format!("接続できません: {why}")),
        }
    }
}

/// What the background reports to the main thread.
#[derive(Debug)]
pub enum LiveEvent {
    Ready(Box<Ready>),
    /// A new message from the gateway.
    Posted(Box<Message>),
    /// One message edited.
    Edited(Box<Message>),
    /// One message deleted.
    Removed {
        channel: ChannelId,
        id: MessageId,
    },
    /// A guild's contents arrived; shells get filled in here.
    GuildChanged(Box<Guild>),
    /// Read from cache, ahead of REST.
    Cached {
        channel: ChannelId,
        list: Vec<Message>,
    },
    /// History from REST, already reordered oldest first.
    Backlog {
        channel: ChannelId,
        list: Vec<Message>,
    },
    /// An older page, prepended. Empty means the top of the channel.
    Older {
        channel: ChannelId,
        list: Vec<Message>,
    },
    /// A member list diff.
    Members(Box<gumicord_gateway::member_list::MemberListUpdate>),
    /// Members we asked for by name.
    ///
    /// REST messages carry no member, so this is the only source for them.
    MemberChunk {
        guild: GuildId,
        members: Vec<gumicord_model::Member>,
    },
    /// Someone started typing.
    Typing {
        channel: ChannelId,
        user: UserId,
        name: String,
    },
    Link(Link),
    /// The token was rejected; drop the keychain entry and the cache.
    TokenRejected,
}

/// Real data and the background work that carries it.
pub struct Live {
    tx: Sender<LiveEvent>,
    rx: Receiver<LiveEvent>,
    rt: Option<tokio::runtime::Handle>,
    rest: Option<RestClient>,
    waker: Option<Waker>,
    started: bool,

    store: Store,
    /// Local cache; the app works without one.
    db: Option<Db>,
    link: Link,
    /// Channels already requested, so nothing is fetched twice.
    requested: HashSet<ChannelId>,
    /// Channels with a page in flight.
    ///
    /// One scroll gesture fires many events; without this, each would send
    /// another request before the first answered.
    paging: HashSet<ChannelId>,
    /// Channels with nothing older left.
    exhausted: HashSet<ChannelId>,
    /// Something was prepended; consumed once by the renderer to hold the
    /// scroll position.
    prepended: bool,
    /// The token was rejected; cleared once the app has acted on it.
    rejected: bool,
    /// The channel open last time.
    last_channel: Option<ChannelId>,
    /// Tells the gateway what is being watched; nothing arrives without it.
    subs: Option<Subscriptions>,
    /// The channel on screen; anything arriving there counts as read.
    watching: Option<ChannelId>,
    /// Members already asked for.
    ///
    /// Asking repeatedly would flood the socket and get the connection
    /// closed. People who never come back stay recorded too: deleted or
    /// invisible users would otherwise be requested forever.
    asked_members: HashSet<(GuildId, UserId)>,
    /// Who is typing where. Never persisted.
    typing: std::collections::HashMap<ChannelId, Vec<Typist>>,
    /// Member lists per guild.
    ///
    /// Never cached: who is online is true at that moment, not at the next
    /// start. Really it is per channel, since permissions decide who is
    /// visible, but only one channel per guild is subscribed today.
    members: std::collections::HashMap<GuildId, gumicord_gateway::MemberList>,
    /// Ourselves, so our own typing indicator is not shown.
    me: Option<UserId>,
    /// Our status as of READY. `PRESENCE_UPDATE` is not handled, so changing
    /// it on a phone does not show here until the next connection.
    status: Option<Status>,
}

impl Live {
    /// Opens the cache and loads the previous state.
    ///
    /// Read synchronously: the first frame needs it, and deferring would show
    /// an empty screen for a moment. It takes a few milliseconds.
    pub fn new() -> Self {
        let mut live = Live::without_cache();

        match gumicord_store::default_path().and_then(|p| Db::open(&p)) {
            Ok((db, snapshot)) => {
                tracing::debug!(
                    guilds = snapshot.guilds.len(),
                    messages = snapshot.messages.len(),
                    "loaded from cache"
                );
                live.store.replace_guilds(snapshot.guilds);
                if !snapshot.guild_order.is_empty() {
                    live.store.set_preferred_order(snapshot.guild_order);
                }
                live.store.set_sidebar(snapshot.folders);
                live.store.set_collapsed(snapshot.collapsed);
                live.last_channel = snapshot.last_channel;
                if let Some(ch) = snapshot.last_channel {
                    // Not marked as requested: REST should still refetch once
                    // connected.
                    live.store.set_backlog(ch, snapshot.messages);
                }
                live.db = Some(db);
            }
            Err(e) => {
                // The cache only makes things faster.
                tracing::warn!(%e, "no cache; everything will be refetched");
            }
        }
        live
    }

    /// Without a cache, for demo mode and tests. `Live::new` would open the
    /// developer's real cache.
    pub fn without_cache() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Live {
            tx,
            rx,
            rt: None,
            rest: None,
            waker: None,
            started: false,
            store: Store::new(),
            db: None,
            link: Link::Idle,
            status: None,
            requested: HashSet::new(),
            paging: HashSet::new(),
            exhausted: HashSet::new(),
            prepended: false,
            rejected: false,
            last_channel: None,
            subs: None,
            watching: None,
            asked_members: HashSet::new(),
            typing: std::collections::HashMap::new(),
            members: std::collections::HashMap::new(),
            me: None,
        }
    }

    /// Takes the waker early, so cache reads before login can still redraw.
    /// The initial channel is read synchronously, but switching channels
    /// before connecting needs this.
    pub fn attach_waker(&mut self, waker: Waker) {
        self.waker.get_or_insert(waker);
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// For building state in tests.
    #[cfg(test)]
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// For injecting one event in tests.
    #[cfg(test)]
    pub fn apply_for_test(&mut self, event: LiveEvent) -> bool {
        self.apply(event)
    }

    /// The channel to reopen at startup.
    pub fn last_channel(&self) -> Option<ChannelId> {
        self.last_channel
    }

    pub fn guilds(&self) -> impl Iterator<Item = &GuildRow> {
        self.store.guilds()
    }

    /// Whether there is nothing at all yet.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Empty because it was fetched, or empty because it was not.
    ///
    /// Without the distinction, "loading" and "no messages" look the same.
    pub fn is_loading(&self, channel: ChannelId) -> bool {
        self.requested.contains(&channel) && !self.store.has_messages(channel)
    }

    /// Whether the token was rejected. Clears on read.
    pub fn take_rejection(&mut self) -> bool {
        std::mem::take(&mut self.rejected)
    }

    /// Starts the gateway. Called once per sign-in.
    pub fn start(
        &mut self,
        rt: &tokio::runtime::Handle,
        rest: RestClient,
        token: Token,
        waker: Waker,
    ) {
        if self.started {
            return;
        }
        self.started = true;
        self.rt = Some(rt.clone());
        self.rest = Some(rest);
        self.waker = Some(waker.clone());
        self.link = Link::Connecting;

        let (gateway, subs) = Gateway::new(token);
        self.subs = Some(subs);

        let tx = self.tx.clone();
        rt.spawn(async move { pump(gateway, tx, waker).await });
    }

    /// Who is typing in a channel.
    ///
    /// Expiry is applied on read; holding it as state would need something
    /// else to do the expiring.
    pub fn typing_in(&self, channel: ChannelId) -> Vec<&str> {
        let now = std::time::Instant::now();
        self.typing
            .get(&channel)
            .into_iter()
            .flatten()
            .filter(|t| now.duration_since(t.at) < TYPING_TTL)
            // Never ourselves; Discord does not either.
            .filter(|t| Some(t.user) != self.me)
            .map(|t| &*t.name)
            .collect()
    }

    /// Our status, if known.
    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// A guild's member list, empty until it arrives.
    pub fn members(&self, guild: GuildId) -> Option<&gumicord_gateway::MemberList> {
        self.members.get(&guild).filter(|m| !m.is_empty())
    }

    /// Records who we are, so our own typing is filtered out.
    pub fn set_me(&mut self, me: UserId) {
        self.me = Some(me);
    }

    /// Opens a channel: cache first, REST behind it.
    ///
    /// Requests are remembered, or reselecting a channel would refetch each
    /// time and hit the rate limit.
    pub fn open_channel(&mut self, guild: GuildId, channel: ChannelId) {
        if let Some(db) = &self.db {
            db.save_last_channel(channel);
        }
        self.watching = Some(channel);
        self.mark_read(channel);

        // Sent every time: without it neither new messages nor typing
        // indicators arrive for the new channel.
        if let Some(subs) = &self.subs {
            subs.watch(guild, channel);
        }

        if !self.requested.insert(channel) {
            return;
        }

        // Cache first: no round trip, so it works before connecting.
        if !self.store.has_messages(channel)
            && let (Some(db), Some(waker)) = (&self.db, &self.waker)
        {
            let (tx, waker) = (self.tx.clone(), waker.clone());
            db.load_messages(channel, move |list| {
                let _ = tx.send(LiveEvent::Cached { channel, list });
                waker.wake();
            });
        }

        // Then REST, which replaces it.
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        rt.spawn(async move {
            match rest.messages(channel, BACKLOG).await {
                Ok(mut list) => {
                    // Discord returns newest first; the view stacks oldest
                    // first.
                    list.reverse();
                    let _ = tx.send(LiveEvent::Backlog { channel, list });
                }
                Err(e) => {
                    // The cache is already on screen; do not leave it stuck
                    // on "loading".
                    tracing::warn!(%e, channel = %channel, "could not fetch messages");
                    let _ = tx.send(LiveEvent::Backlog {
                        channel,
                        list: Vec::new(),
                    });
                }
            }
            waker.wake();
        });
    }

    /// Fetches an older page.
    ///
    /// One scroll gesture reports the top many times, so a page in flight
    /// blocks another. Reaching the very top stops requests for good, or
    /// sitting at the top would keep asking. Does nothing with no messages
    /// yet: there is no anchor to page from.
    pub fn load_older(&mut self, channel: ChannelId) {
        if self.exhausted.contains(&channel) || self.paging.contains(&channel) {
            return;
        }
        let Some(before) = self.store.oldest_message(channel) else {
            return;
        };
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        self.paging.insert(channel);

        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        rt.spawn(async move {
            let list = match rest.messages_before(channel, BACKLOG, before).await {
                // Discord returns newest first; the view stacks oldest first.
                Ok(mut list) => {
                    list.reverse();
                    list
                }
                Err(e) => {
                    // Sent even when empty, or `paging` never clears and the
                    // channel can never page again.
                    tracing::warn!(%e, channel = %channel, "could not page back");
                    Vec::new()
                }
            };
            let _ = tx.send(LiveEvent::Older { channel, list });
            waker.wake();
        });
    }

    /// Whether something was prepended. True once, so the renderer can hold
    /// the scroll position.
    pub fn take_prepended(&mut self) -> bool {
        std::mem::take(&mut self.prepended)
    }

    /// Whether the top has been reached, so "loading" can stop.
    pub fn is_exhausted(&self, channel: ChannelId) -> bool {
        self.exhausted.contains(&channel)
    }

    /// Asks for the members behind the messages in a channel.
    ///
    /// Discord attaches `member` only to gateway messages, so names from REST
    /// start uncoloured and gain their colour a moment later. The official
    /// client behaves the same way; this is that moment.
    ///
    /// Only unknown members are asked for, in batches of 100.
    fn fill_members(&mut self, channel: ChannelId) {
        /// Discord's per-request limit.
        const CHUNK: usize = 100;

        let Some(subs) = &self.subs else { return };
        let Some(guild) = self.store.channel(channel).and_then(|c| c.guild_id) else {
            return;
        };

        let mut want: Vec<UserId> = Vec::new();
        for m in self.store.messages(channel) {
            let user = m.author.id;
            if self.store.member(guild, user).is_some() {
                continue;
            }
            if !self.asked_members.insert((guild, user)) {
                continue;
            }
            want.push(user);
        }
        if want.is_empty() {
            return;
        }
        tracing::debug!(users = want.len(), "requesting unknown members");
        for part in want.chunks(CHUNK) {
            subs.request_members(guild, part.to_vec());
        }
    }

    /// Marks a channel read locally, then tells the server.
    ///
    /// Waiting for the round trip would leave something already being read
    /// lit as unread. A failure does not revert the view either: going back
    /// to unread after opening is the most confusing outcome, and the next
    /// open resends it.
    pub fn mark_read(&mut self, channel: ChannelId) -> bool {
        if !self.store.mark_read(channel) {
            return false;
        }
        let Some(last) = self.store.channel(channel).and_then(|c| c.last_message_id) else {
            return true;
        };
        let (Some(rt), Some(rest)) = (&self.rt, &self.rest) else {
            return true;
        };
        let rest = rest.clone();
        rt.spawn(async move {
            if let Err(e) = rest.ack_message(channel, last).await {
                tracing::warn!(%e, channel = %channel, "could not send the read marker");
            }
        });
        true
    }

    /// Sends a message, as a reply when `reply_to` is set.
    ///
    /// Not added to the view: the gateway echoes it back, and adding it here
    /// too would show it twice.
    pub fn send_message(&self, channel: ChannelId, content: String, reply_to: Option<MessageId>) {
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, waker) = (rest.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.create_message(channel, &content, reply_to).await {
                tracing::warn!(%e, channel = %channel, "could not send");
            }
            waker.wake();
        });
    }

    /// Edits a message.
    ///
    /// Not applied locally: the gateway echoes it. Applying it first would
    /// leave a failed edit looking applied.
    pub fn edit_message(&self, channel: ChannelId, message: MessageId, content: String) {
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, waker) = (rest.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.edit_message(channel, message, &content).await {
                tracing::warn!(%e, channel = %channel, "could not edit");
            }
            waker.wake();
        });
    }

    /// Deletes a message.
    ///
    /// Not removed locally: a failed delete would make it reappear, which is
    /// more alarming than it simply not going away.
    pub fn delete_message(&self, channel: ChannelId, message: MessageId) {
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, waker) = (rest.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.delete_message(channel, message).await {
                tracing::warn!(%e, channel = %channel, "could not delete");
            }
            waker.wake();
        });
    }

    /// Drops everything on sign-out. Leaving the cache behind lets the next
    /// person on this machine read the previous one's messages.
    pub fn forget_everything(&mut self) {
        if let Some(db) = &self.db {
            db.wipe();
        }
        self.store = Store::new();
        self.requested.clear();
        self.paging.clear();
        self.exhausted.clear();
        self.watching = None;
        self.asked_members.clear();
        self.members.clear();
        self.typing.clear();
        self.last_channel = None;

        // The gateway task is already gone. Without clearing this, `start`
        // returns early after the next login and nothing ever reconnects.
        self.started = false;
        self.subs = None;
        self.me = None;
        self.link = Link::Connecting;
    }
    /// Stores the sidebar order.
    ///
    /// Folders and lone guilds arrive in one list; pulling folders out and
    /// appending the rest loses the order the user arranged.
    fn apply_sidebar(&mut self, folders: Vec<gumicord_gateway::Folder>) {
        let rows: Vec<gumicord_store::FolderRow> = folders
            .into_iter()
            .map(|f| gumicord_store::FolderRow {
                id: f.id,
                name: f.name,
                color: f.color,
                guilds: f.guilds,
            })
            .collect();

        if let Some(db) = &self.db {
            db.save_sidebar(&rows);
        }
        self.store.set_sidebar(rows);
    }

    /// Toggles a folder and persists it, so it does not need refolding at
    /// every start.
    pub fn toggle_folder(&mut self, id: u64) {
        self.store.toggle_folder(id);
        if let Some(db) = &self.db {
            db.save_collapsed(&self.store.collapsed());
        }
    }

    /// Drains every pending event.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(event) => changed |= self.apply(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return changed,
            }
        }
    }

    fn apply(&mut self, event: LiveEvent) -> bool {
        match event {
            LiveEvent::Ready(ready) => {
                self.link = Link::Up;
                // Take the order first; replacing guilds moves `ready`.
                self.me = Some(ready.user.user.id);
                // Being connected is not "online": it would lie about anyone
                // set to do not disturb.
                self.status = ready.status();
                let order = ready.guild_order();
                let folders = ready.guild_folders();
                // Before guilds; replacing them moves `ready`.
                let marks: Vec<(ChannelId, gumicord_store::ReadMark)> = ready
                    .read_state
                    .iter()
                    .flat_map(|r| r.entries())
                    .map(|e| {
                        (
                            e.id,
                            gumicord_store::ReadMark {
                                seen: e.last_message_id,
                                mentions: e.mention_count,
                            },
                        )
                    })
                    .collect();
                tracing::debug!(marks = marks.len(), "received read markers");
                self.store.set_read_marks(marks);

                self.store.replace_guilds(ready.guilds);
                if !order.is_empty() {
                    self.store.set_preferred_order(order);
                }
                self.apply_sidebar(folders);
                self.save_guilds();
                true
            }
            LiveEvent::Typing {
                channel,
                user,
                name,
            } => {
                let list = self.typing.entry(channel).or_default();
                let now = std::time::Instant::now();
                match list.iter_mut().find(|t| t.user == user) {
                    // Still typing; just extend the deadline.
                    Some(t) => t.at = now,
                    None => list.push(Typist {
                        user,
                        name,
                        at: now,
                    }),
                }
                true
            }
            LiveEvent::GuildChanged(g) => {
                self.store.upsert_guild(*g);
                self.save_guilds();
                true
            }
            LiveEvent::Posted(m) => {
                let channel = m.channel_id;
                // Channels that are not open pass through here too: unread
                // state advances without holding the body.
                let mut changed = self.store.note_arrival(&m, self.me);
                // Arriving in the open channel counts as read.
                if Some(channel) == self.watching {
                    changed |= self.mark_read(channel);
                }
                if !self.store.push_message(*m) {
                    return changed;
                }
                // Written one at a time so it survives a close.
                if let (Some(db), Some(last)) = (&self.db, self.store.messages(channel).last()) {
                    db.save_messages(channel, vec![last.clone()]);
                }
                true
            }
            LiveEvent::Edited(m) => {
                let channel = m.channel_id;
                // Edits carry `member`, which REST messages do not, so this
                // is another source for role colours.
                self.store.remember_from_message(&m);
                // Write the edited message, not the newest one.
                let saved = (*m).clone();
                if !self.store.update_message(*m) {
                    // Unknown message; adding it would leave a gap in the
                    // list.
                    return false;
                }
                // Also on disk, or reopening brings the old body back.
                if let Some(db) = &self.db {
                    db.save_messages(channel, vec![saved]);
                }
                true
            }
            LiveEvent::Removed { channel, id } => self.store.remove_message(channel, id),
            // Ignored once REST has answered; this would be older.
            LiveEvent::Cached { channel, list } => {
                if self.store.has_messages(channel) || list.is_empty() {
                    return false;
                }
                self.store.set_backlog(channel, list);
                true
            }
            LiveEvent::Backlog { channel, list } => {
                // Keep the cache when the fetch came back empty: losing
                // history while offline is the worst outcome.
                if list.is_empty() && self.store.has_messages(channel) {
                    return false;
                }
                if let Some(db) = &self.db {
                    db.save_messages(channel, list.clone());
                }
                // History was replaced, so paging can start over.
                self.exhausted.remove(&channel);
                self.store.set_backlog(channel, list);
                // REST messages carry no member; ask for them.
                self.fill_members(channel);
                true
            }
            LiveEvent::Older { channel, list } => {
                self.paging.remove(&channel);
                // Empty means the top of the channel.
                if list.is_empty() {
                    self.exhausted.insert(channel);
                    return false;
                }
                if let Some(db) = &self.db {
                    db.save_messages(channel, list.clone());
                }
                let added = self.store.prepend_messages(channel, list);
                self.fill_members(channel);
                if added == 0 {
                    // All known already; asking again returns the same page.
                    self.exhausted.insert(channel);
                    return false;
                }
                // Hold the scroll position: prepending grows the content and
                // would push the line being read downwards.
                self.prepended = true;
                true
            }
            LiveEvent::Members(update) => {
                let guild = update.guild;
                let changed = self.members.entry(guild).or_default().apply(*update);

                // Remember members seen here: for REST messages this can be
                // the only source of nicknames and role colours.
                if changed && let Some(list) = self.members.get(&guild) {
                    let seen: Vec<(UserId, gumicord_model::Member)> = list
                        .rows()
                        .iter()
                        .filter_map(|r| match r {
                            gumicord_gateway::MemberRow::Member(m) => {
                                Some((m.member.user.as_ref()?.id, m.member.clone()))
                            }
                            gumicord_gateway::MemberRow::Group { .. } => None,
                        })
                        .collect();
                    for (user, member) in seen {
                        self.store.remember_member(guild, user, member);
                    }
                }
                changed
            }
            LiveEvent::MemberChunk { guild, members } => {
                let mut changed = false;
                for m in members {
                    // Skip entries with no user.
                    let Some(user) = m.user.as_ref().map(|u| u.id) else {
                        continue;
                    };
                    self.store.remember_member(guild, user, m);
                    changed = true;
                }
                changed
            }
            LiveEvent::Link(link) => {
                let changed = self.link != link;
                self.link = link;
                changed
            }
            LiveEvent::TokenRejected => {
                self.rejected = true;
                self.link = Link::Down("トークンが無効になりました".to_owned());
                true
            }
        }
    }

    /// Written denormalised: the store keeps channels in one place, so they
    /// are reassembled on the way out.
    fn save_guilds(&self) {
        let Some(db) = &self.db else { return };
        let guilds: Vec<Guild> = self
            .store
            .guilds()
            .map(|g| Guild {
                id: g.id,
                name: g.name.clone(),
                icon_hash: g.icon_hash.clone(),
                unavailable: false,
                channels: self.store.channels_of(g.id).cloned().collect(),
                roles: Vec::new(),
            })
            .collect();
        db.save_guilds(guilds);
        db.save_guild_order(self.store.order());
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

/// Pumps the gateway until it reports a fatal error.
async fn pump(mut gateway: Gateway, tx: Sender<LiveEvent>, waker: Waker) {
    loop {
        let event = gateway.next().await;
        let send = |e: LiveEvent| {
            let _ = tx.send(e);
            waker.wake();
        };

        match event {
            Event::Ready(ready) => send(LiveEvent::Ready(ready)),
            // Anything missed arrives afterwards; nothing to do here.
            Event::Resumed => send(LiveEvent::Link(Link::Up)),
            // One unreadable event must not take the connection down;
            // Discord changes shapes without notice.
            Event::Dispatch { kind, data } => match kind.as_str() {
                "MESSAGE_CREATE" => match serde_json::from_value::<Message>(data) {
                    Ok(m) => send(LiveEvent::Posted(Box::new(m))),
                    Err(e) => tracing::warn!(%e, "could not read MESSAGE_CREATE"),
                },
                // Also fires when an embed resolves later, not just on edits.
                "MESSAGE_UPDATE" => match serde_json::from_value::<Message>(data) {
                    Ok(m) => send(LiveEvent::Edited(Box::new(m))),
                    Err(e) => tracing::warn!(%e, "could not read MESSAGE_UPDATE"),
                },
                // Only the id arrives; no body.
                "MESSAGE_DELETE" => match deleted(&data) {
                    Some(e) => send(e),
                    None => tracing::warn!("could not read MESSAGE_DELETE"),
                },
                // Fills in guilds that were shells in READY; without this an
                // unavailable guild never appears.
                "GUILD_CREATE" | "GUILD_UPDATE" => match serde_json::from_value::<Guild>(data) {
                    Ok(g) => send(LiveEvent::GuildChanged(Box::new(g))),
                    Err(e) => tracing::warn!(%e, "could not read {kind}"),
                },
                // Only arrives once the subscription has been sent.
                "GUILD_MEMBER_LIST_UPDATE" => match gumicord_gateway::member_list::parse(&data) {
                    Some(u) => send(LiveEvent::Members(Box::new(u))),
                    None => tracing::warn!("could not read the member list"),
                },
                // The answer to a by-name request; the only member source for
                // REST messages.
                "GUILD_MEMBERS_CHUNK" => match members_chunk(&data) {
                    Some(e) => send(e),
                    None => tracing::warn!("could not read the member chunk"),
                },
                "TYPING_START" => {
                    if let Some(e) = typing_event(&data) {
                        send(e);
                        // The indicator expires on a timer, so schedule the
                        // redraw that removes it.
                        let waker = waker.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(TYPING_TTL).await;
                            waker.wake();
                        });
                    }
                }
                _ => {}
            },
            Event::Reconnecting { reason, .. } => send(LiveEvent::Link(Link::Reconnecting(reason))),
            Event::Fatal(Fatal::Unauthorized) => {
                send(LiveEvent::TokenRejected);
                return;
            }
            Event::Fatal(Fatal::Rejected { code, reason }) => {
                send(LiveEvent::Link(Link::Down(format!("{reason} ({code})"))));
                return;
            }
        }
    }
}

/// Extracts who is typing where.
///
/// The name can be in three places: `member.nick`, `global_name`, then
/// `username`. DMs carry no `member`; with no name nothing is shown, since
/// "someone is typing" tells the reader nothing they can act on.
/// Extracts members from a chunk. One unreadable entry does not discard the
/// rest.
fn members_chunk(data: &serde_json::Value) -> Option<LiveEvent> {
    let guild = data.get("guild_id")?.as_str()?.parse::<u64>().ok()?;
    let members: Vec<gumicord_model::Member> = data
        .get("members")?
        .as_array()?
        .iter()
        .filter_map(|m| serde_json::from_value(m.clone()).ok())
        .collect();

    tracing::debug!(members = members.len(), "requested members arrived");
    Some(LiveEvent::MemberChunk {
        guild: GuildId::from(guild),
        members,
    })
}

/// A delete carries only ids.
fn deleted(data: &serde_json::Value) -> Option<LiveEvent> {
    let channel = data.get("channel_id")?.as_str()?.parse::<u64>().ok()?;
    let id = data.get("id")?.as_str()?.parse::<u64>().ok()?;
    Some(LiveEvent::Removed {
        channel: ChannelId::from(channel),
        id: MessageId::from(id),
    })
}

fn typing_event(data: &serde_json::Value) -> Option<LiveEvent> {
    let channel = data.get("channel_id")?.as_str()?.parse::<u64>().ok()?;
    let user = data.get("user_id")?.as_str()?.parse::<u64>().ok()?;

    let member = data.get("member")?;
    let name = member
        .get("nick")
        .and_then(|v| v.as_str())
        .or_else(|| member.pointer("/user/global_name").and_then(|v| v.as_str()))
        .or_else(|| member.pointer("/user/username").and_then(|v| v.as_str()))?;

    Some(LiveEvent::Typing {
        channel: ChannelId::from(channel),
        user: UserId::from(user),
        name: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_model::{MessageId, User, UserId};

    /// `Live::new` opens the real cache, so tests must not use it.
    fn live() -> Live {
        Live::without_cache()
    }

    fn ch() -> ChannelId {
        ChannelId::from(10u64)
    }

    /// `start` returns early once it has run, so forgetting must clear that
    /// flag. Otherwise signing in again never reconnects the gateway and the
    /// app sits on a cached screen receiving nothing.
    #[test]
    fn forgetting_everything_allows_reconnecting() {
        let mut live = live();
        live.started = true;
        live.me = Some(UserId::from(1u64));

        live.forget_everything();

        assert!(!live.started, "a later start() would return early");
        assert!(live.subs.is_none());
        assert!(live.me.is_none(), "the previous account is still current");
    }

    fn message(id: u64, body: &str) -> Message {
        Message {
            id: MessageId::from(id),
            channel_id: ch(),
            guild_id: None,
            author: User {
                id: UserId::from(1u64),
                username: "ねんねこ".to_owned(),
                global_name: None,
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: body.to_owned(),
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

    /// Cache first, replaced by REST.
    #[test]
    fn the_cache_shows_first_and_rest_replaces_it() {
        let mut live = live();
        live.apply(LiveEvent::Cached {
            channel: ch(),
            list: vec![message(1, "ふるい")],
        });
        assert_eq!(live.store().messages(ch()).len(), 1);

        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(1, "ふるい"), message(2, "あたらしい")],
        });
        let bodies: Vec<_> = live
            .store()
            .messages(ch())
            .iter()
            .map(|m| &*m.content)
            .collect();
        assert_eq!(bodies, vec!["ふるい", "あたらしい"]);
    }

    /// An older page prepends and asks once to hold the scroll position.
    #[test]
    fn an_older_page_goes_in_front_and_asks_to_hold_the_place() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(5, "いま")],
        });
        assert!(!live.take_prepended(), "nothing was prepended yet");

        assert!(live.apply(LiveEvent::Older {
            channel: ch(),
            list: vec![message(3, "むかし"), message(4, "すこしむかし")],
        }));

        let bodies: Vec<_> = live
            .store()
            .messages(ch())
            .iter()
            .map(|m| &*m.content)
            .collect();
        assert_eq!(bodies, vec!["むかし", "すこしむかし", "いま"]);

        assert!(live.take_prepended(), "should hold the scroll position");
        assert!(!live.take_prepended(), "should only report once");
    }

    /// An empty page stops further requests: sitting at the top would
    /// otherwise keep asking.
    #[test]
    fn reaching_the_beginning_stops_the_asking() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(5, "いま")],
        });
        assert!(!live.is_exhausted(ch()));

        assert!(!live.apply(LiveEvent::Older {
            channel: ch(),
            list: Vec::new(),
        }));
        assert!(live.is_exhausted(ch()));
    }

    /// A page of already-known messages is also the top.
    #[test]
    fn a_page_we_already_had_also_stops_the_asking() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(5, "いま")],
        });

        assert!(!live.apply(LiveEvent::Older {
            channel: ch(),
            list: vec![message(5, "いま")],
        }));
        assert!(live.is_exhausted(ch()));
        assert!(!live.take_prepended(), "nothing was prepended");
    }

    /// Replacing the history re-enables paging.
    #[test]
    fn a_fresh_backlog_lets_us_page_back_again() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(5, "いま")],
        });
        live.apply(LiveEvent::Older {
            channel: ch(),
            list: Vec::new(),
        });
        assert!(live.is_exhausted(ch()));

        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(5, "いま"), message(6, "もっといま")],
        });
        assert!(!live.is_exhausted(ch()));
    }

    /// A late cache result must not overwrite what REST already returned.
    #[test]
    fn a_late_cache_does_not_clobber_fresh_data() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(2, "あたらしい")],
        });
        let changed = live.apply(LiveEvent::Cached {
            channel: ch(),
            list: vec![message(1, "ふるい")],
        });

        assert!(!changed, "redrew with older data");
        assert_eq!(live.store().messages(ch())[0].content, "あたらしい");
    }

    /// A failed fetch keeps the cache: losing history while offline is the
    /// worst outcome.
    #[test]
    fn a_failed_fetch_keeps_the_cached_history() {
        let mut live = live();
        live.apply(LiveEvent::Cached {
            channel: ch(),
            list: vec![message(1, "ふるい")],
        });
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: Vec::new(),
        });

        assert_eq!(live.store().messages(ch()).len(), 1, "the history was lost");
    }

    /// No change means no redraw.
    #[test]
    fn nothing_new_means_no_redraw() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(1, "やあ")],
        });
        assert!(!live.apply(LiveEvent::Posted(Box::new(message(1, "やあ")))));
        assert!(live.apply(LiveEvent::Posted(Box::new(message(2, "ふたつめ")))));
    }

    /// Silent while connected; announcing the normal case buries the
    /// abnormal one.
    #[test]
    fn a_healthy_link_says_nothing() {
        assert!(Link::Up.hint().is_none());
        assert!(Link::Reconnecting("切れた".to_owned()).hint().is_some());
        assert!(Link::Down("駄目".to_owned()).hint().is_some());
    }

    /// Rejection is reported once.
    #[test]
    fn a_rejection_is_reported_once() {
        let mut live = live();
        assert!(!live.take_rejection());

        live.apply(LiveEvent::TokenRejected);
        assert!(live.take_rejection());
        assert!(!live.take_rejection(), "reported twice");
    }

    /// Forgetting leaves nothing behind.
    #[test]
    fn forgetting_leaves_nothing() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(1, "ひみつ")],
        });
        live.requested.insert(ch());

        live.forget_everything();
        assert!(live.store().messages(ch()).is_empty());
        assert!(!live.store().has_messages(ch()));
        assert!(live.is_empty());
        assert!(!live.is_loading(ch()));
    }
}
