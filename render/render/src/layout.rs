//! レイアウト。**制約を親から子へ、確定した大きさを子から親へ返す**
//! ([`spec/06-renderer.md`] 8 章)。
//!
//! Flexbox もグリッドも持たない。並べ方は [`Axis`] の 3 種類と、スクロール
//! するかどうかだけである。
//!
//! # 主軸の配り方
//!
//! ```text
//! 1. 余りを取らない子を先に測る          ← 実寸が決まる
//! 2. 残りを、余りを取る子で grow 比に応じて分ける
//! ```
//!
//! この順序が要る。`chat.message` は「アイコン (40px) + 本文 (残り全部)」で
//! あり、本文の折り返し幅は**アイコンの幅を引いたあと**でなければ決まらない。
//! 全員に同じ制約を配ると本文が横にはみ出す。
//!
//! # 計測は覚えておく
//!
//! 親は子を「測る」ときと「置く」ときの 2 回触る。素直に書くと木の深さぶん
//! 指数的に増えるので、(ノード, 制約) を鍵に覚えておく。
//!
//! 単位は**論理ピクセル**である。物理ピクセルへの変換は [`crate::draw`] が
//! 一度だけ行う。

use std::collections::HashMap;

use gumicord_uitree::value::Edges;
use gumicord_uitree::{Content, NodeId, UiNode};

use crate::geom::{EdgesExt, Rect, Size};
use crate::intrinsic::{Axis, Cross, Intrinsic, intrinsic};
use crate::text::{ResolvedFont, Shaper};

/// スクロール位置。安定 ID ごとに、先頭からの距離 (論理 px) を持つ。
///
/// ⚠️ **同じ安定 ID のスクロール領域が複数あると共有される。** M1.1 では
/// 一覧が画面に 1 つずつしかないので足りる。タブや分割ビューを入れるときに
/// `key` まで含めた鍵へ広げる。
pub type ScrollState = HashMap<NodeId, f32>;

/// 一番下に貼り付ける。メッセージ一覧の初期値に使う
pub const SCROLL_TO_END: f32 = f32::MAX;

/// 配置が決まったノード 1 個。
#[derive(Debug, Clone)]
pub struct Placed<'a> {
    pub node: &'a UiNode,
    /// 論理 px。ウィンドウ左上が原点
    pub rect: Rect,
    /// この矩形の外は描かない。`None` なら切り取らない
    pub clip: Option<Rect>,
    /// 内容の描画領域 (余白を引いたあと)。テキストの折り返し幅でもある
    pub inner: Rect,
}

/// 1 フレームぶんの配置結果。**描画順 (深さ優先・前順) に並ぶ。**
#[derive(Debug, Default)]
pub struct LayoutResult<'a> {
    pub placed: Vec<Placed<'a>>,
    /// スクロール領域ごとの、はみ出した量 (論理 px)。
    /// スクロールの上限を決めるのに使う
    pub overflow: HashMap<NodeId, f32>,
}

impl<'a> LayoutResult<'a> {
    /// 点を含む、もっとも手前のノード。
    ///
    /// 描画順の**逆**に走査する。後に描かれたものが上に見えている以上、
    /// 当たり判定もそちらが勝つ。
    pub fn hit(&self, x: f32, y: f32) -> Option<&Placed<'a>> {
        self.placed
            .iter()
            .rev()
            .find(|p| p.rect.contains(x, y) && p.clip.is_none_or(|c| c.contains(x, y)))
    }

    /// 点を含むノードを、手前から順に。
    ///
    /// ホバー状態は「その項目とその祖先」に立てたいことが多いので、
    /// 1 個だけでは足りない。
    pub fn hits(&self, x: f32, y: f32) -> impl Iterator<Item = &Placed<'a>> {
        self.placed
            .iter()
            .rev()
            .filter(move |p| p.rect.contains(x, y) && p.clip.is_none_or(|c| c.contains(x, y)))
    }

    /// その安定 ID のノードの配置。スクロール量の上限を求めるのに使う
    pub fn find(&self, id: NodeId) -> Option<&Placed<'a>> {
        self.placed.iter().find(|p| p.node.id == id)
    }
}

/// 木を配置する。
pub fn layout<'a>(
    root: &'a UiNode,
    viewport: Size,
    text: &mut Shaper,
    scroll: &ScrollState,
) -> LayoutResult<'a> {
    let mut cx = Cx {
        text,
        scroll,
        cache: HashMap::new(),
        out: Vec::new(),
        overflow: HashMap::new(),
    };
    cx.place(root, Rect::from_size(viewport), None);
    LayoutResult {
        placed: cx.out,
        overflow: cx.overflow,
    }
}

struct Cx<'a, 't, 's> {
    text: &'t mut Shaper,
    scroll: &'s ScrollState,
    /// (ノードの番地, 制約) → 大きさ
    cache: HashMap<(usize, u32, u32), Size>,
    out: Vec<Placed<'a>>,
    /// スクロール領域ごとの、はみ出した量
    overflow: HashMap<NodeId, f32>,
}

/// 制約を鍵にするための量子化。`f32` をそのまま鍵にできないため
fn q(v: f32) -> u32 {
    if !v.is_finite() {
        return u32::MAX;
    }
    (v.max(0.0) * 16.0).round() as u32
}

/// その軸で明示された大きさ (テーマ > 既定)。
fn explicit(node: &UiNode, it: &Intrinsic, axis_is_horizontal: bool) -> Option<f32> {
    if axis_is_horizontal {
        node.style.width.or(it.width)
    } else {
        node.style.height.or(it.height)
    }
}

fn clamp_size(node: &UiNode, mut w: f32, mut h: f32) -> Size {
    let s = &node.style;
    if let Some(v) = s.min_width {
        w = w.max(v);
    }
    if let Some(v) = s.max_width {
        w = w.min(v);
    }
    if let Some(v) = s.min_height {
        h = h.max(v);
    }
    if let Some(v) = s.max_height {
        h = h.min(v);
    }
    Size::new(w.max(0.0), h.max(0.0))
}

impl<'a> Cx<'a, '_, '_> {
    // ───────────────────────────────────────────────── 計測

    fn measure(&mut self, node: &UiNode, avail: Size) -> Size {
        let key = (std::ptr::from_ref(node) as usize, q(avail.w), q(avail.h));
        if let Some(s) = self.cache.get(&key) {
            return *s;
        }
        let size = self.measure_uncached(node, avail);
        self.cache.insert(key, size);
        size
    }

    fn measure_uncached(&mut self, node: &UiNode, avail: Size) -> Size {
        let it = intrinsic(node.id);
        let pad = node.style.padding.unwrap_or_default();

        let ex_w = explicit(node, &it, true);
        let ex_h = explicit(node, &it, false);

        let inner = Size::new(
            (ex_w.unwrap_or(avail.w) - pad.horizontal()).max(0.0),
            (ex_h.unwrap_or(avail.h) - pad.vertical()).max(0.0),
        );

        let content = self.measure_content(node, &it, inner);

        clamp_size(
            node,
            ex_w.unwrap_or(content.w + pad.horizontal()),
            ex_h.unwrap_or(content.h + pad.vertical()),
        )
    }

    /// 内容 (テキスト・アイコン・子) の大きさ。余白を含まない。
    fn measure_content(&mut self, node: &UiNode, it: &Intrinsic, inner: Size) -> Size {
        match &node.content {
            Content::Text(s) => {
                let font = ResolvedFont::from_style(&node.style);
                // 折り返し幅が無限なら折り返さない
                let max_w = inner.w.is_finite().then_some(inner.w);
                self.text.measure(s, &font, max_w)
            }
            // アイコンは正方形で、文字と同じ大きさにする。
            // 行の中に混ぜたときに揃うのが自然なため
            Content::Icon(_) => {
                let s = ResolvedFont::from_style(&node.style).size();
                Size::new(s, s)
            }
            Content::None if node.children.is_empty() => Size::ZERO,
            Content::None => self.size_children(node, it, inner).1,
        }
    }

    /// 子の大きさを主軸の規則に従って決める。
    ///
    /// 戻り値は (子ごとの大きさ, 内容全体の大きさ)。
    fn size_children(&mut self, node: &UiNode, it: &Intrinsic, inner: Size) -> (Vec<Size>, Size) {
        let n = node.children.len();
        let gap = node.style.gap.unwrap_or(0.0);
        let mut sizes = vec![Size::ZERO; n];

        if it.axis == Axis::Stack {
            let mut content = Size::ZERO;
            for (i, c) in node.children.iter().enumerate() {
                let m = c.style.margin.unwrap_or_default();
                let s = self.measure(
                    c,
                    Size::new(
                        (inner.w - m.horizontal()).max(0.0),
                        (inner.h - m.vertical()).max(0.0),
                    ),
                );
                sizes[i] = s;
                content.w = content.w.max(s.w + m.horizontal());
                content.h = content.h.max(s.h + m.vertical());
            }
            return (sizes, content);
        }

        let horizontal = it.axis == Axis::Row;
        let main_avail = if horizontal { inner.w } else { inner.h };
        let cross_avail = if horizontal { inner.h } else { inner.w };

        let margins: Vec<Edges> = node
            .children
            .iter()
            .map(|c| c.style.margin.unwrap_or_default())
            .collect();
        let margin_main: f32 = margins
            .iter()
            .map(|m| {
                if horizontal {
                    m.horizontal()
                } else {
                    m.vertical()
                }
            })
            .sum();

        let gaps = gap * (n.saturating_sub(1)) as f32;
        let mut remaining = main_avail - gaps - margin_main;

        // 主軸の大きさが明示されている子は**余りを取らない**。
        //
        // `grow` はレンダラが持つ既定にすぎず、テーマが書いた `width` /
        // `height` はそれより強い。ここを逆にすると、テーマで幅を指定した
        // ノードが黙って引き伸ばされる。
        let grows: Vec<f32> = node
            .children
            .iter()
            .map(|c| {
                let ci = intrinsic(c.id);
                if explicit(c, &ci, horizontal).is_some() {
                    0.0
                } else {
                    ci.grow
                }
            })
            .collect();

        // [1] 余りを取らない子を先に測る

        for (i, c) in node.children.iter().enumerate() {
            if grows[i] > 0.0 && remaining.is_finite() {
                continue;
            }
            let m = margins[i];
            let avail = if horizontal {
                Size::new(remaining.max(0.0), (cross_avail - m.vertical()).max(0.0))
            } else {
                Size::new((cross_avail - m.horizontal()).max(0.0), remaining.max(0.0))
            };
            let s = self.measure(c, avail);
            sizes[i] = s;
            remaining -= if horizontal { s.w } else { s.h };
        }

        // [2] 残りを grow 比で分ける。
        //     制約が無限 (スクロール領域の主軸) のときは分けようがないので、
        //     内容の大きさのまま置く
        let total_grow: f32 = grows.iter().sum();
        if total_grow > 0.0 && remaining.is_finite() {
            let pool = remaining.max(0.0);
            for (i, c) in node.children.iter().enumerate() {
                if grows[i] <= 0.0 {
                    continue;
                }
                let m = margins[i];
                let main = pool * grows[i] / total_grow;
                let avail = if horizontal {
                    Size::new(main, (cross_avail - m.vertical()).max(0.0))
                } else {
                    Size::new((cross_avail - m.horizontal()).max(0.0), main)
                };
                let mut s = self.measure(c, avail);
                // 主軸は配ったぶんに固定する。測った値ではない
                if horizontal {
                    s.w = main;
                } else {
                    s.h = main;
                }
                sizes[i] = s;
            }
        }

        let mut main_total = gaps + margin_main;
        let mut cross_max = 0.0f32;
        for (i, s) in sizes.iter().enumerate() {
            let m = margins[i];
            if horizontal {
                main_total += s.w;
                cross_max = cross_max.max(s.h + m.vertical());
            } else {
                main_total += s.h;
                cross_max = cross_max.max(s.w + m.horizontal());
            }
        }

        let content = if horizontal {
            Size::new(main_total, cross_max)
        } else {
            Size::new(cross_max, main_total)
        };
        (sizes, content)
    }

    // ───────────────────────────────────────────────── 配置

    fn place(&mut self, node: &'a UiNode, rect: Rect, clip: Option<Rect>) {
        let it = intrinsic(node.id);
        let pad = node.style.padding.unwrap_or_default();
        let inner = rect.deflate(pad);

        self.out.push(Placed {
            node,
            rect,
            clip,
            inner,
        });

        // 自分で何かを描くノードは葉として扱う。
        // 子を持たせた場合は無視する (Markdown の子ノードは M1.1 の範囲外)
        if node.content.is_leaf() || node.children.is_empty() {
            return;
        }

        // スクロールする領域では、主軸の制約を外して内容の実寸を得る
        let avail = if it.scroll {
            match it.axis {
                Axis::Row => Size::new(f32::INFINITY, inner.h),
                _ => Size::new(inner.w, f32::INFINITY),
            }
        } else {
            inner.size()
        };

        let (sizes, content) = self.size_children(node, &it, avail);

        let mut offset = 0.0;
        if it.scroll {
            let over = match it.axis {
                Axis::Row => content.w - inner.w,
                _ => content.h - inner.h,
            }
            .max(0.0);
            self.overflow.insert(node.id, over);
            offset = self
                .scroll
                .get(&node.id)
                .copied()
                .unwrap_or(if it.anchor_end { over } else { 0.0 })
                .clamp(0.0, over);
        }

        let clip = if it.scroll {
            Some(clip.map_or(inner, |c| c.intersect(inner)))
        } else {
            clip
        };

        let gap = node.style.gap.unwrap_or(0.0);
        let horizontal = it.axis == Axis::Row;
        let mut cursor = if horizontal { inner.x } else { inner.y } - offset;

        for (i, child) in node.children.iter().enumerate() {
            let m = child.style.margin.unwrap_or_default();
            let ci = intrinsic(child.id);
            let size = sizes[i];

            let child_rect = if it.axis == Axis::Stack {
                let s = Self::stack_size(child, &ci, size, inner);
                Rect::new(
                    inner.x + m.left + (inner.w - m.horizontal() - s.w).max(0.0) * 0.5,
                    inner.y + m.top + (inner.h - m.vertical() - s.h).max(0.0) * 0.5,
                    s.w,
                    s.h,
                )
            } else if horizontal {
                let avail = inner.h - m.vertical();
                let h = Self::cross_size(child, &ci, size.h, avail, it.cross, false);
                let y = inner.y + m.top + Self::cross_offset(it.cross, avail, h);
                Rect::new(cursor + m.left, y, size.w, h)
            } else {
                let avail = inner.w - m.horizontal();
                let w = Self::cross_size(child, &ci, size.w, avail, it.cross, true);
                let x = inner.x + m.left + Self::cross_offset(it.cross, avail, w);
                Rect::new(x, cursor + m.top, w, size.h)
            };

            if it.axis != Axis::Stack {
                cursor += if horizontal {
                    size.w + m.horizontal() + gap
                } else {
                    size.h + m.vertical() + gap
                };
            }

            self.place(child, child_rect, clip);
        }
    }

    /// 重ねの子の大きさ。指定があればそれ、なければ親いっぱい。
    fn stack_size(child: &UiNode, ci: &Intrinsic, measured: Size, inner: Rect) -> Size {
        Size::new(
            if explicit(child, ci, true).is_some() {
                measured.w
            } else {
                inner.w
            },
            if explicit(child, ci, false).is_some() {
                measured.h
            } else {
                inner.h
            },
        )
    }

    /// 交差軸での子の大きさ。
    ///
    /// **明示された大きさは伸ばさない。** `chat.message` は
    /// `cross: Start` だが、仮に `Stretch` にしても 40px のアイコンが
    /// 縦に伸びてはならない。
    fn cross_size(
        child: &UiNode,
        ci: &Intrinsic,
        measured: f32,
        avail: f32,
        cross: Cross,
        cross_is_horizontal: bool,
    ) -> f32 {
        if explicit(child, ci, cross_is_horizontal).is_some() {
            return measured;
        }
        match cross {
            Cross::Stretch if avail.is_finite() => avail.max(0.0),
            _ => measured,
        }
    }

    fn cross_offset(cross: Cross, avail: f32, size: f32) -> f32 {
        match cross {
            Cross::Center => ((avail - size) * 0.5).max(0.0),
            Cross::Start | Cross::Stretch => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use gumicord_uitree::Style;
    use gumicord_uitree::value::Edges;

    use super::*;

    /// レイアウトは GPU を必要としない。[`Shaper`] を直接作れる
    fn shaper() -> Shaper {
        Shaper::new(1.0)
    }

    fn styled(id: NodeId, f: impl FnOnce(&mut Style)) -> UiNode {
        let mut n = UiNode::new(id);
        f(&mut n.style);
        n
    }

    fn rect_of<'a>(r: &'a LayoutResult<'a>, id: NodeId) -> Rect {
        r.find(id)
            .unwrap_or_else(|| panic!("{id} が配置されていない"))
            .rect
    }

    /// 余りを取るノードが主軸を埋め、取らないノードは実寸のままである
    #[test]
    fn grow_children_share_the_remainder() {
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(styled(NodeId::NavGuildList, |s| s.width = Some(64.0)))
            .child(styled(NodeId::NavChannelList, |s| s.width = Some(240.0)))
            .child(UiNode::new(NodeId::ChatView));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        assert_eq!(rect_of(&r, NodeId::NavGuildList).w, 64.0);
        assert_eq!(rect_of(&r, NodeId::NavChannelList).w, 240.0);
        // chat.view だけが grow を持つので、残り全部を取る
        assert_eq!(rect_of(&r, NodeId::ChatView).w, 1000.0 - 64.0 - 240.0);
        assert_eq!(rect_of(&r, NodeId::ChatView).x, 304.0);
    }

    /// 主軸でも、テーマが書いた寸法は既定の `grow` より強い
    #[test]
    fn an_explicit_main_size_beats_the_default_grow() {
        // chat.view は既定で grow=1 だが、幅を書けばそちらが勝つ
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(styled(NodeId::ChatView, |s| s.width = Some(300.0)))
            .child(UiNode::new(NodeId::NavChannelList));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        assert_eq!(rect_of(&r, NodeId::ChatView).w, 300.0);
    }

    /// 交差軸が Stretch なら、明示のない子は親いっぱいに広がる。
    /// **明示された大きさは伸ばさない**
    #[test]
    fn stretch_does_not_override_an_explicit_size() {
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(styled(NodeId::NavChannelList, |s| {
                s.width = Some(240.0);
                s.height = Some(100.0);
            }))
            .child(UiNode::new(NodeId::ChatView));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        assert_eq!(
            rect_of(&r, NodeId::NavChannelList).h,
            100.0,
            "指定は伸ばさない"
        );
        assert_eq!(
            rect_of(&r, NodeId::ChatView).h,
            600.0,
            "指定がなければ広がる"
        );
    }

    /// 余白と隙間が主軸から引かれる。
    ///
    /// **根ノードは自分の寸法指定にかかわらずビューポート全体をもらう**ので、
    /// 幅を効かせたい一覧は親の下に置く必要がある。
    #[test]
    fn padding_and_gap_come_out_of_the_main_axis() {
        let tree = UiNode::new(NodeId::AppScreenMain).child(
            styled(NodeId::NavChannelList, |s| {
                s.width = Some(200.0);
                s.height = Some(300.0);
                s.padding = Some(Edges::all(10.0));
                s.gap = Some(6.0);
            })
            .child(styled(NodeId::NavChannelListItem, |s| {
                s.height = Some(30.0)
            }))
            .child(styled(NodeId::NavChannelListItem, |s| {
                s.height = Some(30.0)
            })),
        );

        let r = layout(
            &tree,
            Size::new(400.0, 400.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        assert_eq!(rect_of(&r, NodeId::NavChannelList).w, 200.0);

        let items: Vec<_> = r
            .placed
            .iter()
            .filter(|p| p.node.id == NodeId::NavChannelListItem)
            .map(|p| p.rect)
            .collect();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].y, 10.0, "上の余白ぶん下がる");
        assert_eq!(items[1].y, 10.0 + 30.0 + 6.0, "隙間ぶん空く");
        assert_eq!(items[0].x, 10.0);
        assert_eq!(items[0].w, 180.0, "交差軸は余白を引いた幅いっぱい");
    }

    /// スクロール領域は主軸の制約を外して測るので、子は縮まずにはみ出す
    #[test]
    fn a_scroll_region_reports_its_overflow() {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..10 {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        let tree = UiNode::new(NodeId::ChatView).child(list);

        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        // 10 件 × 50px = 500px が 100px の枠に入る → 400px はみ出す
        assert_eq!(r.overflow.get(&NodeId::ChatMessageList), Some(&400.0));

        // anchor_end なので、指定がなければ末尾に貼り付く。
        // 最後のメッセージの下端が枠の下端に合う
        let last = r
            .placed
            .iter()
            .rfind(|p| p.node.id == NodeId::ChatMessage)
            .unwrap();
        assert_eq!(last.rect.bottom(), 100.0);
    }

    /// スクロールした子には切り取りが付く。枠の外に出た項目は当たらない
    #[test]
    fn scrolled_children_are_clipped() {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..10 {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        let tree = UiNode::new(NodeId::ChatView).child(list);
        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let first = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::ChatMessage)
            .unwrap();
        assert!(first.rect.y < 0.0, "先頭は枠の上へ追い出されている");
        assert_eq!(first.clip, Some(Rect::new(0.0, 0.0, 400.0, 100.0)));
        // 追い出された先頭は当たらない
        assert!(r.hit(200.0, first.rect.y + 1.0).is_none());
    }

    /// 主軸の 2 段階配分。アイコンの幅を引いたあとで本文の幅が決まる
    #[test]
    fn the_remainder_is_computed_after_fixed_children() {
        let tree = styled(NodeId::ChatMessage, |s| s.gap = Some(8.0))
            .child(UiNode::new(NodeId::ChatMessageAvatar))
            .child(UiNode::new(NodeId::LayoutColumn));

        let r = layout(
            &tree,
            Size::new(500.0, 200.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        // アバターは既定で 40x40
        assert_eq!(rect_of(&r, NodeId::ChatMessageAvatar).w, 40.0);
        // 本文側は 500 - 40 - 8
        let col = rect_of(&r, NodeId::LayoutColumn);
        assert_eq!(col.w, 452.0);
        assert_eq!(col.x, 48.0);
    }

    /// 描画順は深さ優先・前順である。半透明の合成順がこれに依存する
    #[test]
    fn placement_order_is_depth_first_pre_order() {
        let tree = UiNode::new(NodeId::AppWindow)
            .child(
                UiNode::new(NodeId::ChromeTitlebar).child(UiNode::new(NodeId::ChromeTitlebarTitle)),
            )
            .child(UiNode::new(NodeId::AppScreen));

        let r = layout(
            &tree,
            Size::new(400.0, 300.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        let ids: Vec<_> = r.placed.iter().map(|p| p.node.id).collect();
        assert_eq!(
            ids,
            vec![
                NodeId::AppWindow,
                NodeId::ChromeTitlebar,
                NodeId::ChromeTitlebarTitle,
                NodeId::AppScreen,
            ]
        );
    }

    /// テキストは折り返して高さが伸びる。
    /// 同じ文字列でも折り返し幅が狭ければ高くなる。
    ///
    /// ⚠️ **ASCII だけで書いてある。** CI の Linux ランナに日本語フォントが
    /// あるとは限らない。日本語の整形結果まで固定したくなったら、同梱フォント
    /// (R4) を入れてからにする。
    #[test]
    fn narrow_text_wraps_and_grows_taller() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        if s.measure("MMMMMMMM", &font, None).w == 0.0 {
            // 使えるフォントが 1 つもない。整形が成立しないので何も言えない
            eprintln!("フォントが見つからないため、この試験は飛ばす");
            return;
        }

        let long = "The quick brown fox jumps over the lazy dog. \
                    Pack my box with five dozen liquor jugs.";
        let mut make = |w: f32| {
            let tree = styled(NodeId::ChatMessageList, |s| s.width = Some(w))
                .child(UiNode::text(NodeId::ChatMessageContent, long));
            let r = layout(&tree, Size::new(w, 2000.0), &mut s, &ScrollState::new());
            rect_of(&r, NodeId::ChatMessageContent).h
        };

        let wide = make(1000.0);
        let narrow = make(200.0);
        assert!(narrow > wide, "狭いほうが高くなるはず ({narrow} <= {wide})");
    }
}
