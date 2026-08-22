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

use gumicord_platform::{Application, FrameCx};
use gumicord_render::Hit;
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::{Key, NodeId, State, UiNode};

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
];

/// アプリケーションの状態と、そこから UITree を組み立てる責務。
pub struct Gumicord {
    theme: Option<Theme>,
    /// ポインタが乗っているノード
    hovered: Option<(NodeId, Option<Key>)>,
    selected_guild: u64,
    selected_channel: u64,
    /// 入力欄にフォーカスがあるか。P2 (TSF) が入るまでは見た目だけ
    input_focused: bool,
}

impl Gumicord {
    pub fn new() -> Self {
        Gumicord {
            theme: load_theme(),
            hovered: None,
            selected_guild: demo::GUILDS[0].id,
            selected_channel: demo::CHANNELS[1].id,
            input_focused: false,
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

    /// パイプラインの [3] と [5]。
    ///
    /// [4] と [6] (プラグインの介入) はまだない。入るのはこの間である。
    fn build(&mut self, cx: &FrameCx) -> UiNode {
        // [3] UITree 構築
        let mut tree = self.build_tree();

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
    fn build_tree(&self) -> UiNode {
        UiNode::new(NodeId::AppRoot).child(
            UiNode::new(NodeId::AppWindow).child(self.titlebar()).child(
                UiNode::new(NodeId::AppScreen).child(
                    UiNode::new(NodeId::AppScreenMain)
                        .child(self.guild_list())
                        .child(self.channel_list())
                        .child(self.chat_view()),
                ),
            ),
        )
    }

    /// `PLT-020`: 独自タイトルバー。
    ///
    /// ボタンは `key` の [`Key::Slot`] で区別する。プラットフォーム層は
    /// この文字列だけを見てウィンドウ操作へつなぐ。
    fn titlebar(&self) -> UiNode {
        let button = |slot: &'static str, glyph: &str| {
            UiNode::text(NodeId::ChromeTitlebarControl, glyph)
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
                    .child(button("minimize", "\u{2013}"))
                    .child(button("maximize", "\u{25a1}"))
                    .child(button("close", "\u{2715}")),
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
                .child(UiNode::text(NodeId::NavChannelListItemIcon, c.icon).with_data(c.id))
                .child(UiNode::text(NodeId::NavChannelListItemName, c.name).with_data(c.id));

            if c.mentions > 0 {
                item = item.child(
                    UiNode::text(NodeId::NavChannelListItemBadge, c.mentions.to_string())
                        .with_data(c.id),
                );
            }
            list = list.child(item);
        }
        list
    }

    fn chat_view(&self) -> UiNode {
        let channel = demo::CHANNELS
            .iter()
            .find(|c| c.id == self.selected_channel)
            .unwrap_or(&demo::CHANNELS[0]);

        let header = UiNode::new(NodeId::ChatHeader)
            .with_data(channel.id)
            .child(
                UiNode::text(NodeId::ChatHeaderTitle, format!("# {}", channel.name))
                    .with_data(channel.id),
            )
            .child(
                UiNode::text(
                    NodeId::ChatHeaderTopic,
                    "  自前レンダラの縦通し。テーマ JSON だけで見た目が決まる",
                )
                .with_data(channel.id),
            );

        let mut messages = UiNode::new(NodeId::ChatMessageList);
        for m in demo::MESSAGES {
            messages = messages.child(self.message(m));
        }

        UiNode::new(NodeId::ChatView)
            .child(header)
            .child(messages)
            .child(UiNode::text(
                NodeId::ChatTypingIndicator,
                "  みどり が入力中…",
            ))
            .child(
                UiNode::new(NodeId::ChatInput).child(
                    UiNode::text(
                        NodeId::ChatInputField,
                        format!("#{} へメッセージを送信", channel.name),
                    )
                    .with_state_if(self.input_focused, State::Focus),
                ),
            )
    }

    fn message(&self, m: &demo::Message) -> UiNode {
        UiNode::new(NodeId::ChatMessage)
            .with_id_key(m.id)
            .with_data(m.id)
            .with_state_if(m.mentioned, State::Mentioned)
            .with_state_if(self.hovered_id(NodeId::ChatMessage, m.id), State::Hover)
            .child(UiNode::text(NodeId::ChatMessageAvatar, demo::initial(m.author)).with_data(m.id))
            // 送信者行と本文を縦に積む。`layout.column` はこのためにある
            .child(
                UiNode::new(NodeId::LayoutColumn)
                    .child(
                        UiNode::new(NodeId::ChatMessageHeader)
                            .with_data(m.id)
                            .child(
                                UiNode::text(NodeId::ChatMessageHeaderAuthor, m.author)
                                    .with_data(m.id),
                            )
                            .child(
                                UiNode::text(
                                    NodeId::ChatMessageHeaderTime,
                                    format!("  {}", m.time),
                                )
                                .with_data(m.id),
                            ),
                    )
                    .child(UiNode::text(NodeId::ChatMessageContent, m.body).with_data(m.id)),
            )
    }
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
        let tree = a.build_tree();

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
