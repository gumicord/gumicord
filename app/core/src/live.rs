//! 本物の Discord のデータ。Store (C5) / Gateway (C2) / REST (C3) の結線。
//!
//! # 3 つの出所を 1 つの状態に落とす
//!
//! ```text
//!   キャッシュ (SQLite)  ──▶ ┐
//!   REST (過去 50 件)    ──▶ ├─▶ Store ──▶ 画面
//!   Gateway (これから)   ──▶ ┘
//! ```
//!
//! **どれか 1 つでは成立しない。**
//!
//! - キャッシュは**すぐ出る**が古い
//! - REST は正しいが**往復を待つ**
//! - Gateway は繋いだ後のものしか運ばない
//!
//! チャンネルを開いたら、まずキャッシュを出し、REST が返ったら差し替え、
//! そこから先は Gateway が追いかける。
//!
//! ⚠️ **REST が先に返ったら、後から来たキャッシュで上書きしない。**
//! 古いもので新しいものを潰すことになる。
//!
//! # 起動時はキャッシュから描く (`NFR-011`, C6)
//!
//! S4 の実測では Gateway の READY まで 672〜1120 ms かかる。
//! `NFR-001` (コールドスタート 500 ms) に**入らない**ので、READY を待って
//! から描くという選択肢は最初から無い。
//!
//! # 主スレッドは止めない
//!
//! [`crate::session`] と同じ形である。仕事は [`tokio`] と書き込みスレッドの
//! 上で進み、結果だけがチャネルで戻り、[`Waker`] が主スレッドを起こす。

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use gumicord_gateway::{Event, Fatal, Gateway, Ready, Subscriptions, status::Status};
use gumicord_model::{ChannelId, Guild, GuildId, Message, Token, UserId};
use gumicord_platform::Waker;
use gumicord_rest::RestClient;
use gumicord_store::{Db, GuildRow, Store};

/// 1 チャンネルにつき最初に取ってくる件数。
///
/// Discord の API の上限が 100。50 にしてあるのは、**開いた瞬間に見える分
/// より少し多い**あたりで、往復を待たせないため
const BACKLOG: u8 = 50;

/// いま入力中の人が消えるまでの時間。
///
/// Discord は 10 秒で消し、まだ打っていれば `TYPING_START` を送り直す。
/// **こちらが先に消してしまうと、打っている最中に表示が点滅する**
const TYPING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// 入力中の人 1 人。
#[derive(Debug, Clone)]
struct Typist {
    user: UserId,
    name: String,
    at: std::time::Instant,
}

/// Gateway との繋がり具合。**画面に出すためのものである。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// まだ繋いでいない
    Idle,
    Connecting,
    Up,
    /// 切れた。**繋ぎ直しは Gateway の中で続いている**
    Reconnecting(String),
    /// 諦めた
    Down(String),
}

impl Link {
    /// 画面に出す一行。繋がっているときは**何も出さない**。
    ///
    /// 正常な状態をわざわざ知らせると、異常のときの一行が埋もれる
    pub fn hint(&self) -> Option<String> {
        match self {
            Link::Up | Link::Idle => None,
            Link::Connecting => Some("接続しています…".to_owned()),
            Link::Reconnecting(why) => Some(format!("再接続しています… ({why})")),
            Link::Down(why) => Some(format!("接続できません: {why}")),
        }
    }
}

/// 背景から主スレッドへ流れる知らせ。
#[derive(Debug)]
pub enum LiveEvent {
    Ready(Box<Ready>),
    /// Gateway から届いた新着
    Posted(Box<Message>),
    /// ギルドの中身が届いた・変わった。**殻だった分がここで埋まる**
    GuildChanged(Box<Guild>),
    /// キャッシュから読めた分。**REST より先に出る**
    Cached {
        channel: ChannelId,
        list: Vec<Message>,
    },
    /// REST で取ってきた過去分。**古い順に並べ替えてある**
    Backlog {
        channel: ChannelId,
        list: Vec<Message>,
    },
    /// もっと遡った分。**古い順で、いま持っているものより前に付く**。
    ///
    /// 空なら**そこが一番古い**。もう頼まない
    Older {
        channel: ChannelId,
        list: Vec<Message>,
    },
    /// メンバー一覧の差分 (`FR-043`)
    Members(Box<gumicord_gateway::member_list::MemberListUpdate>),
    /// 誰かが打ち始めた
    Typing {
        channel: ChannelId,
        user: UserId,
        name: String,
    },
    Link(Link),
    /// トークンが弾かれた (`FR-004`)。**鍵束もキャッシュも捨てる**
    TokenRejected,
}

/// 本物のデータと、それを運ぶ背景の仕事。
pub struct Live {
    tx: Sender<LiveEvent>,
    rx: Receiver<LiveEvent>,
    rt: Option<tokio::runtime::Handle>,
    rest: Option<RestClient>,
    waker: Option<Waker>,
    started: bool,

    store: Store,
    /// ローカルキャッシュ。**開けなくてもアプリは動く**
    db: Option<Db>,
    link: Link,
    /// 取りに行った (行っている) チャンネル。**二重に叩かないため**
    requested: HashSet<ChannelId>,
    /// いま遡っている最中のチャンネル。
    ///
    /// ⚠️ **これが無いと、上端で待っている間に何回も叩く。** スクロールは
    /// 1 回の操作で何度も来るので、1 回目の応答を待たずに次を投げてしまう
    paging: HashSet<ChannelId>,
    /// これ以上古いものが無いチャンネル。**先頭に着いたら二度と頼まない**
    exhausted: HashSet<ChannelId>,
    /// 上へ足したので、見ている場所を動かしてほしくない。
    /// **1 回だけ効く** ([`Live::take_prepended`])
    prepended: bool,
    /// トークンが弾かれた。アプリが後始末をしたら下ろす
    rejected: bool,
    /// 前回開いていたチャンネル (`NFR-011`)
    last_channel: Option<ChannelId>,
    /// 「見ている」と Gateway へ伝える手。**これが無いと新着が来ない**
    subs: Option<Subscriptions>,
    /// いま画面に出ているチャンネル。**開いている間は読んだことにする**
    watching: Option<ChannelId>,
    /// チャンネルごとの、いま入力中の人。**残さない。消えてよいもの**
    typing: std::collections::HashMap<ChannelId, Vec<Typist>>,
    /// ギルドごとのメンバー一覧 (`FR-043`)。
    ///
    /// ⚠️ **キャッシュに残さない。** 誰がオンラインかは開いた瞬間の話で
    /// あって、次の起動で出してよいものではない。
    ///
    /// ⚠️ **本当はチャンネルごとである。** 見える人はチャンネルの権限で
    /// 変わる。いまは 1 ギルドにつき 1 チャンネルしか購読しないので
    /// ギルドで引いて足りる
    members: std::collections::HashMap<GuildId, gumicord_gateway::MemberList>,
    /// 自分。**自分の「入力中」を出さないために要る**
    me: Option<UserId>,
    /// 自分のステータス。
    ///
    /// ⚠️ **READY の時点の値である。** `PRESENCE_UPDATE` を見ていないので、
    /// 走っている間に携帯で変えても、繋ぎ直すまでここは変わらない (`FR-043`)
    status: Option<Status>,
}

impl Live {
    /// キャッシュを開いて、**前回までの状態を読み込む**。
    ///
    /// ⚠️ ここは同期に読む。最初のフレームに要るものなので、待たないと
    /// 「一瞬空っぽの画面」が出る。実測で数 ms しかかからない
    pub fn new() -> Self {
        let mut live = Live::without_cache();

        match gumicord_store::default_path().and_then(|p| Db::open(&p)) {
            Ok((db, snapshot)) => {
                tracing::debug!(
                    guilds = snapshot.guilds.len(),
                    messages = snapshot.messages.len(),
                    "キャッシュから読み込んだ"
                );
                live.store.replace_guilds(snapshot.guilds);
                if !snapshot.guild_order.is_empty() {
                    live.store.set_preferred_order(snapshot.guild_order);
                }
                live.store.set_sidebar(snapshot.folders);
                live.store.set_collapsed(snapshot.collapsed);
                live.last_channel = snapshot.last_channel;
                if let Some(ch) = snapshot.last_channel {
                    // ⚠️ **取りに行った印は付けない。** 繋がったら REST で
                    // 取り直したいので、古いままで固定しない
                    live.store.set_backlog(ch, snapshot.messages);
                }
                live.db = Some(db);
            }
            Err(e) => {
                // キャッシュは速くするためだけのもの。**無くても動く**
                tracing::warn!(%e, "キャッシュを開けない。毎回取り直す");
            }
        }
        live
    }

    /// キャッシュを開かない。**demo と試験で使う。**
    ///
    /// ⚠️ 試験から `Live::new()` を呼ぶと、**開発機の本物のキャッシュを触る**
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
            typing: std::collections::HashMap::new(),
            members: std::collections::HashMap::new(),
            me: None,
        }
    }

    /// イベントループを起こす手を先に受け取る。
    ///
    /// ⚠️ **ログインより前にキャッシュを読むために要る。** 起動直後に開いて
    /// いる 1 チャンネルは同期に読んであるが、繋がる前に別のチャンネルへ
    /// 移ったときは、ここが無いとキャッシュが出てこない
    pub fn attach_waker(&mut self, waker: Waker) {
        self.waker.get_or_insert(waker);
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// ⚠️ 試験から状態を組み立てるためだけにある
    #[cfg(test)]
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// ⚠️ 試験から知らせを 1 つ流し込むためだけにある
    #[cfg(test)]
    pub fn apply_for_test(&mut self, event: LiveEvent) -> bool {
        self.apply(event)
    }

    /// 前回開いていたチャンネル。**そこを開いた状態で起動する**
    pub fn last_channel(&self) -> Option<ChannelId> {
        self.last_channel
    }

    pub fn guilds(&self) -> impl Iterator<Item = &GuildRow> {
        self.store.guilds()
    }

    /// キャッシュにも Gateway にも何も無いか
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// 取りに行った結果として空なのか、まだ取りに行っていないのか。
    ///
    /// **この区別を画面に出せないと「読み込み中」と「発言なし」が同じに
    /// 見える。** 利用者にはまったく違う意味である
    pub fn is_loading(&self, channel: ChannelId) -> bool {
        self.requested.contains(&channel) && !self.store.has_messages(channel)
    }

    /// トークンが弾かれたか。**読んだら下りる** (`FR-004`)
    pub fn take_rejection(&mut self) -> bool {
        std::mem::take(&mut self.rejected)
    }

    /// Gateway に繋ぎ始める。**ログインできてから 1 回だけ呼ぶ。**
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

    /// そのチャンネルで**いま入力中の人**。
    ///
    /// ⚠️ 期限切れはここで落とす。時間で消えるものを状態として持つと、
    /// 「消す」ための仕掛けがもう 1 つ要る
    pub fn typing_in(&self, channel: ChannelId) -> Vec<&str> {
        let now = std::time::Instant::now();
        self.typing
            .get(&channel)
            .into_iter()
            .flatten()
            .filter(|t| now.duration_since(t.at) < TYPING_TTL)
            // ⚠️ **自分は出さない。** 自分が打っていることは自分が一番
            // よく知っている。Discord も出さない
            .filter(|t| Some(t.user) != self.me)
            .map(|t| &*t.name)
            .collect()
    }

    /// 自分のステータス。**分からなければ `None`**
    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// そのサーバのメンバー一覧。**まだ届いていなければ空**
    pub fn members(&self, guild: GuildId) -> Option<&gumicord_gateway::MemberList> {
        self.members.get(&guild).filter(|m| !m.is_empty())
    }

    /// 自分が誰かを教える。**自分の入力中を出さないために要る**
    pub fn set_me(&mut self, me: UserId) {
        self.me = Some(me);
    }

    /// そのチャンネルを開く。**キャッシュを先に出し、REST で追いかける。**
    ///
    /// ⚠️ 取りに行ったこと自体を覚えておかないと、選び直すたびに叩いて
    /// レート制限に当たる
    pub fn open_channel(&mut self, guild: GuildId, channel: ChannelId) {
        if let Some(db) = &self.db {
            db.save_last_channel(channel);
        }
        self.watching = Some(channel);
        self.mark_read(channel);

        // ⚠️ **毎回伝える。** 見ているチャンネルが変わったことを言わないと、
        // そのチャンネルの新着も入力中の表示も来ない
        if let Some(subs) = &self.subs {
            subs.watch(guild, channel);
        }

        if !self.requested.insert(channel) {
            return;
        }

        // [1] キャッシュ。**往復が無いので、繋がる前でも出る**
        if !self.store.has_messages(channel)
            && let (Some(db), Some(waker)) = (&self.db, &self.waker)
        {
            let (tx, waker) = (self.tx.clone(), waker.clone());
            db.load_messages(channel, move |list| {
                let _ = tx.send(LiveEvent::Cached { channel, list });
                waker.wake();
            });
        }

        // [2] REST。**正しいほうで差し替える**
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        rt.spawn(async move {
            match rest.messages(channel, BACKLOG).await {
                Ok(mut list) => {
                    // ⚠️ Discord は**新しい順**で返す。画面は古い順に積む
                    list.reverse();
                    let _ = tx.send(LiveEvent::Backlog { channel, list });
                }
                Err(e) => {
                    // 取れなくてもキャッシュのぶんは出ている。
                    // **黙って「読み込み中」のまま止めない**
                    tracing::warn!(%e, channel = %channel, "メッセージを取れなかった");
                    let _ = tx.send(LiveEvent::Backlog {
                        channel,
                        list: Vec::new(),
                    });
                }
            }
            waker.wake();
        });
    }

    /// もっと古いほうを取りに行く (`FR-020`)。
    ///
    /// ⚠️ **上端に着いたことを、来るたびに叩かない。** スクロールは 1 回の
    /// 操作で何度も来る。1 回目の応答を待たずに次を投げると、同じ範囲を
    /// 何度も頼んでレート制限に当たる。
    ///
    /// ⚠️ **一番古いところまで行ったら二度と頼まない。** 空が返るたびに
    /// また頼むと、上端に居るあいだ叩き続けることになる。
    ///
    /// まだ 1 件も持っていないときは何もしない。**起点が無い**
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
                // ⚠️ Discord は**新しい順**で返す。画面は古い順に積む
                Ok(mut list) => {
                    list.reverse();
                    list
                }
                Err(e) => {
                    // ⚠️ **空として送る。** 送らないと `paging` が下りず、
                    // そのチャンネルは二度と遡れなくなる
                    tracing::warn!(%e, channel = %channel, "遡れなかった");
                    Vec::new()
                }
            };
            let _ = tx.send(LiveEvent::Older { channel, list });
            waker.wake();
        });
    }

    /// 上へ足したか。**1 回だけ真を返す。**
    ///
    /// 見ている場所を動かさないために、描く側がこれを見る
    pub fn take_prepended(&mut self) -> bool {
        std::mem::take(&mut self.prepended)
    }

    /// もう遡れないか。**「読み込み中」を出しっぱなしにしないために要る**
    pub fn is_exhausted(&self, channel: ChannelId) -> bool {
        self.exhausted.contains(&channel)
    }

    /// そこまで読んだことにする (`FR-042`)。**変わったら真**。
    ///
    /// # ⚠️ 画面を先に直し、サーバへは後から伝える
    ///
    /// 往復を待って未読の印を消すと、**開いてから消えるまでの間、既に
    /// 読んでいるものが未読のまま光っている**ことになる。手元を先に直す。
    ///
    /// ⚠️ **失敗しても画面は戻さない。** 戻すと、開いたのに未読へ戻る
    /// という一番分かりにくい動きになる。次に開いたときに送り直される
    fn mark_read(&mut self, channel: ChannelId) -> bool {
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
                tracing::warn!(%e, channel = %channel, "既読を伝えられなかった");
            }
        });
        true
    }

    /// 送る (`FR-024`)。
    ///
    /// ⚠️ **画面には足さない。** 送れたら Gateway が `MESSAGE_CREATE` を
    /// 返してくるので、そこで 1 回だけ足る。ここでも足すと二重に出る
    pub fn send_message(&self, channel: ChannelId, content: String) {
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, waker) = (rest.clone(), waker.clone());
        rt.spawn(async move {
            if let Err(e) = rest.create_message(channel, &content).await {
                tracing::warn!(%e, channel = %channel, "送れなかった");
            }
            waker.wake();
        });
    }

    /// `SEC-021`: ログアウトしたら**キャッシュも認証情報も残さない**。
    ///
    /// ⚠️ 残しておくと、次に別の人がその機械を使ったときに前の人の
    /// メッセージが読める
    pub fn forget_everything(&mut self) {
        if let Some(db) = &self.db {
            db.wipe();
        }
        self.store = Store::new();
        self.requested.clear();
        self.paging.clear();
        self.exhausted.clear();
        self.watching = None;
        self.members.clear();
        self.typing.clear();
        self.last_channel = None;
    }
    /// 届いた並びを Store へ入れる。
    ///
    /// ⚠️ **フォルダだけを抜き出さない。** Discord は並び順の一覧に、
    /// フォルダも単独のサーバも同じ列として入れてくる。フォルダだけを
    /// 先に出して残りを末尾へ寄せると、**利用者が並べた位置が失われる**
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

    /// フォルダの開閉を切り替え、**残す**。
    ///
    /// ⚠️ 開き直すのは利用者の仕事ではない。閉じたことを覚えていないと、
    /// 起動するたびに畳み直すことになる
    pub fn toggle_folder(&mut self, id: u64) {
        self.store.toggle_folder(id);
        if let Some(db) = &self.db {
            db.save_collapsed(&self.store.collapsed());
        }
    }

    /// 溜まっている知らせを**空になるまで**取り込む。変わったら `true`。
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
                // ⚠️ **順を先に取る。** ギルドを差し替えると `ready` が動く
                self.me = Some(ready.user.user.id);
                // ⚠️ **繋がっていることを根拠に「オンライン」と名乗らない。**
                // 取り込み中にしている人に対して嘘になる
                self.status = ready.status();
                let order = ready.guild_order();
                let folders = ready.guild_folders();
                // ⚠️ **ギルドより先に取る。** 差し替えると `ready` が動く
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
                tracing::debug!(marks = marks.len(), "読んだ印を受け取った");
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
                    // 打ち続けている。**時刻だけ延ばす**
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
                // ⚠️ **開いていないチャンネルの分もここを通る。**
                // 未読の印は本文を持っていなくても進む
                let mut changed = self.store.note_arrival(&m, self.me);
                // 見ているチャンネルなら、来た端から読んだことにする
                if Some(channel) == self.watching {
                    changed |= self.mark_read(channel);
                }
                if !self.store.push_message(*m) {
                    return changed;
                }
                // 1 件ずつ書く。**閉じた後も残る**
                if let (Some(db), Some(last)) = (&self.db, self.store.messages(channel).last()) {
                    db.save_messages(channel, vec![last.clone()]);
                }
                true
            }
            // ⚠️ **REST が先に返っていたら採らない。**
            // 古いもので新しいものを潰すことになる
            LiveEvent::Cached { channel, list } => {
                if self.store.has_messages(channel) || list.is_empty() {
                    return false;
                }
                self.store.set_backlog(channel, list);
                true
            }
            LiveEvent::Backlog { channel, list } => {
                // 取れなかった (空) のにキャッシュがあるなら、そちらを残す。
                // **繋がらないときに履歴が消えるのが一番困る** (`NFR-011`)
                if list.is_empty() && self.store.has_messages(channel) {
                    return false;
                }
                if let Some(db) = &self.db {
                    db.save_messages(channel, list.clone());
                }
                // 履歴を丸ごと置き直したので、**遡り直せる**
                self.exhausted.remove(&channel);
                self.store.set_backlog(channel, list);
                true
            }
            LiveEvent::Older { channel, list } => {
                self.paging.remove(&channel);
                // 空 = そこが一番古い。**もう頼まない**
                if list.is_empty() {
                    self.exhausted.insert(channel);
                    return false;
                }
                if let Some(db) = &self.db {
                    db.save_messages(channel, list.clone());
                }
                let added = self.store.prepend_messages(channel, list);
                if added == 0 {
                    // 全部知っていた。**もう一度頼んでも同じものが来る**
                    self.exhausted.insert(channel);
                    return false;
                }
                // ⚠️ **見ている場所を動かさない。** 上へ足したぶんだけ
                // 中身が伸びるので、そのままだと読んでいた行が下へ逃げる
                self.prepended = true;
                true
            }
            LiveEvent::Members(update) => {
                let guild = update.guild;
                let changed = self.members.entry(guild).or_default().apply(*update);

                // ⚠️ **ここで見かけた姿を覚えておく。** REST で取った発言には
                // `member` が付いていないので、本文の呼び名も役職の色も
                // ここが唯一の出所になることがある
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

    /// ⚠️ **書くのは正規化を解いた形である。** Store の中では
    /// チャンネルは 1 箇所にしかないので、書き出すときに組み直す
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

/// Gateway を回し続ける。**`Fatal` が来るまで終わらない。**
async fn pump(mut gateway: Gateway, tx: Sender<LiveEvent>, waker: Waker) {
    loop {
        let event = gateway.next().await;
        let send = |e: LiveEvent| {
            let _ = tx.send(e);
            waker.wake();
        };

        match event {
            Event::Ready(ready) => send(LiveEvent::Ready(ready)),
            // 取りこぼしはこの後で順に届く。**画面ですることはない**
            Event::Resumed => send(LiveEvent::Link(Link::Up)),
            // ⚠️ **読めない 1 件で接続ごと落とさない。**
            // Discord は予告なく形を変える
            Event::Dispatch { kind, data } => match kind.as_str() {
                "MESSAGE_CREATE" => match serde_json::from_value::<Message>(data) {
                    Ok(m) => send(LiveEvent::Posted(Box::new(m))),
                    Err(e) => tracing::warn!(%e, "MESSAGE_CREATE を読めなかった"),
                },
                // READY で殻だけだったギルドが、遅れてここで埋まる。
                // **これが来ないと落ちていたギルドは永久に出てこない**
                "GUILD_CREATE" | "GUILD_UPDATE" => match serde_json::from_value::<Guild>(data) {
                    Ok(g) => send(LiveEvent::GuildChanged(Box::new(g))),
                    Err(e) => tracing::warn!(%e, "{kind} を読めなかった"),
                },
                // ⚠️ **op 14 を送っていないと来ない。** 購読の
                // `channels` に範囲を書いているのがその頼みである
                "GUILD_MEMBER_LIST_UPDATE" => match gumicord_gateway::member_list::parse(&data) {
                    Some(u) => send(LiveEvent::Members(Box::new(u))),
                    None => tracing::warn!("メンバー一覧を読めなかった"),
                },
                "TYPING_START" => {
                    if let Some(e) = typing_event(&data) {
                        send(e);
                        // ⚠️ **消えるときにも描き直しが要る。** 期限は時間で
                        // 切れるので、放っておくと誰も起こしに来ない
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

/// `TYPING_START` から「誰がどこで打っているか」を取り出す。
///
/// ⚠️ **名前の在処が 3 通りある。**
///
/// ```text
///   member.nick             サーバでの表示名。あればこれ
///   member.user.global_name 表示名
///   member.user.username    最後の頼み
/// ```
///
/// DM には `member` が無い。名前が分からなければ**出さない** —
/// 「誰かが入力中」とだけ出しても、利用者にできることが増えない
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

    /// ⚠️ [`Live::new`] は**実際のキャッシュを開く**。試験では使わない
    fn live() -> Live {
        Live::without_cache()
    }

    fn ch() -> ChannelId {
        ChannelId::from(10u64)
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

    /// キャッシュが先に出て、REST が来たら差し替わる
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

    /// 遡った分は前に付き、**見ている場所を保ってほしいと 1 回だけ言う**
    #[test]
    fn an_older_page_goes_in_front_and_asks_to_hold_the_place() {
        let mut live = live();
        live.apply(LiveEvent::Backlog {
            channel: ch(),
            list: vec![message(5, "いま")],
        });
        assert!(!live.take_prepended(), "まだ何も足していない");

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

        assert!(live.take_prepended(), "場所を保ってほしい");
        assert!(!live.take_prepended(), "**1 回だけ**");
    }

    /// ⚠️ **空が返ったら二度と頼まない。**
    ///
    /// 上端に居るあいだスクロールは何度も来る。空のたびにまた頼むと、
    /// そこに居るだけで叩き続けることになる
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

    /// 全部知っていた頁も**そこが先頭**である。もう一度頼んでも同じ
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
        assert!(!live.take_prepended(), "何も足していないので動かさない");
    }

    /// 履歴を丸ごと取り直したら、**また遡れる**
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

    /// ⚠️ **REST が先に返っていたらキャッシュで上書きしない。**
    /// 古いもので新しいものを潰すことになる
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

        assert!(!changed, "古いもので描き直している");
        assert_eq!(live.store().messages(ch())[0].content, "あたらしい");
    }

    /// REST が失敗しても、キャッシュのぶんは消さない。
    /// **繋がらないときに履歴が消えるのが一番困る** (`NFR-011`)
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

        assert_eq!(live.store().messages(ch()).len(), 1, "履歴が消えた");
    }

    /// 何も変わらなければ再描画を要求しない
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

    /// 繋がっているときは何も出さない。
    /// **正常を知らせると、異常の一行が埋もれる**
    #[test]
    fn a_healthy_link_says_nothing() {
        assert!(Link::Up.hint().is_none());
        assert!(Link::Reconnecting("切れた".to_owned()).hint().is_some());
        assert!(Link::Down("駄目".to_owned()).hint().is_some());
    }

    /// トークンが弾かれたことは**一度だけ**伝わる (`FR-004`)
    #[test]
    fn a_rejection_is_reported_once() {
        let mut live = live();
        assert!(!live.take_rejection());

        live.apply(LiveEvent::TokenRejected);
        assert!(live.take_rejection());
        assert!(!live.take_rejection(), "二度目は下りている");
    }

    /// `SEC-021`: 忘れたら**何も残らない**
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
