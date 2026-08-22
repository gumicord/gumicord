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

use gumicord_gateway::{Event, Fatal, Gateway, Ready};
use gumicord_model::{ChannelId, Guild, Message, Token};
use gumicord_platform::Waker;
use gumicord_rest::RestClient;
use gumicord_store::{Db, GuildRow, Store};

/// 1 チャンネルにつき最初に取ってくる件数。
///
/// Discord の API の上限が 100。50 にしてあるのは、**開いた瞬間に見える分
/// より少し多い**あたりで、往復を待たせないため
const BACKLOG: u8 = 50;

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
    /// トークンが弾かれた。アプリが後始末をしたら下ろす
    rejected: bool,
    /// 前回開いていたチャンネル (`NFR-011`)
    last_channel: Option<ChannelId>,
}

impl Live {
    /// キャッシュを開いて、**前回までの状態を読み込む**。
    ///
    /// ⚠️ ここは同期に読む。最初のフレームに要るものなので、待たないと
    /// 「一瞬空っぽの画面」が出る。実測で数 ms しかかからない
    pub fn new() -> Self {
        let mut live = Live::empty();

        match gumicord_store::default_path().and_then(|p| Db::open(&p)) {
            Ok((db, snapshot)) => {
                tracing::debug!(
                    guilds = snapshot.guilds.len(),
                    messages = snapshot.messages.len(),
                    "キャッシュから読み込んだ"
                );
                live.store.replace_guilds(snapshot.guilds);
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

    /// キャッシュを開かない空の状態。**試験と、キャッシュ無しの起動で使う**
    fn empty() -> Self {
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
            requested: HashSet::new(),
            rejected: false,
            last_channel: None,
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

        let tx = self.tx.clone();
        rt.spawn(async move { pump(Gateway::new(token), tx, waker).await });
    }

    /// そのチャンネルを開く。**キャッシュを先に出し、REST で追いかける。**
    ///
    /// ⚠️ 取りに行ったこと自体を覚えておかないと、選び直すたびに叩いて
    /// レート制限に当たる
    pub fn open_channel(&mut self, channel: ChannelId) {
        if let Some(db) = &self.db {
            db.save_last_channel(channel);
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
        self.last_channel = None;
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
                self.store.replace_guilds(ready.guilds);
                self.save_guilds();
                true
            }
            LiveEvent::GuildChanged(g) => {
                self.store.upsert_guild(*g);
                self.save_guilds();
                true
            }
            LiveEvent::Posted(m) => {
                let channel = m.channel_id;
                if !self.store.push_message(*m) {
                    return false;
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
                self.store.set_backlog(channel, list);
                true
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
                icon: g.icon.clone(),
                unavailable: false,
                channels: self.store.channels_of(g.id).cloned().collect(),
            })
            .collect();
        db.save_guilds(guilds);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_model::{MessageId, User, UserId};

    /// ⚠️ [`Live::new`] は**実際のキャッシュを開く**。試験では使わない
    fn live() -> Live {
        Live::empty()
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
                display_name: None,
                discriminator: "0".to_owned(),
                avatar: None,
                bot: false,
            },
            content: body.to_owned(),
            timestamp: "2026-08-22T00:00:00+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            referenced_message: None,
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
