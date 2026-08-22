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
//! # いま通っているのは [1] [3] [5] [7] [8] だけである
//!
//! | 段 | 状態 |
//! |---|---|
//! | [1] 入力 | ポインタのみ。キーボードは P2 (TSF) と一緒に入る |
//! | [2] 状態更新 | ない。[`demo`] の固定データを読んでいる |
//! | [3] UITree 構築 | ある。ただし毎フレーム全体を組み直している |
//! | [4] [6] プラグイン | ない (E4, E5) |
//! | [5] テーマ解決 | ある |
//! | [7] [8] レイアウトと描画 | ある |
//! | [9] a11y | ない (P3) |
//!
//! 仕様: [`spec/02-architecture.md`]

pub mod demo;

use std::borrow::Cow;

use gumicord_platform::{Application, FrameCx, TextDocument};
use gumicord_render::Hit;
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::{Editable, Key, NodeId, State, UiNode};

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
    /// ポインタが乗っているノード
    hovered: Option<(NodeId, Option<Key>)>,
    selected_guild: u64,
    selected_channel: u64,
    /// 入力欄にフォーカスがあるか
    input_focused: bool,
    /// 入力欄の中身 (`PLT-001`)
    input: TextDocument,
    /// この場で送ったメッセージ。**Store (C5) ができたら消える**
    sent: Vec<demo::Message>,
}

impl Gumicord {
    pub fn new() -> Self {
        Gumicord {
            theme: load_theme(),
            hovered: None,
            selected_guild: demo::GUILDS[0].id,
            selected_channel: demo::CHANNELS[1].id,
            input_focused: false,
            input: TextDocument::new(),
            sent: Vec::new(),
        }
    }

    /// 画面に出すメッセージ。デモの固定分と、この場で送った分
    fn messages(&self) -> impl Iterator<Item = &demo::Message> {
        demo::MESSAGES.iter().chain(&self.sent)
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
                    changed |= self.selected_guild != *id;
                    self.selected_guild = *id;
                }
                (NodeId::NavChannelListItem, Some(Key::Id(id))) => {
                    changed |= self.selected_channel != *id;
                    self.selected_channel = *id;
                }
                _ => continue,
            }
            break;
        }
        changed
    }

    /// `PLT-001`: 入力を受け取るのは、フォーカスのある入力欄だけである
    fn focused_document(&mut self) -> Option<&mut TextDocument> {
        self.input_focused.then_some(&mut self.input)
    }

    /// `FR-024`: 送信。
    ///
    /// Store (C5) と REST (C3) ができるまでは、その場で一覧に足すだけである。
    /// **送れたように見えるが、どこへも行っていない。**
    fn submit(&mut self) -> bool {
        let body = self.input.text().trim().to_owned();
        if body.is_empty() {
            return false;
        }
        self.input.take();

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
        let main = UiNode::new(NodeId::AppScreenMain)
            .child_if(panes.guilds(), || self.guild_list())
            .child_if(panes.channels(), || self.channel_list())
            .child(self.chat_view());

        UiNode::new(NodeId::AppRoot).child(
            UiNode::new(NodeId::AppWindow)
                .child(self.titlebar())
                .child(UiNode::new(NodeId::AppScreen).child(main)),
        )
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

        UiNode::new(NodeId::ChromeTitlebar)
            .child(UiNode::text(NodeId::ChromeTitlebarTitle, "  Gumicord"))
            .child(
                UiNode::new(NodeId::ChromeTitlebarControls)
                    .child(button("minimize", "window.minimize"))
                    .child(button("maximize", "window.maximize"))
                    .child(button("close", "window.close")),
            )
    }

    fn guild_list(&self) -> UiNode {
        let mut list = UiNode::new(NodeId::NavGuildList).child(
            UiNode::text(NodeId::NavGuildListHome, "DM").with_state_if(
                self.is_hovered(NodeId::NavGuildListHome, None),
                State::Hover,
            ),
        );

        for g in demo::GUILDS {
            list = list.child(
                UiNode::text(NodeId::NavGuildListItem, demo::initial(g.name))
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
        list
    }

    fn channel_list(&self) -> UiNode {
        let guild = demo::GUILDS
            .iter()
            .find(|g| g.id == self.selected_guild)
            .unwrap_or(&demo::GUILDS[0]);

        let mut list = UiNode::new(NodeId::NavChannelList)
            .child(UiNode::text(NodeId::NavChannelListHeader, guild.name))
            .child(UiNode::text(
                NodeId::NavChannelListCategory,
                "テキストチャンネル",
            ));

        for c in demo::CHANNELS {
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
        let channel = demo::CHANNELS
            .iter()
            .find(|c| c.id == self.selected_channel)
            .unwrap_or(&demo::CHANNELS[0]);

        let header = UiNode::new(NodeId::ChatHeader)
            .with_data(channel.id)
            .child(UiNode::icon(NodeId::PrimitiveIcon, channel.icon))
            .child(UiNode::text(NodeId::ChatHeaderTitle, channel.name).with_data(channel.id))
            .child(
                UiNode::text(
                    NodeId::ChatHeaderTopic,
                    "自前レンダラの縦通し。テーマ JSON だけで見た目が決まる",
                )
                .with_data(channel.id),
            );

        // 直前と同じ送信者なら送信者行を繰り返さない。
        // **字下げの量はテーマが決める** (`when.state: "grouped"` の padding)
        let mut messages = UiNode::new(NodeId::ChatMessageList);
        let mut prev: Option<&str> = None;
        for m in self.messages() {
            messages = messages.child(self.message(m, prev == Some(&*m.author)));
            prev = Some(&m.author);
        }
        messages = messages.child(scrollbar());

        UiNode::new(NodeId::ChatView)
            .child(header)
            .child(messages)
            .child(UiNode::text(
                NodeId::ChatTypingIndicator,
                "  みどり が入力中…",
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
                            placeholder: format!("#{} へメッセージを送信", channel.name),
                        },
                    )
                    .with_state_if(self.input_focused, State::Focus),
                ),
            )
    }

    /// メッセージ 1 件。
    ///
    /// `grouped` なら送信者アイコンと送信者行を出さない。**字下げはテーマが
    /// `when.state: "grouped"` の `padding` で決める。** クライアントが
    /// 空白のノードを挟むと、字下げの量が焼き付いてテーマから揃えられない。
    fn message(&self, m: &demo::Message, grouped: bool) -> UiNode {
        let body = UiNode::new(NodeId::LayoutColumn)
            .child_if(!grouped, || {
                UiNode::new(NodeId::ChatMessageHeader)
                    .with_data(m.id)
                    .child(
                        UiNode::text(NodeId::ChatMessageHeaderAuthor, &*m.author).with_data(m.id),
                    )
                    .child(
                        UiNode::text(NodeId::ChatMessageHeaderTime, format!("  {}", m.time))
                            .with_data(m.id),
                    )
            })
            .child(UiNode::text(NodeId::ChatMessageContent, &*m.body).with_data(m.id));

        UiNode::new(NodeId::ChatMessage)
            .with_id_key(m.id)
            .with_data(m.id)
            .with_state_if(grouped, State::Grouped)
            .with_state_if(m.mentioned, State::Mentioned)
            .with_state_if(self.hovered_id(NodeId::ChatMessage, m.id), State::Hover)
            .child_if(!grouped, || {
                UiNode::text(NodeId::ChatMessageAvatar, demo::initial(&m.author)).with_data(m.id)
            })
            // 送信者行と本文を縦に積む。`layout.column` はこのためにある
            .child(body)
    }
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
        Gumicord::new()
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
        let a = Gumicord::new();
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
        let a = Gumicord::new();

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
        let mut a = Gumicord::new();
        assert!(a.focused_document().is_none());

        a.input_focused = true;
        assert!(a.focused_document().is_some());
    }

    /// 変換中の範囲が UITree まで届く。**下線を描くのに要る**
    #[test]
    fn a_composition_reaches_the_tree() {
        let mut a = Gumicord::new();
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
        let mut a = Gumicord::new();
        let f = field(&a.build(&cx()));
        assert!(f.text.is_empty());
        assert!(f.placeholder.contains("メッセージを送信"));
        assert!(f.composing.is_none());
    }

    /// FR-024: Enter で送ると一覧に増え、入力欄は空になる
    #[test]
    fn submitting_appends_the_message_and_clears_the_field() {
        let mut a = Gumicord::new();
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
        let mut a = Gumicord::new();
        a.input_focused = true;
        a.focused_document().unwrap().insert("   ");
        assert!(!a.submit());
    }

    /// Esc でフォーカスが外れる。**変換中の Esc は取り消しであって、
    /// フォーカス外しではない** — その分岐はプラットフォーム層が持つ
    #[test]
    fn escape_leaves_the_field() {
        let mut a = Gumicord::new();
        a.input_focused = true;
        assert!(a.cancel_input());
        assert!(!a.input_focused);
        assert!(!a.cancel_input(), "既に外れていれば何も起きない");
    }
}
