//! UITree のノード。
//!
//! 構造は [`spec/03-uitree.md`] 2 章が定める。
//!
//! # 仕様の構造体との差分
//!
//! 仕様に載る `UiNode` には**中身**がない。`chat.message.content` が
//! 「本文である」ことは安定 ID が表しているが、**その本文が何という文字列か**は
//! どのフィールドにも入らない。描画できないので [`Content`] を足している。
//!
//! `Content` は拡張 ABI ではない。プラグインは `ui.text()` のような SDK の
//! 関数を通してのみ中身を作れる ([`spec/05-plugin-api.md`])。
//!
//! **レイアウトの方向 (row / column) はここに持たない。** 安定 ID が
//! 意味を決め、その意味からどう並べるかはレンダラの判断である
//! ([`spec/06-renderer.md`] 8 章)。テーマがレイアウトを上書きできるように
//! なるのは `EXT-014` (M2) からで、そのときもここではなくスタイル側に入る。

use crate::ids::{DataKind, NodeId};
use crate::style::Style;
use crate::value::{Color, Font};
use crate::{Key, State, StateSet};

/// ノードが表示する中身。
///
/// ⚠️ `Eq` を持たない。[`Span`] が大きさや色 (`f32`) を持つためである
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Content {
    /// 子ノードだけを持つ。コンテナ
    #[default]
    None,
    /// 文字列。整形はレンダラが行う
    Text(String),
    /// 飾りの混じった文字列 (`FR-021`)。
    ///
    /// # ⚠️ 飾りごとにノードを作ってはいけない
    ///
    /// 「太字だけ別のノードにして横に並べる」は動かない。並べたノードは
    /// **それぞれが独立して折り返す**ので、`これは **とても長い** 文章`
    /// の行末が合わなくなる。行の折り返しは、混じったまま一度に整形して
    /// はじめて正しく決まる。
    ///
    /// だから飾りは中身側に持たせ、ノードは 1 つのままにする
    Rich(Vec<Span>),
    /// アイコン。**名前で指す。**
    ///
    /// 何が描かれるかはレンダラが決める。名前で指すのは、字として描くのを
    /// やめてテクスチャにしたときに、UITree 側を変えずに済ませるためである。
    /// **知らない名前は誤りではない**。描かずに進む。
    Icon(String),
    /// 画像。**中身ではなく取り出し元の URL を持つ。**
    ///
    /// ⚠️ **UITree に画素を載せない。** 木は毎フレーム組み直されるので、
    /// 画素を持たせると 1 フレームごとに数 MB を複製することになる。
    /// 取ってくるのも復号するのもアプリの仕事で、レンダラは**既に手元に
    /// あるものだけを描く** (無ければ何も描かない)
    Image(String),
    /// QR コード ([ADR-0007](../../../spec/adr/0007-login-paths-and-captcha.md))。
    ///
    /// **中身の文字列だけを持つ。** 符号化も描画もレンダラの仕事である。
    /// QR は角丸矩形の格子なので、自前レンダラでそのまま描ける。
    Qr(String),
    /// 編集中のテキスト (`PLT-001`)。
    ///
    /// ただの文字列と分けているのは、**キャレット・選択・変換中の範囲を
    /// 描くのがレンダラの仕事**だからである。文字の位置を知っているのは
    /// 整形したところだけで、アプリはバイト位置しか持てない。
    Editable(Editable),
}

/// 飾りの付いた一続きの文字。[`Content::Rich`] の要素。
///
/// # ⚠️ ここに入るのは**テーマが決めた結果**である
///
/// 「太字である」ではなく「太さ 700 である」が入る。太字を何で表すかは
/// テーマの領分で ([`spec/04-theme.md`])、ここまで来た時点でその判断は
/// 済んでいる。`Deco::BOLD` のような意味をここへ持ち込むと、
/// **レンダラが太字の見た目を決めることになる**。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub text: String,
    /// 書体。`None` ならノードの書体をそのまま使う
    pub font: Option<Font>,
    /// 文字色。`None` ならノードの色
    pub color: Option<Color>,
    /// 文字に引く線
    pub line: Line,
    /// 中身を隠すか (スポイラー)。
    ///
    /// ⚠️ **場所は空けたまま隠す。** 詰めて描くと、開いた瞬間に
    /// 行の折り返しが変わって本文が飛び跳ねる
    pub hidden: bool,
}

/// 文字に引く線。**重ねられる** — `__~~a~~__` は両方引く
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Line {
    pub under: bool,
    pub through: bool,
}

impl Line {
    pub const fn any(self) -> bool {
        self.under || self.through
    }
}

/// 編集中のテキストと、その上の印。位置は**バイト位置**である。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Editable {
    pub text: String,
    /// キャレットの位置
    pub caret: usize,
    /// 選択範囲。空なら選択なし
    pub selection: core::ops::Range<usize>,
    /// 変換中の範囲。確定していない文字を下線で示す
    pub composing: Option<core::ops::Range<usize>>,
    /// 入力欄が空のときに薄く出す文字列。**編集の対象ではない**
    pub placeholder: String,
}

impl Content {
    /// 整形して描く文字列。
    ///
    /// 編集中のテキストも文字列であることに変わりはないので、ここから
    /// 取れる。空なら代わりに `placeholder` を返す。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Editable(e) if e.text.is_empty() => Some(&e.placeholder),
            Content::Editable(e) => Some(&e.text),
            _ => None,
        }
    }

    pub fn as_image(&self) -> Option<&str> {
        match self {
            Content::Image(url) => Some(url),
            _ => None,
        }
    }

    pub fn as_qr(&self) -> Option<&str> {
        match self {
            Content::Qr(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_editable(&self) -> Option<&Editable> {
        match self {
            Content::Editable(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_icon(&self) -> Option<&str> {
        match self {
            Content::Icon(s) => Some(s),
            _ => None,
        }
    }

    /// 飾りの混じった文字。
    ///
    /// ⚠️ [`Content::as_text`] はここを返さない。繋げて 1 本の文字列に
    /// すると飾りが落ちるので、**飾りを見る側が気づかないまま素の文字に
    /// なる**。見たいなら明示的にこちらを呼ぶこと
    pub fn as_rich(&self) -> Option<&[Span]> {
        match self {
            Content::Rich(s) => Some(s),
            _ => None,
        }
    }

    /// 子ではなく自分自身が何かを描くか。レイアウトが葉として扱う判断に使う
    pub fn is_leaf(&self) -> bool {
        !matches!(self, Content::None)
    }
}

/// そのノードが表現しているドメインオブジェクトへの参照
/// ([`spec/03-uitree.md`] 2.4)。
///
/// ⚠️ **公開するのは読み取り専用のスナップショットであり、内部の状態そのもの
/// ではない。** M1.1 では種別と識別子だけを持つ。フィールドの公開は Store
/// (C5) ができてから、仕様の表に列挙されたものだけを足す。
///
/// **フィールドもまた拡張 ABI である。** 追加は自由だが削除と改名は破壊的変更。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRef {
    pub kind: DataKind,
    /// Discord のスノーフレーク
    pub id: u64,
}

/// UITree のノード 1 個。
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    /// 安定 ID。拡張 ABI そのもの
    pub id: NodeId,
    /// 同じ親の下で同じ `id` を持つノードを区別する鍵
    pub key: Option<Key>,
    /// 状態。テーマの条件分岐に使う (`EXT-013`)
    pub states: StateSet,
    /// このノードが表現しているドメインオブジェクトへの参照
    pub data: Option<DataRef>,
    /// **データが持ってきた色。** 役職の色、サーバフォルダの色。
    ///
    /// # ⚠️ これはスタイルではない。テーマへの材料である
    ///
    /// 見た目を決めるのはテーマであって、Discord のデータではない。
    /// だが「その役職の色」は**利用者が Discord で決めたこと**であり、
    /// テーマの好みで消してよいものでもない。
    ///
    /// そこで、**色はここに載せるだけ**にする。どこに塗るか — 文字色か、
    /// 縁か、背景か、そもそも塗らないか — はテーマが `$data.tint` で
    /// 決める ([`spec/04-theme.md`])。
    ///
    /// ⚠️ **識別子は載せない。** 色は誰のものかを語らないので、
    /// テーマが特定のサーバや相手を狙い撃ちにはできない
    /// (`when.slot` がスノーフレークを弾くのと同じ理由)。
    pub tint: Option<Color>,
    /// 表示する中身
    pub content: Content,
    /// テーマとプラグインによって解決された最終的なスタイル。
    ///
    /// **構築時は空である。** パイプラインの [5] と [6] が埋める
    /// ([`spec/02-architecture.md`])。
    pub style: Style,
    pub children: Vec<UiNode>,
}

impl UiNode {
    pub fn new(id: NodeId) -> Self {
        UiNode {
            id,
            key: None,
            states: StateSet::EMPTY,
            data: None,
            tint: None,
            content: Content::None,
            style: Style::default(),
            children: Vec::new(),
        }
    }

    /// 文字列を持つノード。
    pub fn text(id: NodeId, s: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Text(s.into()))
    }

    /// アイコンを持つノード。
    pub fn icon(id: NodeId, name: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Icon(name.into()))
    }

    /// 画像を持つノード。**中身ではなく URL** である
    pub fn image(id: NodeId, url: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Image(url.into()))
    }

    /// QR コードを持つノード。中身は符号化する前の文字列である
    pub fn qr(id: NodeId, data: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Qr(data.into()))
    }

    /// 編集中のテキストを持つノード (`PLT-001`)。
    pub fn editable(id: NodeId, e: Editable) -> Self {
        UiNode::new(id).with_content(Content::Editable(e))
    }

    pub fn with_key(mut self, key: Key) -> Self {
        self.key = Some(key);
        self
    }

    /// スノーフレークを鍵にする。リスト項目でもっとも多い形。
    pub fn with_id_key(mut self, id: u64) -> Self {
        self.key = Some(Key::Id(id));
        self
    }

    pub fn with_state(mut self, state: State) -> Self {
        self.states = self.states.with(state);
        self
    }

    /// 条件つきで状態を立てる。呼び出し側の `if` を減らすためだけのもの。
    pub fn with_state_if(mut self, cond: bool, state: State) -> Self {
        if cond {
            self.states = self.states.with(state);
        }
        self
    }

    pub fn with_states(mut self, states: StateSet) -> Self {
        self.states = states;
        self
    }

    /// データが持ってきた色を載せる。**どこに塗るかはテーマが決める**
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = Some(tint);
        self
    }

    /// 色があれば載せる。呼び出し側の `if` を減らすためだけのもの
    pub fn with_tint_opt(mut self, tint: Option<Color>) -> Self {
        self.tint = tint;
        self
    }

    /// ドメインオブジェクトへの参照を付ける。
    ///
    /// 種別は安定 ID が決めるので、呼び出し側は識別子だけを渡す。
    /// `data` を持たない ID に付けようとした場合は**黙って無視する**。
    /// ここで落とすと、ID の `data` 種別を後から足したときに呼び出し側が
    /// 壊れるため。
    pub fn with_data(mut self, id: u64) -> Self {
        let kind = self.id.data_kind();
        if kind != DataKind::None {
            self.data = Some(DataRef { kind, id });
        }
        self
    }

    pub fn with_content(mut self, content: Content) -> Self {
        self.content = content;
        self
    }

    pub fn child(mut self, node: UiNode) -> Self {
        self.children.push(node);
        self
    }

    /// 条件つきで子を足す。
    pub fn child_if(self, cond: bool, node: impl FnOnce() -> UiNode) -> Self {
        if cond { self.child(node()) } else { self }
    }

    pub fn children(mut self, nodes: impl IntoIterator<Item = UiNode>) -> Self {
        self.children.extend(nodes);
        self
    }

    /// 深さ優先・前順の走査。**これが描画順である**
    /// ([`spec/06-renderer.md`] 7.5)。
    pub fn walk(&self, f: &mut impl FnMut(&UiNode, usize)) {
        self.walk_at(0, f);
    }

    fn walk_at(&self, depth: usize, f: &mut impl FnMut(&UiNode, usize)) {
        f(self, depth);
        for c in &self.children {
            c.walk_at(depth + 1, f);
        }
    }

    /// ノードの総数。
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(UiNode::count).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_is_attached_only_where_the_id_declares_it() {
        // chat.message は MessageData を持つ
        let m = UiNode::new(NodeId::ChatMessage).with_data(42);
        assert_eq!(
            m.data,
            Some(DataRef {
                kind: DataKind::Message,
                id: 42
            })
        );

        // layout.row は data を持たない。黙って無視する
        let r = UiNode::new(NodeId::LayoutRow).with_data(42);
        assert_eq!(r.data, None);
    }

    /// 走査順が描画順である。前順であることを固定する
    #[test]
    fn walk_is_depth_first_pre_order() {
        let tree = UiNode::new(NodeId::AppRoot)
            .child(UiNode::new(NodeId::NavGuildList).child(UiNode::new(NodeId::NavGuildListItem)))
            .child(UiNode::new(NodeId::ChatView));

        let mut seen = Vec::new();
        tree.walk(&mut |n, d| seen.push((n.id, d)));

        assert_eq!(
            seen,
            vec![
                (NodeId::AppRoot, 0),
                (NodeId::NavGuildList, 1),
                (NodeId::NavGuildListItem, 2),
                (NodeId::ChatView, 1),
            ]
        );
        assert_eq!(tree.count(), 4);
    }

    #[test]
    fn text_content_round_trips() {
        let n = UiNode::text(NodeId::ChatMessageContent, "こんにちは");
        assert_eq!(n.content.as_text(), Some("こんにちは"));
        assert_eq!(UiNode::new(NodeId::ChatView).content.as_text(), None);
    }
}
