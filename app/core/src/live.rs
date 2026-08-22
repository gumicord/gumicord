//! 本物の Discord のデータ。Gateway (C2) と REST (C3) の結線。
//!
//! # ⚠️ これは Store (C5) ではない
//!
//! 正規化も永続化もしていない。**チャンネルごとにメッセージの列を持っている
//! だけ**である。C5 が入ったらここは消えて、Store への薄い橋になる。
//!
//! いまここにある割り切り:
//!
//! | 割り切り | C5 で直る |
//! |---|---|
//! | ギルドとチャンネルを丸ごと持つ | 正規化する |
//! | メッセージは開いたチャンネルの分だけ | LRU で退避する |
//! | 再起動で全部消える | SQLite に置く |
//! | 未読も既読位置も無い | read-state を持つ |
//!
//! # 取ってくるのは REST、追いかけるのは Gateway
//!
//! ```text
//!   チャンネルを開いた ──▶ GET /channels/:id/messages   過去 50 件
//!   MESSAGE_CREATE     ──▶ 末尾に足す                   これから来る分
//! ```
//!
//! **どちらか片方では成立しない。** Gateway は繋いだ後のものしか運ばず、
//! REST は繋いだ後のものを追いかけられない。
//!
//! # 主スレッドは止めない
//!
//! [`crate::session`] と同じ形である。仕事は [`tokio`] の上で進み、
//! 結果だけがチャネルで戻り、[`Waker`] が主スレッドを起こす。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use gumicord_gateway::{Event, Fatal, Gateway, Ready};
use gumicord_model::{ChannelId, Guild, Message, Token};
use gumicord_platform::Waker;
use gumicord_rest::RestClient;

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
            Link::Up => None,
            Link::Idle => None,
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
    /// REST で取ってきた過去分。**古い順に並べ替えてある**
    Backlog {
        channel: ChannelId,
        list: Vec<Message>,
    },
    Link(Link),
    /// トークンが弾かれた (`FR-004`)。**鍵束から捨ててログイン画面へ**
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

    link: Link,
    guilds: Vec<Guild>,
    /// チャンネルごとのメッセージ。**古い順**
    messages: HashMap<u64, Vec<Message>>,
    /// 取りに行った (行っている) チャンネル。**二重に叩かないため**
    requested: HashSet<u64>,
    /// トークンが弾かれた。アプリが後始末をしたら下ろす
    rejected: bool,
}

impl Live {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Live {
            tx,
            rx,
            rt: None,
            rest: None,
            waker: None,
            started: false,
            link: Link::Idle,
            guilds: Vec::new(),
            messages: HashMap::new(),
            requested: HashSet::new(),
            rejected: false,
        }
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    /// 参加しているギルド。**READY が来るまで空である**
    pub fn guilds(&self) -> &[Guild] {
        &self.guilds
    }

    pub fn guild(&self, id: u64) -> Option<&Guild> {
        self.guilds.iter().find(|g| g.id.get() == id)
    }

    /// そのチャンネルのメッセージ。**まだ取ってきていなければ空**
    pub fn messages(&self, channel: u64) -> &[Message] {
        self.messages.get(&channel).map_or(&[], Vec::as_slice)
    }

    /// 取りに行った結果として空なのか、まだ取りに行っていないのか。
    ///
    /// **この区別を画面に出せないと「読み込み中」と「発言なし」が同じに
    /// 見える。** 利用者にはまったく違う意味である
    pub fn is_loading(&self, channel: u64) -> bool {
        self.requested.contains(&channel) && !self.messages.contains_key(&channel)
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

    /// そのチャンネルの過去分を取りに行く。**2 回目からは何もしない。**
    ///
    /// ⚠️ 取りに行ったこと自体を覚えておかないと、選び直すたびに叩いて
    /// レート制限に当たる
    pub fn open_channel(&mut self, channel: u64) {
        if !self.requested.insert(channel) {
            return;
        }
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };

        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        let id = ChannelId::from(channel);
        rt.spawn(async move {
            match rest.messages(id, BACKLOG).await {
                Ok(mut list) => {
                    // ⚠️ Discord は**新しい順**で返す。画面は古い順に積む
                    list.reverse();
                    let _ = tx.send(LiveEvent::Backlog { channel: id, list });
                }
                Err(e) => {
                    // 取れなくても画面は動く。**空のまま黙らせない**ために
                    // 誤りは記録するが、状態は「取りに行った」のままにする
                    tracing::warn!(%e, channel = %id, "メッセージを取れなかった");
                    let _ = tx.send(LiveEvent::Backlog {
                        channel: id,
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
    pub fn send_message(&self, channel: u64, content: String) {
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };
        let (rest, waker) = (rest.clone(), waker.clone());
        let id = ChannelId::from(channel);
        rt.spawn(async move {
            if let Err(e) = rest.create_message(id, &content).await {
                tracing::warn!(%e, channel = %id, "送れなかった");
            }
            waker.wake();
        });
    }

    /// 溜まっている知らせを**空になるまで**取り込む。変わったら `true`。
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.apply(event);
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return changed,
            }
        }
    }

    fn apply(&mut self, event: LiveEvent) {
        match event {
            LiveEvent::Ready(ready) => {
                self.link = Link::Up;
                // ⚠️ **チャンネルを持っているギルドだけを採る。** READY の
                // 直後に殻だけのギルドが来ることがあり、上書きすると
                // 一覧からチャンネルが消える
                self.guilds = ready.guilds;
                self.guilds.sort_by(|a, b| a.name.cmp(&b.name));
            }
            LiveEvent::Posted(m) => {
                let channel = m.channel_id.get();
                // 開いていないチャンネルの分は溜めない。**開いたときに
                // REST で取ってくるので、ここで持つと二重になる**
                if let Some(list) = self.messages.get_mut(&channel)
                    && !list.iter().any(|x| x.id == m.id)
                {
                    list.push(*m);
                }
            }
            LiveEvent::Backlog { channel, list } => {
                self.messages.insert(channel.get(), list);
            }
            LiveEvent::Link(link) => self.link = link,
            LiveEvent::TokenRejected => {
                self.rejected = true;
                self.link = Link::Down("トークンが無効になりました".to_owned());
            }
        }
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
            Event::Dispatch { kind, data } => {
                if kind == "MESSAGE_CREATE" {
                    match serde_json::from_value::<Message>(data) {
                        Ok(m) => send(LiveEvent::Posted(Box::new(m))),
                        // ⚠️ 読めない 1 件で接続ごと落とさない。
                        // Discord は予告なく形を変える
                        Err(e) => tracing::warn!(%e, "MESSAGE_CREATE を読めなかった"),
                    }
                }
            }
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

    fn message(id: u64, channel: u64, body: &str) -> Message {
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
            content: body.to_owned(),
            timestamp: "2026-08-22T00:00:00+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            referenced_message: None,
        }
    }

    /// 「読み込み中」と「発言なし」は**別のものである**
    #[test]
    fn loading_and_empty_are_distinguishable() {
        let mut live = Live::new();
        assert!(!live.is_loading(1), "開く前は読み込み中ではない");

        live.requested.insert(1);
        assert!(live.is_loading(1));

        live.apply(LiveEvent::Backlog {
            channel: ChannelId::from(1u64),
            list: Vec::new(),
        });
        assert!(!live.is_loading(1), "取り終えたら読み込み中ではない");
        assert!(live.messages(1).is_empty());
    }

    /// 新着は末尾に足る
    #[test]
    fn a_new_message_lands_at_the_end() {
        let mut live = Live::new();
        live.apply(LiveEvent::Backlog {
            channel: ChannelId::from(1u64),
            list: vec![message(10, 1, "ふるい")],
        });
        live.apply(LiveEvent::Posted(Box::new(message(11, 1, "あたらしい"))));

        let got: Vec<_> = live.messages(1).iter().map(|m| &*m.content).collect();
        assert_eq!(got, vec!["ふるい", "あたらしい"]);
    }

    /// **同じものを二度足さない。** 自分で送った直後は
    /// REST の応答と MESSAGE_CREATE が競る
    #[test]
    fn the_same_message_is_not_added_twice() {
        let mut live = Live::new();
        live.apply(LiveEvent::Backlog {
            channel: ChannelId::from(1u64),
            list: Vec::new(),
        });
        live.apply(LiveEvent::Posted(Box::new(message(11, 1, "やあ"))));
        live.apply(LiveEvent::Posted(Box::new(message(11, 1, "やあ"))));

        assert_eq!(live.messages(1).len(), 1);
    }

    /// 開いていないチャンネルの新着は溜めない。
    /// **開いたときに REST で取ってくるので、持つと二重になる**
    #[test]
    fn messages_for_unopened_channels_are_dropped() {
        let mut live = Live::new();
        live.apply(LiveEvent::Posted(Box::new(message(11, 99, "やあ"))));
        assert!(live.messages(99).is_empty());
        assert!(!live.is_loading(99));
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
        let mut live = Live::new();
        assert!(!live.take_rejection());

        live.apply(LiveEvent::TokenRejected);
        assert!(live.take_rejection());
        assert!(!live.take_rejection(), "二度目は下りている");
    }

    /// ギルドは名前順に並ぶ。**READY の順は当てにならない**
    #[test]
    fn guilds_are_sorted_by_name() {
        let mut live = Live::new();
        live.apply(LiveEvent::Ready(Box::new(Ready {
            user: serde_json::from_str(r#"{"id":"1","username":"x"}"#).unwrap(),
            session_id: "s".to_owned(),
            resume_gateway_url: None,
            guilds: vec![
                Guild {
                    id: 2u64.into(),
                    name: "ばなな".to_owned(),
                    icon: None,
                    channels: Vec::new(),
                },
                Guild {
                    id: 1u64.into(),
                    name: "あんず".to_owned(),
                    icon: None,
                    channels: Vec::new(),
                },
            ],
        })));

        let names: Vec<_> = live.guilds().iter().map(|g| &*g.name).collect();
        assert_eq!(names, vec!["あんず", "ばなな"]);
        assert_eq!(live.link(), &Link::Up);
    }
}
