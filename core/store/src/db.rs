//! ローカルキャッシュ (SQLite)。
//!
//! # 読むのは同期、書くのは投げっぱなし
//!
//! ```text
//!   起動時          Db::open()          主スレッドで同期に読む (数 ms)
//!                        │              **最初のフレームに要る**ので待つ
//!                        ▼
//!   走っている間    db.save_*()  ──▶ 書き込みスレッド (SQLite を専有)
//!                   すぐ返る            **主スレッドは待たない**
//! ```
//!
//! ⚠️ **`Connection` を複数スレッドで共有しない。** SQLite は 1 本の接続を
//! 1 スレッドが持つのが一番素直で速い。錠を掛けて回すより、書き込み専用の
//! スレッドに送りつけるほうが、待ちも競合も起きない。
//!
//! # 書けなくても動く
//!
//! キャッシュは**速くするためだけのもの**である。ディスクが一杯でも、
//! 権限が無くても、壊れていても、アプリは動かなければならない。
//! したがってここの失敗は**記録するだけで、上へ返さない**。
//!
//! # ⚠️ 中身は暗号化されていない
//!
//! `SEC-020` (キャッシュの暗号化) は M2 である。**いまの版は、読んだ
//! メッセージの本文が平文でディスクに残る。** これは仕様が決めた順序だが、
//! 使う人が知らないままでよいことではない。
//!
//! `SEC-021` (ログアウト時の完全削除) は M1 なので [`Db::wipe`] にある。
//!
//! 要件: `NFR-011`, `SEC-021`
//! 仕様: [`spec/02-architecture.md`]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};

use gumicord_model::{Channel, ChannelId, Guild, GuildId, Message, User};

/// 1 チャンネルにつきディスクに残す件数。
///
/// 開いたときに画面が埋まればよく、それ以上は REST で取り直せる。
/// **際限なく貯めると、使うほど起動が遅くなる**
const KEEP_PER_CHANNEL: usize = 200;

/// 書き込みスレッドへの待ち行列の深さ。
///
/// ⚠️ **溢れたら捨てる。** キャッシュのために主スレッドを待たせるのは
/// 本末転倒である。捨てても次の起動で REST から取り直せる
const QUEUE: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("キャッシュを開けない: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("保存場所を決められない")]
    NoHome,
}

/// 起動時に読み出した、前回までの状態。
#[derive(Debug, Default)]
pub struct Snapshot {
    pub guilds: Vec<Guild>,
    /// 前回開いていたチャンネル。**そこを開いた状態で起動する**
    pub last_channel: Option<ChannelId>,
    /// `last_channel` のメッセージ。古い順
    pub messages: Vec<Message>,
    /// 利用者が並べた順。**空なら分からないということ**
    pub guild_order: Vec<GuildId>,
}

/// 書き込みスレッドへ送る仕事。
enum Job {
    Guilds(Vec<Guild>),
    Messages {
        channel: ChannelId,
        list: Vec<Message>,
    },
    LastChannel(ChannelId),
    GuildOrder(String),
    /// 読み出し。**返す先は呼んだ側が渡す**
    Load {
        channel: ChannelId,
        then: Box<dyn FnOnce(Vec<Message>) + Send>,
    },
    /// `SEC-021`: 全部消す
    Wipe,
    /// ここまでの仕事が片付いたことを知らせる
    Barrier(SyncSender<()>),
}

/// ローカルキャッシュ。**複製して構わない。**
#[derive(Clone)]
pub struct Db {
    tx: SyncSender<Job>,
}

impl core::fmt::Debug for Db {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Db")
    }
}

impl Db {
    /// 開いて、**前回までの状態を読み出す**。
    ///
    /// ここだけは同期に読む。最初のフレームに要るものなので、
    /// 待たないと「一瞬空っぽの画面」が出てしまう (`NFR-011`)。
    pub fn open(path: &Path) -> Result<(Db, Snapshot), DbError> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = rusqlite::Connection::open(path)?;
        init(&conn)?;
        let snapshot = read_snapshot(&conn)?;

        // ⚠️ 溢れたら捨てる。**主スレッドを待たせない**
        let (tx, rx) = sync_channel::<Job>(QUEUE);
        std::thread::Builder::new()
            .name("gumicord-cache".to_owned())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // 書けなくても動く。**記録するだけで諦める**
                    if let Err(e) = run(&conn, job) {
                        tracing::warn!(%e, "キャッシュに書けなかった");
                    }
                }
                tracing::debug!("キャッシュの書き込みスレッドを終える");
            })
            .map_err(|_| DbError::NoHome)?;

        Ok((Db { tx }, snapshot))
    }

    fn send(&self, job: Job) {
        // ⚠️ 待たない。溢れていたら捨てる
        if self.tx.try_send(job).is_err() {
            tracing::debug!("キャッシュの待ち行列が詰まっている。今回は捨てる");
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

    /// `SEC-021`: ログアウトしたら**キャッシュを完全に消す**。
    ///
    /// ⚠️ 残しておくと、次に別の人がその機械を使ったときに前の人の
    /// メッセージが読める
    pub fn wipe(&self) {
        self.send(Job::Wipe);
    }
}

/// 既定の置き場。
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

// ─────────────────────────────────────────────── スキーマ

/// ⚠️ **識別子は文字列で持つ。** スノーフレークは 64 ビット符号なしで、
/// SQLite の INTEGER は符号付きである。いまはまだ収まるが、収まらなくなる
/// 日に静かに壊れるより、最初から文字列にしておく
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
    id           TEXT PRIMARY KEY,
    username     TEXT NOT NULL,
    display_name TEXT,
    avatar       TEXT
);

CREATE TABLE IF NOT EXISTS messages (
    id         TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    author_id  TEXT NOT NULL,
    content    TEXT NOT NULL,
    ts         TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS messages_by_channel ON messages(channel_id, id);

CREATE TABLE IF NOT EXISTS state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

fn init(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

fn read_snapshot(conn: &rusqlite::Connection) -> rusqlite::Result<Snapshot> {
    let mut guilds: Vec<Guild> = conn
        .prepare("SELECT id, name, icon FROM guilds")?
        .query_map([], |row| {
            Ok(Guild {
                id: parse_id(row.get::<_, String>(0)?),
                name: row.get(1)?,
                icon: row.get(2)?,
                unavailable: false,
                channels: Vec::new(),
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

    // ⚠️ **順も戻す。** これが無いと、キャッシュから描いた最初の一瞬だけ
    // 順が違い、READY が届いた瞬間に一覧が跳ねる
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

    Ok(Snapshot {
        guilds,
        last_channel,
        messages,
        guild_order,
    })
}

/// そのチャンネルのメッセージを**古い順**で読む。
fn read_messages(
    conn: &rusqlite::Connection,
    channel: ChannelId,
) -> rusqlite::Result<Vec<Message>> {
    // ⚠️ スノーフレークは時刻順なので、**文字列としてではなく長さと
    // 辞書順で**並べる。桁が同じなら辞書順が時刻順に一致する
    conn.prepare(
        "SELECT m.id, m.author_id, m.content, m.ts,
                u.username, u.display_name, u.avatar
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
                display_name: row.get(5)?,
                discriminator: String::new(),
                avatar: row.get(6)?,
                bot: false,
            },
            content: row.get(2)?,
            timestamp: row.get(3)?,
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            referenced_message: None,
        })
    })?
    .collect()
}

/// 文字列の識別子を戻す。**読めなければ 0。**
///
/// ⚠️ ここで倒れない。自分で書いたものが読めないのは異常だが、
/// **キャッシュが壊れているだけでアプリが起動しないほうが困る**
fn parse_id<T: From<u64>>(s: String) -> T {
    T::from(s.parse::<u64>().unwrap_or(0))
}

fn run(conn: &rusqlite::Connection, job: Job) -> rusqlite::Result<()> {
    match job {
        Job::Guilds(guilds) => write_guilds(conn, &guilds),
        Job::Messages { channel, list } => write_messages(conn, channel, &list),
        // ここに届いた時点で、前に送ったものは全部片付いている
        Job::Barrier(reply) => {
            let _ = reply.send(());
            Ok(())
        }
        Job::Load { channel, then } => {
            // ⚠️ 読めなくても必ず呼び戻す。**返事が来ないと画面が
            // 「読み込み中」のまま止まる**
            then(read_messages(conn, channel).unwrap_or_default());
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
            // `SEC-021`。**中身を消してから縮める**
            conn.execute_batch(
                "DELETE FROM messages; DELETE FROM users; DELETE FROM channels;
                 DELETE FROM guilds;  DELETE FROM state;  VACUUM;",
            )
        }
    }
}

fn write_guilds(conn: &rusqlite::Connection, guilds: &[Guild]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;

    // ⚠️ **消してから入れ直す。** 抜けたサーバやチャンネルが残り続けると、
    // 押しても開けない項目が一覧に居座る
    tx.execute("DELETE FROM guilds", [])?;
    tx.execute("DELETE FROM channels", [])?;

    for g in guilds {
        // 殻だけのギルドは書かない。名前が無いので出しようがない
        if g.unavailable || g.name.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT OR REPLACE INTO guilds(id, name, icon) VALUES(?1, ?2, ?3)",
            rusqlite::params![g.id.get().to_string(), g.name, g.icon],
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
            "INSERT OR REPLACE INTO users(id, username, display_name, avatar)
             VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![
                m.author.id.get().to_string(),
                m.author.username,
                m.author.display_name,
                m.author.avatar,
            ],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO messages(id, channel_id, author_id, content, ts)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                m.id.get().to_string(),
                ch,
                m.author.id.get().to_string(),
                m.content,
                m.timestamp,
            ],
        )?;
    }

    // 古いものを落とす。**際限なく貯めると使うほど起動が遅くなる**
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
    /// そのチャンネルのメッセージを読む。**返るのは別スレッドからである。**
    ///
    /// ⚠️ 読みも書き込みスレッドにやらせる。接続を 2 本持つと、片方が
    /// 書いている最中にもう片方が読んで `SQLITE_BUSY` に当たる。
    /// **読みは稀 (チャンネルを開いたとき) なので、順番待ちで困らない。**
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
    /// 送った仕事が全部片付くまで待つ。
    ///
    /// 待ち行列は先入れ先出しなので、**ここに返事が来た時点で前のものは
    /// 全部書けている**。終了時と、試験で結果を確かめるときに使う。
    ///
    /// ⚠️ **走っている間の常用はしない。** 待たないことがこの層の要点である
    pub fn flush(&self) {
        let (tx, rx) = sync_channel(1);
        if self.tx.send(Job::Barrier(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

impl Db {
    /// 並び順を残す。
    ///
    /// ⚠️ **これが無いと、次の起動でキャッシュから描いたときだけ順が違う。**
    /// READY が届いた瞬間に正しい順へ飛ぶので、起動のたびに一覧が跳ねる
    pub fn save_guild_order(&self, order: &[GuildId]) {
        let joined = order
            .iter()
            .map(|g| g.get().to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.send(Job::GuildOrder(joined));
    }
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
            icon: None,
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
            }],
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
                display_name: Some("ｽﾋﾟｷ".to_owned()),
                discriminator: "0".to_owned(),
                avatar: None,
                bot: false,
            },
            content: body.to_owned(),
            timestamp: "2026-08-22T12:34:56+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            referenced_message: None,
        }
    }

    /// 書いたものが**次の起動で読める**。これが C6 の土台である
    #[test]
    fn what_is_written_survives_a_restart() {
        let path = scratch("roundtrip");

        let (db, first) = Db::open(&path).expect("開けない");
        assert!(first.guilds.is_empty(), "新しいはずが空でない");

        db.save_guilds(vec![guild(1, "テスト鯖", 10)]);
        db.save_messages(
            ChannelId::from(10u64),
            vec![message(100, 10, "こんにちは"), message(101, 10, "またね")],
        );
        db.save_last_channel(ChannelId::from(10u64));
        db.flush();
        drop(db);

        let (_db, again) = Db::open(&path).expect("開き直せない");
        assert_eq!(again.guilds.len(), 1);
        assert_eq!(again.guilds[0].name, "テスト鯖");
        assert_eq!(again.guilds[0].channels.len(), 1);
        assert_eq!(again.guilds[0].channels[0].topic.as_deref(), Some("話題"));
        assert_eq!(again.last_channel, Some(ChannelId::from(10u64)));

        let bodies: Vec<_> = again.messages.iter().map(|m| &*m.content).collect();
        assert_eq!(bodies, vec!["こんにちは", "またね"], "古い順になっていない");
        // 送信者も引ける
        assert_eq!(again.messages[0].author.name(), "ｽﾋﾟｷ");
    }

    /// **抜けたサーバは残らない。** 押しても開けない項目が居座ると困る
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

    /// `SEC-021`: ログアウトで**跡形もなく消える**
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

        // ⚠️ 消したものが**ファイルの中に残っていない**
        let raw = std::fs::read(&path).unwrap_or_default();
        assert!(
            !raw.windows(12).any(|w| w == "ないしょ".as_bytes()),
            "消したはずの本文がファイルに残っている"
        );
    }

    /// 読めなくても呼び戻す。**返事が来ないと画面が止まる**
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
}
