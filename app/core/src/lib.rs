//! アプリケーション層。画面遷移・アプリ状態・各層の結線。
//!
//! フレームのパイプラインの順序を保証する責務を持つ。
//! **この順序が拡張の意味論を決めるため、仕様として固定されている**:
//!
//! ```text
//! [1] 入力・イベント取り込み
//! [2] 状態更新                    gumicord-store
//! [3] UITree 構築 (差分)          gumicord-uitree
//! [4] プラグインの構造介入         gumicord-plugin
//! [5] テーマ解決                  gumicord-theme
//! [6] プラグインのスタイル介入     gumicord-plugin
//! [7] レイアウト                  gumicord-render
//! [8] 描画コマンド生成 → GPU
//! [9] アクセシビリティツリー更新   gumicord-platform
//! ```
//!
//! [4] が [5] より前なのは、プラグインが挿入したノードにもテーマを適用する
//! ためである。[6] が [5] より後なのは、テーマとプラグインが衝突したとき
//! プラグインが勝つと決めたためである。
//!
//! # いま通っているのは [1] [2] [3] [5] [7] [8] である
//!
//! | 段 | 状態 |
//! |---|---|
//! | [1] 入力 | ポインタ・キーボード・IME |
//! | [2] 状態更新 | ある。[`gumicord_store`] が正規化して持ち、SQLite に残す |
//! | [3] UITree 構築 | ある。ただし毎フレーム全体を組み直している |
//! | [4] [6] プラグイン | ない (E4, E5) |
//! | [5] テーマ解決 | ある |
//! | [7] [8] レイアウトと描画 | ある |
//! | [9] a11y | ない (P3) |
//!
//! # 画面は 2 通りある
//!
//! ```text
//!   キャッシュがある   ──▶ app.screen.main    **ログインを待たずに出す**
//!   ログインした       ──▶ app.screen.main    本物のギルド・チャンネル・発言
//!   どちらも無い       ──▶ app.screen.login   QR を出して待つ
//!   GUMICORD_SKIP_LOGIN ─▶ app.screen.main    [`demo`] の固定データ
//! ```
//!
//! ⚠️ **キャッシュがあればログインを待たない** (`NFR-011`, C6)。S4 の実測で
//! READY まで 672〜1120 ms かかるので、待つと `NFR-001` (500 ms) に入らない。
//! ログアウトするとキャッシュごと消える (`SEC-021`) ので、**残っていること
//! 自体が「前回このアカウントで入れていた」証拠**になる。
//!
//! **demo と本物の分かれ目は `uses_live()` の 1 箇所だけ**である。木を組み立てる
//! ところが「どちらのデータか」を気にし始めると、分岐が画面のあちこちに散る。
//! `GuildRow` / `ChannelRow` / `MessageRow` がその境目を吸収する。
//!
//! 仕様: [`spec/02-architecture.md`]

pub mod demo;
pub mod live;
pub mod session;

use std::borrow::Cow;

use gumicord_model::{ChannelId, GuildId};
use gumicord_platform::{Application, FrameCx, TextDocument, Waker};
use gumicord_render::Hit;
use gumicord_store::ChannelEntry;
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::{Editable, Key, NodeId, State, UiNode};
use live::Live;
use session::Login;

/// クライアント同梱の既定テーマ。
///
/// `EXT-016` はテーマが読めなくても動くことを求めるので、既定は
/// **ファイルではなくバイナリに埋め込む**。
const DEFAULT_THEME: &str = include_str!("../../../examples/themes/midnight/theme.json");

/// テーマを差し替える環境変数。
///
/// テーマ作成時に見比べるためのもので、設定画面とホットリロード
/// (`EXT-015`, E2) ができたら消える。
const THEME_ENV: &str = "GUMICORD_THEME";

/// ポインタが乗ったときに反応するノード。
///
/// ⚠️ **ここに挙げていないノードにホバー状態は立たない。** テーマが
/// `when.state = hover` を書いても効かないので、増やすときはここも見る。
const INTERACTIVE: &[NodeId] = &[
    NodeId::NavGuildListHome,
    NodeId::NavGuildListItem,
    NodeId::NavChannelListItem,
    NodeId::NavDmListItem,
    NodeId::ChatMessage,
    NodeId::ChromeTitlebarControl,
    NodeId::PrimitiveButton,
    NodeId::LayoutScrollbarThumb,
];

/// 画面に出すペインの数 (`PLT-046`)。
///
/// # なぜ幅で決めるのか
///
/// **プラットフォームではなく幅で決める。** 縦持ちのタブレットと横に細くした
/// デスクトップの窓は、同じ扱いでよい。プラットフォームで分けると、
/// 「Windows だが幅 500px」に答えられない。
///
/// テーマ側も同じ考えで、`when.maxWidth` が使える (`midnight` は
/// `chat.message` の余白をこれで詰めている)。**ここはその構造版**である。
///
/// ⚠️ **ペインを隠すだけで、戻る手段はまだない。** 触って切り替える操作は
/// M1.2 の X1 でナビゲーション状態と一緒に入る。いまは窓を広げれば戻る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panes {
    /// ギルド + チャンネル + チャット
    Three,
    /// チャンネル + チャット
    Two,
    /// チャットのみ
    One,
}

impl Panes {
    /// 3 ペインを保てる下限 (論理 px)。
    /// ギルド 64 + チャンネル 240 に、チャットが窮屈にならない幅を足した値
    const THREE: f32 = 900.0;
    /// 2 ペインを保てる下限。これを割ると一覧が本文を圧迫する
    const TWO: f32 = 600.0;

    pub fn for_width(w: f32) -> Self {
        if w >= Self::THREE {
            Panes::Three
        } else if w >= Self::TWO {
            Panes::Two
        } else {
            Panes::One
        }
    }

    pub fn guilds(self) -> bool {
        self == Panes::Three
    }

    pub fn channels(self) -> bool {
        self != Panes::One
    }
}

/// アプリケーションの状態と、そこから UITree を組み立てる責務。
pub struct Gumicord {
    theme: Option<Theme>,
    /// 背景の仕事を載せる土台。
    ///
    /// ⚠️ **手放すと走っている仕事が止まる。** アプリがその持ち主である
    runtime: Option<tokio::runtime::Runtime>,
    /// イベントループを起こす手。ログインが済んだ後に Gateway へ渡す
    waker: Option<Waker>,
    /// ログインの進み具合 (`FR-001`)。**画面の切り替えはこれが決める**
    login: Login,
    /// 本物のデータ (C2, C3)。**demo との分かれ目はここが空かどうか**
    live: Live,
    /// ポインタが乗っているノード
    hovered: Option<(NodeId, Option<Key>)>,
    selected_guild: u64,
    selected_channel: u64,
    /// 入力欄にフォーカスがあるか
    input_focused: bool,
    /// 入力欄の中身 (`PLT-001`)
    input: TextDocument,
    /// demo のときにこの場で送ったもの。**本物では使わない**
    sent: Vec<demo::Message>,
}

impl Gumicord {
    pub fn new() -> Self {
        // ⚠️ **ここでキャッシュを読む。** 最初のフレームに間に合わせるため
        // で、これが `NFR-011` と C6 の実体である
        Gumicord::with(Login::new(), Live::new())
    }

    /// ログインを飛ばし、[`demo`] の固定データで画面を組む。
    ///
    /// レンダラやテーマを触るのに毎回スマホを出すのは馬鹿らしい。
    /// `GUMICORD_SKIP_LOGIN=1` で起動したときと同じ状態になる。
    ///
    /// ⚠️ **キャッシュも開かない。** 本物のデータが混ざると、
    /// 「demo を見ている」という前提が崩れる
    pub fn demo() -> Self {
        Gumicord::with(Login::skipped(), Live::without_cache())
    }

    fn with(login: Login, live: Live) -> Self {
        // 前回開いていたチャンネルを、そのギルドごと復元する
        let (guild, channel) = match live.last_channel() {
            Some(ch) => {
                let guild = live
                    .store()
                    .channel(ch)
                    .and_then(|c| c.guild_id)
                    .map(|g| g.get())
                    .unwrap_or(0);
                (guild, ch.get())
            }
            None => (demo::GUILDS[0].id, demo::CHANNELS[1].id),
        };

        Gumicord {
            theme: load_theme(),
            runtime: None,
            waker: None,
            login,
            live,
            hovered: None,
            selected_guild: guild,
            selected_channel: channel,
            input_focused: false,
            input: TextDocument::new(),
            sent: Vec::new(),
        }
    }

    fn is_hovered(&self, id: NodeId, key: Option<&Key>) -> bool {
        match &self.hovered {
            Some((hid, hkey)) => *hid == id && hkey.as_ref() == key,
            None => false,
        }
    }

    fn hovered_id(&self, node: NodeId, id: u64) -> bool {
        self.is_hovered(node, Some(&Key::Id(id)))
    }
}

impl Default for Gumicord {
    fn default() -> Self {
        Self::new()
    }
}

fn load_theme() -> Option<Theme> {
    let src = match std::env::var(THEME_ENV) {
        Ok(path) => match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(%path, %e, "テーマを読めなかった。同梱のものを使う");
                DEFAULT_THEME.to_owned()
            }
        },
        Err(_) => DEFAULT_THEME.to_owned(),
    };

    let result = Theme::parse(&src);
    // EXT-016: ルールが無視されてもテーマ自体は適用する。黙って捨てない
    for d in &result.diagnostics {
        tracing::warn!("テーマ: {d}");
    }
    result.theme
}

impl Application for Gumicord {
    fn title(&self) -> String {
        "Gumicord".to_owned()
    }

    /// ログインを始める。**ウィンドウが出るより前に呼ばれる。**
    ///
    /// 鍵の生成に 1 秒前後かかるので、早く始めたぶんだけ QR が早く出る
    fn start(&mut self, waker: Waker) {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(%e, "非同期処理を始められない");
                return;
            }
        };

        self.login.start(runtime.handle(), waker.clone());
        self.live.attach_waker(waker.clone());
        self.waker = Some(waker);
        self.runtime = Some(runtime);
    }

    /// 背景の知らせを取り込む。**ここが唯一の入り口である。**
    fn wake(&mut self) -> bool {
        let mut changed = self.login.poll();
        changed |= self.live.poll();

        // ログインが済んだら Gateway へ進む。**`Live::start` は 2 回目から
        // 何もしない**ので、ここで毎回呼んでも構わない
        if let (Some(l), Some(rt), Some(waker)) =
            (self.login.session().logged_in(), &self.runtime, &self.waker)
        {
            // ⚠️ **自分が誰かを先に教える。** READY より前に誰かが打ち始めても、
            // 自分の「入力中」を出さないため
            let me = l.me.user.id;
            self.live.start(
                rt.handle(),
                l.client.clone(),
                l.token.clone(),
                waker.clone(),
            );
            self.live.set_me(me);
        }

        // `FR-004`: Gateway にトークンを弾かれた。**捨ててログイン画面へ戻す**
        if self.live.take_rejection()
            && let (Some(rt), Some(waker)) = (&self.runtime, &self.waker)
        {
            tracing::warn!("トークンが無効になった。ログインし直す");
            self.login.forget(rt.handle(), waker.clone());
            // `SEC-021`: **キャッシュも残さない。** 残すと、次に別の人が
            // この機械を使ったときに前の人のメッセージが読める
            self.live.forget_everything();
            changed = true;
        }

        changed |= self.sync_selection();
        changed
    }

    fn hover_changed(&mut self, hits: &[Hit]) -> bool {
        let next = hits
            .iter()
            .find(|h| INTERACTIVE.contains(&h.id))
            .map(|h| (h.id, h.key.clone()));
        if next == self.hovered {
            return false;
        }
        self.hovered = next;
        true
    }

    fn pressed(&mut self, hits: &[Hit]) -> bool {
        let mut changed = false;

        // 入力欄の外を押したらフォーカスが外れる
        let on_input = hits.iter().any(|h| h.id == NodeId::ChatInputField);
        if on_input != self.input_focused {
            self.input_focused = on_input;
            changed = true;
        }

        // 手前から見て、最初に見つかった選択対象だけを処理する
        for h in hits {
            match (h.id, &h.key) {
                (NodeId::NavGuildListItem, Some(Key::Id(id))) => {
                    if self.selected_guild == *id {
                        break;
                    }
                    self.selected_guild = *id;
                    // ⚠️ **チャンネルの選択を落とす。** 別のギルドの
                    // チャンネルが選ばれたままだと、一覧と本文が食い違う
                    self.selected_channel = 0;
                    changed = true;
                }
                (NodeId::NavChannelListItem, Some(Key::Id(id))) => {
                    changed |= self.selected_channel != *id;
                    self.selected_channel = *id;
                }
                _ => continue,
            }
            break;
        }

        // ⚠️ **押した直後にここで取りに行く。** 次に何かが起きるまで
        // 待っていると、選んだのに空のまま止まって見える
        changed |= self.sync_selection();
        changed
    }

    /// `PLT-001`: 入力を受け取るのは、フォーカスのある入力欄だけである
    fn focused_document(&mut self) -> Option<&mut TextDocument> {
        self.input_focused.then_some(&mut self.input)
    }

    /// `FR-024`: 送信。
    fn submit(&mut self) -> bool {
        let body = self.input.text().trim().to_owned();
        if body.is_empty() {
            return false;
        }
        self.input.take();

        if self.uses_live() {
            // ⚠️ **ここで一覧に足さない。** 送れたら Gateway が
            // `MESSAGE_CREATE` を返してくるので、そこで 1 回だけ足る。
            // 先に足すと、届いたときに同じものが二重に並ぶ
            self.live
                .send_message(ChannelId::from(self.selected_channel), body);
            return true;
        }

        // demo ではどこへも行かない。**その場の一覧に足すだけである**
        let id = 1000 + self.sent.len() as u64;
        self.sent.push(demo::Message {
            id,
            author: Cow::Borrowed("ねんねこ"),
            time: Cow::Borrowed("たった今"),
            body: Cow::Owned(body),
            mentioned: false,
        });
        true
    }

    fn cancel_input(&mut self) -> bool {
        if !self.input_focused {
            return false;
        }
        self.input_focused = false;
        true
    }

    /// パイプラインの [3] と [5]。
    ///
    /// [4] と [6] (プラグインの介入) はまだない。入るのはこの間である。
    fn build(&mut self, cx: &FrameCx) -> UiNode {
        // [3] UITree 構築
        let mut tree = self.build_tree(Panes::for_width(cx.viewport.w));

        // [5] テーマ解決
        match &self.theme {
            Some(theme) => {
                let ctx = MatchContext::new(cx.viewport.w);
                gumicord_theme::resolve(theme, &mut tree, &ctx);
            }
            None => gumicord_theme::resolve::clear(&mut tree),
        }
        tree
    }
}

impl Gumicord {
    /// ⚠️ 毎フレーム木を丸ごと組み直している。差分構築 (B2 の残件) は
    /// レンダラ側の要求が固まってから入れる。
    fn build_tree(&self, panes: Panes) -> UiNode {
        // **画面の分かれ目はここだけである。** ログインしていなければ
        // メイン画面は組み立てもしない
        let screen = if self.shows_main() {
            UiNode::new(NodeId::AppScreenMain)
                .child_if(panes.guilds(), || self.guild_list())
                .child_if(panes.channels(), || self.channel_list())
                .child(self.chat_view())
        } else {
            self.login_screen()
        };

        UiNode::new(NodeId::AppRoot).child(
            UiNode::new(NodeId::AppWindow)
                .child(self.titlebar())
                .child(UiNode::new(NodeId::AppScreen).child(screen)),
        )
    }

    /// ログイン画面 (`FR-001`)。QR を出して読まれるのを待つ。
    ///
    /// **入力欄も押しボタンも無い。** 利用者がここですることは、スマホで
    /// QR を読むことだけである。パスワード経路 (C4b) が入るとここに増える。
    ///
    /// 上下の `layout.spacer` が縦の余りを分け合うことで中央に来る。
    /// `app.screen.login` の交差軸は `Center` なので横は勝手に揃う
    fn login_screen(&self) -> UiNode {
        let s = self.login.session();

        UiNode::new(NodeId::AppScreenLogin)
            .child(UiNode::new(NodeId::LayoutSpacer))
            .child(UiNode::text(
                NodeId::AppScreenLoginTitle,
                "QR コードでログイン",
            ))
            // QR が無い間 (接続中・交換中) は出さない。
            // **枠だけ出して中身が空だと、読めない QR を見せることになる**
            .child_if(s.qr().is_some(), || {
                UiNode::qr(NodeId::PrimitiveQr, s.qr().unwrap_or_default())
            })
            .child(UiNode::text(NodeId::AppScreenLoginHint, s.hint()))
            .child(UiNode::new(NodeId::LayoutSpacer))
    }

    /// `PLT-020`: 独自タイトルバー。
    ///
    /// ボタンは `key` の [`Key::Slot`] で区別する。プラットフォーム層は
    /// この文字列だけを見てウィンドウ操作へつなぐ。
    fn titlebar(&self) -> UiNode {
        // ⚠️ 字ではなくアイコンで描く。`−` `□` `✕` を文字として並べると
        // 太さも大きさも書体任せになり、3 つ並べたときに揃わない
        let button = |slot: &'static str, icon: &str| {
            UiNode::icon(NodeId::ChromeTitlebarControl, icon)
                .with_key(Key::Slot(slot))
                .with_state_if(
                    self.is_hovered(NodeId::ChromeTitlebarControl, Some(&Key::Slot(slot))),
                    State::Hover,
                )
        };

        // ログインできたら誰として入っているかを出す。**本物のデータが
        // 通っていることが目で分かる唯一の場所**でもある (Store は C5)
        let title = match self.login.session().logged_in() {
            Some(l) => format!("  Gumicord — {}", l.me.user.name()),
            None => "  Gumicord".to_owned(),
        };

        UiNode::new(NodeId::ChromeTitlebar)
            .child(UiNode::text(NodeId::ChromeTitlebarTitle, title))
            .child(
                UiNode::new(NodeId::ChromeTitlebarControls)
                    .child(button("minimize", "window.minimize"))
                    .child(button("maximize", "window.maximize"))
                    .child(button("close", "window.close")),
            )
    }

    /// ギルド一覧 (`FR-010`)。
    ///
    /// ⚠️ **未読とメンションの印はまだ本物ではない。** read-state は C5 と
    /// 一緒に入る。demo のときだけ印が付く
    fn guild_list(&self) -> UiNode {
        let mut list = UiNode::new(NodeId::NavGuildList).child(
            UiNode::text(NodeId::NavGuildListHome, "DM").with_state_if(
                self.is_hovered(NodeId::NavGuildListHome, None),
                State::Hover,
            ),
        );

        for g in self.guild_rows() {
            list = list.child(
                UiNode::text(NodeId::NavGuildListItem, initial(&g.name))
                    .with_id_key(g.id)
                    .with_data(g.id)
                    .with_state_if(g.id == self.selected_guild, State::Selected)
                    .with_state_if(g.unread, State::Unread)
                    .with_state_if(g.mentions > 0, State::Mentioned)
                    .with_state_if(
                        self.hovered_id(NodeId::NavGuildListItem, g.id),
                        State::Hover,
                    ),
            );
        }
        list.child(scrollbar())
    }

    fn channel_list(&self) -> UiNode {
        let title = self
            .guild_rows()
            .into_iter()
            .find(|g| g.id == self.selected_guild)
            .map(|g| g.name)
            .unwrap_or_else(|| "Gumicord".to_owned());

        let mut list = UiNode::new(NodeId::NavChannelList)
            .child(UiNode::text(NodeId::NavChannelListHeader, title));

        for c in self.channel_rows() {
            // カテゴリは見出しである。**押しても開かないので、当たりも作らない**
            if c.category {
                list = list
                    .child(UiNode::text(NodeId::NavChannelListCategory, c.name).with_id_key(c.id));
                continue;
            }

            let mut item = UiNode::new(NodeId::NavChannelListItem)
                .with_id_key(c.id)
                .with_data(c.id)
                .with_state_if(c.id == self.selected_channel, State::Selected)
                .with_state_if(c.unread, State::Unread)
                .with_state_if(c.mentions > 0, State::Mentioned)
                .with_state_if(
                    self.hovered_id(NodeId::NavChannelListItem, c.id),
                    State::Hover,
                )
                .child(UiNode::icon(NodeId::NavChannelListItemIcon, c.icon).with_data(c.id))
                .child(UiNode::text(NodeId::NavChannelListItemName, c.name).with_data(c.id));

            if c.mentions > 0 {
                item = item.child(
                    UiNode::text(NodeId::NavChannelListItemBadge, c.mentions.to_string())
                        .with_data(c.id),
                );
            }
            list = list.child(item);
        }
        list.child(scrollbar())
    }

    fn chat_view(&self) -> UiNode {
        let channels = self.channel_rows();
        let channel = channels
            .iter()
            .find(|c| c.id == self.selected_channel)
            .or(channels.first());

        let (id, name, icon, topic) = match channel {
            Some(c) => (c.id, c.name.clone(), c.icon, c.topic.clone()),
            None => (0, String::new(), "channel.text", None),
        };

        let header = UiNode::new(NodeId::ChatHeader)
            .with_data(id)
            .child(UiNode::icon(NodeId::PrimitiveIcon, icon))
            .child(UiNode::text(NodeId::ChatHeaderTitle, &name).with_data(id))
            .child(UiNode::text(NodeId::ChatHeaderTopic, topic.unwrap_or_default()).with_data(id));

        // 直前と同じ送信者なら送信者行を繰り返さない。
        // **字下げの量はテーマが決める** (`when.state: "grouped"` の padding)
        let rows = self.message_rows();
        let mut messages = UiNode::new(NodeId::ChatMessageList);
        let mut prev: Option<&str> = None;
        for m in &rows {
            messages = messages.child(self.message(m, prev == Some(&*m.author)));
            prev = Some(&m.author);
        }
        messages = messages.child(scrollbar());

        UiNode::new(NodeId::ChatView)
            .child(header)
            .child(messages)
            .child(UiNode::text(
                NodeId::ChatTypingIndicator,
                self.status_line(),
            ))
            .child(
                UiNode::new(NodeId::ChatInput).child(
                    UiNode::editable(
                        NodeId::ChatInputField,
                        Editable {
                            text: self.input.text().to_owned(),
                            caret: self.input.caret(),
                            selection: self.input.selection(),
                            composing: self.input.composing(),
                            placeholder: if name.is_empty() {
                                "メッセージを送信".to_owned()
                            } else {
                                format!("#{name} へメッセージを送信")
                            },
                        },
                    )
                    .with_state_if(self.input_focused, State::Focus),
                ),
            )
    }

    /// 一覧の下に出す 1 行。
    ///
    /// ⚠️ **繋がっている間は接続のことを言わない。** 正常をわざわざ知らせると、
    /// 異常のときの一行が埋もれる
    fn status_line(&self) -> String {
        if let Some(hint) = self.live.link().hint() {
            return format!("  {hint}");
        }
        if self.uses_live() {
            let channel = ChannelId::from(self.selected_channel);
            if self.live.is_loading(channel) {
                return "  読み込んでいます…".to_owned();
            }
            return typing_line(&self.live.typing_in(channel));
        }
        "  みどり が入力中…".to_owned()
    }

    /// メッセージ 1 件。
    ///
    /// `grouped` なら送信者アイコンと送信者行を出さない。**字下げはテーマが
    /// `when.state: "grouped"` の `padding` で決める。** クライアントが
    /// 空白のノードを挟むと、字下げの量が焼き付いてテーマから揃えられない。
    fn message(&self, m: &MessageRow, grouped: bool) -> UiNode {
        let body = UiNode::new(NodeId::LayoutColumn)
            .child_if(!grouped, || {
                UiNode::new(NodeId::ChatMessageHeader)
                    .with_data(m.id)
                    .child(UiNode::text(NodeId::ChatMessageHeaderAuthor, &m.author).with_data(m.id))
                    .child(
                        UiNode::text(NodeId::ChatMessageHeaderTime, format!("  {}", m.time))
                            .with_data(m.id),
                    )
            })
            .child(UiNode::text(NodeId::ChatMessageContent, &m.body).with_data(m.id));

        UiNode::new(NodeId::ChatMessage)
            .with_id_key(m.id)
            .with_data(m.id)
            .with_state_if(grouped, State::Grouped)
            .with_state_if(m.mentioned, State::Mentioned)
            .with_state_if(self.hovered_id(NodeId::ChatMessage, m.id), State::Hover)
            .child_if(!grouped, || {
                UiNode::text(NodeId::ChatMessageAvatar, initial(&m.author)).with_data(m.id)
            })
            // 送信者行と本文を縦に積む。`layout.column` はこのためにある
            .child(body)
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  画面に出す行
//
//  ⚠️ **demo と本物をここへ寄せる。** 木を組み立てるところが「どちらの
//  データか」を気にし始めると、分岐が画面のあちこちに散る。
// ═══════════════════════════════════════════════════════════════════════

struct GuildRow {
    id: u64,
    name: String,
    unread: bool,
    mentions: u32,
}

struct ChannelRow {
    id: u64,
    name: String,
    icon: &'static str,
    topic: Option<String>,
    unread: bool,
    mentions: u32,
    /// カテゴリの見出し。**押しても開かない**
    category: bool,
}

struct MessageRow {
    id: u64,
    author: String,
    time: String,
    body: String,
    mentioned: bool,
}

impl Gumicord {
    /// 本物のデータを出す状態か。**demo との分かれ目はここだけである。**
    ///
    /// ⚠️ **ログインを待たない。** キャッシュに中身があるなら、それは
    /// 前回このアカウントで入れていたということである。ログアウトすると
    /// キャッシュごと消える (`SEC-021`) ので、残っていること自体が根拠になる。
    ///
    /// 待つと、S4 実測で 672〜1120 ms のあいだ空の画面を見せることになる
    fn uses_live(&self) -> bool {
        self.login.session().logged_in().is_some() || !self.live.is_empty()
    }

    /// メイン画面を出してよいか。**ログイン画面との分かれ目**
    fn shows_main(&self) -> bool {
        self.login.shows_main() || !self.live.is_empty()
    }

    /// 選んでいるものが実在するように直し、要るものを取りに行く。
    ///
    /// ⚠️ **起動直後の選択は demo の ID である。** READY が来た時点で
    /// そんなギルドは無いので、放っておくと一覧だけ出て中身が空になる。
    ///
    /// 戻り値は「画面が変わったか」。取りに行っただけでは変わらない —
    /// 届いたときに [`Live::poll`] が改めて真を返す
    fn sync_selection(&mut self) -> bool {
        if !self.uses_live() {
            return false;
        }
        let mut changed = false;

        let guilds = self.guild_rows();
        if !guilds.iter().any(|g| g.id == self.selected_guild) {
            let Some(first) = guilds.first() else {
                // READY がまだ来ていない。**何も選ばない**
                return false;
            };
            self.selected_guild = first.id;
            self.selected_channel = 0;
            changed = true;
        }

        let channels = self.channel_rows();
        if !channels.iter().any(|c| c.id == self.selected_channel)
            && let Some(first) = channels.first()
        {
            self.selected_channel = first.id;
            changed = true;
        }

        if self.selected_channel != 0 {
            // 2 回目からは何もしない
            self.live.open_channel(
                GuildId::from(self.selected_guild),
                ChannelId::from(self.selected_channel),
            );
        }
        changed
    }

    fn guild_rows(&self) -> Vec<GuildRow> {
        if !self.uses_live() {
            return demo::GUILDS
                .iter()
                .map(|g| GuildRow {
                    id: g.id,
                    name: g.name.to_owned(),
                    unread: g.unread,
                    mentions: g.mentions,
                })
                .collect();
        }

        // ⚠️ 落ちているギルドは [`Store`] が既に落としている。
        // ここには出せるものだけが来る
        self.live
            .guilds()
            .map(|g| GuildRow {
                id: g.id.get(),
                name: g.name.clone(),
                // read-state はまだ無い。**無いものを在るように見せない**
                unread: false,
                mentions: 0,
            })
            .collect()
    }

    fn channel_rows(&self) -> Vec<ChannelRow> {
        if !self.uses_live() {
            return demo::CHANNELS
                .iter()
                .map(|c| ChannelRow {
                    id: c.id,
                    name: c.name.to_owned(),
                    icon: c.icon,
                    topic: Some(
                        "自前レンダラの縦通し。テーマ JSON だけで見た目が決まる".to_owned(),
                    ),
                    unread: c.unread,
                    mentions: c.mentions,
                    category: false,
                })
                .collect();
        }

        // ⚠️ 絞り込みも並べ替えもカテゴリの入れ子も [`Store`] が済ませている。
        // **画面が毎フレーム並べ替えると、フレームごとに順が揺れうる**
        self.live
            .store()
            .entries_of(GuildId::from(self.selected_guild))
            .map(|e| match e {
                ChannelEntry::Category(c) => ChannelRow {
                    id: c.id.get(),
                    name: c.display_name(),
                    icon: "",
                    topic: None,
                    unread: false,
                    mentions: 0,
                    category: true,
                },
                ChannelEntry::Channel(c) => ChannelRow {
                    id: c.id.get(),
                    name: c.display_name(),
                    icon: c.kind.icon(),
                    topic: c.topic.clone(),
                    // read-state はまだ無い。**無いものを在るように見せない**
                    unread: false,
                    mentions: 0,
                    category: false,
                },
            })
            .collect()
    }

    fn message_rows(&self) -> Vec<MessageRow> {
        if !self.uses_live() {
            return demo::MESSAGES
                .iter()
                .chain(&self.sent)
                .map(|m| MessageRow {
                    id: m.id,
                    author: m.author.to_string(),
                    time: m.time.to_string(),
                    body: m.body.to_string(),
                    mentioned: m.mentioned,
                })
                .collect();
        }

        let me = self.login.session().logged_in().map(|l| l.me.user.id);
        self.live
            .store()
            .messages(ChannelId::from(self.selected_channel))
            .iter()
            .map(|m| MessageRow {
                id: m.id.get(),
                author: m.author.name().to_owned(),
                time: local_time(&m.timestamp),
                body: m.content.clone(),
                // ⚠️ 本物のメンション判定は本文の解析が要る (C7)。
                // いまは**自分への返信かどうかだけ**を見ている
                mentioned: m
                    .referenced_message
                    .as_ref()
                    .is_some_and(|r| Some(r.author.id) == me),
            })
            .collect()
    }
}

/// 名前の頭 1 文字。アイコンが無いときの代わりに出す
fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

/// ISO 8601 の時刻を現地時刻の `HH:MM` にする。
///
/// ⚠️ **Discord が返すのは UTC である。** ずらさずに出すと、日本の利用者に
/// 9 時間ずれた時刻を見せることになる。
///
/// 読めない形が来たら**そのまま返す**。嘘の時刻を作るよりはましである。
fn local_time(iso: &str) -> String {
    // "2026-08-22T12:34:56.789000+00:00"
    let Some((_, time)) = iso.split_once('T') else {
        return iso.to_owned();
    };
    let mut parts = time.split(':');
    let (Some(h), Some(m)) = (parts.next(), parts.next()) else {
        return iso.to_owned();
    };
    let (Ok(h), Ok(m)) = (h.parse::<i32>(), m.parse::<i32>()) else {
        return iso.to_owned();
    };

    let total = h * 60 + m + gumicord_platform::local_utc_offset_minutes();
    // 日を跨いだぶんを畳む。**負の余りにならないよう rem_euclid を使う**
    let total = total.rem_euclid(24 * 60);
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// スクロールバー。**摘みの大きさと位置はレンダラが決める。**
///
/// はみ出し量はレイアウトしないと分からないので、テーマにもクライアントにも
/// 書けない。ここが渡すのは「この一覧にはスクロールバーがある」ことだけで、
/// 幅・余白・色はテーマが決める。
fn scrollbar() -> UiNode {
    UiNode::new(NodeId::LayoutScrollbar).child(UiNode::new(NodeId::LayoutScrollbarThumb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Gumicord {
        Gumicord::demo()
    }

    /// 同梱テーマが常に読める。ここが壊れると起動して真っ黒になる
    #[test]
    fn the_bundled_theme_parses() {
        let result = Theme::parse(DEFAULT_THEME);
        let errors: Vec<_> = result.errors().collect();
        assert!(errors.is_empty(), "同梱テーマに誤りがある: {errors:?}");
        assert!(result.is_applied());
    }

    /// 木が組め、選択状態が反映される
    #[test]
    fn the_tree_reflects_the_selection() {
        let mut a = app();
        a.selected_channel = demo::CHANNELS[2].id;
        let tree = a.build_tree(Panes::Three);

        let mut selected = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::NavChannelListItem && n.states.contains(State::Selected) {
                selected.push(n.key.clone());
            }
        });
        assert_eq!(selected, vec![Some(Key::Id(demo::CHANNELS[2].id))]);
    }

    /// テーマ解決まで通すと、`app.window` に背景が付く
    #[test]
    fn theme_reaches_the_tree() {
        let mut a = app();
        let cx = FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let window = &tree.children[0];
        assert_eq!(window.id, NodeId::AppWindow);
        assert!(
            window.style.background.is_some(),
            "app.window に背景が解決されていない"
        );
        // 継承が末端まで届いている
        let title = &window.children[0].children[0];
        assert_eq!(title.id, NodeId::ChromeTitlebarTitle);
        assert!(title.style.color.is_some(), "文字色が継承されていない");
    }

    /// 押しても何も変わらないなら、再描画を要求しない
    #[test]
    fn pressing_the_current_channel_changes_nothing() {
        let mut a = app();
        let hit = Hit {
            id: NodeId::NavChannelListItem,
            key: Some(Key::Id(a.selected_channel)),
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        };
        assert!(!a.pressed(std::slice::from_ref(&hit)));
    }
}

#[cfg(test)]
mod responsive_tests {
    use super::*;

    fn panes_in(tree: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| {
            if matches!(
                n.id,
                NodeId::NavGuildList | NodeId::NavChannelList | NodeId::ChatView
            ) {
                out.push(n.id);
            }
        });
        out
    }

    /// 幅で段が切り替わること。境界のちょうどの値は広いほうに入る
    #[test]
    fn panes_are_chosen_by_width() {
        assert_eq!(Panes::for_width(1280.0), Panes::Three);
        assert_eq!(Panes::for_width(900.0), Panes::Three);
        assert_eq!(Panes::for_width(899.0), Panes::Two);
        assert_eq!(Panes::for_width(600.0), Panes::Two);
        assert_eq!(Panes::for_width(599.0), Panes::One);
        assert_eq!(Panes::for_width(320.0), Panes::One);
    }

    /// 狭くしても**チャットは必ず残る**。
    /// 何も出ない幅があってはいけない
    #[test]
    fn the_chat_view_never_disappears() {
        let a = Gumicord::demo();
        for w in [320.0, 599.0, 600.0, 899.0, 900.0, 1920.0] {
            let tree = a.build_tree(Panes::for_width(w));
            assert!(
                panes_in(&tree).contains(&NodeId::ChatView),
                "幅 {w} でチャットが消えた"
            );
        }
    }

    #[test]
    fn narrower_windows_drop_panes_from_the_left() {
        let a = Gumicord::demo();

        assert_eq!(
            panes_in(&a.build_tree(Panes::Three)),
            vec![
                NodeId::NavGuildList,
                NodeId::NavChannelList,
                NodeId::ChatView
            ]
        );
        assert_eq!(
            panes_in(&a.build_tree(Panes::Two)),
            vec![NodeId::NavChannelList, NodeId::ChatView]
        );
        assert_eq!(panes_in(&a.build_tree(Panes::One)), vec![NodeId::ChatView]);
    }
}

#[cfg(test)]
mod input_tests {
    use gumicord_uitree::Content;

    use super::*;

    fn cx() -> FrameCx {
        FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        }
    }

    fn field(tree: &UiNode) -> Editable {
        let mut found = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatInputField
                && let Content::Editable(e) = &n.content
            {
                found = Some(e.clone());
            }
        });
        found.expect("入力欄が見つからない")
    }

    fn bodies(tree: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageContent
                && let Some(s) = n.content.as_text()
            {
                out.push(s.to_owned());
            }
        });
        out
    }

    /// フォーカスが無いと入力は届かない (`PLT-001`)
    #[test]
    fn input_only_reaches_a_focused_field() {
        let mut a = Gumicord::demo();
        assert!(a.focused_document().is_none());

        a.input_focused = true;
        assert!(a.focused_document().is_some());
    }

    /// 変換中の範囲が UITree まで届く。**下線を描くのに要る**
    #[test]
    fn a_composition_reaches_the_tree() {
        let mut a = Gumicord::demo();
        a.input_focused = true;

        let doc = a.focused_document().unwrap();
        doc.insert("送信: ");
        doc.set_composition("にほんご", None);

        let f = field(&a.build(&cx()));
        assert_eq!(f.text, "送信: にほんご");
        assert_eq!(
            f.composing,
            Some("送信: ".len().."送信: にほんご".len()),
            "変換中の範囲が伝わっていない"
        );
        assert_eq!(f.caret, f.text.len());
    }

    /// 空なら placeholder が入り、**変換の印は出ない**
    #[test]
    fn an_empty_field_shows_only_its_placeholder() {
        let mut a = Gumicord::demo();
        let f = field(&a.build(&cx()));
        assert!(f.text.is_empty());
        assert!(f.placeholder.contains("メッセージを送信"));
        assert!(f.composing.is_none());
    }

    /// FR-024: Enter で送ると一覧に増え、入力欄は空になる
    #[test]
    fn submitting_appends_the_message_and_clears_the_field() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        a.focused_document().unwrap().insert("こんにちは");

        let before = bodies(&a.build(&cx())).len();
        assert!(a.submit());

        let tree = a.build(&cx());
        let after = bodies(&tree);
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().map(String::as_str), Some("こんにちは"));
        assert!(field(&tree).text.is_empty(), "入力欄が空になっていない");
    }

    /// 空白だけのものは送らない
    #[test]
    fn whitespace_is_not_submitted() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        a.focused_document().unwrap().insert("   ");
        assert!(!a.submit());
    }

    /// Esc でフォーカスが外れる。**変換中の Esc は取り消しであって、
    /// フォーカス外しではない** — その分岐はプラットフォーム層が持つ
    #[test]
    fn escape_leaves_the_field() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        assert!(a.cancel_input());
        assert!(!a.input_focused);
        assert!(!a.cancel_input(), "既に外れていれば何も起きない");
    }
}

#[cfg(test)]
mod login_tests {
    use super::session::{Login, LoginEvent};
    use super::*;

    /// まだログインしていないアプリ。
    ///
    /// ⚠️ `Gumicord::new()` は `GUMICORD_SKIP_LOGIN` を読む。**開発機の
    /// 環境変数で試験の結果が変わってはいけない**ので、ここで潰す
    fn pending() -> Gumicord {
        Gumicord::with(Login::fresh_for_test(), Live::without_cache())
    }

    fn ids(tree: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| out.push(n.id));
        out
    }

    /// **ログインしていなければメイン画面は組み立てもしない。**
    /// 中身が見えているのに触れない状態が一番たちが悪い
    #[test]
    fn the_main_screen_is_not_built_before_login() {
        let a = pending();
        let seen = ids(&a.build_tree(Panes::Three));

        assert!(seen.contains(&NodeId::AppScreenLogin));
        assert!(!seen.contains(&NodeId::AppScreenMain));
        assert!(!seen.contains(&NodeId::ChatMessageList), "本文が漏れている");
    }

    /// QR が来る前に QR ノードを出さない。
    /// **読めない QR を見せるのは、何も見せないより悪い**
    #[test]
    fn the_qr_node_appears_only_once_there_is_a_qr() {
        let mut a = pending();
        assert!(!ids(&a.build_tree(Panes::Three)).contains(&NodeId::PrimitiveQr));

        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        let tree = a.build_tree(Panes::Three);
        assert!(ids(&tree).contains(&NodeId::PrimitiveQr));

        let mut data = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveQr {
                data = n.content.as_qr().map(str::to_owned);
            }
        });
        assert_eq!(data.as_deref(), Some("https://example/1"));
    }

    /// 進み具合が必ず文字で出ている。**黙って止まって見える状態を作らない**
    #[test]
    fn every_state_says_something() {
        let mut a = pending();
        for event in [
            None,
            Some(LoginEvent::Qr("x".to_owned())),
            Some(LoginEvent::Approved),
            Some(LoginEvent::Failed("接続できない".to_owned())),
        ] {
            if let Some(e) = event {
                a.login.apply_for_test(e);
            }
            let tree = a.build_tree(Panes::Three);

            let mut hint = None;
            tree.walk(&mut |n, _| {
                if n.id == NodeId::AppScreenLoginHint {
                    hint = n.content.as_text().map(str::to_owned);
                }
            });
            let hint = hint.expect("説明文が無い");
            assert!(!hint.trim().is_empty(), "説明文が空である");
        }
    }

    /// テーマがログイン画面まで届く。**QR の地は必ず明るい**
    #[test]
    fn the_theme_reaches_the_login_screen() {
        let mut a = pending();
        a.login.apply_for_test(LoginEvent::Qr("x".to_owned()));

        let tree = a.build(&FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        });

        let mut qr_style = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveQr {
                qr_style = Some(n.style.clone());
            }
        });
        let s = qr_style.expect("QR が無い");
        assert!(s.background.is_some(), "QR の地が解決されていない");
        assert!(s.padding.is_some(), "静音領域ぶんの余白が無い");
    }

    /// `GUMICORD_SKIP_LOGIN` 相当ならメイン画面が出る
    #[test]
    fn skipping_shows_the_main_screen() {
        let a = Gumicord::demo();
        assert!(a.login.shows_main());
        assert!(ids(&a.build_tree(Panes::Three)).contains(&NodeId::AppScreenMain));
    }

    /// 未ログインでも `Login::new` が勝手に走り出さない (試験が網を叩かない)
    #[test]
    fn nothing_starts_until_start_is_called() {
        let login = Login::fresh_for_test();
        assert!(!login.shows_main());
        assert!(login.session().qr().is_none());
    }
}

/// 「入力中」の一行を組み立てる。
///
/// ⚠️ **名前を全部並べない。** 賑やかなサーバでは 10 人が同時に打つことが
/// あり、そのまま並べると一行に収まらず、他の表示を押し出す。
/// Discord も 3 人までで打ち切る。
fn typing_line(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [a] => format!("  {a} が入力中…"),
        [a, b] => format!("  {a} と {b} が入力中…"),
        [a, b, c] => format!("  {a}、{b}、{c} が入力中…"),
        [a, b, rest @ ..] => format!("  {a}、{b} ほか {} 人が入力中…", rest.len()),
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    /// 誰も打っていなければ**何も出さない**
    #[test]
    fn nobody_typing_says_nothing() {
        assert_eq!(typing_line(&[]), "");
    }

    #[test]
    fn one_and_two_and_three_are_named() {
        assert_eq!(typing_line(&["あ"]), "  あ が入力中…");
        assert_eq!(typing_line(&["あ", "い"]), "  あ と い が入力中…");
        assert_eq!(typing_line(&["あ", "い", "う"]), "  あ、い、う が入力中…");
    }

    /// ⚠️ **賑やかなサーバでも一行に収まる。**
    /// 全部並べると他の表示を押し出す
    #[test]
    fn a_crowd_is_summarised() {
        let many = ["あ", "い", "う", "え", "お", "か"];
        assert_eq!(typing_line(&many), "  あ、い ほか 4 人が入力中…");
    }
}
