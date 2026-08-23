//! 浮かせるもの — コンテクストメニュー (`FR-024`, `FR-028` の受け皿)。
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
/// ⚠️ 2 つ開くと、どちらを閉じるのかが押した場所から決まらなくなる
#[derive(Debug, Clone, PartialEq)]
pub struct Floating {
    /// 押された場所 (論理 px)。**置き場所ではない**
    pub at: (f32, f32),
    pub items: Vec<Item>,
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

        // ⚠️ **覆いを先に置く。** 描く順は木の順なので、後ろに置くと
        // メニューの上に暗幕が掛かる
        UiNode::new(NodeId::OverlayLayer)
            .child(
                UiNode::new(NodeId::OverlayScrim).with_key(Key::Slot(match how {
                    // 机の上では暗くしない。**暗くすると、下を読みながら
                    // 選ぶという当たり前のことができなくなる**
                    Present::Popover => "quiet",
                    Present::Sheet => "dim",
                })),
            )
            .child(body)
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
        Floating {
            at: (10.0, 20.0),
            items: vec![
                Item::new(Action::Copy("a".into()), "本文をコピー"),
                Item::new(Action::MarkRead(1), "既読にする").danger(),
            ],
        }
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
}
