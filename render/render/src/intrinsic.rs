//! 安定 ID ごとの既定のレイアウト。
//!
//! # なぜレンダラが持つのか
//!
//! テーマは寸法を**すべては書かない**。公式サンプル `midnight` には
//! `nav.channel_list` の幅がなく、`chat.view` の「余りを埋める」も書かれていない。
//! それでも意図どおりに並ぶ必要がある。
//!
//! 「`nav.channel_list` は縦に並ぶ細い列である」は**その安定 ID の意味**から
//! 決まることであり、テーマの好みではない。したがってここが既定を持ち、
//! テーマは `width` / `height` でそれを上書きする。
//!
//! ⚠️ **この表は拡張 ABI ではない。** ここを変えても既存のテーマとプラグインは
//! 壊れない (見た目は変わる)。安定 ID の追加・削除の規則 (`EXT-003`) とは別物である。
//!
//! 仕様: [`spec/06-renderer.md`] 8 章

use gumicord_uitree::NodeId;

/// 子をどう並べるか。**この 4 種類しかない** ([`spec/03-uitree.md`] 3.6)。
/// Flexbox もグリッドも持たない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// 横並び
    Row,
    /// 縦並び
    Column,
    /// 重ね。子は全員同じ矩形をもらう
    Stack,
}

/// 交差軸での子の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cross {
    /// 交差軸いっぱいに広げる
    Stretch,
    /// 内容の大きさのまま先頭に置く
    Start,
    /// 内容の大きさのまま中央に置く
    Center,
}

/// 安定 ID から決まる既定のレイアウト。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intrinsic {
    pub axis: Axis,
    /// 親の主軸の余りをどれだけ取るか。0 なら取らない
    pub grow: f32,
    pub cross: Cross,
    /// 既定の幅 (論理 px)。テーマの `width` が優先される
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// はみ出しを切り、スクロールできるか
    pub scroll: bool,
    /// スクロール位置の既定が**末尾**か。
    ///
    /// メッセージ一覧は最新が下にあり、開いたときに見えているべきなのは
    /// 一番古い行ではなく一番新しい行である。これは利用者の好みではなく
    /// 「メッセージ一覧とは何か」から決まる。
    pub anchor_end: bool,
}

impl Intrinsic {
    const fn row() -> Self {
        Intrinsic {
            axis: Axis::Row,
            grow: 0.0,
            cross: Cross::Center,
            width: None,
            height: None,
            scroll: false,
            anchor_end: false,
        }
    }

    const fn column() -> Self {
        Intrinsic {
            axis: Axis::Column,
            ..Intrinsic::row()
        }
    }

    const fn stack() -> Self {
        Intrinsic {
            axis: Axis::Stack,
            ..Intrinsic::row()
        }
    }

    const fn grow(mut self, g: f32) -> Self {
        self.grow = g;
        self
    }

    const fn cross(mut self, c: Cross) -> Self {
        self.cross = c;
        self
    }

    const fn w(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    const fn h(mut self, h: f32) -> Self {
        self.height = Some(h);
        self
    }

    const fn scrollable(mut self) -> Self {
        self.scroll = true;
        self
    }

    /// スクロールし、既定で末尾を見せる。
    const fn scrollable_to_end(mut self) -> Self {
        self.scroll = true;
        self.anchor_end = true;
        self
    }
}

/// 既定のサイドバー幅。Discord の慣習に合わせてある
const CHANNEL_LIST_W: f32 = 240.0;
/// 独自タイトルバーの高さ (`PLT-020`)
const TITLEBAR_H: f32 = 32.0;
/// タイトルバーのボタン 1 個の幅。Windows の慣習値
const TITLEBAR_BUTTON_W: f32 = 46.0;

/// その安定 ID の既定のレイアウト。
///
/// **知らない ID には縦並びを返す。** 新しいクライアント向けのプラグインが
/// 挿入したノードでも、少なくとも積まれて見える。
pub fn intrinsic(id: NodeId) -> Intrinsic {
    use NodeId::*;
    match id {
        // ── app.* — 画面いっぱいに広がる入れ物
        AppRoot | AppWindow | AppScreen => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        AppScreenLoading | AppScreenLogin => Intrinsic::column().grow(1.0).cross(Cross::Center),
        // メイン画面だけが横 3 ペインである
        AppScreenMain => Intrinsic::row().grow(1.0).cross(Cross::Stretch),

        // ── chrome.*
        ChromeTitlebar => Intrinsic::row().h(TITLEBAR_H).cross(Cross::Stretch),
        // 題名が余りを取ることで、操作ボタンが右端へ寄る
        ChromeTitlebarTitle => Intrinsic::row().grow(1.0),
        ChromeTitlebarControls => Intrinsic::row().cross(Cross::Stretch),
        ChromeTitlebarControl => Intrinsic::stack().w(TITLEBAR_BUTTON_W),

        // ── nav.* — ギルド一覧は内容 (48px の丸) が幅を決める
        NavGuildList => Intrinsic::column().cross(Cross::Start).scrollable(),
        NavGuildListHome | NavGuildListItem | NavGuildListItemIcon => {
            Intrinsic::stack().w(48.0).h(48.0)
        }
        NavGuildListItemBadge => Intrinsic::row(),

        NavChannelList | NavDmList => Intrinsic::column()
            .w(CHANNEL_LIST_W)
            .cross(Cross::Stretch)
            .scrollable(),
        NavChannelListHeader => Intrinsic::row().h(48.0).cross(Cross::Center),
        NavChannelListCategory => Intrinsic::row().cross(Cross::Center),
        NavChannelListItem | NavDmListItem => Intrinsic::row().cross(Cross::Center),
        NavChannelListItemIcon => Intrinsic::stack().w(20.0).h(20.0),
        // 名前が余りを取ることで、バッジが右端へ寄る
        NavChannelListItemName => Intrinsic::row().grow(1.0),
        NavChannelListItemBadge => Intrinsic::row(),

        // ── chat.*
        ChatView => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        ChatHeader => Intrinsic::row().h(48.0).cross(Cross::Center),
        ChatHeaderTitle => Intrinsic::row(),
        // 話題が余りを取る。長ければ切り詰められる
        ChatHeaderTopic => Intrinsic::row().grow(1.0),
        // ここが縦の余りを全部取り、はみ出したぶんがスクロールになる
        ChatMessageList => Intrinsic::column()
            .grow(1.0)
            .cross(Cross::Stretch)
            .scrollable_to_end(),
        // アイコンと本文の横並び。本文側は layout.column が包む
        ChatMessage => Intrinsic::row().cross(Cross::Start),
        ChatMessageAvatar => Intrinsic::stack().w(40.0).h(40.0),
        ChatMessageHeader => Intrinsic::row().cross(Cross::Center),
        ChatMessageHeaderAuthor | ChatMessageHeaderTime => Intrinsic::row(),
        ChatMessageHeaderBadges => Intrinsic::row(),
        ChatMessageReplyRef => Intrinsic::row().cross(Cross::Center),
        ChatMessageContent => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageAttachments | ChatMessageEmbeds => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageAttachment | ChatMessageEmbed => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageActions => Intrinsic::row(),
        ChatTypingIndicator => Intrinsic::row().h(24.0).cross(Cross::Center),
        ChatInput => Intrinsic::column().cross(Cross::Stretch),
        ChatInputField => Intrinsic::column().cross(Cross::Stretch),
        ChatInputToolbar => Intrinsic::row().cross(Cross::Center),
        ChatInputActions => Intrinsic::row().cross(Cross::Center),

        // ── primitive.* — ほとんどが内容そのもの
        PrimitiveDivider => Intrinsic::row().h(1.0).cross(Cross::Stretch),
        PrimitiveAvatar => Intrinsic::stack().w(40.0).h(40.0),
        PrimitiveIcon | PrimitiveEmoji => Intrinsic::stack().w(20.0).h(20.0),
        PrimitiveSpinner => Intrinsic::stack().w(20.0).h(20.0),
        PrimitiveImage => Intrinsic::stack(),
        PrimitiveCodeBlock => Intrinsic::column().cross(Cross::Stretch),
        PrimitiveText | PrimitiveBadge | PrimitiveButton | PrimitiveMention | PrimitiveSpoiler
        | PrimitiveLink => Intrinsic::row().cross(Cross::Center),

        // ── layout.* — プラグインの語彙
        //
        // row / column が既定で余りを取るのは、これが「入れ物」として使われる
        // ためである。chat.message の中で本文側を包む column は、アイコンの
        // 右の余りを全部取らないと折り返し幅が決まらない。
        LayoutRow => Intrinsic::row().grow(1.0).cross(Cross::Center),
        LayoutColumn => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        LayoutStack => Intrinsic::stack().grow(1.0).cross(Cross::Stretch),
        LayoutScroll => Intrinsic::column()
            .grow(1.0)
            .cross(Cross::Stretch)
            .scrollable(),
        // 余りを食べるためだけのノード
        LayoutSpacer => Intrinsic::row().grow(1.0),

        // `#[non_exhaustive]` なので網羅できない。知らない ID は縦に積む
        _ => Intrinsic::column().cross(Cross::Stretch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 定義済みのすべての ID が既定を持つ (どれも panic しない)
    #[test]
    fn every_stable_id_has_a_default() {
        for id in NodeId::ALL {
            let i = intrinsic(*id);
            assert!(i.grow >= 0.0, "{id} の grow が負");
        }
    }

    /// 主軸の余りを取るノードが各軸に少なくとも 1 つあること。
    /// なければ画面が埋まらない
    #[test]
    fn the_main_screen_can_fill_the_window() {
        assert_eq!(intrinsic(NodeId::AppScreenMain).axis, Axis::Row);
        assert!(intrinsic(NodeId::ChatView).grow > 0.0, "横の余りを取る");
        assert!(
            intrinsic(NodeId::ChatMessageList).grow > 0.0,
            "縦の余りを取る"
        );
    }

    /// スクロールするのは一覧だけである。
    /// 予期しないノードで切り取りが起きると原因が分かりにくい
    #[test]
    fn only_lists_scroll() {
        let scrolling: Vec<_> = NodeId::ALL
            .iter()
            .filter(|id| intrinsic(**id).scroll)
            .copied()
            .collect();
        assert_eq!(
            scrolling,
            vec![
                NodeId::NavGuildList,
                NodeId::NavChannelList,
                NodeId::NavDmList,
                NodeId::ChatMessageList,
                NodeId::LayoutScroll,
            ]
        );
    }
}
