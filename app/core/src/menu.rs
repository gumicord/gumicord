//! 浮かせるもの — コンテクストメニューと確認の窓 (`FR-024`, `FR-028`)。
//!
//! # ⚠️ 戻せないことの前には窓を挟む
//!
//! メニューの中に埋もれた「削除」を押した瞬間に発言が消えるのは危うい。
//! 隣の項目と 1 行しか離れていないうえ、**消した発言は戻せない**。
//!
//! ```text
//!   副ボタン ──▶ overlay.menu ──「削除」──▶ overlay.modal ──「削除する」──▶ 消える
//!                                              └──「やめる」──▶ 何も起きない
//! ```
//!
//! ⚠️ **窓は「本当に？」だけを出さない。** 何が消えるのかを一緒に出す。
//! 一覧が入れ替わったあとに窓だけ残っていると、押した人が思っているものと
//! 違うものが消えうる ([`Confirm::preview`])。
//!
//! # ⚠️ 開いている間は、下を押させない
//!
//! メニューを出したまま下の一覧が押せると、**メニューを閉じるつもりで
//! 押したチャンネルに移動する**。押した場所は当たり判定としては両方に
//! 掛かっているので、こちらが「上が開いているなら上だけ」と決めなければ
//! 素通りする。
//!
//! そのために [`Floating`] が開いている間は、層が窓いっぱいに広がって
//! 当たりを受け止める。外を押したら閉じるだけで、下へは渡さない。
//!
//! # ⚠️ 押せない項目を出さない
//!
//! 「自分の発言でないのに削除」「まだできないのに返信」を灰色で並べるのは、
//! 押せる場所を探す手間を増やすだけである。**できることだけを並べる。**
//!
//! # 携帯では下から出す
//!
//! 指で押す画面に、指の下へ出るメニューは向かない。**押した場所が
//! メニューで隠れる。** 携帯では画面の下から面を出す ([`Present`])。
//! 中身は同じで、包み方だけが変わる。

use gumicord_uitree::{Anchor, Key, NodeId, UiNode};

/// 浮かんでいるもの。**同時に 1 つだけ。**
///
/// ⚠️ 2 つ開くと、どちらを閉じるのかが押した場所から決まらなくなる。
/// メニューと窓を別々の場所に持たず 1 つの列挙にしてあるのは、
/// **持ち方の側で「同時に 1 つ」を守るため**である
#[derive(Debug, Clone, PartialEq)]
pub enum Floating {
    /// 操作の並び
    Menu(Menu),
    /// 戻せないことの前に挟む窓
    Confirm(Confirm),
}

/// コンテクストメニュー。
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// 押された場所 (論理 px)。**置き場所ではない**
    pub at: (f32, f32),
    pub items: Vec<Item>,
}

/// 確かめてから進む窓。
///
/// ⚠️ **「本当に？」だけでは足りない。** 何が起きるのか ([`Self::body`]) と、
/// 何に対して起きるのか ([`Self::preview`]) を出す
#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    /// 見出し。**何をしようとしているか**
    pub title: String,
    /// 起きること。**「戻せない」ならそう書く**
    pub body: String,
    /// 対象そのもの。無ければ出さない
    pub preview: Option<String>,
    /// 進むほうを押したときに起きること
    pub action: Action,
    /// 進むほうの文字。
    ///
    /// ⚠️ **「はい」にしない。** 見出しを読み飛ばした人にとって、
    /// 「はい」は何に対する「はい」か分からない。動詞を書く
    pub confirm: String,
    /// 戻せない操作か。テーマが赤く出す
    pub danger: bool,
}

/// 窓に出す 1 行。**何が起きるかの対象を示すためだけのものである。**
///
/// ⚠️ **全文を出さない。** 窓は読ませる場所ではなく、どれのことかが
/// 分かれば足りる。長い発言をそのまま出すと窓が画面を埋める。
///
/// ⚠️ **改行を潰す。** 1 行の場所なので、そのまま入れると 2 行目以降が
/// 見えないところへ消える。
///
/// 中身が無ければ `None`。**添付だけの発言で空の枠を出しても、何も
/// 伝わらない**
pub fn preview_line(body: &str) -> Option<String> {
    /// ここまでで切る (文字数)。**桁ではなく読める長さで決める**
    const LIMIT: usize = 60;

    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    // ⚠️ **バイトで切らない。** 日本語の途中で切ると壊れる
    if one_line.chars().count() <= LIMIT {
        return Some(one_line);
    }
    let mut out: String = one_line.chars().take(LIMIT).collect();
    out.push('…');
    Some(out)
}

/// 窓のボタンの番号。**位置ではなく意味で指す。**
///
/// ⚠️ 番号を入れ替えたときに、押される先が黙って入れ替わってはならない
pub mod button {
    /// やめる
    pub const CANCEL: usize = 0;
    /// 進む
    pub const CONFIRM: usize = 1;
}

/// メニューの項目 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// 押されたときに何をするか
    pub action: Action,
    pub label: String,
    /// 左に出す絵。無ければ出さない
    pub icon: Option<&'static str>,
    /// **消える操作**か。テーマが赤く出す
    pub danger: bool,
}

impl Item {
    pub fn new(action: Action, label: impl Into<String>) -> Item {
        Item {
            action,
            label: label.into(),
            icon: None,
            danger: false,
        }
    }

    pub fn icon(mut self, name: &'static str) -> Item {
        self.icon = Some(name);
        self
    }

    pub fn danger(mut self) -> Item {
        self.danger = true;
        self
    }
}

/// 項目を押したときに起きること。
///
/// ⚠️ **ここに「何を」の情報まで入れる。** 「コピー」とだけ持って、
/// 対象は別に覚えておく作りにすると、メニューを開いたあとに一覧が
/// 入れ替わったときに**別のものをコピーする**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 文字をクリップボードへ
    Copy(String),
    /// チャンネルを既読にする
    MarkRead(u64),
    /// この発言に返信する (`FR-028`)
    Reply(u64),
    /// この発言を書き換える (`FR-024`)
    Edit(u64),
    /// この発言を消す (`FR-024`)
    Delete(u64),

    // ── 入力欄の中 (机の上だけ)
    //
    // ⚠️ **携帯には出さない。** 触る画面には OS の選択操作があり、
    // そちらのほうが指に合っている。副ボタンは机の上にしか無いので、
    // 何もしなくてもここへは来ない
    /// 選んだところを切り取る
    Cut,
    /// 選んだところをコピーする
    CopySelection,
    /// クリップボードの中身を貼る
    Paste,
    /// 全部選ぶ
    SelectAll,
}

/// メニューをどう包むか。
///
/// ⚠️ **中身は同じである。** 変わるのは包み方だけで、項目を減らしたり
/// 増やしたりはしない。同じ操作が端末によって出たり出なかったりするのは、
/// 覚えられない
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Present {
    /// 押した場所に浮かべる (机の上)
    Popover,
    /// 画面の下から出す (携帯)
    Sheet,
}

impl Floating {
    /// 浮かせる層を組む。**開いているときだけ呼ぶこと。**
    pub fn node(&self, how: Present, hovered: Option<usize>) -> UiNode {
        match self {
            Floating::Menu(m) => m.node(how, hovered),
            Floating::Confirm(c) => c.node(hovered),
        }
    }

    /// メニューなら中の項目。窓なら空
    pub fn items(&self) -> &[Item] {
        match self {
            Floating::Menu(m) => &m.items,
            Floating::Confirm(_) => &[],
        }
    }
}

/// 層と暗幕で包む。**中身が何であれ、包み方は同じである。**
///
/// ⚠️ **覆いを先に置く。** 描く順は木の順なので、後ろに置くと中身の上に
/// 暗幕が掛かる
fn layer(scrim: &'static str, body: UiNode) -> UiNode {
    UiNode::new(NodeId::OverlayLayer)
        .child(UiNode::new(NodeId::OverlayScrim).with_key(Key::Slot(scrim)))
        .child(body)
}

impl Menu {
    fn node(&self, how: Present, hovered: Option<usize>) -> UiNode {
        let menu = UiNode::new(NodeId::OverlayMenu).children(
            self.items
                .iter()
                .enumerate()
                .map(|(i, it)| it.node(i, hovered == Some(i))),
        );

        let body = match how {
            // ⚠️ **基準の点だけを渡す。** 返すのも押し込むのもレンダラの
            // 仕事である ([`gumicord_uitree::Anchor`])
            Present::Popover => UiNode::new(NodeId::OverlayPopover)
                .with_anchor(Anchor::at(self.at.0, self.at.1))
                .child(menu),
            Present::Sheet => UiNode::new(NodeId::OverlaySheet)
                .child(UiNode::new(NodeId::OverlaySheetHandle))
                .child(menu),
        };

        // 机の上では暗くしない。**暗くすると、下を読みながら選ぶという
        // 当たり前のことができなくなる**
        layer(
            match how {
                Present::Popover => "quiet",
                Present::Sheet => "dim",
            },
            body,
        )
    }
}

impl Confirm {
    /// 窓を組む。
    ///
    /// ⚠️ **端末で包み方を変えない。** メニューと違い、窓は押した場所とは
    /// 関係のないところ (画面の真ん中) に出る。机の上でも携帯でも同じである
    fn node(&self, hovered: Option<usize>) -> UiNode {
        let modal = UiNode::new(NodeId::OverlayModal)
            .child(UiNode::text(NodeId::OverlayModalTitle, &self.title))
            .child(UiNode::text(NodeId::OverlayModalBody, &self.body))
            .child_if(self.preview.is_some(), || {
                UiNode::text(
                    NodeId::OverlayModalPreview,
                    self.preview.as_deref().unwrap_or_default(),
                )
            })
            .child(
                UiNode::new(NodeId::OverlayModalActions)
                    // ⚠️ **やめるほうを先に置く。** 押し間違えたときに
                    // 起きることが軽いほうを、指と目が先に当たる場所に置く
                    .child(self.button(button::CANCEL, "やめる", "cancel", hovered))
                    .child(self.button(
                        button::CONFIRM,
                        &self.confirm,
                        if self.danger { "danger" } else { "confirm" },
                        hovered,
                    )),
            );

        // ⚠️ **窓のときは必ず暗くする。** 後ろが読めたままだと、窓が
        // 出ていることに気付かずに下を押そうとする
        layer("dim", modal)
    }

    fn button(
        &self,
        index: usize,
        label: &str,
        slot: &'static str,
        hovered: Option<usize>,
    ) -> UiNode {
        UiNode::new(NodeId::OverlayModalAction)
            // ⚠️ **番号で指す。** 文字で指すと、言語を変えた瞬間に
            // どちらが押されたか分からなくなる
            .with_key(Key::Index(index as u32))
            .with_state_if(hovered == Some(index), gumicord_uitree::State::Hover)
            .child(UiNode::text(NodeId::OverlayModalActionLabel, label).with_key(Key::Slot(slot)))
    }
}

impl Item {
    fn node(&self, index: usize, hovered: bool) -> UiNode {
        UiNode::new(NodeId::OverlayMenuItem)
            // ⚠️ **番号で指す。** 名前で指すと、同じ名前の項目が 2 つ
            // 並んだときに片方しか押せない
            .with_key(Key::Index(index as u32))
            .with_state_if(hovered, gumicord_uitree::State::Hover)
            .child_if(self.icon.is_some(), || {
                UiNode::new(NodeId::OverlayMenuItemIcon)
                    .with_content(gumicord_uitree::Content::Icon(
                        self.icon.unwrap_or_default().to_owned(),
                    ))
                    .with_key(Key::Slot(if self.danger { "danger" } else { "normal" }))
            })
            .child(
                UiNode::text(NodeId::OverlayMenuItemLabel, &self.label)
                    .with_key(Key::Slot(if self.danger { "danger" } else { "normal" })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Floating {
        Floating::Menu(Menu {
            at: (10.0, 20.0),
            items: vec![
                Item::new(Action::Copy("a".into()), "本文をコピー"),
                Item::new(Action::MarkRead(1), "既読にする").danger(),
            ],
        })
    }

    fn confirm() -> Floating {
        Floating::Confirm(Confirm {
            title: "この発言を削除しますか".to_owned(),
            body: "削除した発言は元に戻せません。".to_owned(),
            preview: Some("おはよう".to_owned()),
            action: Action::Delete(1),
            confirm: "削除する".to_owned(),
            danger: true,
        })
    }

    fn ids(n: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        n.walk(&mut |c, _| out.push(c.id));
        out
    }

    /// ⚠️ **暗幕はメニューより先に置く。** 後ろに置くと、描く順が木の順
    /// なのでメニューの上に暗幕が掛かる
    #[test]
    fn 暗幕はメニューより先に来る() {
        let order = ids(&menu().node(Present::Popover, None));
        let scrim = order.iter().position(|i| *i == NodeId::OverlayScrim);
        let m = order.iter().position(|i| *i == NodeId::OverlayMenu);
        assert!(scrim < m, "暗幕がメニューより後ろにある {order:?}");
    }

    /// 机の上では点に浮かべ、携帯では下から出す
    #[test]
    fn 包み方だけが端末で変わる() {
        let pop = ids(&menu().node(Present::Popover, None));
        assert!(pop.contains(&NodeId::OverlayPopover));
        assert!(!pop.contains(&NodeId::OverlaySheet));

        let sheet = ids(&menu().node(Present::Sheet, None));
        assert!(sheet.contains(&NodeId::OverlaySheet));
        assert!(!sheet.contains(&NodeId::OverlayPopover));
    }

    /// ⚠️ **中身は端末で変わらない。** 同じ操作が出たり出なかったりすると
    /// 覚えられない
    #[test]
    fn 項目は端末で変わらない() {
        let labels = |how| {
            let mut out = Vec::new();
            menu().node(how, None).walk(&mut |c, _| {
                if c.id == NodeId::OverlayMenuItemLabel
                    && let Some(s) = c.content.as_text()
                {
                    out.push(s.to_owned());
                }
            });
            out
        };
        assert_eq!(labels(Present::Popover), labels(Present::Sheet));
        assert_eq!(labels(Present::Popover).len(), 2);
    }

    /// 基準の点はそのまま運ばれること。**置き場所を先に決めない**
    #[test]
    fn 基準の点はそのまま運ばれる() {
        let n = menu().node(Present::Popover, None);
        let mut found = None;
        n.walk(&mut |c, _| {
            if c.id == NodeId::OverlayPopover {
                found = c.anchor;
            }
        });
        assert_eq!(found, Some(Anchor::at(10.0, 20.0)));
    }

    /// ⚠️ **番号で指す。** 名前で指すと、同じ名前の項目が 2 つ並んだ
    /// ときに片方しか押せない
    #[test]
    fn 項目は番号で指す() {
        let mut keys = Vec::new();
        menu().node(Present::Popover, None).walk(&mut |c, _| {
            if c.id == NodeId::OverlayMenuItem {
                keys.push(c.key.clone());
            }
        });
        assert_eq!(keys, vec![Some(Key::Index(0)), Some(Key::Index(1))]);
    }

    // ═══════════════════════════════════════════════════════════════
    //  確認の窓

    fn texts(n: &UiNode, want: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        n.walk(&mut |c, _| {
            if c.id == want
                && let Some(s) = c.content.as_text()
            {
                out.push(s.to_owned());
            }
        });
        out
    }

    /// ⚠️ **窓は必ず暗くする。** 後ろが読めたままだと、窓が出ていることに
    /// 気付かずに下を押そうとする
    #[test]
    fn 窓は後ろを暗くする() {
        let n = confirm().node(Present::Popover, None);
        let mut slot = None;
        n.walk(&mut |c, _| {
            if c.id == NodeId::OverlayScrim {
                slot = c.key.clone();
            }
        });
        assert_eq!(slot, Some(Key::Slot("dim")));
    }

    /// ⚠️ **端末で包み方を変えない。** 窓は押した場所と関係のないところに
    /// 出るので、机の上でも携帯でも同じである
    #[test]
    fn 窓は端末で変わらない() {
        let pop = ids(&confirm().node(Present::Popover, None));
        let sheet = ids(&confirm().node(Present::Sheet, None));
        assert_eq!(pop, sheet);
        assert!(pop.contains(&NodeId::OverlayModal));
        // ⚠️ **基準の点を持たない。** 持つと押した場所に出てしまう
        assert!(!pop.contains(&NodeId::OverlayPopover));
        assert!(!pop.contains(&NodeId::OverlaySheet));
    }

    /// ⚠️ **やめるほうを先に置く。** 押し間違えたときに起きることが
    /// 軽いほうを、指と目が先に当たる場所に置く
    #[test]
    fn やめるほうが先に来る() {
        let labels = texts(
            &confirm().node(Present::Popover, None),
            NodeId::OverlayModalActionLabel,
        );
        assert_eq!(labels, vec!["やめる", "削除する"]);
    }

    /// ⚠️ **「はい」にしない。** 見出しを読み飛ばした人には、何に対する
    /// 「はい」か分からない
    #[test]
    fn 進むほうの文字は動詞である() {
        let labels = texts(
            &confirm().node(Present::Popover, None),
            NodeId::OverlayModalActionLabel,
        );
        assert!(!labels.contains(&"はい".to_owned()));
    }

    /// ⚠️ **番号で指す。** 文字で指すと、言語を変えた瞬間にどちらが
    /// 押されたか分からなくなる
    #[test]
    fn ボタンは番号で指す() {
        let mut keys = Vec::new();
        confirm().node(Present::Popover, None).walk(&mut |c, _| {
            if c.id == NodeId::OverlayModalAction {
                keys.push(c.key.clone());
            }
        });
        assert_eq!(
            keys,
            vec![
                Some(Key::Index(button::CANCEL as u32)),
                Some(Key::Index(button::CONFIRM as u32))
            ]
        );
    }

    /// 消えるものそのものを出す。**「本当に？」だけでは何が消えるか
    /// 分からない**
    #[test]
    fn 何が消えるのかを一緒に出す() {
        let n = confirm().node(Present::Popover, None);
        assert_eq!(texts(&n, NodeId::OverlayModalPreview), vec!["おはよう"]);
    }

    /// 中身が無ければ枠ごと出さない。**空の枠は何も伝えない**
    #[test]
    fn 出すものが無ければ枠を出さない() {
        let Floating::Confirm(mut c) = confirm() else {
            unreachable!()
        };
        c.preview = None;
        let n = Floating::Confirm(c).node(Present::Popover, None);
        assert!(!ids(&n).contains(&NodeId::OverlayModalPreview));
    }

    /// 指が乗っているボタンだけが光る
    #[test]
    fn 指の乗ったボタンだけが光る() {
        let n = confirm().node(Present::Popover, Some(button::CONFIRM));
        let mut hovered = Vec::new();
        n.walk(&mut |c, _| {
            if c.id == NodeId::OverlayModalAction {
                hovered.push(c.states.contains(gumicord_uitree::State::Hover));
            }
        });
        assert_eq!(hovered, vec![false, true]);
    }

    // ── 出す 1 行

    /// 短い本文はそのまま出る
    #[test]
    fn 短い本文はそのまま出る() {
        assert_eq!(preview_line("おはよう"), Some("おはよう".to_owned()));
    }

    /// ⚠️ **改行を潰す。** 1 行の場所なので、そのまま入れると 2 行目
    /// 以降が見えないところへ消える
    #[test]
    fn 改行は空白に潰れる() {
        assert_eq!(
            preview_line("いち\nに\r\nさん"),
            Some("いち に さん".to_owned())
        );
    }

    /// ⚠️ **バイトで切らない。** 日本語の途中で切ると壊れる
    #[test]
    fn 長い本文は文字数で切れる() {
        let out = preview_line(&"あ".repeat(200)).expect("出るはず");
        assert_eq!(out.chars().count(), 61, "60 文字 + …");
        assert!(out.ends_with('…'));
    }

    /// 添付だけの発言は本文が空である。**空の枠を出しても何も伝わらない**
    #[test]
    fn 中身が無ければ出さない() {
        assert_eq!(preview_line(""), None);
        assert_eq!(preview_line("   \n\t "), None);
    }
}
