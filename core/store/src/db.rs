//! The local cache. Reads are synchronous, writes are fire and forget.
//!
//! ```text
//!   at startup   Db::open()      read on the main thread; the first frame
//!                    │           needs it, so it waits (a few ms)
//!                    ▼
//!   while running  save_*()  ──▶  writer thread, owning the connection
//!                  returns at once
//! ```
//!
//! The connection is never shared: one thread owning one connection is the
//! simplest and fastest arrangement, and sending work to a writer thread
//! avoids both locking and waiting.
//!
//! The cache only makes things faster, so a full disk, missing permissions or
//! a corrupt file must not stop the app. Failures here are logged and not
//! propagated.
//!
//! The contents are not encrypted: message bodies sit on disk in the clear.
//! That is the planned order of work, but not something a user should have to
//! discover. Signing out deletes all of it — see [`Db::wipe`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};

use gumicord_model::{Channel, ChannelId, Guild, GuildId, Member, Message, User, UserId};

/// Messages kept per channel: enough to fill the screen on open, since the
/// rest can be refetched. Keeping everything makes startup slower over time.
const KEEP_PER_CHANNEL: usize = 200;

/// Writer queue depth. Overflow is dropped: blocking the main thread for the
/// cache defeats its purpose, and the next start refetches anyway.
const QUEUE: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("cannot open the cache: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("cannot determine where to store it")]
    NoHome,
}

/// The previous state, read at startup.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub guilds: Vec<Guild>,
    /// The channel to reopen.
    pub last_channel: Option<ChannelId>,
    /// Its messages, oldest first.
    pub messages: Vec<Message>,
    /// The user's guild order; empty means unknown.
    pub guild_order: Vec<GuildId>,
    /// Sidebar folders.
    pub folders: Vec<crate::FolderRow>,
    /// Folded folders; refolding them is not the user's job.
    pub collapsed: Vec<u64>,
}

/// Work sent to the writer thread.
enum Job {
    Guilds(Vec<Guild>),
    Messages {
        channel: ChannelId,
        list: Vec<Message>,
    },
    LastChannel(ChannelId),
    GuildOrder(String),
    /// Writes one row of key-value state.
    State(&'static str, String),
    /// A read; the caller supplies where the answer goes.
    Load {
        channel: ChannelId,
        then: Box<dyn FnOnce(Vec<Message>) + Send>,
    },
    /// Deletes everything.
    Wipe,
    /// Signals that everything queued has been done.
    Barrier(SyncSender<()>),
}

/// The local cache.
pub struct Db {
    tx: Option<SyncSender<Job>>,
    /// The writer thread, owned so that dropping the handle can stop it.
    handle: Option<std::thread::JoinHandle<()>>,
}

impl core::fmt::Debug for Db {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Db")
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // Taking the sender out closes the channel; the writer thread then
        // sees the disconnect and stops by itself. Joining right after makes
        // that deterministic: the SQLite connection is guaranteed closed
        // before anything opens the same file again. On POSIX a connection
        // left open by a leaked writer thread can hang a second write to the
        // file, and a detached thread would keep the process from exiting.
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Db {
    /// Opens the cache and reads the previous state.
    ///
    /// Synchronous, because the first frame needs it and deferring shows an
    /// empty screen for a moment.
    pub fn open(path: &Path) -> Result<(Db, Snapshot), DbError> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = rusqlite::Connection::open(path)?;
        // Bound any SQLite lock wait so a stalled peer cannot block the writer
        // (or a flush) forever; on POSIX two connections to one file can wait
        // on each other's locks. A few seconds is a generous budget for a
        // cache write, and an error is logged rather than hanging the caller.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        init(&conn)?;
        let snapshot = read_snapshot(&conn)?;

        // Overflow is dropped; the main thread never waits.
        let (tx, rx) = sync_channel::<Job>(QUEUE);
        let handle = std::thread::Builder::new()
            .name("gumicord-cache".to_owned())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // The app works without writing; log and move on.
                    if let Err(e) = run(&conn, job) {
                        tracing::warn!(%e, "could not write to the cache");
                    }
                }
                tracing::debug!("cache writer thread stopping");
            })
            .map_err(|_| DbError::NoHome)?;

        Ok((
            Db {
                tx: Some(tx),
                handle: Some(handle),
            },
            snapshot,
        ))
    }

    fn send(&self, job: Job) {
        // Never waits; drops on overflow.
        if self.tx.as_ref().is_none_or(|tx| tx.try_send(job).is_err()) {
            tracing::debug!("cache queue is full; dropping this write");
        }
    }

    pub fn save_guilds(&self, guilds: Vec<Guild>) {
        self.send(Job::Guilds(guilds));
    }

    pub fn save_messages(&self, channel: ChannelId, list: Vec<Message>) {
        self.send(Job::Messages { channel, list });
    }

    pub fn save_last_channel(&self, channel: ChannelId) {
        self.send(Job::LastChannel(channel));
    }

    /// Deletes the cache on sign-out. Leaving it behind lets the next person
    /// on this machine read the previous one's messages.
    pub fn wipe(&self) {
        self.send(Job::Wipe);
    }
}

/// The default location.
pub fn default_path() -> Result<PathBuf, DbError> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));

    Ok(base
        .ok_or(DbError::NoHome)?
        .join("gumicord")
        .join("cache.db"))
}

/// The location for a specific account's cache.
pub fn account_path(is_bot: bool, id: UserId) -> Result<PathBuf, DbError> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));

    let prefix = if is_bot { "bot" } else { "user" };
    Ok(base
        .ok_or(DbError::NoHome)?
        .join("gumicord")
        .join("cache")
        .join("accounts")
        .join(format!("{prefix}_{id}.db")))
}

// ─────────────────────────────────────────────── Schema

/// Ids are stored as text: snowflakes are unsigned 64-bit and SQLite's
/// integers are signed. They still fit, but breaking silently on the day they
/// stop is worse than storing text from the start.
const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS guilds (
    id   TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    icon TEXT
);

CREATE TABLE IF NOT EXISTS channels (
    id       TEXT PRIMARY KEY,
    guild_id TEXT NOT NULL,
    kind     INTEGER NOT NULL,
    name     TEXT,
    position INTEGER NOT NULL,
    topic    TEXT
);

CREATE TABLE IF NOT EXISTS users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL,
    global_name   TEXT,
    discriminator TEXT NOT NULL DEFAULT '',
    avatar        TEXT
);

CREATE TABLE IF NOT EXISTS messages (
    id            TEXT PRIMARY KEY,
    channel_id    TEXT NOT NULL,
    author_id     TEXT NOT NULL,
    content       TEXT NOT NULL,
    ts            TEXT NOT NULL,
    nick          TEXT,
    member_avatar TEXT
);

CREATE INDEX IF NOT EXISTS messages_by_channel ON messages(channel_id, id);

CREATE TABLE IF NOT EXISTS state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// The schema version; bump it on any change.
///
/// A mismatch drops everything and starts over. This is a cache, not a
/// record: the contents can always be rebuilt, and writing a migration per
/// column would add paths nobody ever exercises.
///
/// That reasoning does not transfer to anything that must survive, so nothing
/// of the sort belongs here.
const SCHEMA_VERSION: i32 = 3;

fn init(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let found: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found != SCHEMA_VERSION {
        tracing::info!(found, want = SCHEMA_VERSION, "rebuilding the cache");
        conn.execute_batch(
            "DROP TABLE IF EXISTS guilds;
             DROP TABLE IF EXISTS channels;
             DROP TABLE IF EXISTS users;
             DROP TABLE IF EXISTS messages;
             DROP TABLE IF EXISTS state;",
        )?;
    }
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
}

fn read_snapshot(conn: &rusqlite::Connection) -> rusqlite::Result<Snapshot> {
    let mut guilds: Vec<Guild> = conn
        .prepare("SELECT id, name, icon FROM guilds")?
        .query_map([], |row| {
            Ok(Guild {
                id: parse_id(row.get::<_, String>(0)?),
                name: row.get(1)?,
                icon_hash: row.get(2)?,
                unavailable: false,
                channels: Vec::new(),
                // Roles are not kept: the member list is refetched on
                // connect, and a cached one is only stale.
                roles: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let channels: Vec<(GuildId, Channel)> = conn
        .prepare("SELECT id, guild_id, kind, name, position, topic FROM channels")?
        .query_map([], |row| {
            let guild: GuildId = parse_id(row.get::<_, String>(1)?);
            Ok((
                guild,
                Channel {
                    id: parse_id(row.get::<_, String>(0)?),
                    kind: (row.get::<_, i64>(2)? as u8).into(),
                    name: row.get(3)?,
                    guild_id: Some(guild),
                    parent_id: None,
                    position: row.get::<_, i64>(4)? as i32,
                    topic: row.get(5)?,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    for (guild, channel) in channels {
        if let Some(g) = guilds.iter_mut().find(|g| g.id == guild) {
            g.channels.push(channel);
        }
    }

    let last_channel: Option<ChannelId> = conn
        .query_row(
            "SELECT value FROM state WHERE key = 'last_channel'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .map(parse_id);

    let messages = match last_channel {
        Some(ch) => read_messages(conn, ch)?,
        None => Vec::new(),
    };

    // The order too, or the first frame from cache is ordered differently and
    // the list jumps when READY lands.
    let guild_order: Vec<GuildId> = conn
        .query_row(
            "SELECT value FROM state WHERE key = 'guild_order'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .into_iter()
        .flat_map(|s| {
            s.split(',')
                .filter(|p| !p.is_empty())
                .map(|p| parse_id(p.to_owned()))
                .collect::<Vec<GuildId>>()
        })
        .collect();

    let folders: Vec<crate::FolderRow> = read_state(conn, "folders")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let collapsed: Vec<u64> = read_state(conn, "collapsed")
        .into_iter()
        .flat_map(|s| {
            s.split(',')
                .filter_map(|p| p.parse::<u64>().ok())
                .collect::<Vec<u64>>()
        })
        .collect();

    Ok(Snapshot {
        guilds,
        last_channel,
        messages,
        guild_order,
        folders,
        collapsed,
    })
}

/// Reads a channel's messages, oldest first.
fn read_messages(
    conn: &rusqlite::Connection,
    channel: ChannelId,
) -> rusqlite::Result<Vec<Message>> {
    // Snowflakes sort by time, so order by length then lexically: at equal
    // length, lexical order is chronological.
    conn.prepare(
        "SELECT m.id, m.author_id, m.content, m.ts,
                u.username, u.global_name, u.discriminator, u.avatar,
                m.nick, m.member_avatar
           FROM messages m
           LEFT JOIN users u ON u.id = m.author_id
          WHERE m.channel_id = ?1
          ORDER BY length(m.id), m.id",
    )?
    .query_map([channel.get().to_string()], |row| {
        let author_id: String = row.get(1)?;
        Ok(Message {
            id: parse_id(row.get::<_, String>(0)?),
            channel_id: channel,
            guild_id: None,
            author: User {
                id: parse_id(author_id),
                username: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                global_name: row.get(5)?,
                discriminator: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                avatar_hash: row.get(7)?,
                bot: false,
            },
            content: row.get(2)?,
            timestamp: row.get(3)?,
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: member(row.get(8)?, row.get(9)?),
            referenced_message: None,
            // Not stored: READY brings the unread state every time.
            mentions: Vec::new(),
            mention_everyone: false,
        })
    })?
    .collect()
}

/// Restores the stored guild members.
///
/// No member is built when neither is present: a member that overrides
/// nothing is not the same as not being a member at all.
///
/// Roles and join dates are not stored; storing something not yet shown means
/// showing a stale value the day it is.
fn member(nick: Option<String>, avatar_hash: Option<String>) -> Option<Member> {
    if nick.is_none() && avatar_hash.is_none() {
        return None;
    }
    Some(Member {
        nick,
        avatar_hash,
        roles: Vec::new(),
        joined_at: None,
        user: None,
    })
}

/// Parses a stored id, falling back to zero.
///
/// Never panics: failing to read what we wrote is abnormal, but a corrupt
/// cache must not stop the app from starting.
fn parse_id<T: From<u64>>(s: String) -> T {
    T::from(s.parse::<u64>().unwrap_or(0))
}

fn run(conn: &rusqlite::Connection, job: Job) -> rusqlite::Result<()> {
    match job {
        Job::Guilds(guilds) => write_guilds(conn, &guilds),
        Job::Messages { channel, list } => write_messages(conn, channel, &list),
        // Reaching this means everything queued before it is done.
        Job::Barrier(reply) => {
            let _ = reply.send(());
            Ok(())
        }
        Job::Load { channel, then } => {
            // Always answers, even on failure; silence leaves the screen on
            // "loading" forever.
            then(read_messages(conn, channel).unwrap_or_default());
            Ok(())
        }
        Job::State(key, value) => {
            conn.execute(
                "INSERT INTO state(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        }
        Job::GuildOrder(joined) => {
            conn.execute(
                "INSERT INTO state(key, value) VALUES('guild_order', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [joined],
            )?;
            Ok(())
        }
        Job::LastChannel(ch) => {
            conn.execute(
                "INSERT INTO state(key, value) VALUES('last_channel', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [ch.get().to_string()],
            )?;
            Ok(())
        }
        Job::Wipe => {
            // Delete the contents, then shrink the file.
            conn.execute_batch(
                "DELETE FROM messages; DELETE FROM users; DELETE FROM channels;
                 DELETE FROM guilds;  DELETE FROM state;  VACUUM;",
            )
        }
    }
}

fn write_guilds(conn: &rusqlite::Connection, guilds: &[Guild]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;

    // Cleared before rewriting, or guilds and channels that were left stay
    // in the list as rows that do not open.
    tx.execute("DELETE FROM guilds", [])?;
    tx.execute("DELETE FROM channels", [])?;

    for g in guilds {
        // Shells are not written; with no name there is nothing to show.
        if g.unavailable || g.name.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT OR REPLACE INTO guilds(id, name, icon) VALUES(?1, ?2, ?3)",
            rusqlite::params![g.id.get().to_string(), g.name, g.icon_hash],
        )?;

        for c in &g.channels {
            tx.execute(
                "INSERT OR REPLACE INTO channels(id, guild_id, kind, name, position, topic)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    c.id.get().to_string(),
                    g.id.get().to_string(),
                    u8::from(c.kind) as i64,
                    c.name,
                    c.position as i64,
                    c.topic,
                ],
            )?;
        }
    }
    tx.commit()
}

fn write_messages(
    conn: &rusqlite::Connection,
    channel: ChannelId,
    list: &[Message],
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let ch = channel.get().to_string();

    for m in list {
        tx.execute(
            "INSERT OR REPLACE INTO users(id, username, global_name, discriminator, avatar)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                m.author.id.get().to_string(),
                m.author.username,
                m.author.global_name,
                // Kept: it decides which default avatar applies.
                m.author.discriminator,
                m.author.avatar_hash,
            ],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO messages(id, channel_id, author_id, content, ts, nick, member_avatar)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                m.id.get().to_string(),
                ch,
                m.author.id.get().to_string(),
                m.content,
                m.timestamp,
                // Per-guild names and avatars too, or messages restored from
                // cache appear under a different name.
                m.member.as_ref().and_then(|x| x.nick.as_deref()),
                m.member.as_ref().and_then(|x| x.avatar_hash.as_deref()),
            ],
        )?;
    }

    // Trim: keeping everything makes startup slower over time.
    tx.execute(
        "DELETE FROM messages
          WHERE channel_id = ?1
            AND id NOT IN (
                SELECT id FROM messages WHERE channel_id = ?1
                 ORDER BY length(id) DESC, id DESC LIMIT ?2
            )",
        rusqlite::params![ch, KEEP_PER_CHANNEL as i64],
    )?;
    tx.commit()
}

impl Db {
    /// Reads a channel's messages; the answer arrives from another thread.
    ///
    /// Reads go through the writer thread too: a second connection would hit
    /// a busy error mid-write. Reads only happen when a channel opens, so
    /// queueing costs nothing.
    pub fn load_messages(
        &self,
        channel: ChannelId,
        then: impl FnOnce(Vec<Message>) + Send + 'static,
    ) {
        self.send(Job::Load {
            channel,
            then: Box::new(then),
        });
    }
}

impl Db {
    /// Waits for everything queued to be written.
    ///
    /// The queue is FIFO, so an answer here means everything before it
    /// landed. For shutdown and for tests; not for ordinary use, since not
    /// waiting is the point of this layer.
    pub fn flush(&self) {
        let (tx, rx) = sync_channel(1);
        if let Some(send) = &self.tx
            && send.send(Job::Barrier(tx)).is_ok()
        {
            let _ = rx.recv();
        }
    }
}

impl Db {
    /// Stores the guild order. Without it the first frame from cache is
    /// ordered differently and the list jumps when READY lands.
    pub fn save_guild_order(&self, order: &[GuildId]) {
        let joined = order
            .iter()
            .map(|g| g.get().to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.send(Job::GuildOrder(joined));
    }
}

impl Db {
    /// Stores the folders as one row of JSON. There are at most a few dozen,
    /// never searched or joined, and always read together; a normalised table
    /// would only add work.
    pub fn save_sidebar(&self, folders: &[crate::FolderRow]) {
        let json = serde_json::to_string(folders).unwrap_or_else(|_| "[]".to_owned());
        self.send(Job::State("folders", json));
    }

    /// Stores which folders are folded; refolding them at every start is not
    /// the user's job.
    pub fn save_collapsed(&self, ids: &[u64]) {
        let joined = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.send(Job::State("collapsed", joined));
    }
}

/// Reads one row of key-value state.
fn read_state(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM state WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_model::{ChannelKind, MessageId};

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("gumicord-db-test-{tag}.db"));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", p.display()));
        }
        p
    }

    fn guild(id: u64, name: &str, channel: u64) -> Guild {
        Guild {
            id: id.into(),
            name: name.to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![Channel {
                id: channel.into(),
                kind: ChannelKind::GuildText,
                name: Some("いっぱん".to_owned()),
                guild_id: Some(id.into()),
                parent_id: None,
                position: 3,
                topic: Some("話題".to_owned()),
                nsfw: false,
                recipients: Vec::new(),
                last_message_id: None,
            }],
            roles: Vec::new(),
        }
    }

    fn message(id: u64, channel: u64, body: &str) -> Message {
        Message {
            id: MessageId::from(id),
            channel_id: ChannelId::from(channel),
            guild_id: None,
            author: User {
                id: 7u64.into(),
                username: "ねんねこ".to_owned(),
                global_name: Some("ｽﾋﾟｷ".to_owned()),
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: body.to_owned(),
            timestamp: "2026-08-22T12:34:56+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: None,
            referenced_message: None,
            mentions: Vec::new(),
            mention_everyone: false,
        }
    }

    /// What was written comes back on the next start.
    #[test]
    fn what_is_written_survives_a_restart() {
        let path = scratch("roundtrip");

        let (db, first) = Db::open(&path).expect("cannot open");
        assert!(first.guilds.is_empty(), "a fresh cache is not empty");

        db.save_guilds(vec![guild(1, "テスト鯖", 10)]);
        db.save_messages(
            ChannelId::from(10u64),
            vec![message(100, 10, "こんにちは"), message(101, 10, "またね")],
        );
        db.save_last_channel(ChannelId::from(10u64));
        db.flush();
        drop(db);

        let (_db, again) = Db::open(&path).expect("cannot reopen");
        assert_eq!(again.guilds.len(), 1);
        assert_eq!(again.guilds[0].name, "テスト鯖");
        assert_eq!(again.guilds[0].channels.len(), 1);
        assert_eq!(again.guilds[0].channels[0].topic.as_deref(), Some("話題"));
        assert_eq!(again.last_channel, Some(ChannelId::from(10u64)));

        let bodies: Vec<_> = again.messages.iter().map(|m| &*m.content).collect();
        assert_eq!(bodies, vec!["こんにちは", "またね"], "not oldest first");
        // The author resolves too.
        assert_eq!(again.messages[0].author.display_name(), "ｽﾋﾟｷ");
    }

    /// The four digits are kept: they decide the default avatar, and dropping
    /// them makes a cached bot's face differ from a freshly arrived one.
    #[test]
    fn the_old_four_digits_survive_too() {
        let path = scratch("discriminator");

        let (db, _) = Db::open(&path).expect("cannot open");
        let mut m = message(100, 10, "ぼっとです");
        m.author.discriminator = "0007".to_owned();
        m.author.bot = true;
        db.save_messages(ChannelId::from(10u64), vec![m]);
        db.save_last_channel(ChannelId::from(10u64));
        db.flush();
        drop(db);

        let (_db, again) = Db::open(&path).expect("cannot reopen");
        let author = &again.messages[0].author;
        assert_eq!(author.tag(), Some("0007"));
        assert_eq!(author.default_avatar_index(), 2, "7 % 5");
    }

    /// A guild that was left does not linger as an unopenable row.
    #[test]
    fn leaving_a_guild_removes_it() {
        let path = scratch("leave");

        let (db, _) = Db::open(&path).unwrap();
        db.save_guilds(vec![guild(1, "のこる", 10), guild(2, "ぬける", 20)]);
        db.flush();
        db.save_guilds(vec![guild(1, "のこる", 10)]);
        db.flush();
        drop(db);

        let (_db, again) = Db::open(&path).unwrap();
        let names: Vec<_> = again.guilds.iter().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["のこる"]);
    }

    /// Signing out leaves nothing behind.
    #[test]
    fn wiping_leaves_nothing_behind() {
        let path = scratch("wipe");

        let (db, _) = Db::open(&path).unwrap();
        db.save_guilds(vec![guild(1, "ひみつ", 10)]);
        db.save_messages(ChannelId::from(10u64), vec![message(1, 10, "ないしょ")]);
        db.flush();
        db.wipe();
        db.flush();
        drop(db);

        let (_db, again) = Db::open(&path).unwrap();
        assert!(again.guilds.is_empty());
        assert!(again.messages.is_empty());

        // And nothing survives inside the file.
        let raw = std::fs::read(&path).unwrap_or_default();
        assert!(
            !raw.windows(12).any(|w| w == "ないしょ".as_bytes()),
            "消したはずの本文がファイルに残っている"
        );
    }

    /// A failed read still answers; silence stalls the screen.
    #[test]
    fn loading_an_unknown_channel_still_answers() {
        let path = scratch("load");
        let (db, _) = Db::open(&path).unwrap();

        let (tx, rx) = sync_channel(1);
        db.load_messages(ChannelId::from(999u64), move |list| {
            let _ = tx.send(list);
        });
        let got = rx.recv().expect("返事が来ない");
        assert!(got.is_empty());
    }

    #[test]
    fn account_path_generates_correct_locations() {
        let expected_user = PathBuf::from("cache")
            .join("accounts")
            .join("user_123456789.db");
        let user = account_path(false, UserId::from(123456789u64)).unwrap();
        assert!(user.ends_with(&expected_user));

        let expected_bot = PathBuf::from("cache")
            .join("accounts")
            .join("bot_987654321.db");
        let bot = account_path(true, UserId::from(987654321u64)).unwrap();
        assert!(bot.ends_with(&expected_bot));
    }
}
