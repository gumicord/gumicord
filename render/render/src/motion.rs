//! アニメーション。**確定したスタイルを、そこへ動かす。**
//!
//! # ⚠️ フレーム駆動ではなく時間駆動である
//!
//! ([`spec/06-renderer.md`] 13 章の未確定項目に対する答え。)
//!
//! 「1 フレームごとに少しずつ近づける」書き方は簡単だが、**60Hz と 144Hz
//! で速さが変わる**。`EXT-020` (全プラットフォームで同じ描画結果) を
//! 掲げている以上、それは採れない。経過時間で位置を決める。
//!
//! ```text
//!   t = (いま − 始めた時刻) / 長さ
//!   出す値 = from + (to − from) × ease(t)
//! ```
//!
//! # 何が動くか
//!
//! **木の形は動かない。** 動くのは既に確定したスタイルの値だけである。
//! 出たり消えたりするノード (選択の印など) が滑らかに現れるようにするには
//! 別の仕組みが要り、それはここには無い。
//!
//! 動くのは色・角の丸み・枠の太さ・不透明度・寸法である。**寸法が動くと
//! レイアウトも動く** — メンションの印が伸び縮みするのはこれによる。
//!
//! # 動くのはテーマが言ったものだけ
//!
//! `transition` を書いたノードだけが動く。書いていなければ即座に切り替わる。
//! ⚠️ **既定で全部動かさない。** 一覧をめくるたびに何十行も動き出すのは
//! 見栄えではなく騒音である。
//!
//! # 止まったら寝る (`NFR-005`)
//!
//! [`Motion::apply`] は「まだ動いているか」を返す。動いていない間は
//! 1 フレームも描かない。**動いているあいだだけ** 60Hz で回る。

use std::collections::HashMap;
use std::time::Instant;

use gumicord_uitree::value::{Background, Color};
use gumicord_uitree::{Key, NodeId, Style, UiNode};

/// これ以下の差は「同じ」とみなす (論理 px / 0〜1 の比)
const EPSILON: f32 = 0.01;

/// ノードを跨いで覚えておくための鍵。
///
/// ⚠️ **木は毎フレーム組み直される。** 同じノードだと言えるのは
/// 安定 ID と `key` の組だけである ([`spec/03-uitree.md`] 2.2)。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Ident(NodeId, Option<Key>);

/// 1 ノードぶんの、動いている最中の記録。
#[derive(Debug, Clone)]
struct Track {
    /// 動き始めた時点で見えていた値
    from: Style,
    /// 向かっている先。**テーマが確定させた値そのもの**
    to: Style,
    started: Instant,
    /// ミリ秒
    duration: f32,
    /// このフレームで見かけたか。**見かけなかったものは捨てる**
    seen: u64,
}

/// 動いているものの記録。
#[derive(Debug, Default)]
pub struct Motion {
    tracks: HashMap<Ident, Track>,
    /// 何フレーム目か。**消えたノードを捨てるためだけに使う**
    frame: u64,
}

impl Motion {
    pub fn new() -> Self {
        Motion::default()
    }

    /// 木のスタイルを「いまの値」へ書き換える。**まだ動いていれば真**。
    ///
    /// 真が返る間、呼び出し側は次のフレームを要求すること。止まったら
    /// 요求をやめる — それが `NFR-005` である。
    pub fn apply(&mut self, root: &mut UiNode, now: Instant) -> bool {
        self.frame = self.frame.wrapping_add(1);
        let moving = self.walk(root, now);
        // ⚠️ **消えたノードの記録を捨てる。** 残すと、一覧をめくるたびに
        // 増え続ける
        let frame = self.frame;
        self.tracks.retain(|_, t| t.seen == frame);
        moving
    }

    fn walk(&mut self, node: &mut UiNode, now: Instant) -> bool {
        let mut moving = self.node(node, now);
        for child in &mut node.children {
            moving |= self.walk(child, now);
        }
        moving
    }

    fn node(&mut self, node: &mut UiNode, now: Instant) -> bool {
        let Some(duration) = node.style.transition.filter(|d| *d > 0.0) else {
            return false;
        };
        let ident = Ident(node.id, node.key.clone());
        let frame = self.frame;

        let Some(track) = self.tracks.get_mut(&ident) else {
            // 初めて見たノードは動かさない。**開いた瞬間に画面じゅうが
            // 動き出すのは見栄えではない**
            self.tracks.insert(
                ident,
                Track {
                    from: node.style.clone(),
                    to: node.style.clone(),
                    started: now,
                    duration,
                    seen: frame,
                },
            );
            return false;
        };
        track.seen = frame;

        // 行き先が変わったら、**いま見えている値から**引き直す。
        // ⚠️ 途中で向きが変わったときに `to` から始めると、一度飛ぶ
        if track.to != node.style {
            track.from = displayed(&track.from, &track.to, progress(track, now));
            track.to = node.style.clone();
            track.started = now;
            track.duration = duration;
        }

        let t = progress(track, now);
        node.style = displayed(&track.from, &track.to, t);
        t < 1.0
    }
}

/// 0.0〜1.0。**長さが 0 なら即座に 1.0**
fn progress(track: &Track, now: Instant) -> f32 {
    if track.duration <= 0.0 {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(track.started).as_secs_f32() * 1000.0;
    (elapsed / track.duration).clamp(0.0, 1.0)
}

/// 出だしが速く、着地が静かな曲線。
///
/// ⚠️ **等速にしない。** 等速で動くものは機械が動いているように見え、
/// 止まった瞬間が不自然に目立つ。Discord も含め、UI はほぼ例外なく
/// 「速く出て静かに止まる」を使う。
fn ease_out(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

/// `from` と `to` の間の、`t` の位置の値。
fn displayed(from: &Style, to: &Style, t: f32) -> Style {
    if t >= 1.0 {
        return to.clone();
    }
    let e = ease_out(t);

    // ⚠️ **混ぜられないものは行き先をそのまま使う。** 書体も影も、
    // 途中の値に意味が無い。中途半端に混ぜるより即座に切り替わるほうが
    // 読み違えを生まない
    let mut out = to.clone();
    out.color = lerp_color(from.color, to.color, e);
    out.border_color = lerp_color(from.border_color, to.border_color, e);
    out.background = lerp_background(from.background.as_ref(), to.background.as_ref(), e);
    out.border_width = lerp_opt(from.border_width, to.border_width, e);
    out.radius = lerp_opt(from.radius, to.radius, e);
    out.opacity = lerp_opt(from.opacity, to.opacity, e);
    out.width = lerp_opt(from.width, to.width, e);
    out.height = lerp_opt(from.height, to.height, e);
    out.gap = lerp_opt(from.gap, to.gap, e);
    out
}

/// ⚠️ **片方が未指定なら混ぜない。** 「指定なし」は値ではないので、
/// 0 とみなして混ぜると黒や幅 0 を経由することになる
fn lerp_opt(from: Option<f32>, to: Option<f32>, t: f32) -> Option<f32> {
    match (from, to) {
        (Some(a), Some(b)) if (a - b).abs() > EPSILON => Some(a + (b - a) * t),
        _ => to,
    }
}

fn lerp_color(from: Option<Color>, to: Option<Color>, t: f32) -> Option<Color> {
    let (Some(a), Some(b)) = (from, to) else {
        return to;
    };
    Some(Color {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
        a: lerp_u8(a.a, b.a, t),
    })
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
    v.round().clamp(0.0, 255.0) as u8
}

/// 背景は**色だけ**を混ぜる。
///
/// ⚠️ **絵は混ぜない。** 2 枚の画像の中間に意味は無く、混ぜるには
/// 両方を描いて重ねる必要がある。そこまでするものではない
fn lerp_background(
    from: Option<&Background>,
    to: Option<&Background>,
    t: f32,
) -> Option<Background> {
    let to = to?;
    let Some(from) = from else {
        return Some(to.clone());
    };
    if from.image.is_some() || to.image.is_some() {
        return Some(to.clone());
    }
    let mut out = to.clone();
    out.color = lerp_color(from.color, to.color, t);
    out.tint = lerp_color(from.tint, to.tint, t);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn node(radius: f32, transition: Option<f32>) -> UiNode {
        let mut n = UiNode::new(NodeId::NavGuildListItemIcon);
        n.style.radius = Some(radius);
        n.style.transition = transition;
        n
    }

    /// ⚠️ **初めて見たノードは動かさない。**
    /// 開いた瞬間に画面じゅうが動き出すのは見栄えではない
    #[test]
    fn the_first_sight_of_a_node_does_not_move() {
        let mut m = Motion::new();
        let mut n = node(12.0, Some(100.0));
        let now = Instant::now();

        assert!(!m.apply(&mut n, now));
        assert_eq!(n.style.radius, Some(12.0));
    }

    /// 行き先が変わったら、そこへ**時間をかけて**動く
    #[test]
    fn a_changed_value_travels_over_time() {
        let mut m = Motion::new();
        let start = Instant::now();

        m.apply(&mut node(12.0, Some(100.0)), start);

        // 変わった直後は、まだ元の値のあたりにいる
        let mut n = node(8.0, Some(100.0));
        assert!(m.apply(&mut n, start), "動いている");
        assert_eq!(n.style.radius, Some(12.0), "まだ動き出していない");

        // 途中
        let mut n = node(8.0, Some(100.0));
        assert!(m.apply(&mut n, start + Duration::from_millis(50)));
        let mid = n.style.radius.expect("値がある");
        assert!(mid < 12.0 && mid > 8.0, "{mid}");

        // 終わり
        let mut n = node(8.0, Some(100.0));
        assert!(
            !m.apply(&mut n, start + Duration::from_millis(200)),
            "止まった"
        );
        assert_eq!(n.style.radius, Some(8.0));
    }

    /// ⚠️ **`transition` を書いていないノードは即座に切り替わる。**
    /// 既定で全部動かすと、一覧をめくるたびに何十行も動き出す
    #[test]
    fn without_a_transition_nothing_moves() {
        let mut m = Motion::new();
        let start = Instant::now();

        m.apply(&mut node(12.0, None), start);
        let mut n = node(8.0, None);
        assert!(!m.apply(&mut n, start));
        assert_eq!(n.style.radius, Some(8.0), "すぐそこへ行く");
    }

    /// ⚠️ **途中で向きが変わったら、いま見えている値から引き直す。**
    /// 行き先から始めると一度飛ぶ
    #[test]
    fn reversing_midway_starts_from_where_it_is() {
        let mut m = Motion::new();
        let start = Instant::now();

        m.apply(&mut node(12.0, Some(100.0)), start);
        let mut n = node(8.0, Some(100.0));
        m.apply(&mut n, start + Duration::from_millis(50));
        let mid = n.style.radius.expect("値がある");

        // 引き返す
        let mut n = node(12.0, Some(100.0));
        m.apply(&mut n, start + Duration::from_millis(50));
        assert_eq!(n.style.radius, Some(mid), "飛ばずにその場から");
    }

    /// ⚠️ **片方が未指定なら混ぜない。** 0 とみなすと黒や幅 0 を経由する
    #[test]
    fn an_unset_value_is_not_treated_as_zero() {
        assert_eq!(lerp_opt(None, Some(10.0), 0.5), Some(10.0));
        assert_eq!(lerp_opt(Some(10.0), None, 0.5), None);
        assert_eq!(
            lerp_color(None, Color::parse("#ffffff"), 0.5),
            Color::parse("#ffffff")
        );
    }

    /// ⚠️ **消えたノードの記録は捨てる。** 一覧をめくるたびに増え続ける
    #[test]
    fn a_node_that_went_away_is_forgotten() {
        let mut m = Motion::new();
        let now = Instant::now();

        m.apply(&mut node(12.0, Some(100.0)), now);
        assert_eq!(m.tracks.len(), 1);

        // 別のノードだけの木
        let mut other = UiNode::new(NodeId::ChatView);
        other.style.transition = Some(100.0);
        m.apply(&mut other, now);
        assert_eq!(m.tracks.len(), 1, "前のは捨てられている");
    }

    /// 出だしが速く、着地が静か
    #[test]
    fn the_curve_starts_fast_and_lands_quietly() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "前半で半分より進む");
    }
}
