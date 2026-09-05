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

use gumicord_gateway::{
    Event, Fatal, Gateway, GuildSettingsEntry, Ready, Subscriptions,
    member_list::{ListOp, MemberEntry, MemberRow},
    status::{Status, from_settings_proto},
};
use gumicord_model::{ChannelId, Guild, GuildId, Message, MessageId, RoleId, Token, UserId};
use gumicord_platform::Waker;
use gumicord_rest::{RestClient, RestError};
use gumicord_store::{Db, GuildRow, Store};

/// Messages fetched when a channel opens. The API allows 100; 50 is a little
/// more than fits on screen, which keeps the round trip short.
const BACKLOG: u8 = 50;

/// How long a typing indicator lives.
///
/// Discord resends `TYPING_START` while someone keeps typing; expiring sooner
/// than it does makes the indicator flicker mid-sentence.
const TYPING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Which member-list rows the subscription asks for: exactly what the list
/// shows.
///
/// The list stops at 100, so asking further used to fetch 200 people nobody
/// saw — and asking wider than an official client does is itself a signal.
/// Scrolling past these needs re-requesting, which does not exist yet
/// (`NEXT.md` 4.).
const MEMBER_ROWS: [gumicord_gateway::MemberRange; 1] = [[0, 99]];

/// The least a hidden member pane still subscribes to. An empty subscription
/// never worked, so hidden narrows instead of unsubscribing.
const MIN_MEMBER_ROWS: [gumicord_gateway::MemberRange; 1] = [[0, 0]];

/// How long a bot roster ask covers. OP 8 answers can be lost without
/// closing the session, and asking every frame instead gets rate-limited;
///
/// this throttles repeats while still healing a dropped ask.
const BOT_ROSTER_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

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
        /// Present for bot OP 8 responses; absent for targeted user requests.
        index: Option<usize>,
        count: Option<usize>,
        /// Who is online, from the chunk's own `presences`. Empty for
        /// targeted user requests, which never carry any.
        presences: Vec<(UserId, Status)>,
    },
    /// Someone started typing.
    Typing {
        channel: ChannelId,
        user: UserId,
        name: String,
    },
    /// A presence changed on the wire.
    ///
    /// Arrives for anyone sharing a guild; only our own is kept, since
    /// everyone else's comes with the member list.
    Presence {
        user: UserId,
        /// `None` when absent or an unknown name; the last known value stays
        /// showing rather than wiping the dot to unknown.
        status: Option<Status>,
    },
    /// Our status changed through the settings sync.
    ///
    /// Switching it on another device reaches every session as settings,
    /// which arrive even where presence events do not.
    SelfStatus(Status),
    Link(Link),
    /// The token was rejected; drop the keychain entry and the cache.
    TokenRejected,
    /// A per-guild notification setting changed on another device.
    NotifSettings(serde_json::Value),
}

/// Real data and the background work that carries it.
pub struct Live {
    tx: Sender<LiveEvent>,
    rx: Receiver<LiveEvent>,
    rt: Option<tokio::runtime::Handle>,
    rest: Option<RestClient>,
    waker: Option<Waker>,
    started: bool,
    task: Option<tokio::task::JoinHandle<()>>,

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
    /// The member rows asked for, per guild.
    ///
    /// Widened on scroll: the gateway re-sends the subscription when the
    /// ranges change and skips an identical one, so announcing again is free.
    member_rows: std::collections::HashMap<GuildId, Vec<gumicord_gateway::MemberRange>>,
    /// Whether the member pane shows. Hidden narrows the subscription to the
    /// minimum instead of unsubscribing, which never worked. The app sets it
    /// every frame; widened rows are remembered regardless.
    members_visible: bool,
    /// A bot's roster, accumulated across OP 8 chunks keyed by user. Chunks
    /// carry no grouping, so the rows are rebuilt from this on every chunk.
    bot_roster: std::collections::HashMap<
        GuildId,
        std::collections::HashMap<UserId, (gumicord_model::Member, Status)>,
    >,
    /// Guilds whose roster was asked for, with when. OP 8 is rate-limited
    /// per session; asking every frame gets the connection closed.
    bot_asked: std::collections::HashMap<GuildId, std::time::Instant>,
    /// Guilds whose chunks all arrived. Only those skip asking again.
    bot_complete: HashSet<GuildId>,
    /// Ourselves, so our own typing indicator is not shown.
    me: Option<UserId>,
    /// Our status. READY starts it; presences about us keep it current, so
    /// changing it on a phone shows up without reconnecting.
    status: Option<Status>,
    /// The account currently backing the open cache.
    current_account_cache: Option<(bool, UserId)>,
}

impl Live {
    /// Starts with an empty state. An account's cache is opened explicitly.
    pub fn new() -> Self {
        Live::without_cache()
    }

    /// Without a cache, for demo mode and tests.
    pub fn without_cache() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Live {
            tx,
            rx,
            rt: None,
            rest: None,
            waker: None,
            started: false,
            task: None,
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
            member_rows: std::collections::HashMap::new(),
            members_visible: true,
            bot_roster: std::collections::HashMap::new(),
            bot_asked: std::collections::HashMap::new(),
            bot_complete: HashSet::new(),
            me: None,
            current_account_cache: None,
        }
    }

    /// Stops the gateway task without touching the cache. Old events still
    /// queued are dropped with the channel, so nothing from the previous
    /// account reaches the new one.
    fn stop_background(&mut self) {
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx = tx;
        self.rx = rx;
    }

    /// Clears everything that belongs to one account. The cache file itself
    /// is handled separately: `disconnect` and `open_cache` close it,
    /// `forget_everything` wipes it but keeps it open.
    fn clear_account_state(&mut self) {
        self.store = Store::new();
        self.requested.clear();
        self.paging.clear();
        self.exhausted.clear();
        self.watching = None;
        self.asked_members.clear();
        self.members.clear();
        self.member_rows.clear();
        self.bot_roster.clear();
        self.bot_asked.clear();
        self.bot_complete.clear();
        self.typing.clear();
        self.last_channel = None;
        self.started = false;
        self.subs = None;
        self.me = None;
        self.status = None;
        self.link = Link::Connecting;
        self.rejected = false;
        self.prepended = false;
        self.rest = None;
    }

    /// Opens the cache for a specific account, replacing any currently open
    /// cache after joining its writer thread.
    pub fn open_cache(&mut self, is_bot: bool, id: UserId) -> bool {
        if self.current_account_cache == Some((is_bot, id)) && self.db.is_some() {
            return false;
        }

        // A different account must never see the previous one's in-memory
        // state, even when it has no cache file yet.
        self.stop_background();
        // Dropping the existing Db stops and joins its writer thread.
        self.close_cache();
        self.clear_account_state();
        self.current_account_cache = Some((is_bot, id));

        match gumicord_store::account_path(is_bot, id).and_then(|p| Db::open(&p)) {
            Ok((db, snapshot)) => {
                tracing::debug!(
                    guilds = snapshot.guilds.len(),
                    messages = snapshot.messages.len(),
                    "loaded account cache"
                );
                self.store.replace_guilds(snapshot.guilds);
                if !snapshot.guild_order.is_empty() {
                    self.store.set_preferred_order(snapshot.guild_order);
                }
                self.store.set_sidebar(snapshot.folders);
                self.store.set_collapsed(snapshot.collapsed);
                self.last_channel = snapshot.last_channel;
                if let Some(ch) = snapshot.last_channel {
                    self.store.set_backlog(ch, snapshot.messages);
                }
                self.db = Some(db);
                true
            }
            Err(e) => {
                tracing::warn!(%e, "no account cache; will fetch from network");
                false
            }
        }
    }

    /// Closes the currently open cache cleanly.
    pub fn close_cache(&mut self) {
        self.db = None;
        self.current_account_cache = None;
    }

    /// Disconnects from the current account without wiping its cache.
    /// Used when switching accounts.
    pub fn disconnect(&mut self) {
        self.stop_background();
        self.close_cache();
        self.clear_account_state();
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
        self.task = Some(rt.spawn(async move { pump(gateway, tx, waker).await }));
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

    /// Whether the member pane shows. The app sets it every frame; hidden
    /// narrows the subscription to the minimum (see [`visible_rows`]).
    /// Flipping re-asks: opening while hidden subscribes to the minimum,
    /// and nothing else re-asks when the pane comes back.
    pub fn set_members_visible(&mut self, visible: bool) {
        if self.members_visible == visible {
            return;
        }
        self.members_visible = visible;
        let (Some(subs), Some(channel)) = (&self.subs, self.watching) else {
            return;
        };
        let Some(guild) = self.store.channel(channel).and_then(|c| c.guild_id) else {
            return;
        };
        // Bots have no member-list subscription; their roster arrives
        // through OP 8 chunks.
        if self.rest.as_ref().is_some_and(RestClient::is_bot) {
            return;
        }
        let rows = if visible {
            self.member_rows
                .entry(guild)
                .or_insert_with(|| MEMBER_ROWS.to_vec())
                .clone()
        } else {
            MIN_MEMBER_ROWS.to_vec()
        };
        subs.watch(guild, channel, &rows);
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
        // indicators arrive for the new channel. Whatever rows were widened
        // on scroll stay asked for: narrowing again would drop them.
        if self.rest.as_ref().is_some_and(RestClient::is_bot) {
            // Bots do not support the user-client member-list
            // subscription. Their list comes through OP 8 chunks, asked
            // once: OP 8 is rate-limited per session.
            self.request_bot_roster(guild);
        } else if let Some(subs) = &self.subs {
            let rows = self
                .member_rows
                .entry(guild)
                .or_insert_with(|| MEMBER_ROWS.to_vec());
            subs.watch(guild, channel, visible_rows(self.members_visible, rows));
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
                    if report_dead_token(&tx, Some(&waker), &e) {
                        return;
                    }
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
                    if report_dead_token(&tx, Some(&waker), &e) {
                        // Nothing else to send; signing out drops the page.
                        return;
                    }
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
        // Bot member lists are requested as OP 8 chunks when a guild opens;
        // targeted member requests would only update the cache, not the
        // visible member-list model.
        if self.rest.as_ref().is_some_and(RestClient::is_bot) {
            return;
        }
        tracing::debug!(users = want.len(), "requesting unknown members");
        for part in want.chunks(CHUNK) {
            subs.request_members(guild, part.to_vec());
        }
    }

    /// Asks for the next page of the member list, once scrolled near its end.
    ///
    /// The subscription is widened rather than a second kind of request sent:
    /// rows arrive as diffs over one list, so a page that does not continue
    /// what is held would be dropped on arrival. The last page answers with
    /// nothing, and the asking then stops by itself.
    pub fn extend_members(&mut self, guild: GuildId) {
        let Some(subs) = &self.subs else { return };
        let Some(channel) = self.watching else { return };
        if self.store.channel(channel).and_then(|c| c.guild_id) != Some(guild) {
            return;
        }
        let held = self.members.get(&guild).map_or(0, |m| m.rows().len());

        let asked = self
            .member_rows
            .entry(guild)
            .or_insert_with(|| MEMBER_ROWS.to_vec());
        let Some(next) = next_member_rows(held, asked) else {
            return;
        };
        tracing::debug!(%guild, from = next[0], "asking for more member rows");
        asked.push(next);
        subs.watch(guild, channel, visible_rows(self.members_visible, asked));
    }

    /// Asks for a bot guild's roster. OP 8 answers in chunks, which
    /// accumulate in `bot_roster`.
    ///
    /// Sent only over a live session: an ask leaving before READY is
    /// dropped by the server, and asking every frame instead gets the
    /// session rate-limited. A recent ask covers repeats; an old one
    /// without a completed roster is sent again, in case its answer was
    /// lost without closing the session.
    fn request_bot_roster(&mut self, guild: GuildId) {
        if self.bot_complete.contains(&guild) {
            return;
        }
        if self.link != Link::Up {
            return;
        }
        let Some(subs) = &self.subs else { return };
        if self
            .bot_asked
            .get(&guild)
            .is_some_and(|at| at.elapsed() < BOT_ROSTER_RETRY)
        {
            return;
        }
        self.bot_asked.insert(guild, std::time::Instant::now());
        tracing::debug!(%guild, "requesting bot member list");
        subs.request_all_members(guild);
    }

    /// Re-asks for the watched bot guild's roster after a reconnect. Chunks
    /// in flight belong to the old connection; the kept roster merges any
    /// repeats by user, so asking again is safe.
    fn refresh_bot_roster(&mut self) {
        if !self.rest.as_ref().is_some_and(RestClient::is_bot) {
            return;
        }
        let Some(channel) = self.watching else { return };
        let Some(guild) = self.store.channel(channel).and_then(|c| c.guild_id) else {
            return;
        };
        if self.bot_complete.contains(&guild) {
            return;
        }
        self.bot_asked.remove(&guild);
        self.request_bot_roster(guild);
    }

    /// Rebuilds one bot guild's rows from its roster: chunks and roles
    /// arrive separately, so either one regroups what is held.
    fn rebuild_bot_rows(&mut self, guild: GuildId) -> bool {
        let (rows, online, total) = bot_rows(
            self.bot_roster.get(&guild),
            self.store.guild_roles(guild),
            guild,
        );
        self.members.entry(guild).or_default().apply(
            gumicord_gateway::member_list::MemberListUpdate {
                guild,
                online,
                total,
                ops: vec![ListOp::Sync { start: 0, rows }],
            },
        )
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
        // MESSAGE_ACK is a user-account endpoint. A bot can read and write
        // channel messages, but it has no user read-state to update. Sending
        // this anyway returns an auth error, which used to be mistaken for a
        // revoked token and signed the newly logged-in bot out immediately.
        if rest.is_bot() {
            return true;
        }
        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), self.waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.ack_message(channel, last).await {
                if report_dead_token(&tx, waker.as_ref(), &e) {
                    return;
                }
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
        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.create_message(channel, &content, reply_to).await {
                if report_dead_token(&tx, Some(&waker), &e) {
                    return;
                }
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
        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.edit_message(channel, message, &content).await {
                if report_dead_token(&tx, Some(&waker), &e) {
                    return;
                }
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
        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.delete_message(channel, message).await {
                if report_dead_token(&tx, Some(&waker), &e) {
                    return;
                }
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
        // The old gateway may still be connected; without aborting it, its
        // events would repopulate the cleared store. Without clearing
        // `started`, `start` returns early after the next login and nothing
        // ever reconnects.
        self.stop_background();
        self.clear_account_state();
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

                // Before guilds; replacing them moves `ready`.
                let raw_notifs = ready.notification_settings();
                tracing::debug!(notifs = raw_notifs.len(), "received notification settings");

                self.store.replace_guilds(ready.guilds);
                for &(guild, msg_notif, muted, ref overrides) in &raw_notifs {
                    let default = to_notif_level(msg_notif, muted);
                    let resolved: Vec<(ChannelId, gumicord_store::NotifLevel)> = overrides
                        .iter()
                        .map(|&(c, n, m)| (c, to_notif_level(n, m)))
                        .collect();
                    self.store.set_notifs(guild, default, &resolved);
                }
                if !order.is_empty() {
                    self.store.set_preferred_order(order);
                }
                self.apply_sidebar(folders);
                self.save_guilds();
                // A fresh session drops OP 8 answers in flight and may
                // carry a different intent set; ask the roster anew. The
                // kept roster merges repeats by user.
                if self.rest.as_ref().is_some_and(RestClient::is_bot) {
                    self.bot_asked.clear();
                    self.bot_complete.clear();
                }
                self.refresh_bot_roster();
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
            LiveEvent::Presence { user, status } => {
                if Some(user) != self.me {
                    return false;
                }
                // Whether our own presence is delivered at all is still an
                // open question; the log answers it from a real session.
                tracing::debug!(status = ?status, "our own presence arrived");
                match status {
                    // An unreadable update keeps the last known value showing
                    // rather than erasing the dot.
                    Some(s) if self.status != Some(s) => {
                        self.status = Some(s);
                        true
                    }
                    _ => false,
                }
            }
            LiveEvent::SelfStatus(s) => {
                if self.status != Some(s) {
                    self.status = Some(s);
                    true
                } else {
                    false
                }
            }
            LiveEvent::GuildChanged(g) => {
                let id = g.id;
                self.store.upsert_guild(*g);
                self.save_guilds();
                // Roles arrive apart from chunks; regroup what is held.
                if self.bot_roster.contains_key(&id) {
                    return self.rebuild_bot_rows(id);
                }
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
            LiveEvent::MemberChunk {
                guild,
                members,
                index,
                count,
                presences,
            } => {
                if let Some(index) = index {
                    // Bot roster chunks: accumulate by user and regroup.
                    // Chunks arrive flat and in any order, so offsets are
                    // never trusted and counts are the roster held.
                    if members.is_empty() {
                        tracing::warn!(%guild, "empty member chunk; the bot may lack the Server Members intent");
                    }
                    let table: std::collections::HashMap<UserId, Status> =
                        presences.into_iter().collect();
                    let roster = self.bot_roster.entry(guild).or_default();
                    for member in members {
                        let Some(user) = member.user.as_ref().map(|u| u.id) else {
                            continue;
                        };
                        let status = table.get(&user).copied().unwrap_or(Status::Offline);
                        roster.insert(user, (member.clone(), status));
                        self.store.remember_member(guild, user, member);
                    }
                    if count.is_some_and(|c| index + 1 >= c) {
                        self.bot_complete.insert(guild);
                    }
                    tracing::debug!(
                        %guild,
                        roster = self.bot_roster.get(&guild).map_or(0, |r| r.len()),
                        index,
                        count,
                        "bot roster merged"
                    );
                    return self.rebuild_bot_rows(guild);
                }
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
                let resumed = matches!(link, Link::Up) && self.link != Link::Up;
                self.link = link;
                // A resume can drop OP 8 answers in flight; ask again
                // unless the roster already completed.
                if resumed {
                    self.refresh_bot_roster();
                }
                changed
            }
            LiveEvent::TokenRejected => {
                self.rejected = true;
                self.link = Link::Down("トークンが無効になりました".to_owned());
                true
            }
            LiveEvent::NotifSettings(data) => {
                match serde_json::from_value::<GuildSettingsEntry>(data) {
                    Ok(entry) => {
                        let default = to_notif_level(entry.message_notifications, entry.muted);
                        let resolved: Vec<(ChannelId, gumicord_store::NotifLevel)> = entry
                            .channel_overrides
                            .iter()
                            .map(|o| {
                                (
                                    o.channel_id,
                                    to_notif_level(o.message_notifications, o.muted),
                                )
                            })
                            .collect();
                        self.store.set_notifs(entry.guild_id, default, &resolved);
                        tracing::debug!(
                            guild = %entry.guild_id,
                            "notification settings updated"
                        );
                    }
                    Err(e) => tracing::warn!(%e, "could not read USER_GUILD_SETTINGS_UPDATE"),
                }
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

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
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
                // A status switch made elsewhere arrives as settings; the
                // payload carries the whole proto, status included.
                "USER_SETTINGS_PROTO_UPDATE" => match self_status_of(&data) {
                    Some(s) => send(LiveEvent::SelfStatus(s)),
                    // Only the shape goes to the log, never the settings
                    // themselves.
                    None => tracing::debug!(
                        keys = ?data.as_object().map(|o| o.keys().collect::<Vec<_>>()),
                        settings = settings_shape(&data).as_deref(),
                        partial = data.get("partial").and_then(|p| p.as_bool()),
                        "no status in USER_SETTINGS_PROTO_UPDATE"
                    ),
                },
                // Fired for anyone sharing a guild; ours moves the user
                // panel's dot, the rest is dropped on arrival.
                "PRESENCE_UPDATE" => match presence_of(&data) {
                    Some((user, status)) => send(LiveEvent::Presence { user, status }),
                    None => tracing::warn!("could not read PRESENCE_UPDATE"),
                },
                // A per-guild notification setting changed on another device.
                "USER_GUILD_SETTINGS_UPDATE" => {
                    send(LiveEvent::NotifSettings(data));
                }
                // What we drop is worth seeing at debug: an event that
                // "never arrives" may arrive under a name dropped here.
                _ => tracing::debug!(%kind, "unhandled dispatch"),
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

/// Reports a dead token through the same flag the gateway raises.
///
/// REST learns of a revoked token before the gateway's next reconnect does;
/// leaving it to the log alone keeps the client failing every request while
/// looking alive. True when the token was the problem and nothing else should
/// be sent.
fn report_dead_token(tx: &Sender<LiveEvent>, waker: Option<&Waker>, e: &RestError) -> bool {
    if !e.is_unauthorized() {
        return false;
    }
    let _ = tx.send(LiveEvent::TokenRejected);
    if let Some(w) = waker {
        w.wake();
    }
    true
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

    // OP 8 answers carry presences apart from members; without them every
    // bot roster would read as offline.
    let presences: Vec<(UserId, Status)> = data
        .get("presences")
        .and_then(|p| p.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|p| presence_of(p).and_then(|(user, status)| status.map(|s| (user, s))))
                .collect()
        })
        .unwrap_or_default();

    tracing::debug!(
        members = members.len(),
        index = data.get("chunk_index").and_then(serde_json::Value::as_u64),
        count = data.get("chunk_count").and_then(serde_json::Value::as_u64),
        "requested members arrived"
    );
    let index = data
        .get("chunk_index")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);
    let count = data
        .get("chunk_count")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);
    Some(LiveEvent::MemberChunk {
        guild: GuildId::from(guild),
        members,
        index,
        count,
        presences,
    })
}

/// Groups a bot roster the way the official list groups: hoisted roles by
/// position, then online, then offline. OP 8 chunks arrive flat, so the
/// headings are built here from the guild's roles and the counts are the
/// roster actually held. Offline members always land under "offline",
/// whatever roles they hold.
fn bot_rows(
    roster: Option<&std::collections::HashMap<UserId, (gumicord_model::Member, Status)>>,
    roles: Option<&[gumicord_model::Role]>,
    guild: GuildId,
) -> (Vec<MemberRow>, u32, u32) {
    struct Person<'a> {
        user: UserId,
        member: &'a gumicord_model::Member,
        status: Status,
    }

    fn name_key(p: &Person) -> String {
        p.member
            .nick
            .clone()
            .or_else(|| {
                p.member
                    .user
                    .as_ref()
                    .map(|u| u.global_name.clone().unwrap_or_else(|| u.username.clone()))
            })
            .unwrap_or_default()
            .to_lowercase()
    }

    let Some(roster) = roster else {
        return (Vec::new(), 0, 0);
    };
    let mut people: Vec<Person> = roster
        .iter()
        .filter_map(|(user, (member, status))| {
            member.user.as_ref()?;
            Some(Person {
                user: *user,
                member,
                status: *status,
            })
        })
        .collect();
    people.sort_by_key(|p| p.user.get());

    // Hoisted roles, highest position first. @everyone shares the guild's
    // id and is never a heading.
    let mut hoisted: Vec<&gumicord_model::Role> = roles
        .unwrap_or(&[])
        .iter()
        .filter(|r| r.hoist && r.id.get() != guild.get())
        .collect();
    hoisted.sort_by(|a, b| {
        b.position
            .cmp(&a.position)
            .then(a.id.get().cmp(&b.id.get()))
    });
    let slot: std::collections::HashMap<RoleId, usize> =
        hoisted.iter().enumerate().map(|(i, r)| (r.id, i)).collect();

    // One bucket per role, then online, then offline.
    let mut buckets: Vec<Vec<Person>> = Vec::new();
    buckets.resize_with(hoisted.len() + 2, Vec::new);
    let (online_at, offline_at) = (hoisted.len(), hoisted.len() + 1);
    let mut online = 0u32;
    for p in people {
        if matches!(p.status, Status::Online | Status::Idle | Status::Dnd) {
            online += 1;
            match p.member.roles.iter().filter_map(|r| slot.get(r)).max() {
                Some(i) => buckets[*i].push(p),
                None => buckets[online_at].push(p),
            }
        } else {
            buckets[offline_at].push(p);
        }
    }
    for bucket in buckets.iter_mut() {
        bucket.sort_by(|a, b| {
            name_key(a)
                .cmp(&name_key(b))
                .then(a.user.get().cmp(&b.user.get()))
        });
    }

    let mut rows = Vec::new();
    let push_bucket = |id: String, people: &mut Vec<Person>, rows: &mut Vec<MemberRow>| {
        if people.is_empty() {
            return;
        }
        rows.push(MemberRow::Group {
            id,
            count: people.len() as u32,
        });
        rows.extend(people.drain(..).map(|p| {
            MemberRow::Member(Box::new(MemberEntry {
                member: p.member.clone(),
                status: p.status,
            }))
        }));
    };
    for (i, role) in hoisted.iter().enumerate() {
        push_bucket(role.id.get().to_string(), &mut buckets[i], &mut rows);
    }
    push_bucket("online".to_owned(), &mut buckets[online_at], &mut rows);
    push_bucket("offline".to_owned(), &mut buckets[offline_at], &mut rows);

    let total = rows
        .iter()
        .filter(|r| matches!(r, MemberRow::Member(_)))
        .count() as u32;
    (rows, online, total)
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

/// Extracts whose presence changed and what it became.
///
/// `status` may be absent when nothing about it changed, and names this
/// client does not know are dropped by [`Status::from_wire`] rather than
/// guessed at.
fn presence_of(data: &serde_json::Value) -> Option<(UserId, Option<Status>)> {
    let id = data.pointer("/user/id")?.as_str()?.parse::<u64>().ok()?;
    let status = data
        .get("status")
        .and_then(|s| s.as_str())
        .and_then(Status::from_wire);
    Some((UserId::from(id), status))
}

/// Extracts our status out of the settings sync.
///
/// The same proto READY carries, so the same reader answers. The proto
/// arrives bare or wrapped with a type name; an unreadable one keeps the
/// last known value showing.
fn self_status_of(data: &serde_json::Value) -> Option<Status> {
    let settings = data.get("settings")?;
    let proto = settings
        .as_str()
        .or_else(|| settings.get("proto").and_then(|p| p.as_str()))?;
    from_settings_proto(proto)
}

/// Describes the `settings` value's shape, for the log. The contents are the
/// user's settings and never go out.
fn settings_shape(data: &serde_json::Value) -> Option<String> {
    Some(match data.get("settings")? {
        serde_json::Value::String(s) => format!("string({})", s.len()),
        serde_json::Value::Object(o) => {
            format!("object keys={:?}", o.keys().collect::<Vec<_>>())
        }
        serde_json::Value::Array(a) => format!("array({})", a.len()),
        serde_json::Value::Null => "null".to_owned(),
        other => format!("{other:?}"),
    })
}

/// The next member range to ask for: the one continuing what is held.
///
/// Wire indices count headings too, so the next page starts at however many
/// rows actually arrived, not at a round hundred.
///
/// - Nothing held means nothing subscribed; widening comes first from
///   opening a channel.
/// - The last ask reaching past what is held is still on its way — one
///   scroll gesture fires many events, and each must not send another
///   request. When nothing more exists, nothing arrives and it stops here
///   for good.
/// - Discord takes three ranges at most and silently ignores the rest.
fn next_member_rows(
    held: usize,
    asked: &[gumicord_gateway::MemberRange],
) -> Option<gumicord_gateway::MemberRange> {
    /// Rows per page.
    const PAGE: u32 = 100;
    /// Discord silently ignores more.
    const MAX_RANGES: usize = 3;

    let last = *asked.last()?;
    let held = u32::try_from(held).ok()?;
    if last[1] >= held {
        return None;
    }
    if asked.len() >= MAX_RANGES {
        return None;
    }
    let start = held;
    Some([start, start + PAGE - 1])
}

/// What to actually send: the remembered rows while the member pane shows,
/// the bare minimum while it does not. An empty subscription never worked,
/// so hidden never means zero.
fn visible_rows(
    visible: bool,
    remembered: &[gumicord_gateway::MemberRange],
) -> &[gumicord_gateway::MemberRange] {
    if visible {
        remembered
    } else {
        &MIN_MEMBER_ROWS
    }
}

/// Maps Discord's notification level integer to the store enum.
///
/// `message_notifications`: 0 = all, 1 = only @mentions, 2 = nothing.
/// The `muted` flag overrides to nothing.
fn to_notif_level(raw: u8, muted: bool) -> gumicord_store::NotifLevel {
    use gumicord_store::NotifLevel;
    if muted || raw >= 2 {
        NotifLevel::Nothing
    } else if raw == 1 {
        NotifLevel::Mentions
    } else {
        NotifLevel::All
    }
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

    #[test]
    fn disconnect_clears_per_account_state() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(1, "old account")],
        });
        live.requested.insert(ch());
        live.member_rows.insert(GuildId::from(5u64), vec![[0, 99]]);
        live.bot_roster
            .insert(GuildId::from(5u64), std::collections::HashMap::new());
        live.bot_asked
            .insert(GuildId::from(5u64), std::time::Instant::now());
        live.bot_complete.insert(GuildId::from(5u64));
        live.status = Some(Status::Online);
        live.me = Some(UserId::from(1u64));
        live.last_channel = Some(ch());

        live.disconnect();

        assert!(live.is_empty());
        assert!(live.last_channel().is_none());
        assert!(live.requested.is_empty());
        assert!(live.member_rows.is_empty());
        assert!(live.bot_roster.is_empty());
        assert!(live.bot_asked.is_empty());
        assert!(live.bot_complete.is_empty());
        assert!(live.status.is_none());
        assert!(live.me.is_none());
        assert!(live.rest.is_none());
    }

    #[test]
    fn disconnect_drops_events_queued_before_the_switch() {
        let mut live = live();
        let tx = live.tx.clone();
        tx.send(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(1, "old account")],
        })
        .unwrap();

        live.disconnect();

        assert!(!live.poll(), "old account events must not leak");
        assert!(live.is_empty());
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

    /// A change made on another device moves our own dot. Other people's
    /// presences belong to the member list, an unreadable one keeps the last
    /// known value showing, and the same value twice is not news.
    #[test]
    fn only_our_own_presence_moves_the_dot() {
        let mut live = live();
        live.me = Some(UserId::from(1u64));
        live.status = Some(Status::Online);

        assert!(live.apply(LiveEvent::Presence {
            user: UserId::from(1u64),
            status: Some(Status::Dnd),
        }));
        assert_eq!(live.status, Some(Status::Dnd));

        assert!(!live.apply(LiveEvent::Presence {
            user: UserId::from(9u64),
            status: Some(Status::Idle),
        }));
        assert_eq!(live.status, Some(Status::Dnd));

        assert!(!live.apply(LiveEvent::Presence {
            user: UserId::from(1u64),
            status: None,
        }));
        assert_eq!(live.status, Some(Status::Dnd));

        assert!(!live.apply(LiveEvent::Presence {
            user: UserId::from(1u64),
            status: Some(Status::Dnd),
        }));
    }

    /// The shape arrives with the id nested under `user`; a name this client
    /// does not know reads as no change rather than a guess.
    #[test]
    fn presence_extraction_reads_the_nested_id() {
        let (user, status) = presence_of(&serde_json::json!({
            "user": { "id": "42" },
            "status": "dnd",
            "activities": [],
        }))
        .expect("読める形");
        assert_eq!(user, UserId::from(42u64));
        assert_eq!(status, Some(Status::Dnd));

        let (_, status) = presence_of(&serde_json::json!({
            "user": { "id": "42" },
            "status": "streaming",
        }))
        .expect("id だけでも読める");
        assert_eq!(status, None);

        // Without the user there is nothing to hang it on.
        assert!(presence_of(&serde_json::json!({ "status": "online" })).is_none());
    }

    /// The settings sync speaks about us by definition, so its status moves
    /// the dot without the membership check a wire presence needs; the same
    /// value twice is not news.
    #[test]
    fn the_settings_sync_moves_our_own_dot() {
        let mut live = live();
        live.me = Some(UserId::from(1u64));

        assert!(live.apply(LiveEvent::SelfStatus(Status::Dnd)));
        assert_eq!(live.status, Some(Status::Dnd));
        assert!(!live.apply(LiveEvent::SelfStatus(Status::Dnd)));
    }

    use base64::Engine as _;

    /// Builds the settings proto by hand, the way the `status` tests do.
    fn block(field: u64, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, field << 3 | 2);
        put_varint(&mut out, body.len() as u64);
        out.extend_from_slice(body);
        out
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    /// The sync carries the whole proto under `settings`; one without a
    /// readable status leaves the last value showing rather than a guess.
    #[test]
    fn the_status_is_read_out_of_the_settings_sync() {
        let value = block(1, b"dnd");
        let status_settings = block(1, &value);
        let root = block(5, &status_settings);
        let proto = base64::engine::general_purpose::STANDARD.encode(root);

        let status = self_status_of(&serde_json::json!({ "settings": proto })).expect("読める形");
        assert_eq!(status, Status::Dnd);

        // The wire wraps it with a type name.
        let wrapped = self_status_of(&serde_json::json!({
            "partial": false,
            "settings": { "proto": proto, "type": "user_settings" },
        }))
        .expect("包まれていても読める");
        assert_eq!(wrapped, Status::Dnd);

        assert_eq!(self_status_of(&serde_json::json!({ "other": 1 })), None);
        assert_eq!(
            self_status_of(&serde_json::json!({ "settings": "base64 ではない" })),
            None
        );
    }

    /// Builds a member list holding `n` rows, the way a SYNC delivers them.
    fn held_list(n: usize) -> gumicord_gateway::MemberList {
        let items: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "member": {
                        "user": { "id": i.to_string(), "username": format!("う{i}") },
                        "roles": [],
                    }
                })
            })
            .collect();
        let raw = serde_json::json!({
            "guild_id": "7",
            "member_count": n,
            "online_count": 1,
            "ops": [{ "op": "SYNC", "range": [0, 99], "items": items }],
        });
        let mut list = gumicord_gateway::MemberList::default();
        assert!(list.apply(gumicord_gateway::member_list::parse(&raw).expect("読める")));
        list
    }

    /// The next page starts at however many rows actually arrived: wire
    /// indices count headings too, so a round hundred would join across a
    /// gap.
    #[test]
    fn the_next_member_range_continues_what_is_held() {
        let first = [0u32, 99];
        assert_eq!(next_member_rows(100, &[first]), Some([100, 199]));

        // Nothing asked yet: widening comes from opening a channel.
        assert_eq!(next_member_rows(50, &[]), None);

        // A short page means the range held everything there is; asking
        // again would only wait forever.
        assert_eq!(next_member_rows(98, &[first]), None);

        // Still on its way, or already at what Discord takes.
        assert_eq!(next_member_rows(150, &[first, [100, 199]]), None);
        let three = [first, [100, 199], [200, 299]];
        assert_eq!(next_member_rows(250, &three), None);
    }

    /// Hidden narrows to one row; shown restores the remembered rows. Empty
    /// never goes out: a subscription without any was never seen to work.
    #[test]
    fn hiding_the_member_pane_narrows_instead_of_unsubscribing() {
        let widened = [[0u32, 99], [100, 199]];
        assert_eq!(visible_rows(true, &widened), &widened);
        assert_eq!(visible_rows(false, &widened), &[[0, 0]]);
        assert_eq!(visible_rows(false, &[]), &[[0, 0]], "never zero");
    }

    /// Widened rows survive hiding, so showing again restores them without
    /// asking from scratch.
    #[test]
    fn remembered_rows_survive_hiding() {
        let mut live = live();
        assert!(
            live.members_visible,
            "hidden by default would change current behaviour"
        );
        live.member_rows
            .insert(GuildId::from(5u64), vec![[0, 99], [100, 199]]);

        live.set_members_visible(false);
        assert_eq!(
            visible_rows(
                live.members_visible,
                &live.member_rows[&GuildId::from(5u64)]
            ),
            &[[0, 0]]
        );
        live.set_members_visible(true);
        assert_eq!(
            visible_rows(
                live.members_visible,
                &live.member_rows[&GuildId::from(5u64)]
            ),
            &[[0, 99], [100, 199]]
        );
    }

    /// Flipping the member pane re-asks: narrowing sends the minimum,
    /// showing again restores the remembered rows. Opening while hidden
    /// subscribes to the minimum, and nothing else re-asks on the way back.
    #[test]
    fn flipping_the_member_pane_reasks() {
        use gumicord_gateway::Request;
        use gumicord_model::{Channel, ChannelKind};

        let guild = GuildId::from(7u64);
        let mut live = live();
        let (mut gateway, subs) = Gateway::new(Token::new("t"));
        live.subs = Some(subs);
        live.watching = Some(ch());
        live.store.upsert_guild(Guild {
            id: guild,
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![Channel {
                id: ch(),
                kind: ChannelKind::GuildText,
                name: Some("一般".to_owned()),
                guild_id: Some(guild),
                parent_id: None,
                position: 0,
                topic: None,
                nsfw: false,
                recipients: Vec::new(),
                last_message_id: None,
            }],
            roles: Vec::new(),
        });
        live.member_rows.insert(guild, vec![[0, 99]]);
        let watching = |requests: Vec<Request>| {
            requests.into_iter().find_map(|r| match r {
                Request::Watch(g, c, rows) if g == guild && c == ch() => Some(rows),
                _ => None,
            })
        };

        live.set_members_visible(false);
        assert_eq!(
            watching(gateway.take_requests()),
            Some(vec![[0u32, 0]]),
            "hiding did not narrow the ask"
        );
        live.set_members_visible(true);
        assert_eq!(
            watching(gateway.take_requests()),
            Some(vec![[0u32, 99]]),
            "showing did not restore the ask"
        );
        // Steady state sends nothing, every frame sets this.
        live.set_members_visible(true);
        assert!(
            gateway.take_requests().is_empty(),
            "re-asked without a flip"
        );
    }

    /// One scroll widens the ask once and then holds until the page lands.
    #[test]
    fn scrolling_the_member_list_widens_the_ask_once() {
        use gumicord_model::{Channel, ChannelKind};

        let guild = GuildId::from(7u64);
        let mut live = live();
        let (_gateway, subs) = Gateway::new(Token::new("t"));
        live.subs = Some(subs);
        live.watching = Some(ch());
        live.store.upsert_guild(Guild {
            id: guild,
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![Channel {
                id: ch(),
                kind: ChannelKind::GuildText,
                name: Some("一般".to_owned()),
                guild_id: Some(guild),
                parent_id: None,
                position: 0,
                topic: None,
                nsfw: false,
                recipients: Vec::new(),
                last_message_id: None,
            }],
            roles: Vec::new(),
        });
        live.members.insert(guild, held_list(100));

        live.extend_members(guild);
        assert_eq!(
            live.member_rows[&guild],
            vec![[0u32, 99], [100, 199]],
            "the ask continues at the rows actually held"
        );

        // Every scroll event while the page is away asks again; only the
        // first may reach the wire.
        live.extend_members(guild);
        assert_eq!(live.member_rows[&guild].len(), 2);
    }

    /// Another guild's scroll does not widen this one's ask.
    #[test]
    fn another_guild_s_scroll_changes_nothing_here() {
        let mut live = live();
        let (_gateway, subs) = Gateway::new(Token::new("t"));
        live.subs = Some(subs);
        live.watching = Some(ch());
        live.members.insert(GuildId::from(7u64), held_list(100));

        live.extend_members(GuildId::from(9u64));
        assert!(live.member_rows.is_empty(), "asked for someone else");
    }

    /// A bot roster groups by hoisted role with honest counts, and chunks
    /// arriving out of order still all land.
    #[test]
    fn bot_chunks_group_by_role_with_honest_counts() {
        use gumicord_model::{Member, Role, RoleId, User};

        let guild = GuildId::from(7u64);
        let admin = RoleId::from(11u64);
        let modo = RoleId::from(12u64);
        let mut live = live();
        live.store.upsert_guild(Guild {
            id: guild,
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: vec![
                Role {
                    id: admin,
                    name: "管理".to_owned(),
                    position: 2,
                    hoist: true,
                    color: None,
                },
                Role {
                    id: modo,
                    name: "モデ".to_owned(),
                    position: 1,
                    hoist: true,
                    color: None,
                },
            ],
        });

        fn member(id: u64, name: &str, roles: Vec<RoleId>) -> Member {
            Member {
                nick: None,
                avatar_hash: None,
                roles,
                joined_at: None,
                user: Some(User {
                    id: UserId::from(id),
                    username: name.to_owned(),
                    global_name: None,
                    discriminator: "0".to_owned(),
                    avatar_hash: None,
                    bot: false,
                }),
            }
        }

        // Second chunk first: order must not matter.
        live.apply_for_test(LiveEvent::MemberChunk {
            guild,
            members: vec![member(3, "cara", vec![]), member(4, "dan", vec![admin])],
            index: Some(1),
            count: Some(2),
            presences: vec![(UserId::from(3), Status::Idle)],
        });
        live.apply_for_test(LiveEvent::MemberChunk {
            guild,
            members: vec![
                member(1, "alice", vec![admin]),
                member(2, "bob", vec![modo]),
            ],
            index: Some(0),
            count: Some(2),
            presences: vec![(UserId::from(1), Status::Online)],
        });

        let list = live.members(guild).expect("roster held");
        let shown: Vec<String> = list
            .rows()
            .iter()
            .map(|r| match r {
                MemberRow::Member(m) => m.member.user.as_ref().expect("居る").username.clone(),
                MemberRow::Group { id, count } => format!("[{id} {count}]"),
            })
            .collect();
        // dan is offline, so he lands under offline despite his role, and
        // the mod heading stays empty and is skipped.
        assert_eq!(
            shown,
            vec![
                "[11 1]",
                "alice",
                "[online 1]",
                "cara",
                "[offline 2]",
                "bob",
                "dan"
            ]
        );
        assert_eq!(list.online(), 2);
        assert_eq!(list.total(), 4);
        assert!(live.bot_complete.contains(&guild));
    }

    /// Roles arriving after the chunks regroup what is held.
    #[test]
    fn bot_roles_arriving_late_regroup_the_roster() {
        use gumicord_model::{Member, Role, RoleId, User};

        let guild = GuildId::from(7u64);
        let admin = RoleId::from(11u64);
        let mut live = live();
        live.store.upsert_guild(Guild {
            id: guild,
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: Vec::new(),
        });
        live.apply_for_test(LiveEvent::MemberChunk {
            guild,
            members: vec![Member {
                nick: None,
                avatar_hash: None,
                roles: vec![admin],
                joined_at: None,
                user: Some(User {
                    id: UserId::from(1u64),
                    username: "alice".to_owned(),
                    global_name: None,
                    discriminator: "0".to_owned(),
                    avatar_hash: None,
                    bot: false,
                }),
            }],
            index: Some(0),
            count: Some(1),
            presences: vec![(UserId::from(1u64), Status::Online)],
        });

        // No roles known yet: everyone lands under online.
        let shown = |live: &Live| {
            live.members(guild)
                .expect("roster held")
                .rows()
                .iter()
                .map(|r| match r {
                    MemberRow::Member(m) => m.member.user.as_ref().expect("居る").username.clone(),
                    MemberRow::Group { id, count } => format!("[{id} {count}]"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&live), vec!["[online 1]", "alice"]);

        live.apply_for_test(LiveEvent::GuildChanged(Box::new(Guild {
            id: guild,
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: vec![Role {
                id: admin,
                name: "管理".to_owned(),
                position: 2,
                hoist: true,
                color: None,
            }],
        })));
        assert_eq!(shown(&live), vec!["[11 1]", "alice"]);
    }

    /// A bot roster is asked once a live session exists; asking every
    /// frame would rate-limit the session before a large guild finishes,
    /// and asking before READY is dropped by the server. A resume re-asks
    /// unless the roster already completed.
    #[test]
    fn bot_roster_is_asked_once_until_reconnect() {
        use gumicord_model::{Channel, ChannelKind};

        let guild = GuildId::from(7u64);
        let mut live = live();
        let (_gateway, subs) = Gateway::new(Token::new("t"));
        live.subs = Some(subs);
        live.rest = Some(RestClient::anonymous().unwrap().with_token(Token::bot("x")));
        live.store.upsert_guild(Guild {
            id: guild,
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![Channel {
                id: ch(),
                kind: ChannelKind::GuildText,
                name: Some("一般".to_owned()),
                guild_id: Some(guild),
                parent_id: None,
                position: 0,
                topic: None,
                nsfw: false,
                recipients: Vec::new(),
                last_message_id: None,
            }],
            roles: Vec::new(),
        });

        // Before the session is up nothing leaves, not even once.
        live.open_channel(guild, ch());
        assert!(
            !live.bot_asked.contains_key(&guild),
            "asking before READY is dropped by the server"
        );

        live.link = Link::Up;
        live.open_channel(guild, ch());
        let first = live.bot_asked.get(&guild).copied().expect("asked once");
        live.open_channel(guild, ch());
        assert_eq!(
            live.bot_asked.get(&guild),
            Some(&first),
            "asked once, not once per frame"
        );

        // A stale ask without a completed roster goes out again, in case
        // its answer was lost without closing the session.
        live.bot_asked.insert(guild, first - BOT_ROSTER_RETRY);
        live.open_channel(guild, ch());
        assert!(
            live.bot_asked.get(&guild).is_some_and(|at| *at > first),
            "stale ask is retried"
        );

        // Reconnecting alone asks nothing; the resume after it does.
        live.apply_for_test(LiveEvent::Link(Link::Reconnecting("x".to_owned())));
        assert!(live.bot_asked.contains_key(&guild));
        live.apply_for_test(LiveEvent::Link(Link::Up));
        assert!(live.bot_asked.contains_key(&guild), "resume re-asks");

        // Once complete, a resume asks nothing more.
        live.bot_complete.insert(guild);
        live.bot_asked.clear();
        live.apply_for_test(LiveEvent::Link(Link::Up));
        assert!(!live.bot_asked.contains_key(&guild));
    }
}
