//! 確定したスタイル。
//!
//! # カスケード規則は 1 つだけである
//!
//! | # | 規則 |
//! |---|---|
//! | **K1** | ルールは記述順に適用され、後のルールが前のルールを**プロパティ単位で**上書きする |
//!
//! CSS の詳細度を計算しない。セレクタの具体性も `when` の数も考慮しない。
//! テーマ作者が「なぜこのルールが効かないのか」を上から読んで必ず分かる
//! 状態を優先する ([`spec/04-theme.md`] 5 章)。
//!
//! 未指定のプロパティは `None` のままにする。**`None` と「既定値が入っている」
//! を区別する**ためである。区別できないと [`Style::overlay`] が
//! 「指定していないのに上書きした」という誤りを起こす。

use crate::value::{Background, Color, Edges, Font, Shadow};

/// 1 ノードに確定したスタイル。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub background: Option<Background>,
    /// 前景 (文字色)。**子へ継承する**
    pub color: Option<Color>,
    /// 書体。**子へ継承する**
    pub font: Option<Font>,
    pub border_color: Option<Color>,
    pub border_width: Option<f32>,
    pub radius: Option<f32>,
    pub padding: Option<Edges>,
    pub margin: Option<Edges>,
    pub gap: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
    pub min_height: Option<f32>,
    pub max_height: Option<f32>,
    pub opacity: Option<f32>,
    pub shadow: Option<Shadow>,
}

impl Style {
    /// **K1 の実装。** `other` で指定されているプロパティだけを上書きする。
    ///
    /// ```
    /// # use gumicord_theme::{Style, value::Color};
    /// let mut a = Style {
    ///     color: Color::parse("#111"),
    ///     radius: Some(8.0),
    ///     ..Default::default()
    /// };
    /// let b = Style {
    ///     color: Color::parse("#222"),
    ///     ..Default::default()
    /// };
    /// a.overlay(&b);
    /// assert_eq!(a.color, Color::parse("#222"));
    /// assert_eq!(a.radius, Some(8.0)); // 上書きされない
    /// ```
    pub fn overlay(&mut self, other: &Style) {
        // マクロにしないのは、プロパティを足したときに「書き忘れ」が
        // コンパイルエラーではなく静かな不具合になるのを避けるため。
        // 数が増えたら、そのときに網羅性を試験で担保する。
        overlay_field(&mut self.background, &other.background);
        overlay_field(&mut self.color, &other.color);
        overlay_field(&mut self.font, &other.font);
        overlay_field(&mut self.border_color, &other.border_color);
        overlay_field(&mut self.border_width, &other.border_width);
        overlay_field(&mut self.radius, &other.radius);
        overlay_field(&mut self.padding, &other.padding);
        overlay_field(&mut self.margin, &other.margin);
        overlay_field(&mut self.gap, &other.gap);
        overlay_field(&mut self.width, &other.width);
        overlay_field(&mut self.height, &other.height);
        overlay_field(&mut self.min_width, &other.min_width);
        overlay_field(&mut self.max_width, &other.max_width);
        overlay_field(&mut self.min_height, &other.min_height);
        overlay_field(&mut self.max_height, &other.max_height);
        overlay_field(&mut self.opacity, &other.opacity);
        overlay_field(&mut self.shadow, &other.shadow);
    }

    /// 親から継承する。**継承するのは `color` と `font` だけ**である
    /// ([`spec/04-theme.md`] 6 章)。
    ///
    /// 自分で指定している側が優先される。木を歩くのはレイアウト段の仕事で
    /// あり、ここでは 1 段ぶんの規則だけを定義する。
    pub fn inherit_from(&mut self, parent: &Style) {
        if self.color.is_none() {
            self.color = parent.color;
        }
        if self.font.is_none() {
            self.font.clone_from(&parent.font);
        }
    }

    /// 何も指定されていないか。
    pub fn is_empty(&self) -> bool {
        *self == Style::default()
    }
}

fn overlay_field<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
    if src.is_some() {
        dst.clone_from(src);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(s: &str) -> Option<Color> {
        Color::parse(s)
    }

    /// K1: 後のルールが勝つ
    #[test]
    fn later_wins() {
        let mut a = Style {
            color: color("#111"),
            ..Default::default()
        };
        a.overlay(&Style {
            color: color("#222"),
            ..Default::default()
        });
        assert_eq!(a.color, color("#222"));
    }

    /// 5.1: 上書きの単位はプロパティであり、ルール全体ではない
    #[test]
    fn overlay_is_per_property() {
        let mut a = Style {
            background: Some(Background::solid(color("#111").unwrap())),
            radius: Some(8.0),
            ..Default::default()
        };
        a.overlay(&Style {
            background: Some(Background::solid(color("#222").unwrap())),
            ..Default::default()
        });
        assert_eq!(
            a.background,
            Some(Background::solid(color("#222").unwrap()))
        );
        assert_eq!(a.radius, Some(8.0), "指定のないプロパティは残る");
    }

    #[test]
    fn overlay_with_empty_changes_nothing() {
        let original = Style {
            radius: Some(4.0),
            gap: Some(8.0),
            ..Default::default()
        };
        let mut a = original.clone();
        a.overlay(&Style::default());
        assert_eq!(a, original);
    }

    /// 継承するのは color と font だけ
    #[test]
    fn only_color_and_font_inherit() {
        let parent = Style {
            color: color("#eee"),
            font: Some(Font {
                size: Some(15.0),
                ..Default::default()
            }),
            radius: Some(8.0),
            padding: Some(Edges::all(4.0)),
            background: Some(Background::solid(color("#111").unwrap())),
            ..Default::default()
        };
        let mut child = Style::default();
        child.inherit_from(&parent);

        assert_eq!(child.color, color("#eee"));
        assert_eq!(child.font, parent.font);
        assert_eq!(child.radius, None, "radius は継承しない");
        assert_eq!(child.padding, None, "padding は継承しない");
        assert_eq!(child.background, None, "background は継承しない");
    }

    #[test]
    fn own_value_beats_inherited() {
        let parent = Style {
            color: color("#eee"),
            ..Default::default()
        };
        let mut child = Style {
            color: color("#f00"),
            ..Default::default()
        };
        child.inherit_from(&parent);
        assert_eq!(child.color, color("#f00"));
    }
}
