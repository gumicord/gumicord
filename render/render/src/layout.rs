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
use crate::intrinsic::{Axis, Cross, Intrinsic, intrinsic, is_overlay};
use crate::text::{ResolvedFont, Shaper};

/// スクロール位置。安定 ID ごとに、先頭からの距離 (論理 px) を持つ。
///
/// ⚠️ **同じ安定 ID のスクロール領域が複数あると共有される。** M1.1 では
/// 一覧が画面に 1 つずつしかないので足りる。タブや分割ビューを入れるときに
/// `key` まで含めた鍵へ広げる。
pub type ScrollState = HashMap<NodeId, f32>;

/// 摘みがこれより小さくなると掴めない (論理 px)
const MIN_THUMB: f32 = 24.0;

/// 一番下に貼り付ける。メッセージ一覧の初期値に使う
pub const SCROLL_TO_END: f32 = f32::MAX;

/// スクロール位置を覚えるときの値。
///
/// # ⚠️ 一番下は「数」ではなく「意図」で持つ
///
/// 位置を px で覚えると、新しいメッセージが来て中身が伸びたぶんだけ
/// **見ている場所が上へずれる**。一番下で待っている人にとって、それは
/// 「新しい行が来たのに見えない」ということである。
///
/// そこで、一番下に着いたときだけ [`SCROLL_TO_END`] を覚える。
/// [`layout`] は最後に `clamp` するので、中身が伸びれば伸びた先の
/// 一番下になる。
///
/// **末尾に貼り付く一覧だけの話である。** サーバ一覧の一番下まで
/// 巻いた人は「一番下に居たい」のではなく、そこにあるサーバを見て
/// いるので、増えたときに勝手に動いてはいけない。
pub fn remember(id: NodeId, at: f32, max: f32) -> f32 {
    if intrinsic(id).anchor_end && at >= max {
        SCROLL_TO_END
    } else {
        at
    }
}

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

/// 置かれたスクロールバー 1 本。**掴んで動かすために要る。**
///
/// 当たり判定だけでは足りない。摘みをどこまで動かせるかは
/// 「溝の高さ − 摘みの高さ」で決まり、それはレイアウトしか知らない。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollBar {
    /// このスクロールバーが動かすスクロール領域
    pub owner: NodeId,
    /// 溝。摘みが動ける範囲 (余白を引いたあと)
    pub track: Rect,
    pub thumb: Rect,
}

/// 1 フレームぶんの配置結果。**描画順 (深さ優先・前順) に並ぶ。**
#[derive(Debug, Default)]
pub struct LayoutResult<'a> {
    pub placed: Vec<Placed<'a>>,
    /// スクロール領域ごとの、はみ出した量 (論理 px)。
    /// スクロールの上限を決めるのに使う
    pub overflow: HashMap<NodeId, f32>,
    pub scrollbars: Vec<ScrollBar>,
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
        scrollbars: Vec::new(),
    };
    cx.place(root, Rect::from_size(viewport), None);
    LayoutResult {
        placed: cx.out,
        overflow: cx.overflow,
        scrollbars: cx.scrollbars,
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
    scrollbars: Vec<ScrollBar>,
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
            // 編集中のテキストも、大きさの上ではただの文字列である。
            // キャレットや選択の印は既にある行の上に描かれるだけで、
            // 入れ物を広げない
            Content::Text(_) | Content::Editable(_) => {
                let s = node.content.as_text().unwrap_or_default();
                let font = ResolvedFont::from_style(&node.style);

                // ⚠️ **1 行のものは折り返して測らない。** 折り返して測ると
                // 「2 行に収まっている」ことになり、行の高さが 2 倍になる。
                // 実際にチャンネル名が 2 行で出た
                if intrinsic(node.id).single_line {
                    let mut size = self.text.measure(s, &font, None);
                    if inner.w.is_finite() {
                        size.w = size.w.min(inner.w);
                    }
                    return size;
                }

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
            // 画像は入れ物いっぱいに広がる。**内容から大きさは決まらない**
            Content::Image(_) => Size::ZERO,
            // QR は正方形で、**入れ物いっぱいに広がる**。
            // 内容から大きさは決まらないので、与えられた分を使う
            Content::Qr(_) => {
                let s = inner.w.min(inner.h);
                if s.is_finite() {
                    Size::new(s, s)
                } else {
                    Size::ZERO
                }
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
                if intrinsic(c.id).follows_cross {
                    continue;
                }
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

        // 重ねて置く子 (スクロールバー) は主軸を消費しない。
        // 隙間の数にも数えない
        let overlay: Vec<bool> = node.children.iter().map(|c| is_overlay(c.id)).collect();
        let in_flow = overlay.iter().filter(|o| !**o).count();

        let gaps = gap * (in_flow.saturating_sub(1)) as f32;
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
            if overlay[i] || (grows[i] > 0.0 && remaining.is_finite()) {
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
            if overlay[i] {
                continue;
            }
            let m = margins[i];
            if horizontal {
                main_total += s.w;
            } else {
                main_total += s.h;
            }
            // 交差軸を決めない子は数に入れない。**主軸には要る** ―
            // 積まれている以上、場所は取っている
            if intrinsic(node.children[i].id).follows_cross {
                continue;
            }
            if horizontal {
                cross_max = cross_max.max(s.h + m.vertical());
            } else {
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
        let mut over = 0.0;
        if it.scroll {
            over = match it.axis {
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

        // ⚠️ **足りないぶんは下へ寄せる。**
        //
        // 末尾に貼り付く一覧は「一番新しいものが下にある」ものである。
        // 中身が枠に満たないときにそれを上へ置くと、**発言が 1 件しか
        // 無いチャンネルだけ天井に貼り付いて見える**。実際にそうなった。
        //
        // はみ出していないので巻く話ではない。位置を負へずらすことで、
        // 置き始めを下げている
        if it.anchor_end {
            let short = match it.axis {
                Axis::Row => inner.w - content.w,
                _ => inner.h - content.h,
            };
            if short > 0.0 {
                offset -= short;
            }
        }

        // ⚠️ **スクロールバーは中身の切り取りに従わない。**
        //
        // 中身は余白の内側で切るが、バーは入れ物の縁に立っている。同じ
        // 切り取りを掛けると、**余白のぶん外にあるバーが丸ごと消える**。
        // 実際に見えなくなった
        let bar_clip = clip.map(|c| c.intersect(rect));

        let clip = if it.scroll {
            Some(clip.map_or(inner, |c| c.intersect(inner)))
        } else {
            clip
        };

        let gap = node.style.gap.unwrap_or(0.0);
        let horizontal = it.axis == Axis::Row;
        let mut cursor = if horizontal { inner.x } else { inner.y } - offset;

        for (i, child) in node.children.iter().enumerate() {
            // 重ねて置く子は流れに入らない。カーソルも進めない
            if is_overlay(child.id) {
                // ⚠️ `inner` ではなく `rect`。**余白の内側へ入れない**
                self.place_scrollbar(node.id, child, rect, offset, over, bar_clip);
                continue;
            }

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

    /// スクロールバーを一覧の縁へ重ねて置く。
    ///
    /// 摘みの大きさと位置は**はみ出し量から決まる**ので、テーマには書けない。
    /// テーマが決めるのは幅・余白・色である。
    ///
    /// # ⚠️ 余白の内側ではなく、外縁に置く
    ///
    /// `track` に渡すのは余白を引く**前**の矩形である。引いた後に置くと、
    /// 余白の分だけ内側へ入り込み、**中身の上に乗る**。サーバ一覧では
    /// 48px の丸の上に線が重なった。
    ///
    /// スクロールバーは中身ではなく入れ物の縁に属している。
    ///
    /// **スクロールできるものが何もなければ、何も置かない。** 動かない
    /// スクロールバーは嘘をつく。
    fn place_scrollbar(
        &mut self,
        owner: NodeId,
        node: &'a UiNode,
        track: Rect,
        offset: f32,
        over: f32,
        clip: Option<Rect>,
    ) {
        if over <= 0.0 || track.is_empty() {
            return;
        }

        let ci = intrinsic(node.id);
        let w = explicit(node, &ci, true).unwrap_or(0.0).min(track.w);
        let bar = Rect::new(track.right() - w, track.y, w, track.h);
        let inner = bar.deflate(node.style.padding.unwrap_or_default());

        self.out.push(Placed {
            node,
            rect: bar,
            clip,
            inner,
        });

        let Some(thumb) = node
            .children
            .iter()
            .find(|c| c.id == NodeId::LayoutScrollbarThumb)
        else {
            return;
        };

        // 見えている割合がそのまま摘みの割合になる。
        // ただし掴めなくなるほど小さくはしない
        let visible = track.h;
        let content = visible + over;
        let h = (inner.h * (visible / content)).max(MIN_THUMB).min(inner.h);
        let t = (offset / over).clamp(0.0, 1.0);
        let rect = Rect::new(inner.x, inner.y + (inner.h - h) * t, inner.w, h);

        self.out.push(Placed {
            node: thumb,
            rect,
            clip,
            inner: rect.deflate(thumb.style.padding.unwrap_or_default()),
        });

        // 掴んで動かすのに要る寸法を残す。当たり判定だけでは
        // 「どこまで動かせるか」が分からない
        self.scrollbars.push(ScrollBar {
            owner,
            track: inner,
            thumb: rect,
        });
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

    /// 自分の欄は左側の幅を**決めない**。一覧が決めた幅をもらうだけである。
    ///
    /// これを外すと、帯が入れ物いっぱいに広がろうとした分だけ左側が広がり、
    /// `chat.view` に配る余りが無くなって**チャットが消える**。実際に消えた
    #[test]
    fn the_user_panel_takes_the_width_it_is_given() {
        let lists = UiNode::new(NodeId::NavSidebarLists)
            .child(styled(NodeId::NavGuildList, |s| s.width = Some(64.0)))
            .child(styled(NodeId::NavChannelList, |s| s.width = Some(240.0)));
        let panel = UiNode::new(NodeId::NavUserPanel).child(
            UiNode::new(NodeId::LayoutColumn).child(UiNode::text(
                NodeId::NavUserPanelName,
                "ずいぶん長い名前のひと".to_owned(),
            )),
        );
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(UiNode::new(NodeId::NavSidebar).child(lists).child(panel))
            .child(UiNode::new(NodeId::ChatView));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        assert_eq!(rect_of(&r, NodeId::NavSidebar).w, 304.0, "一覧が幅を決める");
        assert_eq!(
            rect_of(&r, NodeId::NavUserPanel).w,
            304.0,
            "帯は両方にまたがる"
        );
        assert_eq!(
            rect_of(&r, NodeId::ChatView).w,
            696.0,
            "チャットに余りが残る"
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

    /// メッセージ一覧を建てる。`n` 件 × 50px
    fn messages(n: u64) -> UiNode {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..n {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        UiNode::new(NodeId::ChatView).child(list)
    }

    /// ⚠️ **一番下で待っていたら、新しい行が来てもそこに居続ける。**
    ///
    /// 位置を px で覚えると、増えたぶんだけ見ている場所が上へずれ、
    /// **来たはずの行が見えない**
    #[test]
    fn a_list_pinned_to_the_bottom_follows_new_rows() {
        let mut scroll = ScrollState::new();
        // 一番下まで巻いた、という意図を覚えている
        scroll.insert(NodeId::ChatMessageList, SCROLL_TO_END);

        for n in [10, 11, 20] {
            let tree = messages(n);
            let r = layout(&tree, Size::new(400.0, 100.0), &mut shaper(), &scroll);
            let last = r
                .placed
                .iter()
                .rfind(|p| p.node.id == NodeId::ChatMessage)
                .expect("メッセージがある");
            assert_eq!(last.rect.bottom(), 100.0, "{n} 件でも一番下に居る");
        }
    }

    /// ⚠️ **中身が枠に満たなくても下に寄る。**
    ///
    /// 一番新しいものが下にある一覧で、1 件しか無いときだけ天井に
    /// 貼り付いて見えた
    #[test]
    fn a_short_list_still_sits_at_the_bottom() {
        let tree = messages(1);
        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let only = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::ChatMessage)
            .expect("1 件ある");
        assert_eq!(only.rect.bottom(), 100.0, "下端に着いている");
        assert_eq!(only.rect.y, 50.0, "50px の行が 100px の枠の下半分にいる");

        // はみ出してはいないので、巻けるものは何も無い
        assert_eq!(r.overflow.get(&NodeId::ChatMessageList), Some(&0.0));
    }

    /// ⚠️ **末尾に貼り付かない一覧は上のままである。**
    /// サーバ一覧が 1 つだけのときに下へ落ちてはいけない
    #[test]
    fn a_list_that_does_not_anchor_stays_at_the_top() {
        let mut list = styled(NodeId::NavGuildList, |s| s.height = Some(100.0));
        list = list.child(styled(NodeId::NavGuildListItem, |s| {
            s.width = Some(48.0);
            s.height = Some(48.0);
        }));
        let tree = UiNode::new(NodeId::AppScreenMain).child(list);

        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        let only = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::NavGuildListItem)
            .expect("1 つある");
        assert_eq!(only.rect.y, 0.0);
    }

    /// 途中で止めていたら、そこに残る。**勝手に追いかけない**
    #[test]
    fn a_list_stopped_midway_stays_where_it_was() {
        let mut scroll = ScrollState::new();
        scroll.insert(NodeId::ChatMessageList, 100.0);

        let tree = messages(20);
        let r = layout(&tree, Size::new(400.0, 100.0), &mut shaper(), &scroll);
        let first = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::ChatMessage)
            .expect("メッセージがある");
        assert_eq!(first.rect.y, -100.0, "100px 巻いた場所のまま");
    }

    /// ⚠️ **末尾に貼り付かない一覧では、一番下でも意図にしない。**
    ///
    /// サーバ一覧の一番下まで巻いた人は「一番下に居たい」のではなく、
    /// そこにあるサーバを見ている。増えたときに動いてはいけない
    #[test]
    fn only_lists_that_anchor_to_the_end_stick_there() {
        assert_eq!(
            remember(NodeId::ChatMessageList, 400.0, 400.0),
            SCROLL_TO_END
        );
        assert_eq!(remember(NodeId::ChatMessageList, 399.0, 400.0), 399.0);
        assert_eq!(remember(NodeId::NavGuildList, 400.0, 400.0), 400.0);
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

#[cfg(test)]
mod scrollbar_tests {
    use gumicord_uitree::Style;

    use super::*;

    fn shaper() -> Shaper {
        Shaper::new(1.0)
    }

    fn styled(id: NodeId, f: impl FnOnce(&mut Style)) -> UiNode {
        let mut n = UiNode::new(id);
        f(&mut n.style);
        n
    }

    /// 高さ 100 の枠に 50px のメッセージを `n` 件と、スクロールバーを入れる
    fn list(n: u64, scroll: Option<f32>) -> (UiNode, ScrollState) {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..n {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        list = list.child(
            styled(NodeId::LayoutScrollbar, |s| s.width = Some(10.0))
                .child(UiNode::new(NodeId::LayoutScrollbarThumb)),
        );

        let mut state = ScrollState::new();
        if let Some(at) = scroll {
            state.insert(NodeId::ChatMessageList, at);
        }
        (UiNode::new(NodeId::ChatView).child(list), state)
    }

    fn place<'a>(tree: &'a UiNode, state: &ScrollState) -> LayoutResult<'a> {
        layout(tree, Size::new(400.0, 100.0), &mut shaper(), state)
    }

    /// スクロールバーは流れに入らない。**入れても一覧の高さが変わらない**
    #[test]
    fn a_scrollbar_does_not_consume_the_main_axis() {
        let (with_bar, state) = list(10, None);
        let over_with = *place(&with_bar, &state)
            .overflow
            .get(&NodeId::ChatMessageList)
            .unwrap();

        // 10 件 × 50px = 500px が 100px の枠に入る → 400px はみ出す。
        // スクロールバーがここに 1 件ぶん足されていたら 400 にならない
        assert_eq!(over_with, 400.0);
    }

    /// 縁に置かれ、中身と一緒にスクロールしない
    #[test]
    fn the_scrollbar_sits_on_the_trailing_edge() {
        let (tree, state) = list(10, None);
        let r = place(&tree, &state);
        let bar = r.find(NodeId::LayoutScrollbar).expect("置かれていない");

        assert_eq!(bar.rect.w, 10.0);
        assert_eq!(bar.rect.right(), 400.0, "右端に付く");
        assert_eq!(bar.rect.y, 0.0, "スクロールしても上端のまま");
        assert_eq!(bar.rect.h, 100.0, "枠いっぱいの高さ");
    }

    /// ⚠️ **余白の内側へ入らない。**
    ///
    /// 余白を引いた後に置くと、その分だけ内側へ入り込んで**中身の上に
    /// 乗る**。サーバ一覧で 48px の丸の上に線が重なった
    #[test]
    fn the_scrollbar_ignores_the_padding() {
        let mut list = styled(NodeId::ChatMessageList, |s| {
            s.height = Some(100.0);
            s.padding = Some(Edges::all(12.0));
        });
        for i in 0..10 {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        list = list.child(
            styled(NodeId::LayoutScrollbar, |s| s.width = Some(10.0))
                .child(UiNode::new(NodeId::LayoutScrollbarThumb)),
        );
        let tree = UiNode::new(NodeId::ChatView).child(list);

        let r = place(&tree, &ScrollState::new());
        let bar = r.find(NodeId::LayoutScrollbar).expect("置かれていない");

        assert_eq!(bar.rect.right(), 400.0, "余白があっても外縁に付く");
        assert_eq!(bar.rect.h, 100.0, "高さも余白を引かない");

        // ⚠️ **中身の切り取りを掛けると消える。** 実際に見えなくなった
        if let Some(c) = bar.clip {
            assert!(
                !c.intersect(bar.rect).is_empty(),
                "切り取られて何も描かれない: clip={c:?} bar={:?}",
                bar.rect
            );
        }
        let thumb = r.find(NodeId::LayoutScrollbarThumb).expect("摘みがある");
        if let Some(c) = thumb.clip {
            assert!(!c.intersect(thumb.rect).is_empty(), "摘みも消えている");
        }
    }

    /// 摘みの大きさは見えている割合になり、位置はスクロール量に従う
    #[test]
    fn the_thumb_reflects_how_far_we_are() {
        // 先頭
        let (tree, state) = list(10, Some(0.0));
        let r = place(&tree, &state);
        let top = r.find(NodeId::LayoutScrollbarThumb).unwrap().rect;
        // 500px 中 100px が見えている → 摘みは 1/5
        assert_eq!(top.h, 20.0_f32.max(MIN_THUMB));
        assert_eq!(top.y, 0.0);

        // 末尾
        let (tree, state) = list(10, Some(f32::MAX));
        let r = place(&tree, &state);
        let bottom = r.find(NodeId::LayoutScrollbarThumb).unwrap().rect;
        assert_eq!(bottom.bottom(), 100.0, "下端まで来る");
        assert_eq!(bottom.h, top.h, "大きさは変わらない");
    }

    /// **スクロールできないなら置かない。** 動かないスクロールバーは嘘をつく
    #[test]
    fn no_scrollbar_when_nothing_overflows() {
        let (tree, state) = list(1, None);
        let r = place(&tree, &state);
        assert!(r.find(NodeId::LayoutScrollbar).is_none());
        assert!(r.find(NodeId::LayoutScrollbarThumb).is_none());
    }
}
