//! The Discord gateway connection.
//!
//! Callers only drive [`Gateway::next`]; disconnects and reconnects happen
//! inside. Nothing but `Fatal` ends the loop — while the network is down it
//! keeps retrying with growing backoff.
//!
//! ```text
//!   ├─ open the WebSocket ───────▶│
//!   │◀────────────── op=10 Hello ─┤   carries heartbeat_interval
//!   ├─ op=2 Identify ────────────▶│   op=6 when resuming
//!   │◀───────── op=0 t=READY ─────┤
//!   ├─ op=1 Heartbeat ───────────▶│   first one after interval * jitter
//!   │◀────────── op=11 ACK ───────┤
//! ```
//!
//! Resuming connects to `resume_gateway_url`, a region-specific host from
//! READY. Going back to the original host lands on a different server and the
//! resume can fail.
//!
//! A missing heartbeat ACK means the connection is dead. TCP does not notice a
//! severed network for a long time, so this is the only detection there is:
//! drop it and resume rather than waiting.
//!
//! The identify claims to be the official desktop client. That is a
//! deliberate reversal of an earlier decision, taken after the previous
//! honest claim got the account flagged; see `spec/09-discord-protocol.md`
//! and [`gumicord_model::identity`]. It violates Discord's terms of service
//! and does not make anything safe.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use gumicord_model::identity::Identity;
use gumicord_model::{ChannelId, CurrentUser, Guild, GuildId, MessageId, Token, UserId};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::zstd_stream::ZstdStream;

/// Where the first connection goes.
const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json&compress=zstd-stream";
/// Query appended when resuming; `resume_gateway_url` arrives without one.
const QUERY: &str = "?v=10&encoding=json&compress=zstd-stream";

/// Reconnects never stop; the backoff stops growing here.
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_MIN: Duration = Duration::from_secs(1);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("接続できない: {0}")]
    Connect(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("解凍できない: {0}")]
    Decompress(#[from] std::io::Error),
    #[error("読めない応答: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("Hello が来ないまま接続が終わった")]
    NoHello,
    #[error("接続が閉じられた (コード {0})")]
    Closed(u16),
}

/// Why the connection is over. The caller decides what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fatal {
    /// The token was rejected; discard it and return to login.
    Unauthorized,
    /// We sent something wrong; retrying changes nothing.
    Rejected { code: u16, reason: String },
}

/// What the gateway delivers.
#[derive(Debug, Clone)]
pub enum Event {
    /// Connected, with the initial state.
    Ready(Box<Ready>),
    /// The gap was filled; no `Ready` follows.
    Resumed,
    /// An event with no type of its own yet, passed through untouched: which
    /// ones matter is the store's decision.
    Dispatch {
        kind: String,
        data: serde_json::Value,
    },
    /// Dropped; retrying continues internally. For display only.
    Reconnecting { reason: String, wait: Duration },
    /// Given up.
    Fatal(Fatal),
}

/// The initial state from READY.
#[derive(Debug, Clone, Deserialize)]
pub struct Ready {
    pub user: CurrentUser,
    pub session_id: String,
    /// Where to resume; a region-specific host.
    #[serde(default)]
    pub resume_gateway_url: Option<String>,
    /// Unavailable guilds arrive as an id and nothing else.
    ///
    /// Unreadable entries are skipped: one shell once made the whole payload
    /// unreadable and the gateway reconnected forever.
    #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
    pub guilds: Vec<Guild>,
    /// User settings, as base64 protobuf. Carries the guild order.
    #[serde(default)]
    pub user_settings_proto: Option<String>,
    /// How far each channel is read. Arrives either as a bare array or
    /// wrapped in `entries`.
    #[serde(default)]
    pub read_state: Option<ReadStates>,
    /// Per-guild and per-channel notification overrides.
    #[serde(default)]
    pub user_guild_settings: Option<UserGuildSettings>,
}

/// Accepts both shapes of `read_state`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ReadStates {
    /// The newer shape.
    Wrapped {
        #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
        entries: Vec<ReadState>,
    },
    /// The older shape: a bare array.
    Flat(#[serde(deserialize_with = "gumicord_model::de::lenient_vec")] Vec<ReadState>),
}

impl ReadStates {
    pub fn entries(&self) -> &[ReadState] {
        match self {
            ReadStates::Wrapped { entries } => entries,
            ReadStates::Flat(v) => v,
        }
    }
}

/// One channel's read marker.
///
/// The same array carries markers for guild events and achievements; anything
/// whose id resolves to no channel is dropped, so no filtering is needed.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadState {
    /// The channel.
    pub id: ChannelId,
    /// Read up to here; anything newer is unread.
    #[serde(default)]
    pub last_message_id: Option<MessageId>,
    /// Unread mentions, counted by the server.
    #[serde(default)]
    pub mention_count: u32,
}

/// Per-guild and per-channel notification overrides from READY.
///
/// Discord sends this as `{ "entries": [...] }`.
#[derive(Debug, Clone, Deserialize)]
pub struct UserGuildSettings {
    #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
    pub entries: Vec<GuildSettingsEntry>,
}

/// One guild's notification settings, including per-channel overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct GuildSettingsEntry {
    pub guild_id: GuildId,
    /// 0 = all messages, 1 = only @mentions, 2 = nothing.
    #[serde(default)]
    pub message_notifications: u8,
    /// Whether the guild is fully muted.
    #[serde(default)]
    pub muted: bool,
    #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
    pub channel_overrides: Vec<ChannelOverride>,
}

/// A per-channel notification override within a guild.
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelOverride {
    pub channel_id: ChannelId,
    /// 0 = all messages, 1 = only @mentions, 2 = nothing.
    #[serde(default)]
    pub message_notifications: u8,
    /// Whether this channel is muted.
    #[serde(default)]
    pub muted: bool,
}

impl Ready {
    /// The order the user arranged in Discord.
    ///
    /// Never sorted by name: any other order stops being their guild list.
    /// Empty if it cannot be read, and the caller falls back to READY's order.
    pub fn guild_order(&self) -> Vec<GuildId> {
        let Some(proto) = &self.user_settings_proto else {
            return Vec::new();
        };
        crate::guild_order::from_settings_proto(proto, &self.known_guilds())
    }

    /// Sidebar folders; lone guilds appear in the same list.
    pub fn guild_folders(&self) -> Vec<crate::guild_order::Folder> {
        let Some(proto) = &self.user_settings_proto else {
            return Vec::new();
        };
        crate::guild_order::folders_from_settings_proto(proto, &self.known_guilds())
    }

    /// Our status, or `None` when unreadable. Being connected is not
    /// "online": it would lie about anyone set to do not disturb.
    pub fn status(&self) -> Option<crate::status::Status> {
        crate::status::from_settings_proto(self.user_settings_proto.as_deref()?)
    }

    /// Resolved notification levels per guild:
    /// (guild, default_notifications, default_muted, overrides).
    pub fn notification_settings(
        &self,
    ) -> Vec<(GuildId, u8, bool, Vec<(ChannelId, u8, bool)>)> {
        let Some(settings) = &self.user_guild_settings else {
            return Vec::new();
        };
        settings
            .entries
            .iter()
            .map(|entry| {
                let overrides = entry
                    .channel_overrides
                    .iter()
                    .map(|o| (o.channel_id, o.message_notifications, o.muted))
                    .collect();
                (
                    entry.guild_id,
                    entry.message_notifications,
                    entry.muted,
                    overrides,
                )
            })
            .collect()
    }

    fn known_guilds(&self) -> std::collections::HashSet<u64> {
        self.guilds.iter().map(|g| g.id.get()).collect()
    }
}

/// Keeps the connection alive.
pub struct Gateway {
    token: Token,
    conn: Option<Connection>,
    /// What resuming needs; survives a disconnect.
    session: Option<SessionInfo>,
    backoff: Duration,
    /// A pending `Reconnecting`, emitted at the top of `next`.
    pending_notice: Option<Event>,
    /// What is being watched, member rows included. Resent on every
    /// reconnect: subscriptions belong to a connection.
    wanted: std::collections::HashMap<GuildId, (ChannelId, Vec<MemberRange>)>,
    requests: tokio::sync::mpsc::UnboundedReceiver<Request>,
}

/// A request to send to the gateway.
#[derive(Debug, Clone)]
enum Request {
    /// We are watching this channel, and which rows of its member list.
    Watch(GuildId, ChannelId, Vec<MemberRange>),
    /// We need these members in this guild.
    Members(GuildId, Vec<UserId>),
}

/// Tells the gateway what is being watched.
///
/// A user token receives no `MESSAGE_CREATE` until it subscribes. Guilds
/// arrive marked lazy, and the official client subscribes only to the guild
/// on screen — sending everything to someone in hundreds of servers would
/// waste both ends. Without this, nothing arrives at all.
#[derive(Clone, Debug)]
pub struct Subscriptions {
    tx: tokio::sync::mpsc::UnboundedSender<Request>,
}

impl Subscriptions {
    /// Announces the watched channel and which rows of the member list to
    /// receive. Safe to call repeatedly.
    ///
    /// The ranges are compared too: a change re-sends the subscription, so
    /// widening or narrowing the list reaches the wire. An empty slice asks
    /// for no rows at all, and a subscription without any was never seen to
    /// work — the caller supplies at least one.
    pub fn watch(&self, guild: GuildId, channel: ChannelId, ranges: &[MemberRange]) {
        let _ = self
            .tx
            .send(Request::Watch(guild, channel, ranges.to_vec()));
    }

    /// Requests members by id.
    ///
    /// REST messages carry no `member`, so nicknames, per-guild avatars and
    /// role colours only appear once this is asked for. The official client
    /// behaves the same way.
    ///
    /// At most 100 per call, and never the same person twice: too many of
    /// these gets the connection closed.
    pub fn request_members(&self, guild: GuildId, users: Vec<UserId>) {
        if users.is_empty() {
            return;
        }
        let _ = self.tx.send(Request::Members(guild, users));
    }
}

/// The URL to connect to; a region-specific host when resuming.
///
/// The path separator must survive. `resume_gateway_url` arrives without a
/// path, and appending the query directly produces a URL with an empty path,
/// which sends a request line of
///
/// ```text
///   GET ?v=10&encoding=json&compress=zstd-stream HTTP/1.1
/// ```
///
/// Discord answers 400, and since the session is still held it retries the
/// same URL forever and never recovers.
fn connect_url(resume_host: Option<&str>) -> String {
    match resume_host {
        Some(host) => format!("{}/{QUERY}", host.trim_end_matches('/')),
        None => GATEWAY.to_owned(),
    }
}

/// Announces what is being watched.
///
/// The ranges say which rows of the member list are wanted; only those rows
/// arrive. The subscription does not work without them, so the caller always
/// supplies at least one.
fn subscribe(guild: GuildId, channel: ChannelId, ranges: &[MemberRange]) -> serde_json::Value {
    json!({
        "op": OP_GUILD_SUBSCRIBE,
        "d": {
            "guild_id": guild.get().to_string(),
            "typing": true,
            "threads": false,
            "activities": true,
            "channels": { channel.get().to_string(): ranges },
        },
    })
}

/// One row span of the member list: start and end, both inclusive.
pub type MemberRange = [u32; 2];

/// Requests members by id, 100 at a time.
///
/// No presences: the member list already carries who is online.
fn request_members(guild: GuildId, users: &[UserId]) -> serde_json::Value {
    json!({
        "op": OP_REQUEST_MEMBERS,
        "d": {
            "guild_id": guild.get().to_string(),
            "user_ids": users.iter().map(|u| u.get().to_string()).collect::<Vec<_>>(),
            "presences": false,
        },
    })
}

impl core::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never prints the token.
        f.debug_struct("Gateway")
            .field("connected", &self.conn.is_some())
            .field("resumable", &self.session.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct SessionInfo {
    id: String,
    url: String,
    seq: Option<u64>,
}

impl Gateway {
    /// Builds the connection and the handle used to subscribe.
    pub fn new(token: Token) -> (Self, Subscriptions) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Gateway {
                token,
                conn: None,
                session: None,
                backoff: BACKOFF_MIN,
                pending_notice: None,
                wanted: std::collections::HashMap::new(),
                requests: rx,
            },
            Subscriptions { tx },
        )
    }

    /// Resends every subscription. They belong to a connection and survive
    /// neither identify nor resume.
    async fn resend_subscriptions(&mut self) -> Result<(), GatewayError> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        for (guild, (channel, ranges)) in self.wanted.clone() {
            conn.send(subscribe(guild, channel, &ranges)).await?;
        }
        Ok(())
    }

    /// Advances to the next event. After `Fatal` it just repeats itself.
    pub async fn next(&mut self) -> Event {
        loop {
            if let Some(notice) = self.pending_notice.take() {
                return notice;
            }

            if self.conn.is_none() {
                match self.open().await {
                    Ok(()) => {}
                    Err(e) => {
                        if let Some(fatal) = fatal_of(&e) {
                            return Event::Fatal(fatal);
                        }
                        // Do not cling to a resume host that would not open.
                        //
                        // Resuming fills a gap on a working connection; it
                        // means nothing against a host that will not answer.
                        // Keeping it repeats the same failure forever.
                        if self.session.take().is_some() {
                            tracing::warn!("cannot reach the resume host; starting from identify");
                        }
                        let wait = self.grow_backoff();
                        tracing::warn!(error = %e, wait_ms = wait.as_millis() as u64, "cannot connect");
                        // Reported before waiting, or the screen looks frozen
                        // for up to a minute.
                        self.pending_notice = Some(Event::Reconnecting {
                            reason: e.to_string(),
                            wait,
                        });
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                }
            }

            let conn = self.conn.as_mut().expect("just opened");
            match conn
                .pump(&mut self.session, &mut self.requests, &mut self.wanted)
                .await
            {
                Ok(Some(event)) => {
                    // Anything arriving means the connection works.
                    self.backoff = BACKOFF_MIN;
                    // Subscriptions belong to the connection; without
                    // resending, nothing arrives again.
                    if matches!(event, Event::Ready(_) | Event::Resumed)
                        && let Err(e) = self.resend_subscriptions().await
                    {
                        tracing::warn!(error = %e, "could not resend subscriptions");
                    }
                    return event;
                }
                // Handled internally, such as heartbeats.
                Ok(None) => continue,
                Err(e) => {
                    self.conn = None;
                    if let Some(fatal) = fatal_of(&e) {
                        // Not resumable; drop it.
                        self.session = None;
                        return Event::Fatal(fatal);
                    }
                    if !recoverable_session(&e) {
                        // The session is gone, but identify still works.
                        self.session = None;
                    }
                    let wait = self.grow_backoff();
                    tracing::warn!(error = %e, "disconnected; reconnecting");
                    self.pending_notice = Some(Event::Reconnecting {
                        reason: e.to_string(),
                        wait,
                    });
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Doubles the backoff, up to the maximum.
    fn grow_backoff(&mut self) -> Duration {
        let wait = self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        wait
    }

    /// Connects, takes Hello, and sends identify or resume.
    async fn open(&mut self) -> Result<(), GatewayError> {
        let url = connect_url(self.session.as_ref().map(|s| &*s.url));

        crate::install_crypto_provider();
        let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
        let mut conn = Connection::new(ws)?;

        let hello = conn.wait_for_hello().await?;
        conn.heartbeat = Duration::from_millis(hello);

        match &self.session {
            Some(s) => {
                tracing::debug!(session = %s.id, "resuming");
                conn.last_seq = s.seq;
                conn.send(json!({
                    "op": OP_RESUME,
                    "d": { "token": self.token.expose(), "session_id": s.id, "seq": s.seq },
                }))
                .await?;
            }
            None => {
                tracing::debug!("identifying");
                conn.send(identify(&self.token)).await?;
            }
        }

        // interval * jitter, so clients do not all beat at once.
        conn.schedule_first_heartbeat();
        self.conn = Some(conn);
        Ok(())
    }
}

// ─────────────────────────────────────────────── Opcodes

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_RESUME: u8 = 6;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESSION: u8 = 9;
const OP_HELLO: u8 = 10;
const OP_HEARTBEAT_ACK: u8 = 11;
/// Requests members by id, 100 at a time.
const OP_REQUEST_MEMBERS: u8 = 8;
/// Announces the watched guild and channel; required for a user token.
const OP_GUILD_SUBSCRIBE: u8 = 14;

#[derive(Debug, Deserialize)]
struct Payload {
    op: u8,
    #[serde(default)]
    d: Option<serde_json::Value>,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

/// The identify payload.
///
/// The claim comes from one place; rebuilding it here would let it drift from
/// the REST header, and the disagreement is itself a signal.
fn identify(token: &Token) -> serde_json::Value {
    json!({
        "op": OP_IDENTIFY,
        "d": {
            "token": token.expose(),
            "capabilities": CAPABILITIES,
            "properties": Identity::detect().properties(),
            "presence": {
                "status": "unknown",
                "since": 0,
                "activities": [],
                "afk": false,
            },
            "compress": false,
            "client_state": {
                "guild_versions": {},
                "highest_last_message_id": "0",
                "read_state_version": 0,
                "user_guild_settings_version": -1,
                "user_settings_version": -1,
                "private_channels_version": "0",
                "api_code_version": 0,
            },
        },
    })
}

/// Capabilities requested with a user token.
///
/// The individual bits are unverified; this is the documented value sent
/// verbatim. Split it into named constants once they are known.
const CAPABILITIES: u32 = 161789;

// ─────────────────────────────────────────────── One connection

/// State for one connection; discarded entirely on reconnect.
struct Connection {
    ws: Ws,
    /// One stream spanning frames, so it lives as long as the connection.
    zstd: ZstdStream,
    heartbeat: Duration,
    /// When the next heartbeat is due.
    next_beat: tokio::time::Instant,
    /// Whether the last heartbeat was acknowledged; if not, the connection is
    /// dead.
    acked: bool,
    last_seq: Option<u64>,
    /// Leftovers from a frame that held several payloads.
    queued: std::collections::VecDeque<Payload>,
}

impl Connection {
    fn new(ws: Ws) -> Result<Self, GatewayError> {
        Ok(Connection {
            ws,
            zstd: ZstdStream::new()?,
            heartbeat: Duration::from_secs(45),
            next_beat: tokio::time::Instant::now() + Duration::from_secs(45),
            acked: true,
            last_seq: None,
            queued: std::collections::VecDeque::new(),
        })
    }

    async fn send(&mut self, value: serde_json::Value) -> Result<(), GatewayError> {
        self.ws.send(Message::text(value.to_string())).await?;
        Ok(())
    }

    /// Schedules the first heartbeat after interval * jitter.
    ///
    /// The randomness only has to stop every client beating at the same
    /// moment, so the clock's low bits are enough.
    fn schedule_first_heartbeat(&mut self) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter = nanos as f64 / 1e9;
        self.next_beat = tokio::time::Instant::now() + self.heartbeat.mul_f64(jitter);
    }

    /// Waits for Hello and returns the heartbeat interval.
    async fn wait_for_hello(&mut self) -> Result<u64, GatewayError> {
        loop {
            let payload = match self.queued.pop_front() {
                Some(p) => p,
                None => match self.recv().await? {
                    Some(()) => continue,
                    None => return Err(GatewayError::NoHello),
                },
            };
            if payload.op == OP_HELLO {
                #[derive(Deserialize)]
                struct Hello {
                    heartbeat_interval: u64,
                }
                let hello: Hello = serde_json::from_value(payload.d.unwrap_or_default())?;
                return Ok(hello.heartbeat_interval);
            }
        }
    }

    /// Advances one step.
    ///
    /// Everything awaited here must be cancel-safe: `select!` drops the losing
    /// future, and dropping mid-send would truncate a WebSocket write. So
    /// sending only happens after winning.
    async fn pump(
        &mut self,
        session: &mut Option<SessionInfo>,
        requests: &mut tokio::sync::mpsc::UnboundedReceiver<Request>,
        wanted: &mut std::collections::HashMap<GuildId, (ChannelId, Vec<MemberRange>)>,
    ) -> Result<Option<Event>, GatewayError> {
        if let Some(payload) = self.queued.pop_front() {
            return self.handle(payload, session).await;
        }

        enum Step {
            Beat,
            Received(Option<()>),
            Watch(Option<Request>),
        }

        let step = tokio::select! {
            _ = tokio::time::sleep_until(self.next_beat) => Step::Beat,
            got = self.recv() => Step::Received(got?),
            got = requests.recv() => Step::Watch(got),
        };

        match step {
            Step::Beat => {
                if !self.acked {
                    // TCP takes a long time to notice a severed network; this
                    // is the only detection there is.
                    return Err(GatewayError::Closed(CLOSE_NO_ACK));
                }
                self.acked = false;
                self.next_beat = tokio::time::Instant::now() + self.heartbeat;
                self.send(json!({ "op": OP_HEARTBEAT, "d": self.last_seq }))
                    .await?;
                Ok(None)
            }
            Step::Received(Some(())) => Ok(None),
            Step::Received(None) => Err(GatewayError::Closed(CLOSE_ABNORMAL)),
            Step::Watch(Some(Request::Members(guild, users))) => {
                tracing::debug!(%guild, users = users.len(), "requesting members by id");
                self.send(request_members(guild, &users)).await?;
                Ok(None)
            }
            Step::Watch(Some(Request::Watch(guild, channel, ranges))) => {
                // Never resend an identical subscription.
                //
                // Callers announce every time, since missing a change is
                // worse — but that is no reason to put it on the wire. With
                // one channel open this sent hundreds of times and Discord
                // closed the connection, which reconnected and resent, and so
                // on. Reconnects resend everything anyway.
                let watching = (channel, ranges);
                if wanted.get(&guild) == Some(&watching) {
                    return Ok(None);
                }
                tracing::debug!(%guild, %channel, rows = ?watching.1, "subscribing");
                // Stored first, so the reconnect that follows a failed send
                // still carries it.
                wanted.insert(guild, watching.clone());
                self.send(subscribe(guild, watching.0, &watching.1)).await?;
                Ok(None)
            }
            // No requesters left, which is not a reason to disconnect.
            Step::Watch(None) => Ok(None),
        }
    }

    /// Reads one frame, decompresses it and queues the payloads. An empty
    /// frame is not an error.
    async fn recv(&mut self) -> Result<Option<()>, GatewayError> {
        let Some(message) = self.ws.next().await else {
            return Ok(None);
        };

        let plain = match message? {
            Message::Binary(bytes) => self.zstd.push(&bytes)?,
            // Unexpected given the compression request, but read it anyway.
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Close(frame) => {
                let code = frame.map(|f| u16::from(f.code)).unwrap_or(CLOSE_ABNORMAL);
                return Err(GatewayError::Closed(code));
            }
            // Answered by the WebSocket library.
            _ => return Ok(Some(())),
        };

        if plain.is_empty() {
            // Mid-message across frames; wait for the next.
            return Ok(Some(()));
        }

        // One frame can hold several JSON payloads.
        for value in serde_json::Deserializer::from_slice(&plain).into_iter::<Payload>() {
            self.queued.push_back(value?);
        }
        Ok(Some(()))
    }

    async fn handle(
        &mut self,
        payload: Payload,
        session: &mut Option<SessionInfo>,
    ) -> Result<Option<Event>, GatewayError> {
        if let Some(s) = payload.s {
            self.last_seq = Some(s);
            if let Some(info) = session.as_mut() {
                info.seq = Some(s);
            }
        }

        match payload.op {
            OP_DISPATCH => {
                let kind = payload.t.unwrap_or_default();
                let data = payload.d.unwrap_or_default();
                match kind.as_str() {
                    "READY" => {
                        // Discord renests fields without notice, so a shape
                        // change should be visible from this one place.
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            log_ready_shape(&data);
                        }
                        let ready: Ready = serde_json::from_value(data)?;
                        // Without a resume host, fall back to the original
                        // URL: possibly the wrong server beats failing.
                        *session = Some(SessionInfo {
                            id: ready.session_id.clone(),
                            url: ready
                                .resume_gateway_url
                                .clone()
                                .unwrap_or_else(|| "wss://gateway.discord.gg".to_owned()),
                            seq: self.last_seq,
                        });
                        tracing::info!(
                            user = %ready.user.user.display_name(),
                            guilds = ready.guilds.len(),
                            "READY"
                        );
                        Ok(Some(Event::Ready(Box::new(ready))))
                    }
                    "RESUMED" => {
                        tracing::info!("RESUMED");
                        Ok(Some(Event::Resumed))
                    }
                    _ => Ok(Some(Event::Dispatch { kind, data })),
                }
            }
            // The server asked; answer at once.
            OP_HEARTBEAT => {
                self.acked = false;
                self.next_beat = tokio::time::Instant::now() + self.heartbeat;
                self.send(json!({ "op": OP_HEARTBEAT, "d": self.last_seq }))
                    .await?;
                Ok(None)
            }
            OP_HEARTBEAT_ACK => {
                self.acked = true;
                Ok(None)
            }
            // Told to reconnect; resuming is still allowed.
            OP_RECONNECT => Err(GatewayError::Closed(CLOSE_RECONNECT)),
            OP_INVALID_SESSION => {
                // True means resumable; false means the session is gone.
                let resumable = payload.d.and_then(|d| d.as_bool()).unwrap_or(false);
                if !resumable {
                    *session = None;
                }
                Err(GatewayError::Closed(CLOSE_INVALID_SESSION))
            }
            // `open` consumed the first Hello; only a duplicate reaches here.
            other => {
                tracing::debug!(op = other, "unknown opcode; skipping");
                Ok(None)
            }
        }
    }
}

// ─────────────────────────────────────────────── Disconnects

/// The next heartbeat came due with the previous one unacknowledged.
const CLOSE_NO_ACK: u16 = 4_900;
/// Told to reconnect.
const CLOSE_RECONNECT: u16 = 4_901;
/// Invalid session; whether it is resumable is decided by the payload.
const CLOSE_INVALID_SESSION: u16 = 4_902;
/// Closed without explanation.
const CLOSE_ABNORMAL: u16 = 1_006;

/// Whether reconnecting is pointless.
///
/// Anything not listed is retried: Discord adds codes without notice, and
/// treating an unknown one as fatal would eject the user over a recoverable
/// disconnect.
fn fatal_of(error: &GatewayError) -> Option<Fatal> {
    let GatewayError::Closed(code) = error else {
        return None;
    };
    match code {
        // Authentication failed; discard the token.
        4004 => Some(Fatal::Unauthorized),
        4010 => Some(Fatal::Rejected {
            code: *code,
            reason: "シャードの指定が誤っている".to_owned(),
        }),
        4011 => Some(Fatal::Rejected {
            code: *code,
            reason: "シャード分割が必要である".to_owned(),
        }),
        4012 => Some(Fatal::Rejected {
            code: *code,
            reason: "API の版が古い".to_owned(),
        }),
        4013 => Some(Fatal::Rejected {
            code: *code,
            reason: "intent の指定が誤っている".to_owned(),
        }),
        4014 => Some(Fatal::Rejected {
            code: *code,
            reason: "許可されていない intent を要求した".to_owned(),
        }),
        _ => None,
    }
}

/// Whether resuming is still worth trying after this close.
///
/// When unsure, resume: a wasted resume costs one round trip, while a wasted
/// identify loses the missed events for good.
fn recoverable_session(error: &GatewayError) -> bool {
    match error {
        // A network problem; the session is still alive.
        GatewayError::Connect(_) | GatewayError::Decompress(_) | GatewayError::Decode(_) => true,
        GatewayError::NoHello => true,
        GatewayError::Closed(code) => !matches!(
            *code,
            // Bad sequence, or an expired session.
            4007 | 4009 | CLOSE_INVALID_SESSION
        ),
    }
}

/// Logs the shape of READY's guilds.
///
/// An empty list looks the same whether the user is in no guilds or the
/// payload could not be read. Eleven once arrived and none could be shown,
/// because the name had moved inside `properties`.
///
/// Keys only, never values: guild names and the token pass through here.
fn log_ready_shape(data: &serde_json::Value) {
    let guilds = data.get("guilds").and_then(|g| g.as_array());
    let keys: Vec<&str> = guilds
        .and_then(|g| g.first())
        .and_then(|g| g.as_object())
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();

    tracing::debug!(
        guilds = guilds.map_or(0, |g| g.len()),
        first_guild = ?keys,
        "READY の形"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(code: u16) -> GatewayError {
        GatewayError::Closed(code)
    }

    /// The request line's path must start with `/`. Without it Discord
    /// answers 400, and the retained session makes it retry forever.
    #[test]
    fn the_resume_url_keeps_its_path() {
        use tokio_tungstenite::tungstenite::http::Uri;

        let target = |url: &str| {
            url.parse::<Uri>()
                .expect("a valid URL")
                .path_and_query()
                .expect("a path")
                .as_str()
                .to_owned()
        };

        for host in [
            "wss://gateway-us-east1-b.discord.gg",
            // A trailing slash must not be doubled.
            "wss://gateway-us-east1-b.discord.gg/",
        ] {
            let url = connect_url(Some(host));
            assert!(
                url.starts_with(&format!("{}/?", host.trim_end_matches('/'))),
                "{url}"
            );
            assert!(target(&url).starts_with("/?v=10"), "{url}");
        }

        // The first connection has the same shape.
        assert!(target(&connect_url(None)).starts_with("/?v=10"));
    }

    /// A rejected token is fatal, so the caller can discard it.
    #[test]
    fn an_invalid_token_is_fatal() {
        assert_eq!(fatal_of(&closed(4004)), Some(Fatal::Unauthorized));
    }

    /// The subscription carries exactly the rows it was given, under the
    /// channel's id as text: asking wider than intended would show in this
    /// one place.
    #[test]
    fn the_subscription_asks_only_for_the_given_rows() {
        let payload = subscribe(GuildId::from(2), ChannelId::from(3), &[[0, 99]]);
        assert_eq!(payload["op"], OP_GUILD_SUBSCRIBE);
        assert_eq!(payload["d"]["guild_id"], "2");
        assert_eq!(payload["d"]["typing"], true);
        assert_eq!(payload["d"]["channels"]["3"], serde_json::json!([[0, 99]]));

        // Several ranges survive as several spans; scrolling will widen the
        // ask through this same shape.
        let wide = subscribe(GuildId::from(2), ChannelId::from(3), &[[0, 99], [100, 199]]);
        assert_eq!(
            wide["d"]["channels"]["3"],
            serde_json::json!([[0, 99], [100, 199]])
        );
    }

    /// Unknown codes are retried; Discord adds them without notice.
    #[test]
    fn unknown_close_codes_are_retried() {
        for code in [4000, 4001, 4002, 4003, 4005, 4008, 4020, 4999, 1006] {
            assert_eq!(fatal_of(&closed(code)), None, "gave up on code {code}");
        }
    }

    /// A network close leaves the session alive.
    #[test]
    fn a_network_hiccup_keeps_the_session() {
        assert!(recoverable_session(&GatewayError::NoHello));
        assert!(recoverable_session(&closed(1006)));
        assert!(recoverable_session(&closed(CLOSE_NO_ACK)));
        assert!(recoverable_session(&closed(CLOSE_RECONNECT)));
    }

    /// A dead session means starting from identify.
    #[test]
    fn an_expired_session_is_not_resumed() {
        assert!(!recoverable_session(&closed(4007)));
        assert!(!recoverable_session(&closed(4009)));
        assert!(!recoverable_session(&closed(CLOSE_INVALID_SESSION)));
    }

    /// The backoff doubles and stops at the maximum.
    #[test]
    fn the_backoff_grows_but_is_capped() {
        let (mut g, _subs) = Gateway::new(Token::new("x"));
        assert_eq!(g.grow_backoff(), BACKOFF_MIN);
        assert_eq!(g.grow_backoff(), BACKOFF_MIN * 2);
        assert_eq!(g.grow_backoff(), BACKOFF_MIN * 4);

        for _ in 0..20 {
            g.grow_backoff();
        }
        assert_eq!(g.grow_backoff(), BACKOFF_MAX, "grew past the maximum");
    }

    /// The token never reaches the debug output.
    #[test]
    fn the_token_never_appears_in_debug() {
        let (g, _subs) = Gateway::new(Token::new("mfa.SUPER_SECRET"));
        let shown = format!("{g:?}");
        assert!(
            !shown.contains("SUPER_SECRET"),
            "トークンが漏れている: {shown}"
        );
    }

    /// The identify claim matches the one source.
    ///
    /// This test used to assert the opposite — that the client identified as
    /// itself. Doing so got the account flagged and forced a password reset,
    /// and the decision was reversed. What is checked now is that the gateway
    /// and REST agree, since a disagreement is itself a signal.
    #[test]
    fn identify_matches_the_rest_headers() {
        let payload = identify(&Token::new("t"));
        let props = &payload["d"]["properties"];

        assert_eq!(
            *props,
            Identity::detect().properties(),
            "the two claims disagree"
        );
        assert_eq!(payload["d"]["token"], "t");
        // The user-token shape; `intents` would be the bot one.
        assert!(payload["d"]["capabilities"].is_number());
        assert!(payload["d"].get("intents").is_none());
    }

    /// READY parses, and unknown fields do not break it.
    #[test]
    fn ready_is_parsed_and_tolerates_unknown_fields() {
        let ready: Ready = serde_json::from_str(
            r#"{
                "user": {"id": "1", "username": "ねんねこ"},
                "session_id": "abc",
                "resume_gateway_url": "wss://gateway-us-east1-b.discord.gg",
                "guilds": [{"id": "2", "name": "テスト"}],
                "まだ知らないもの": {"入れ子": [1,2]}
            }"#,
        )
        .expect("could not read READY");

        assert_eq!(ready.session_id, "abc");
        assert_eq!(ready.guilds.len(), 1);
        assert_eq!(ready.user.user.display_name(), "ねんねこ");
    }

    /// A missing `resume_gateway_url` is unusual but must still work.
    #[test]
    fn ready_without_a_resume_url_still_parses() {
        let ready: Ready =
            serde_json::from_str(r#"{"user":{"id":"1","username":"x"},"session_id":"s"}"#).unwrap();
        assert!(ready.resume_gateway_url.is_none());
        assert!(ready.guilds.is_empty());
    }

    /// Several payloads in one frame all get read.
    #[test]
    fn several_payloads_in_one_frame_are_all_read() {
        let raw = br#"{"op":11}{"op":0,"t":"MESSAGE_CREATE","s":5,"d":{}}"#;
        let payloads: Vec<Payload> = serde_json::Deserializer::from_slice(raw)
            .into_iter::<Payload>()
            .collect::<Result<_, _>>()
            .expect("could not read concatenated JSON");

        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].op, OP_HEARTBEAT_ACK);
        assert_eq!(payloads[1].s, Some(5));
        assert_eq!(payloads[1].t.as_deref(), Some("MESSAGE_CREATE"));
    }
}
