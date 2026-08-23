//! Resolved style.
//!
//! There is one cascade rule: later rules override earlier ones, per
//! property. No specificity is computed — not from the selector, not from the
//! number of `when` clauses. A theme author must be able to read top to
//! bottom and see why a rule did not take effect.
//!
//! Unspecified properties stay `None` so that "unset" and "set to the
//! default" stay distinguishable; without that, [`Style::overlay`] would
//! overwrite values nobody asked it to.

use crate::value::{Background, Color, Edges, Font, Shadow};

/// The resolved style of one node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub background: Option<Background>,
    /// Foreground colour. Inherited by children.
    pub color: Option<Color>,
    /// Font. Inherited by children.
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
    /// Milliseconds to move to a new value.
    ///
    /// Unlike every other property this draws nothing itself: it only says
    /// that the next change to this node's style should move rather than
    /// jump.
    ///
    /// Not inherited. Animating children because their parent animates would
    /// set places in motion the theme never asked for.
    pub transition: Option<f32>,
    /// Lines drawn through text.
    ///
    /// Whether `__a__` becomes an underline, a colour change or a weight
    /// change is the theme's judgement. The parser only reports that it was
    /// wrapped in `__`.
    pub decoration: Option<Decoration>,
}

/// Lines drawn through text. Stackable: `__~~a~~__` draws both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decoration {
    pub underline: bool,
    pub strikethrough: bool,
}

impl Style {
    /// Overrides only the properties `other` actually specifies.
    ///
    /// ```
    /// # use gumicord_uitree::{Style, value::Color};
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
    /// assert_eq!(a.radius, Some(8.0)); // untouched
    /// ```
    pub fn overlay(&mut self, other: &Style) {
        // Written out rather than generated: a forgotten property should be
        // visible here rather than becoming a silent bug.
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
        overlay_field(&mut self.transition, &other.transition);
        overlay_field(&mut self.decoration, &other.decoration);
    }

    /// Inherits from a parent. Only `color` and `font` are inherited, and an
    /// own value wins.
    ///
    /// Walking the tree is the layout stage's job; this defines one step.
    pub fn inherit_from(&mut self, parent: &Style) {
        if self.color.is_none() {
            self.color = parent.color;
        }
        if self.font.is_none() {
            self.font.clone_from(&parent.font);
        }
    }

    /// Whether nothing is specified.
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

    /// Later rules win.
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

    /// Overriding is per property, not per rule.
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
        assert_eq!(a.radius, Some(8.0), "unspecified properties survive");
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

    /// Only colour and font inherit.
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
        assert_eq!(child.radius, None, "radius does not inherit");
        assert_eq!(child.padding, None, "padding does not inherit");
        assert_eq!(child.background, None, "background does not inherit");
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
