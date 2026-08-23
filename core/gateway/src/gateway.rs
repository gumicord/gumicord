//! Discord Gateway への接続 (C2)。
//!
//! 責務: 接続 / identify / ハートビート / resume / zstd-stream の解凍 /
//! イベント配信 (`NFR-010`, `NFR-020`)。
//!
//! # 呼び出し側から見た形
//!
//! [`Gateway::next`] を回すだけでよい。**切断も再接続もこの中で起きる。**
//! 呼び出し側が知るのは「いま繋がっているか」と「何が届いたか」だけである。
//!
//! ```text
//!   loop {
//!       match gateway.next().await {
//!           Event::Ready(r)        => 一覧を作り直す
//!           Event::Resumed         => 取りこぼしが埋まった。何もしなくてよい
//!           Event::Dispatch { .. } => 状態を更新する
//!           Event::Reconnecting{..}=> 「再接続中」と出す
//!           Event::Fatal(reason)   => 諦める。トークンを捨てる場合もある
//!       }
//!   }
//! ```
//!
//! **`Fatal` 以外で `next` が終わることはない。** 網が切れている間は
//! 待ち時間を伸ばしながら繋ぎ直し続ける。
//!
//! # 接続シーケンス
//!
//! ```text
//!   ├─ WebSocket 確立 ───────────▶│   実測 338〜390 ms
//!   │◀────────────── op=10 Hello ─┤   heartbeat_interval = 41250 ms
//!   ├─ op=2 Identify ────────────▶│   (resume なら op=6)
//!   │◀───────── op=0 t=READY ─────┤   実測 672〜1120 ms
//!   ├─ op=1 Heartbeat ───────────▶│   最初は interval × jitter 後
//!   │◀────────── op=11 ACK ───────┤
//! ```
//!
//! ⚠️ **resume は `resume_gateway_url` へ繋ぐ。** READY で渡されるリージョン
//! 別のホストで、初回と同じ `gateway.discord.gg` に戻ると別のサーバへ
//! 割り当てられ、resume に失敗しうる。
//!
//! # ハートビートの ACK は生存確認である
//!
//! 送る番が来たのに前回の ACK がまだ来ていない ⇒ **その接続は死んでいる**。
//! 網が切れても TCP はすぐには気付かないので、これが唯一の検知手段になる。
//! 待たずに捨てて resume する。
//!
//! # identify では嘘をつかない (`NFR-020`)
//!
//! ⚠️ 公式クライアントの `client_build_number` を騙るようなことはしない。
//! `NFR-020` の「公式クライアントと同等の identify プロパティ」は**検出を
//! 回避するためではなく、サーバーに嘘の情報を渡さないため**の要件である。
//! 名乗るのは Gumicord であり、版も OS も実際の値を送る。
//!
//! 仕様: [`spec/09-discord-protocol.md`] 2〜6 章

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use gumicord_model::{ChannelId, CurrentUser, Guild, GuildId, MessageId, Token};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::zstd_stream::ZstdStream;

/// 初回の接続先 ([`spec/09-discord-protocol.md`] 1 章)
const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json&compress=zstd-stream";
/// resume 先に付ける問い合わせ。**`resume_gateway_url` には付いていない**
const QUERY: &str = "?v=10&encoding=json&compress=zstd-stream";

/// 再接続を諦めない代わりに、間隔を伸ばす上限
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

/// もう繋がらない理由。**呼び出し側が後始末を決める。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fatal {
    /// トークンが弾かれた (`FR-004`)。**捨ててログイン画面へ戻す**
    Unauthorized,
    /// こちらの送り方が誤っている。何度やっても同じ
    Rejected { code: u16, reason: String },
}

/// Gateway から届くもの。
#[derive(Debug, Clone)]
pub enum Event {
    /// 繋がって初期状態が届いた
    Ready(Box<Ready>),
    /// 取りこぼしを埋め終えた。**`Ready` は来ない**
    Resumed,
    /// まだ型を付けていない出来事。
    ///
    /// ⚠️ **捨てずにそのまま渡す。** どれを使うかを決めるのは Store (C5)
    /// であって、ここではない
    Dispatch {
        kind: String,
        data: serde_json::Value,
    },
    /// 切れた。**繋ぎ直しはこの中で続く。** 画面に出すためだけの知らせ
    Reconnecting { reason: String, wait: Duration },
    /// 諦めた
    Fatal(Fatal),
}

/// READY で届く初期状態。
#[derive(Debug, Clone, Deserialize)]
pub struct Ready {
    pub user: CurrentUser,
    pub session_id: String,
    /// resume で繋ぐ先。**リージョン別のホストが返る**
    #[serde(default)]
    pub resume_gateway_url: Option<String>,
    /// ⚠️ **落ちているギルドは識別子だけの殻で来る。**
    ///
    /// 読めない要素があっても飛ばして残りを採る。**殻が 1 つ混ざっただけで
    /// READY 全体が読めなくなり、Gateway が永久に繋ぎ直し続けた**ことがある
    #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
    pub guilds: Vec<Guild>,
    /// 利用者の設定。**中身は base64 された protobuf** である。
    ///
    /// ここにサーバの並び順が入っている ([`crate::guild_order`])
    #[serde(default)]
    pub user_settings_proto: Option<String>,
    /// どこまで読んだか (`FR-042`)。
    ///
    /// ⚠️ **形が 2 通りある。** 古い版は配列そのもの、新しい版は
    /// `{ "entries": [...] }` で来る ([`ReadStates`])
    #[serde(default)]
    pub read_state: Option<ReadStates>,
}

/// READY の `read_state`。**2 通りの形をそのまま受ける入れ物。**
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ReadStates {
    /// 新しい版
    Wrapped {
        #[serde(default, deserialize_with = "gumicord_model::de::lenient_vec")]
        entries: Vec<ReadState>,
    },
    /// 古い版。**配列がそのまま来る**
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

/// 1 チャンネルぶんの「どこまで読んだか」。
///
/// ⚠️ **チャンネル以外のものも混ざる。** ギルドのイベントや実績にも
/// 読んだ印が付いており、同じ配列で来る。`id` で引けないものは
/// **黙って落ちる**ので、選り分けは要らない
#[derive(Debug, Clone, Deserialize)]
pub struct ReadState {
    /// チャンネルの識別子
    pub id: ChannelId,
    /// ここまで読んだ。**これより新しい発言があれば未読である**
    #[serde(default)]
    pub last_message_id: Option<MessageId>,
    /// 自分宛ての未読の数。**サーバが数えている**
    #[serde(default)]
    pub mention_count: u32,
}

impl Ready {
    /// 利用者が Discord で並べ替えたサーバの順。
    ///
    /// ⚠️ **名前順で出してはいけない。** 自分で並べた順以外で並ぶと、
    /// 「自分のサーバ一覧ではない」ものになる。
    ///
    /// 取り出せなければ空。呼び出し側は READY の順に落とす
    pub fn guild_order(&self) -> Vec<GuildId> {
        let Some(proto) = &self.user_settings_proto else {
            return Vec::new();
        };
        crate::guild_order::from_settings_proto(proto, &self.known_guilds())
    }

    /// サーバ一覧のフォルダ。**中身が 1 つのただのサーバも混ざる**
    pub fn guild_folders(&self) -> Vec<crate::guild_order::Folder> {
        let Some(proto) = &self.user_settings_proto else {
            return Vec::new();
        };
        crate::guild_order::folders_from_settings_proto(proto, &self.known_guilds())
    }

    /// 自分のステータス。
    ///
    /// ⚠️ **読めなければ `None`。** 繋がっていることを根拠に
    /// 「オンライン」と名乗らない。取り込み中にしている人に対して嘘になる
    pub fn status(&self) -> Option<crate::status::Status> {
        crate::status::from_settings_proto(self.user_settings_proto.as_deref()?)
    }

    fn known_guilds(&self) -> std::collections::HashSet<u64> {
        self.guilds.iter().map(|g| g.id.get()).collect()
    }
}

/// 接続を保ち続けるもの。
pub struct Gateway {
    token: Token,
    conn: Option<Connection>,
    /// resume に要る 3 つ組。**切断を跨いで持ち続ける**
    session: Option<SessionInfo>,
    backoff: Duration,
    /// 次に返す `Reconnecting`。`next` の頭で吐き出す
    pending_notice: Option<Event>,
    /// 見ているギルドと、その中で開いているチャンネル。
    ///
    /// ⚠️ **繋ぎ直すたびに送り直す。** 購読は接続に紐づく
    wanted: std::collections::HashMap<GuildId, ChannelId>,
    requests: tokio::sync::mpsc::UnboundedReceiver<(GuildId, ChannelId)>,
}

/// 「このチャンネルを見ている」と Gateway へ伝える手。
///
/// # なぜ要るのか
///
/// ⚠️ **利用者トークンでは、黙っていても `MESSAGE_CREATE` は来ない。**
///
/// READY のギルドには `"lazy": true` が付いている。公式クライアントは
/// 画面に出ているギルドだけを `op 14` で購読し、Discord はそのギルドの
/// 出来事だけを送る。**何百のサーバに入っている利用者へ全部送るのは、
/// 双方にとって無駄だからである。**
///
/// 購読しないと、新着も入力中の表示も一切届かない。
#[derive(Clone, Debug)]
pub struct Subscriptions {
    tx: tokio::sync::mpsc::UnboundedSender<(GuildId, ChannelId)>,
}

impl Subscriptions {
    /// そのギルドの、そのチャンネルを見ていると伝える。
    ///
    /// **何度呼んでもよい。** 同じものは送り直されるだけである
    pub fn watch(&self, guild: GuildId, channel: ChannelId) {
        let _ = self.tx.send((guild, channel));
    }
}

/// 繋ぎ先の URL。`resume` するなら**リージョン別のホスト**へ。
///
/// ⚠️ **経路の `/` を落とさない。**
///
/// `resume_gateway_url` は `wss://gateway-us-east1-b.discord.gg` の形で来る。
/// ここへ問い合わせをそのまま繋ぐと `wss://host?v=10…` になり、経路が
/// **空**の URL になる。`http` はこれを `?v=10…` として持つので、
/// 送られる要求行が
///
/// ```text
///   GET ?v=10&encoding=json&compress=zstd-stream HTTP/1.1
/// ```
///
/// になる。要求先として不正なので Discord は **400 Bad Request** を返し、
/// 繋がらない。しかも `session` を持ったままなので**同じ URL へ延々と
/// 繋ぎ直し続け、二度と復帰しなかった**。
fn connect_url(resume_host: Option<&str>) -> String {
    match resume_host {
        Some(host) => format!("{}/{QUERY}", host.trim_end_matches('/')),
        None => GATEWAY.to_owned(),
    }
}

/// `op 14` — 見ているものを伝える。
///
/// `channels` の `[[0, 99]]` は「メンバー一覧の 0〜99 番目が要る」という
/// 意味である。**この形でないと購読そのものが成立しない**ので、一覧を
/// 出さない画面でも送る。
///
/// ⚠️ **頼んだ範囲しか来ない。** これが [`crate::member_list`] が
/// 100 人目で止まる理由である。巻いた先を見せるには、範囲を広げて
/// 送り直す必要がある
fn subscribe(guild: GuildId, channel: ChannelId) -> serde_json::Value {
    json!({
        "op": OP_GUILD_SUBSCRIBE,
        "d": {
            "guild_id": guild.get().to_string(),
            "typing": true,
            "threads": false,
            "activities": true,
            "channels": { channel.get().to_string(): [[0, 99]] },
        },
    })
}

impl core::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // ⚠️ トークンを出さない (`SEC-001`)
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
    /// 繋ぐものと、外から購読を頼む手を作る。
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

    /// 頼まれている購読を全部送り直す。
    ///
    /// ⚠️ **繋ぎ直すたびに送る。** 購読は接続に紐づくので、resume でも
    /// identify でも、新しい接続には引き継がれない
    async fn resend_subscriptions(&mut self) -> Result<(), GatewayError> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        for (guild, channel) in self.wanted.clone() {
            conn.send(subscribe(guild, channel)).await?;
        }
        Ok(())
    }

    /// 次の出来事まで進める。
    ///
    /// **[`Event::Fatal`] を返した後は呼ばないこと。** 呼んでも同じものを
    /// 返し続ける。
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
                        // ⚠️ **開けなかった resume 先に固執しない。**
                        //
                        // resume は「繋がった上で取りこぼしを埋める」もので
                        // あって、繋がらない相手に何度掛けても意味がない。
                        // 持ち越すと**同じ失敗を延々と繰り返し、二度と
                        // 復帰しない**。次は identify からやり直す
                        if self.session.take().is_some() {
                            tracing::warn!("resume 先へ繋げない。identify からやり直す");
                        }
                        let wait = self.grow_backoff();
                        tracing::warn!(error = %e, wait_ms = wait.as_millis() as u64, "繋げない");
                        // 待つ前に知らせる。**待ってから知らせると、
                        // 画面が黙ったまま最大 1 分固まったように見える**
                        self.pending_notice = Some(Event::Reconnecting {
                            reason: e.to_string(),
                            wait,
                        });
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                }
            }

            let conn = self.conn.as_mut().expect("直前に開いた");
            match conn
                .pump(&mut self.session, &mut self.requests, &mut self.wanted)
                .await
            {
                Ok(Some(event)) => {
                    // 何か届いたということは繋がっている。待ち時間を戻す
                    self.backoff = BACKOFF_MIN;
                    // ⚠️ **購読は接続に紐づく。** 繋ぎ直したら送り直さないと、
                    // 新着も入力中の表示も二度と来ない
                    if matches!(event, Event::Ready(_) | Event::Resumed)
                        && let Err(e) = self.resend_subscriptions().await
                    {
                        tracing::warn!(error = %e, "購読を送り直せなかった");
                    }
                    return event;
                }
                // 心拍など、内部で片付いたもの
                Ok(None) => continue,
                Err(e) => {
                    self.conn = None;
                    if let Some(fatal) = fatal_of(&e) {
                        // ⚠️ 二度と resume できない。持ち越さない
                        self.session = None;
                        return Event::Fatal(fatal);
                    }
                    if !recoverable_session(&e) {
                        // セッションは死んだが、identify からならやり直せる
                        self.session = None;
                    }
                    let wait = self.grow_backoff();
                    tracing::warn!(error = %e, "切れた。繋ぎ直す");
                    self.pending_notice = Some(Event::Reconnecting {
                        reason: e.to_string(),
                        wait,
                    });
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// 待ち時間を倍にする。**上限で止める**
    fn grow_backoff(&mut self) -> Duration {
        let wait = self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        wait
    }

    /// 繋いで Hello を受け、identify か resume を送る。
    async fn open(&mut self) -> Result<(), GatewayError> {
        let url = connect_url(self.session.as_ref().map(|s| &*s.url));

        crate::install_crypto_provider();
        let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
        let mut conn = Connection::new(ws)?;

        let hello = conn.wait_for_hello().await?;
        conn.heartbeat = Duration::from_millis(hello);

        match &self.session {
            Some(s) => {
                tracing::debug!(session = %s.id, "resume する");
                conn.last_seq = s.seq;
                conn.send(json!({
                    "op": OP_RESUME,
                    "d": { "token": self.token.expose(), "session_id": s.id, "seq": s.seq },
                }))
                .await?;
            }
            None => {
                tracing::debug!("identify する");
                conn.send(identify(&self.token)).await?;
            }
        }

        // 最初の心拍は間隔 × ゆらぎの後。
        // **全クライアントが同時に叩かないため** (仕様 5 章)
        conn.schedule_first_heartbeat();
        self.conn = Some(conn);
        Ok(())
    }
}

// ─────────────────────────────────────────────── オペコード

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_RESUME: u8 = 6;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESSION: u8 = 9;
const OP_HELLO: u8 = 10;
const OP_HEARTBEAT_ACK: u8 = 11;
/// 見ているギルドとチャンネルを伝える。**利用者トークンでは必須**
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

/// identify のペイロード ([`spec/09-discord-protocol.md`] 3 章)。
///
/// ⚠️ **公式クライアントを騙らない** (`NFR-020`)。`browser` も `device` も
/// Gumicord である。版と OS は実際の値を送る。
fn identify(token: &Token) -> serde_json::Value {
    json!({
        "op": OP_IDENTIFY,
        "d": {
            "token": token.expose(),
            "capabilities": CAPABILITIES,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "Gumicord",
                "device": "Gumicord",
                "system_locale": locale(),
                "client_version": env!("CARGO_PKG_VERSION"),
                "release_channel": "stable",
            },
            "compress": false,
            "client_state": { "guild_versions": {} },
        },
    })
}

/// 利用者トークンで要求する機能の組 ([`spec/09-discord-protocol.md`] 3 章)。
///
/// ⚠️ **意味の内訳は未検証である。** 仕様に載っている値をそのまま送っている。
/// 分かったら名前付きの定数に割る
const CAPABILITIES: u32 = 161789;

/// システムの言語。分からなければ英語を名乗る。
///
/// **嘘をつくくらいなら既定を送る** (`NFR-020`)
fn locale() -> String {
    std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(|s| s.replace('_', "-")))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "en-US".to_owned())
}

// ─────────────────────────────────────────────── 接続 1 本

/// 接続 1 本ぶんの状態。**繋ぎ直したら丸ごと捨てる。**
struct Connection {
    ws: Ws,
    /// ⚠️ 接続の生存期間中ずっと持つ。フレームを跨ぐ 1 本のストリームである
    zstd: ZstdStream,
    heartbeat: Duration,
    /// 次に心拍を送る時刻
    next_beat: tokio::time::Instant,
    /// 前回の心拍に ACK が返ったか。**返っていなければその接続は死んでいる**
    acked: bool,
    last_seq: Option<u64>,
    /// 1 枚のフレームに複数入っていたぶんの残り
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

    /// 最初の心拍を `間隔 × ゆらぎ` 後に置く。
    ///
    /// ⚠️ **乱数の質は問題ではない。** 目的は世界中のクライアントが同じ瞬間に
    /// 叩かないようにすることだけなので、時刻の端数で足りる
    fn schedule_first_heartbeat(&mut self) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter = nanos as f64 / 1e9;
        self.next_beat = tokio::time::Instant::now() + self.heartbeat.mul_f64(jitter);
    }

    /// Hello を待って `heartbeat_interval` を返す。
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

    /// 1 歩進める。返すものが無ければ `Ok(None)`。
    ///
    /// ⚠️ **ここで待つものは全部 cancel-safe でなければならない。**
    /// `select!` は負けたほうの未来を捨てるので、送信の途中で捨てられると
    /// WebSocket の書き込みが半端なところで切れる。だから**送るのは
    /// 勝ったあと**にしかしない。
    async fn pump(
        &mut self,
        session: &mut Option<SessionInfo>,
        requests: &mut tokio::sync::mpsc::UnboundedReceiver<(GuildId, ChannelId)>,
        wanted: &mut std::collections::HashMap<GuildId, ChannelId>,
    ) -> Result<Option<Event>, GatewayError> {
        if let Some(payload) = self.queued.pop_front() {
            return self.handle(payload, session).await;
        }

        enum Step {
            Beat,
            Received(Option<()>),
            Watch(Option<(GuildId, ChannelId)>),
        }

        let step = tokio::select! {
            _ = tokio::time::sleep_until(self.next_beat) => Step::Beat,
            got = self.recv() => Step::Received(got?),
            got = requests.recv() => Step::Watch(got),
        };

        match step {
            Step::Beat => {
                if !self.acked {
                    // ⚠️ 網が切れても TCP はすぐには気付かない。
                    // **これが唯一の検知手段である**
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
            Step::Watch(Some((guild, channel))) => {
                // ⚠️ **同じ購読を送り直さない。**
                //
                // 呼ぶ側は「毎回伝える」でよい — 見ているものが変わった
                // ことを言い落とすほうが困るからである。だが**同じことを
                // 何度も線に流してよい理由にはならない**。
                //
                // 実機では 1 つのチャンネルを開いているだけで数百回送られ、
                // Discord に**レート制限で切られた** (`4008`)。切れては
                // 繋ぎ直し、繋ぎ直しては送り直す循環になっていた。
                //
                // 繋ぎ直したときは [`Gateway::resend_subscriptions`] が
                // 改めて全部送るので、ここで覚えていて構わない。
                if wanted.get(&guild) == Some(&channel) {
                    return Ok(None);
                }
                wanted.insert(guild, channel);
                tracing::debug!(%guild, %channel, "購読する");
                self.send(subscribe(guild, channel)).await?;
                Ok(None)
            }
            // 頼む側が居なくなった。**接続を切る理由にはならない**
            Step::Watch(None) => Ok(None),
        }
    }

    /// フレームを 1 枚読み、解凍して `queued` へ積む。
    ///
    /// 接続が終わったら `Ok(None)`。**空のフレームは誤りではない**
    async fn recv(&mut self) -> Result<Option<()>, GatewayError> {
        let Some(message) = self.ws.next().await else {
            return Ok(None);
        };

        let plain = match message? {
            Message::Binary(bytes) => self.zstd.push(&bytes)?,
            // 圧縮を頼んでいるので普通は来ないが、来たらそのまま読む
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Close(frame) => {
                let code = frame.map(|f| u16::from(f.code)).unwrap_or(CLOSE_ABNORMAL);
                return Err(GatewayError::Closed(code));
            }
            // ping/pong は tokio-tungstenite が返す
            _ => return Ok(Some(())),
        };

        if plain.is_empty() {
            // フレームを跨いだ途中。次を待つ
            return Ok(Some(()));
        }

        // ⚠️ 1 枚に複数の JSON が入っていることがある。連結したまま読む
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
                        // ⚠️ **形が変わったときにここだけを見れば分かるように
                        // しておく。** Discord は同じ名前のフィールドの
                        // 入れ子をこちらに断りなく変えてくる
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            log_ready_shape(&data);
                        }
                        let ready: Ready = serde_json::from_value(data)?;
                        // resume 先が来なければ初回の URL へ戻る。
                        // 落ちるよりは「別サーバに当たるかもしれない」ほうがまし
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
            // サーバから催促された。**即座に返す**
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
            // 繋ぎ直せと言われた。resume はできる
            OP_RECONNECT => Err(GatewayError::Closed(CLOSE_RECONNECT)),
            OP_INVALID_SESSION => {
                // d が true なら resume し直せる。false ならセッションごと死んだ
                let resumable = payload.d.and_then(|d| d.as_bool()).unwrap_or(false);
                if !resumable {
                    *session = None;
                }
                Err(GatewayError::Closed(CLOSE_INVALID_SESSION))
            }
            // Hello は open が食べている。ここへ来るのは二重の Hello だけ
            other => {
                tracing::debug!(op = other, "知らないオペコード。読み飛ばす");
                Ok(None)
            }
        }
    }
}

// ─────────────────────────────────────────────── 切断の分岐

/// ACK が返らないまま次の番が来た。**内部で作る番号**
const CLOSE_NO_ACK: u16 = 4_900;
/// op=7 で繋ぎ直しを指示された
const CLOSE_RECONNECT: u16 = 4_901;
/// op=9。セッションの生死は `session` 側で決めてある
const CLOSE_INVALID_SESSION: u16 = 4_902;
/// 何も言われずに切れた
const CLOSE_ABNORMAL: u16 = 1_006;

/// もう繋いでも無駄な切断か ([`spec/09-discord-protocol.md`] 6 章)。
///
/// ⚠️ **ここに挙げていないものは全部やり直す。** Discord は予告なく
/// コードを足すので、知らないコードを「諦める」側に倒すと、直せる切断で
/// 利用者を追い出すことになる。
fn fatal_of(error: &GatewayError) -> Option<Fatal> {
    let GatewayError::Closed(code) = error else {
        return None;
    };
    match code {
        // 認証失敗。**トークンを捨てる** (`FR-004`)
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

/// その切断の後も **resume を試してよい**か。
///
/// 駄目なら identify からやり直す。⚠️ **迷ったら resume 側に倒す。**
/// 無駄な resume は 1 往復で失敗が分かるだけだが、無駄な identify は
/// 取りこぼしたイベントを永久に失う。
fn recoverable_session(error: &GatewayError) -> bool {
    match error {
        // 網の都合。セッションは生きている
        GatewayError::Connect(_) | GatewayError::Decompress(_) | GatewayError::Decode(_) => true,
        GatewayError::NoHello => true,
        GatewayError::Closed(code) => !matches!(
            *code,
            // 4007: seq が不正 / 4009: セッション期限切れ
            4007 | 4009 | CLOSE_INVALID_SESSION
        ),
    }
}

/// READY の中の guilds が**どういう形で来ているか**を記録に残す。
///
/// # なぜ残すのか
///
/// 一覧が空になったとき、原因が「入っていない」のか「読めていない」のかは
/// 外から見分けが付かない。実際に **11 件届いていたのに 1 件も出せなかった**
/// ことがある (名前が `properties` の中に移っていた)。
///
/// ⚠️ **中身は出さない。鍵の名前だけを出す。** ここには利用者のサーバ名も
/// トークンも通るので、丸ごと記録すると秘密が残る
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

    /// ⚠️ **要求行に載る経路が `/` で始まること。**
    ///
    /// `resume_gateway_url` には経路が付いていない。問い合わせをそのまま
    /// 繋ぐと `GET ?v=10… HTTP/1.1` になり、Discord は 400 を返す。
    /// 繋がらないまま `session` を持ち続けて**二度と復帰しなかった**
    #[test]
    fn the_resume_url_keeps_its_path() {
        use tokio_tungstenite::tungstenite::http::Uri;

        let target = |url: &str| {
            url.parse::<Uri>()
                .expect("URL として読める")
                .path_and_query()
                .expect("経路がある")
                .as_str()
                .to_owned()
        };

        for host in [
            "wss://gateway-us-east1-b.discord.gg",
            // 末尾に `/` が付いてきても二重にしない
            "wss://gateway-us-east1-b.discord.gg/",
        ] {
            let url = connect_url(Some(host));
            assert!(
                url.starts_with(&format!("{}/?", host.trim_end_matches('/'))),
                "{url}"
            );
            assert!(target(&url).starts_with("/?v=10"), "{url}");
        }

        // 初回も同じ形である
        assert!(target(&connect_url(None)).starts_with("/?v=10"));
    }

    /// トークンが弾かれたら諦める。**捨ててログイン画面へ戻すため**
    #[test]
    fn an_invalid_token_is_fatal() {
        assert_eq!(fatal_of(&closed(4004)), Some(Fatal::Unauthorized));
    }

    /// **知らないコードは諦めない。** Discord は予告なく足す
    #[test]
    fn unknown_close_codes_are_retried() {
        for code in [4000, 4001, 4002, 4003, 4005, 4008, 4020, 4999, 1006] {
            assert_eq!(fatal_of(&closed(code)), None, "コード {code} で諦めている");
        }
    }

    /// 網の都合で切れただけならセッションは生きている
    #[test]
    fn a_network_hiccup_keeps_the_session() {
        assert!(recoverable_session(&GatewayError::NoHello));
        assert!(recoverable_session(&closed(1006)));
        assert!(recoverable_session(&closed(CLOSE_NO_ACK)));
        assert!(recoverable_session(&closed(CLOSE_RECONNECT)));
    }

    /// セッションが死んだと言われたら identify からやり直す
    #[test]
    fn an_expired_session_is_not_resumed() {
        assert!(!recoverable_session(&closed(4007)));
        assert!(!recoverable_session(&closed(4009)));
        assert!(!recoverable_session(&closed(CLOSE_INVALID_SESSION)));
    }

    /// 待ち時間は倍々に伸び、**上限で止まる**
    #[test]
    fn the_backoff_grows_but_is_capped() {
        let (mut g, _subs) = Gateway::new(Token::new("x"));
        assert_eq!(g.grow_backoff(), BACKOFF_MIN);
        assert_eq!(g.grow_backoff(), BACKOFF_MIN * 2);
        assert_eq!(g.grow_backoff(), BACKOFF_MIN * 4);

        for _ in 0..20 {
            g.grow_backoff();
        }
        assert_eq!(g.grow_backoff(), BACKOFF_MAX, "上限を超えて伸びている");
    }

    /// ⚠️ **トークンが Debug に出ない** (`SEC-001`)
    #[test]
    fn the_token_never_appears_in_debug() {
        let (g, _subs) = Gateway::new(Token::new("mfa.SUPER_SECRET"));
        let shown = format!("{g:?}");
        assert!(
            !shown.contains("SUPER_SECRET"),
            "トークンが漏れている: {shown}"
        );
    }

    /// identify に嘘が入っていない (`NFR-020`)。
    /// **公式クライアントの版を騙らない**
    #[test]
    fn identify_does_not_impersonate_the_official_client() {
        let payload = identify(&Token::new("t"));
        let props = &payload["d"]["properties"];

        assert_eq!(props["browser"], "Gumicord");
        assert_eq!(props["device"], "Gumicord");
        assert_eq!(props["client_version"], env!("CARGO_PKG_VERSION"));
        assert!(
            props.get("client_build_number").is_none(),
            "公式の build number を騙っている"
        );
        assert_eq!(payload["d"]["token"], "t");
    }

    /// READY が読める。**知らないフィールドで落ちない**
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
        .expect("READY を読めない");

        assert_eq!(ready.session_id, "abc");
        assert_eq!(ready.guilds.len(), 1);
        assert_eq!(ready.user.user.display_name(), "ねんねこ");
    }

    /// `resume_gateway_url` が無くても落ちない。
    /// **無いほうが普通ではないが、無くても動けるべきである**
    #[test]
    fn ready_without_a_resume_url_still_parses() {
        let ready: Ready =
            serde_json::from_str(r#"{"user":{"id":"1","username":"x"},"session_id":"s"}"#).unwrap();
        assert!(ready.resume_gateway_url.is_none());
        assert!(ready.guilds.is_empty());
    }

    /// 1 枚のフレームに複数の JSON が入っていても全部読む
    #[test]
    fn several_payloads_in_one_frame_are_all_read() {
        let raw = br#"{"op":11}{"op":0,"t":"MESSAGE_CREATE","s":5,"d":{}}"#;
        let payloads: Vec<Payload> = serde_json::Deserializer::from_slice(raw)
            .into_iter::<Payload>()
            .collect::<Result<_, _>>()
            .expect("連結した JSON を読めない");

        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].op, OP_HEARTBEAT_ACK);
        assert_eq!(payloads[1].s, Some(5));
        assert_eq!(payloads[1].t.as_deref(), Some("MESSAGE_CREATE"));
    }
}
