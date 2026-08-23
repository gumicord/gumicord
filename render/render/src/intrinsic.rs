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
    /// 重ねの中で、**親いっぱいに広がらない**か。
    ///
    /// # ⚠️ 重ねの子は既定で広がる
    ///
    /// サーバの絵もスクロールバーの摘みも、入れ物の形をそのまま使いたい。
    /// だから大きさを書かない子は親の矩形をもらう。
    ///
    /// **印は違う。** メンションの数は数字のぶんの幅しか要らない。同じ
    /// 規則を当てると、56×48 の赤い丸がアイコンを覆う。実際にそうなった。
    ///
    /// 大きさを書けば済む話ではない。**桁が増えれば横に伸びる**ものなので、
    /// 幅を焼き付けるわけにはいかない。
    pub hugs_content: bool,
    /// 交差軸の大きさを**決めない**。親が決めた分をもらうだけか。
    ///
    /// ⚠️ **自分の欄は左側の幅を決めない。** 幅を決めるのはサーバ一覧と
    /// チャンネル一覧のほうであって、その下に敷かれる帯ではない。ここを
    /// 外すと、帯が入れ物いっぱいに広がろうとした分だけ左側が広がり、
    /// **チャットの幅が 0 になって何も出なくなる**。
    pub follows_cross: bool,
    /// 1 行に収めて、はみ出したら「…」で切るか。
    ///
    /// ⚠️ **一覧の項目は折り返してはいけない。** 行の高さが揃わなくなり、
    /// 一覧として読めなくなる。Discord も切って「…」を出す
    pub single_line: bool,
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
            hugs_content: false,
            follows_cross: false,
            single_line: false,
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

    /// 1 行に収める。はみ出したら「…」で切る
    const fn one_line(mut self) -> Self {
        self.single_line = true;
        self
    }

    /// 重ねの中でも、中身の大きさで置かれる
    const fn hugs_content(mut self) -> Self {
        self.hugs_content = true;
        self
    }

    /// 交差軸では親に従い、親の大きさを決めない
    const fn follows_cross(mut self) -> Self {
        self.follows_cross = true;
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
/// メンバー一覧の幅。Discord の慣習に合わせてある
const MEMBER_LIST_W: f32 = 240.0;
/// 独自タイトルバーの高さ (`PLT-020`)
const TITLEBAR_H: f32 = 32.0;
/// タイトルバーのボタン 1 個の幅。Windows の慣習値
const TITLEBAR_BUTTON_W: f32 = 46.0;
/// スクロールバーの幅。細くても掴めるよう、摘みより広めに取る
const SCROLLBAR_W: f32 = 10.0;
/// QR の既定の一辺 (論理 px)。公式クライアントとほぼ同じ大きさ
const QR_SIZE: f32 = 176.0;

/// そのノードは親の流れに入らず、親の矩形へ重ねて置かれるか。
///
/// スクロールバーは一覧の縁に浮いていて、**中身と一緒にスクロールしない**。
/// 縦に積まれる子の 1 つとして扱うと、一覧の末尾に居座ってしまう。
pub fn is_overlay(id: NodeId) -> bool {
    id == NodeId::LayoutScrollbar
}

/// その安定 ID の既定のレイアウト。
///
/// **知らない ID には縦並びを返す。** 新しいクライアント向けのプラグインが
/// 挿入したノードでも、少なくとも積まれて見える。
pub fn intrinsic(id: NodeId) -> Intrinsic {
    use NodeId::*;
    match id {
        // ── app.* — 画面いっぱいに広がる入れ物
        // ⚠️ **根だけは重ねである。** 浮かせる層をここへ載せるためで、
        // 窓は [タイトルバー, 画面] の縦並びなので載せられない
        AppRoot => Intrinsic::stack().grow(1.0).cross(Cross::Stretch),
        AppWindow | AppScreen => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        AppScreenLoading | AppScreenLogin => Intrinsic::column().grow(1.0).cross(Cross::Center),
        AppScreenLoginTitle | AppScreenLoginHint => Intrinsic::row().cross(Cross::Center),
        // メイン画面だけが横 3 ペインである
        AppScreenMain => Intrinsic::row().grow(1.0).cross(Cross::Stretch),

        // ── chrome.*
        ChromeTitlebar => Intrinsic::row().h(TITLEBAR_H).cross(Cross::Stretch),
        // 題名が余りを取ることで、操作ボタンが右端へ寄る
        ChromeTitlebarTitle => Intrinsic::row().grow(1.0).one_line(),
        ChromeTitlebarControls => Intrinsic::row().cross(Cross::Stretch),
        ChromeTitlebarControl => Intrinsic::stack().w(TITLEBAR_BUTTON_W),

        // ── nav.* — ギルド一覧は内容 (48px の丸) が幅を決める
        NavGuildList => Intrinsic::column().cross(Cross::Start).scrollable(),
        NavGuildListHome | NavGuildListFolderIcon => Intrinsic::stack().w(48.0).h(48.0),
        // ⚠️ **絵より広い。** 左端に印 (`pill`) の通り道がある。
        // 絵をそのまま項目にすると、印を置く場所が無い
        NavGuildListItem => Intrinsic::stack().w(56.0).h(48.0),
        // ⚠️ **大きさを持たない。** 入れ物いっぱいに広がる。
        // 閉じたフォルダに敷き詰めるときだけ、テーマが小さくする
        NavGuildListItemIcon => Intrinsic::stack(),
        // ⚠️ **フォルダは中身を抱える入れ物である。** 開いているときは
        // 目印と中のサーバが縦に並び、**その背景が 1 枚で後ろを通る**。
        // 高さは中身が決めるので、ここでは決めない
        NavGuildListFolder => Intrinsic::column().cross(Cross::Center).w(48.0),
        // ⚠️ **絵の隣ではなく、絵と同じ場所に重なる。** 流れの中に置くと
        // 出た瞬間にサーバの絵が右へずれる。左端へ寄せるのは
        // テーマの `margin` の仕事である
        NavGuildListItemPill => Intrinsic::stack().w(4.0),
        // ⚠️ **重ねの中で広がらない。** 数字のぶんの幅しか要らない
        NavGuildListItemBadge => Intrinsic::row().one_line().hugs_content(),

        // ⚠️ **これ自体は巻かない。** 中の `layout.scroll` だけが巻く。
        // 全部を 1 つの領域にすると、下まで巻いたときに**見出しも自分も
        // 見えなくなる**
        NavChannelList => Intrinsic::column().w(CHANNEL_LIST_W).cross(Cross::Stretch),
        NavDmList => Intrinsic::column()
            .w(CHANNEL_LIST_W)
            .cross(Cross::Stretch)
            .scrollable(),
        NavChannelListHeader => Intrinsic::row().h(48.0).cross(Cross::Center).one_line(),
        NavChannelListCategory => Intrinsic::row().cross(Cross::Center).one_line(),
        NavChannelListItem | NavDmListItem => Intrinsic::row().cross(Cross::Center),
        NavChannelListItemIcon => Intrinsic::stack().w(20.0).h(20.0),
        // 名前が余りを取ることで、バッジが右端へ寄る
        NavChannelListItemName => Intrinsic::row().grow(1.0).one_line(),
        NavChannelListItemBadge => Intrinsic::row(),

        // ── nav.sidebar — 左側全体
        //
        // ⚠️ **余りを取らない。** 中身 (サーバ一覧 + チャンネル一覧) が
        // 幅を決める。ここが伸びるとチャットから幅を奪う
        NavSidebar => Intrinsic::column().cross(Cross::Stretch),
        NavSidebarLists => Intrinsic::row().grow(1.0).cross(Cross::Stretch),

        // ── nav.user_panel — 一覧の一番下に居座る
        //
        // ⚠️ **一覧と一緒にスクロールしない。** 自分が誰かは、
        // どこまで巻いていても見えていなければならない。
        // 幅は一覧が決める (`follows_cross`)
        NavUserPanel => Intrinsic::row()
            .h(52.0)
            .cross(Cross::Center)
            .follows_cross(),
        NavUserPanelAvatar => Intrinsic::stack().w(32.0).h(32.0),
        // 名前と言葉を縦に積む。**余りを取って、右に何か置けるようにする**
        NavUserPanelName => Intrinsic::row().grow(1.0).one_line(),
        NavUserPanelStatus => Intrinsic::row().grow(1.0).one_line(),
        // ⚠️ **アバターの隅に重なる。** 流れの中に置くと名前が右へずれる
        NavUserPanelPresence => Intrinsic::stack().w(12.0).h(12.0),

        // ── nav.member_list — 右端。**チャットの隣に立つ細い列**
        //
        // ⚠️ **これ自体が巻く。** 見出しも一緒に流れてよい。役職の見出しは
        // その群の先頭であって、一覧全体の見出しではない
        NavMemberList => Intrinsic::column()
            .w(MEMBER_LIST_W)
            .cross(Cross::Stretch)
            .scrollable(),
        NavMemberListGroup => Intrinsic::row().cross(Cross::Center).one_line(),
        NavMemberListItem => Intrinsic::row().cross(Cross::Center),
        NavMemberListItemAvatar => Intrinsic::stack().w(32.0).h(32.0),
        // ⚠️ **アバターの隅に重なる。** 流れの中に置くと名前が右へずれる
        NavMemberListItemPresence => Intrinsic::stack().w(12.0).h(12.0),
        NavMemberListItemName => Intrinsic::row().grow(1.0).one_line(),

        // ── chat.*
        ChatView => Intrinsic::column().grow(1.0).cross(Cross::Stretch),
        ChatHeader => Intrinsic::row().h(48.0).cross(Cross::Center),
        ChatHeaderTitle => Intrinsic::row().one_line(),
        // 話題が余りを取る。長ければ切り詰められる
        ChatHeaderTopic => Intrinsic::row().grow(1.0).one_line(),
        // ここが縦の余りを全部取り、はみ出したぶんがスクロールになる
        ChatMessageList => Intrinsic::column()
            .grow(1.0)
            .cross(Cross::Stretch)
            .scrollable_to_end(),
        // アイコンと本文の横並び。本文側は layout.column が包む
        ChatMessage => Intrinsic::row().cross(Cross::Start),
        ChatMessageAvatar => Intrinsic::stack().w(40.0).h(40.0),
        ChatMessageHeader => Intrinsic::row().cross(Cross::Center),
        ChatMessageHeaderAuthor | ChatMessageHeaderTime => Intrinsic::row().one_line(),
        ChatMessageHeaderBadges => Intrinsic::row(),
        ChatMessageReplyRef => Intrinsic::row().cross(Cross::Center),
        ChatMessageContent => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageAttachments | ChatMessageEmbeds => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageAttachment | ChatMessageEmbed => Intrinsic::column().cross(Cross::Stretch),
        ChatMessageActions => Intrinsic::row(),
        ChatTypingIndicator => Intrinsic::row().h(24.0).cross(Cross::Center).one_line(),
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
        // QR は**読める大きさ**が要る。内容から決まらないので既定を持つ。
        // 小さすぎるとスマホのカメラが拾えず、そこで詰まる
        PrimitiveQr => Intrinsic::stack().w(QR_SIZE).h(QR_SIZE),
        PrimitiveCodeBlock => Intrinsic::column().cross(Cross::Stretch),
        PrimitiveText | PrimitiveBadge | PrimitiveButton | PrimitiveMention | PrimitiveSpoiler
        | PrimitiveLink => Intrinsic::row().cross(Cross::Center),

        // ── overlay.* — 流れの上に浮かせるもの
        //
        // ⚠️ **層は窓いっぱいに広がる。** 広がらないと、外を押して閉じる
        // ための当たりが無くなり、開いたら閉じられなくなる
        OverlayLayer | OverlayScrim => Intrinsic::stack().grow(1.0).cross(Cross::Stretch),
        // 浮かぶ箱は**中身の大きさになる**。広がると、押して閉じるための
        // 外側が無くなる
        OverlayPopover | OverlayMenu => Intrinsic::column().cross(Cross::Stretch).hugs_content(),
        // 面は横いっぱい・縦は中身。下から出てくる
        OverlaySheet => Intrinsic::column().cross(Cross::Stretch).hugs_content(),
        OverlaySheetHandle => Intrinsic::row().cross(Cross::Center),
        OverlayMenuItem => Intrinsic::row().cross(Cross::Center),
        OverlayMenuItemIcon => Intrinsic::row().cross(Cross::Center),
        OverlayMenuItemLabel => Intrinsic::row().cross(Cross::Center).one_line(),
        OverlayMenuSeparator => Intrinsic::row().h(1.0).cross(Cross::Stretch),

        // 確かめる窓は**基準の点を持たない**。押した場所ではなく画面の
        // 真ん中に出る。重ねの子は流れにも従わず中央に置かれるので、
        // 中身の大きさになる (`hugs_content`) だけで真ん中に来る
        OverlayModal => Intrinsic::column().cross(Cross::Stretch).hugs_content(),
        OverlayModalTitle => Intrinsic::row().cross(Cross::Center),
        // ⚠️ **1 行に切らない。** 何が起きるかの説明は、切り詰めると
        // 一番大事な後半が「…」に化ける
        OverlayModalBody => Intrinsic::column().cross(Cross::Stretch),
        // 消えるものそのもの。**こちらは切ってよい** — 長い発言の全文を
        // 出す場所ではなく、どれのことかが分かれば足りる
        OverlayModalPreview => Intrinsic::row().cross(Cross::Center).one_line(),
        OverlayModalActions => Intrinsic::row().cross(Cross::Stretch),
        // ⚠️ **ボタンは横幅を等分する。** 縦に積む (`column`) のは
        // 交差軸が横になり、`Center` で文字が真ん中に来るからである
        OverlayModalAction => Intrinsic::column().grow(1.0).cross(Cross::Center),
        OverlayModalActionLabel => Intrinsic::row().cross(Cross::Center).one_line(),

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

        // スクロールバーは**流れに入らない**。スクロール領域の縁へ重ねる。
        // 幅だけがここで決まり、高さと摘みの位置はレイアウトが計算する
        LayoutScrollbar => Intrinsic::stack().w(SCROLLBAR_W),
        LayoutScrollbarThumb => Intrinsic::stack(),

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
                // ⚠️ **`nav.channel_list` は入らない。** 見出しと自分を
                // 巻かないので、巻くのは中の `layout.scroll` だけである
                NodeId::NavDmList,
                NodeId::NavMemberList,
                NodeId::ChatMessageList,
                NodeId::LayoutScroll,
            ]
        );
    }
}
