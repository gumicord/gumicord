//! Theme resolution: parsing, validation, tokens, selector matching and
//! settling a style.
//!
//! One cascade rule: later rules override earlier ones, per property. No
//! specificity, so a theme author can read top to bottom and see why a rule
//! did not take effect.
//!
//! A validation failure never discards the whole theme; only the offending
//! rule or property is ignored.
//!
//! ```
//! use gumicord_theme::{MatchContext, Theme};
//! use gumicord_uitree::NodeId;
//!
//! let src = r##"{
//!   "manifest": { "id": "com.example.t", "name": "T", "version": "1.0.0", "abi": 1 },
//!   "tokens": { "color.bg": "#0f0f17" },
//!   "rules": [{ "select": "app.window", "style": { "background": "$color.bg" } }]
//! }"##;
//!
//! let loaded = Theme::parse(src);
//! let theme = loaded.theme.expect("usable");
//! let style = theme.style_for(NodeId::AppWindow, &MatchContext::new(1280.0));
//! assert!(style.background.is_some());
//! ```
//!
//! See `spec/04-theme.md`.

pub mod cond;
pub mod diag;
mod parse;
pub mod resolve;
pub mod token;

// `Style` and the value types belong to the extension ABI and are touched by
// themes, plugins and the renderer, so they live in the ABI crate and are
// re-exported here. Defining them here would make the dependency circular.
pub use gumicord_uitree::{style, value};

use std::collections::HashMap;

use serde_json::Value;

use gumicord_uitree::NodeId;

pub use crate::cond::{ColorScheme, MatchContext, Platform, PlatformSel, When};
pub use crate::diag::{Diagnostic, Diagnostics, Ignored, Severity};
pub use crate::parse::{Manifest, Rule, Tinted};
pub use crate::resolve::resolve;
pub use crate::style::Style;
pub use crate::token::{TokenValue, Tokens};
pub use crate::value::{AssetKind, AssetRef, Background, Color, Edges, Fit, Font, Shadow};

/// The ABI version this client understands. A theme declaring a higher one is
/// reported as too new, and only its known rules apply.
pub const CLIENT_ABI: u32 = 1;

/// A usable theme.
///
/// Selectors match a stable ID exactly. Without wildcards, adding an ID can
/// never change how an existing theme looks.
#[derive(Debug, Clone)]
pub struct Theme {
    pub manifest: Manifest,
    rules: Vec<Rule>,
    /// Stable ID to the rules selecting it, in order.
    ///
    /// Scanning every rule per node would be quadratic; a theme never changes
    /// once parsed, so it is indexed then.
    index: HashMap<NodeId, Vec<u32>>,
}

/// The result of parsing.
///
/// Not a `Result`: whether the theme applies and what was ignored are
/// different questions, and three ignored rules still leave a usable theme.
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// The theme, or `None` if none of it applies.
    pub theme: Option<Theme>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ParseResult {
    pub fn is_applied(&self) -> bool {
        self.theme.is_some()
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
    }
}

impl Theme {
    /// Parses a theme.
    ///
    /// Only two things discard the whole theme: malformed JSON, and a missing
    /// or invalid manifest.
    pub fn parse(src: &str) -> ParseResult {
        let mut diags = Diagnostics::new();

        let root: Value = match serde_json::from_str(src) {
            Ok(v) => v,
            Err(e) => {
                // Only a syntax error carries a line number.
                diags.error(
                    format!("{}行{}列", e.line(), e.column()),
                    Ignored::Theme,
                    format!("JSON として壊れている: {e}"),
                );
                return ParseResult {
                    theme: None,
                    diagnostics: diags.into_items(),
                };
            }
        };

        let Some(obj) = root.as_object() else {
            diags.error("", Ignored::Theme, "最上位はオブジェクトである必要がある");
            return ParseResult {
                theme: None,
                diagnostics: diags.into_items(),
            };
        };

        for key in obj.keys() {
            if !matches!(key.as_str(), "$schema" | "manifest" | "tokens" | "rules") {
                diags.warn(
                    key.as_str(),
                    Ignored::Nothing,
                    format!("最上位の未知のキー {key} を無視した"),
                );
            }
        }

        let Some(manifest_value) = obj.get("manifest") else {
            diags.error("manifest", Ignored::Theme, "manifest が必要");
            return ParseResult {
                theme: None,
                diagnostics: diags.into_items(),
            };
        };
        let Some(manifest) = parse::manifest(manifest_value, &mut diags) else {
            return ParseResult {
                theme: None,
                diagnostics: diags.into_items(),
            };
        };

        if manifest.abi > CLIENT_ABI {
            diags.warn(
                "manifest.abi",
                Ignored::Nothing,
                format!(
                    "このテーマは ABI {} を想定しているが、\
                     このクライアントは {CLIENT_ABI} までしか解釈できない。\
                     既知のルールのみを適用する",
                    manifest.abi
                ),
            );
        }

        let tokens = match obj.get("tokens") {
            Some(Value::Object(t)) => Tokens::build(t, &mut diags),
            Some(_) => {
                diags.error(
                    "tokens",
                    Ignored::Nothing,
                    "tokens はオブジェクトである必要がある。トークンなしとして扱った",
                );
                Tokens::default()
            }
            None => Tokens::default(),
        };

        let rules = match obj.get("rules") {
            Some(Value::Array(a)) => parse::rules(a, &tokens, &manifest.remote_assets, &mut diags),
            Some(_) => {
                diags.error(
                    "rules",
                    Ignored::Nothing,
                    "rules は配列である必要がある。ルールなしとして扱った",
                );
                Vec::new()
            }
            None => Vec::new(),
        };

        let mut index: HashMap<NodeId, Vec<u32>> = HashMap::new();
        for (i, rule) in rules.iter().enumerate() {
            index.entry(rule.select).or_default().push(i as u32);
        }

        ParseResult {
            theme: Some(Theme {
                manifest,
                rules,
                index,
            }),
            diagnostics: diags.into_items(),
        }
    }

    /// Settles one node's style: rules in order, layered per property, no
    /// specificity.
    pub fn style_for(&self, node: NodeId, ctx: &MatchContext) -> Style {
        self.style_for_tinted(node, ctx, None)
    }

    /// Every background image the rules name, with its fit and blur. The
    /// fetcher resolves these; the renderer only ever sees lookup keys.
    pub fn background_images(
        &self,
    ) -> Vec<(
        gumicord_uitree::value::AssetRef,
        gumicord_uitree::value::Fit,
        f32,
    )> {
        let mut out = Vec::new();
        for rule in &self.rules {
            if let Some(bg) = rule.style.background.as_ref()
                && let Some(image) = bg.image.clone()
            {
                let entry = (image, bg.fit, bg.blur);
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
        out
    }

    /// A node's style, with any data-supplied colour substituted in.
    ///
    /// The tint marker says "use that colour here"; it is not a value. A later
    /// rule writing a literal colour clears it, or a rule branching on `when`
    /// would silently stop working.
    ///
    /// A node without a colour changes nothing, which is what makes "the
    /// colour if there is one, the default otherwise" expressible.
    pub fn style_for_tinted(&self, node: NodeId, ctx: &MatchContext, tint: Option<Color>) -> Style {
        let mut out = Style::default();
        let Some(indices) = self.index.get(&node) else {
            return out;
        };

        let mut tinted = crate::parse::Tinted::default();
        for &i in indices {
            let rule = &self.rules[i as usize];
            if !rule.when.matches(ctx) {
                continue;
            }
            tinted.color = rule.tinted.color || (tinted.color && rule.style.color.is_none());
            tinted.background =
                rule.tinted.background || (tinted.background && rule.style.background.is_none());
            tinted.border_color = rule.tinted.border_color
                || (tinted.border_color && rule.style.border_color.is_none());
            out.overlay(&rule.style);
        }

        if let Some(t) = tint {
            if tinted.color {
                out.color = Some(t);
            }
            if tinted.border_color {
                out.border_color = Some(t);
            }
            if tinted.background {
                out.background = Some(crate::value::Background::solid(t));
            }
        }
        out
    }

    /// Whether any rule selects this ID, so unaffected nodes can be skipped
    /// early.
    pub fn affects(&self, node: NodeId) -> bool {
        self.index.contains_key(&node)
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_uitree::{State, StateSet};

    /// Wraps a body with a minimal manifest.
    fn wrap(body: &str) -> String {
        format!(
            r#"{{
              "manifest": {{ "id": "com.example.t", "name": "T", "version": "1.0.0", "abi": 1 }},
              {body}
            }}"#
        )
    }

    fn parse_ok(src: &str) -> (Theme, Vec<Diagnostic>) {
        let r = Theme::parse(src);
        let diags = r.diagnostics.clone();
        let theme = r
            .theme
            .unwrap_or_else(|| panic!("適用されるはずだった: {diags:?}"));
        (theme, diags)
    }

    fn ctx() -> MatchContext {
        MatchContext {
            states: StateSet::EMPTY,
            slot: None,
            platform: Platform::Windows,
            color_scheme: ColorScheme::Dark,
            window_width: 1280.0,
        }
    }

    fn solid(hex: &str) -> Option<Background> {
        Some(Background::solid(Color::parse(hex).unwrap()))
    }

    // ------------------------------------------------ Discarding the theme

    #[test]
    fn broken_json_rejects_the_whole_theme() {
        let r = Theme::parse("{ not json");
        assert!(!r.is_applied());
        assert_eq!(r.errors().count(), 1);
        // A syntax error carries a line number.
        assert!(r.diagnostics[0].path.contains('行'));
    }

    #[test]
    fn missing_manifest_rejects_the_whole_theme() {
        let r = Theme::parse(r#"{ "rules": [] }"#);
        assert!(!r.is_applied());
        assert_eq!(r.diagnostics[0].ignored, Ignored::Theme);
    }

    #[test]
    fn invalid_manifest_fields_reject_the_whole_theme() {
        let cases = [
            r#"{ "manifest": { "name": "T", "version": "1.0.0", "abi": 1 } }"#,
            r#"{ "manifest": { "id": "nodots", "name": "T", "version": "1.0.0", "abi": 1 } }"#,
            r#"{ "manifest": { "id": "com.e.t", "name": "T", "version": "1.0", "abi": 1 } }"#,
            r#"{ "manifest": { "id": "com.e.t", "name": "T", "version": "1.0.0" } }"#,
            r#"{ "manifest": { "id": "com.e.t", "name": "T", "version": "1.0.0", "abi": 0 } }"#,
        ];
        for src in cases {
            assert!(!Theme::parse(src).is_applied(), "適用してはならない: {src}");
        }
    }

    /// These, by contrast, are accepted.
    #[test]
    fn valid_manifest_variants() {
        for version in ["1.0.0", "0.0.1", "10.20.30", "1.0.0-beta.1"] {
            let src = format!(
                r#"{{ "manifest": {{ "id": "com.example.t", "name": "T",
                     "version": "{version}", "abi": 1 }} }}"#
            );
            assert!(Theme::parse(&src).is_applied(), "{version}");
        }
    }

    // ------------------------------------------------ K1

    /// Later rules win.
    #[test]
    fn later_rule_wins() {
        let (theme, _) = parse_ok(&wrap(
            r##""rules": [
                { "select": "chat.message", "style": { "background": "#111111" } },
                { "select": "chat.message", "style": { "background": "#222222" } }
            ]"##,
        ));
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.background, solid("#222222"));
    }

    /// Overriding is per property.
    #[test]
    fn override_is_per_property() {
        let (theme, _) = parse_ok(&wrap(
            r##""rules": [
                { "select": "chat.message", "style": { "background": "#111111", "radius": 8 } },
                { "select": "chat.message", "style": { "background": "#222222" } }
            ]"##,
        ));
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.background, solid("#222222"));
        assert_eq!(s.radius, Some(8.0));
    }

    /// Reversing the order stops hover taking effect.
    #[test]
    fn order_matters_even_for_conditional_rules() {
        let hover_last = wrap(
            r##""rules": [
                { "select": "chat.message", "style": { "background": "#111111" } },
                { "select": "chat.message", "when": { "state": "hover" },
                  "style": { "background": "#222222" } }
            ]"##,
        );
        let hover_first = wrap(
            r##""rules": [
                { "select": "chat.message", "when": { "state": "hover" },
                  "style": { "background": "#222222" } },
                { "select": "chat.message", "style": { "background": "#111111" } }
            ]"##,
        );
        let hovered = ctx().with_state(State::Hover);

        let (a, _) = parse_ok(&hover_last);
        assert_eq!(
            a.style_for(NodeId::ChatMessage, &hovered).background,
            solid("#222222"),
            "後に書いた hover は効く"
        );

        let (b, _) = parse_ok(&hover_first);
        assert_eq!(
            b.style_for(NodeId::ChatMessage, &hovered).background,
            solid("#111111"),
            "先に書いた hover は後のルールに上書きされる"
        );
    }

    #[test]
    fn unmatched_condition_does_not_apply() {
        let (theme, _) = parse_ok(&wrap(
            r#""rules": [
                { "select": "chat.message", "when": { "state": "hover" },
                  "style": { "radius": 8 } }
            ]"#,
        ));
        assert_eq!(theme.style_for(NodeId::ChatMessage, &ctx()).radius, None);
        assert_eq!(
            theme
                .style_for(NodeId::ChatMessage, &ctx().with_state(State::Hover))
                .radius,
            Some(8.0)
        );
    }

    #[test]
    fn no_rules_yields_empty_style() {
        let (theme, _) = parse_ok(&wrap(r#""rules": []"#));
        assert!(theme.style_for(NodeId::ChatMessage, &ctx()).is_empty());
        assert!(!theme.affects(NodeId::ChatMessage));
    }

    // ------------------------------------------------ Granularity

    /// An unknown stable ID drops the rule, with a warning.
    #[test]
    fn unknown_node_id_is_a_warning_and_skips_the_rule() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [
                { "select": "future.node", "style": { "radius": 4 } },
                { "select": "chat.message", "style": { "radius": 8 } }
            ]"#,
        ));
        assert_eq!(theme.rules().len(), 1, "既知のルールは適用される");
        assert_eq!(
            theme.style_for(NodeId::ChatMessage, &ctx()).radius,
            Some(8.0)
        );
        let warnings: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].ignored, Ignored::Rule);
    }

    /// An unknown property drops the property, with a warning.
    #[test]
    fn unknown_property_is_a_warning_and_skips_only_the_property() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [
                { "select": "chat.message", "style": { "borderRadius": 4, "radius": 8 } }
            ]"#,
        ));
        assert_eq!(
            theme.style_for(NodeId::ChatMessage, &ctx()).radius,
            Some(8.0),
            "既知のプロパティは適用される"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.ignored == Ignored::Property)
        );
    }

    /// An unknown `when` key drops the rule, with a warning.
    #[test]
    fn unknown_when_key_is_a_warning_and_skips_the_rule() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [
                { "select": "chat.message", "when": { "phase": "moon" },
                  "style": { "radius": 4 } }
            ]"#,
        ));
        assert_eq!(theme.rules().len(), 0);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.ignored == Ignored::Rule)
        );
    }

    /// An unknown state may be one added later, so it warns.
    #[test]
    fn unknown_state_name_is_a_warning() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [
                { "select": "chat.message", "when": { "state": "levitating" },
                  "style": { "radius": 4 } }
            ]"#,
        ));
        assert_eq!(theme.rules().len(), 0);
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    /// An undefined token drops the property, as an error.
    #[test]
    fn undefined_token_is_an_error_and_skips_only_the_property() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [
                { "select": "chat.message", "style": { "color": "$nope", "radius": 8 } }
            ]"#,
        ));
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.color, None);
        assert_eq!(s.radius, Some(8.0));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.ignored == Ignored::Property)
        );
    }

    /// A wrong value type drops the property, as an error.
    #[test]
    fn wrong_type_is_an_error_and_skips_only_the_property() {
        let (theme, diags) = parse_ok(&wrap(
            r##""rules": [
                { "select": "chat.message", "style": { "radius": "#111111", "gap": 4 } }
            ]"##,
        ));
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.radius, None);
        assert_eq!(s.gap, Some(4.0));
        assert!(diags.iter().any(|d| d.severity == Severity::Error));
    }

    /// A rule using a cyclic token loses only that property.
    #[test]
    fn cyclic_token_only_kills_the_referring_property() {
        let (theme, _) = parse_ok(&wrap(
            r#""tokens": { "a": "$b", "b": "$a", "ok": 8 },
               "rules": [
                 { "select": "chat.message", "style": { "color": "$a", "radius": "$ok" } }
               ]"#,
        ));
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.color, None);
        assert_eq!(s.radius, Some(8.0));
    }

    /// A rule with every property dropped is not kept.
    #[test]
    fn rule_with_nothing_left_is_dropped() {
        let (theme, _) = parse_ok(&wrap(
            r#""rules": [{ "select": "chat.message", "style": { "color": "$missing" } }]"#,
        ));
        assert_eq!(theme.rules().len(), 0);
    }

    // ------------------------------------------------ Tokens

    #[test]
    fn token_reference_resolves_in_styles() {
        let (theme, diags) = parse_ok(&wrap(
            r##""tokens": {
                 "color.brand": "#7c6cf0", "color.accent": "$color.brand", "radius.md": 8
               },
               "rules": [
                 { "select": "chat.message",
                   "style": { "color": "$color.accent", "radius": "$radius.md" } }
               ]"##,
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.color, Color::parse("#7c6cf0"));
        assert_eq!(s.radius, Some(8.0));
    }

    /// An object token is typed at the point of use.
    #[test]
    fn object_token_is_typed_by_use_site() {
        let (theme, diags) = parse_ok(&wrap(
            r#""tokens": { "font.title": { "family": "Inter", "size": 16, "weight": 600 } },
               "rules": [
                 { "select": "chat.message.header.author", "style": { "font": "$font.title" } }
               ]"#,
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let f = theme
            .style_for(NodeId::ChatMessageHeaderAuthor, &ctx())
            .font
            .expect("フォントが解決される");
        assert_eq!(f.family.as_deref(), Some("Inter"));
        assert_eq!(f.size, Some(16.0));
        assert_eq!(f.weight, Some(600));
    }

    /// Using the same object as another type fails there.
    #[test]
    fn object_token_used_as_wrong_type_fails_at_use_site() {
        let (theme, diags) = parse_ok(&wrap(
            r#""tokens": { "font.title": { "family": "Inter", "size": 16 } },
               "rules": [
                 { "select": "chat.message", "style": { "shadow": "$font.title", "radius": 4 } }
               ]"#,
        ));
        let s = theme.style_for(NodeId::ChatMessage, &ctx());
        assert_eq!(s.shadow, None, "影として読めないので落ちる");
        assert_eq!(s.radius, Some(4.0), "他のプロパティは生きる");
        assert!(diags.iter().any(|d| d.severity == Severity::Error));
    }

    // ------------------------------------------------ Backgrounds

    #[test]
    fn background_shorthand_is_a_color() {
        let (theme, _) = parse_ok(&wrap(
            r##""rules": [{ "select": "app.window", "style": { "background": "#0f0f17" } }]"##,
        ));
        let bg = theme
            .style_for(NodeId::AppWindow, &ctx())
            .background
            .expect("背景がある");
        assert_eq!(bg.color, Color::parse("#0f0f17"));
        assert!(!bg.has_image());
        assert_eq!(bg.fit, Fit::Cover, "既定値");
    }

    #[test]
    fn background_object_full() {
        let (theme, diags) = parse_ok(&wrap(
            r##""rules": [{
                "select": "app.window",
                "style": { "background": {
                    "color": "#0f0f17",
                    "image": "assets/wallpaper.png",
                    "fit": "contain",
                    "position": [0.5, 0.35],
                    "opacity": 0.8,
                    "blur": 12,
                    "tint": "#0f0f1766"
                } }
            }]"##,
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let bg = theme
            .style_for(NodeId::AppWindow, &ctx())
            .background
            .unwrap();
        assert_eq!(
            bg.image,
            Some(AssetRef::Bundled("assets/wallpaper.png".into()))
        );
        assert_eq!(bg.fit, Fit::Contain);
        assert_eq!(bg.position, [0.5, 0.35]);
        assert_eq!(bg.opacity, 0.8);
        assert_eq!(bg.blur, 12.0);
        assert_eq!(bg.tint, Color::parse("#0f0f1766"));
    }

    /// An unreadable image keeps the colour and the theme.
    #[test]
    fn bad_image_falls_back_to_color() {
        let (theme, diags) = parse_ok(&wrap(
            r##""rules": [{
                "select": "app.window",
                "style": { "background": { "color": "#0f0f17", "image": "../secret.png" } }
            }]"##,
        ));
        let bg = theme
            .style_for(NodeId::AppWindow, &ctx())
            .background
            .expect("背景そのものは残る");
        assert_eq!(bg.image, None);
        assert_eq!(bg.color, Color::parse("#0f0f17"), "色にフォールバックする");
        assert!(diags.iter().any(|d| d.severity == Severity::Error));
    }

    /// An undeclared host is never contacted.
    #[test]
    fn remote_image_requires_declaration() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [{
                "select": "app.window",
                "style": { "background": { "image": "https://cdn.example.com/bg.png" } }
            }]"#,
        ));
        // The image is dropped; the theme and the rule survive, falling back
        // to the background colour.
        let bg = theme
            .style_for(NodeId::AppWindow, &ctx())
            .background
            .expect("背景そのものは残る");
        assert!(!bg.has_image(), "宣言のないホストの画像は読み込まれない");
        assert_eq!(bg.color, None);
        assert!(diags.iter().any(|d| d.message.contains("SEC-022")));

        let declared = r#"{
          "manifest": {
            "id": "com.example.t", "name": "T", "version": "1.0.0", "abi": 1,
            "remoteAssets": ["cdn.example.com"]
          },
          "rules": [{
            "select": "app.window",
            "style": { "background": { "image": "https://cdn.example.com/bg.png" } }
          }]
        }"#;
        let (theme, diags) = parse_ok(declared);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            theme
                .style_for(NodeId::AppWindow, &ctx())
                .background
                .unwrap()
                .has_image()
        );
    }

    /// A whole background object can be a token.
    #[test]
    fn background_can_come_from_a_token() {
        let (theme, diags) = parse_ok(&wrap(
            r#""tokens": { "bg.main": { "image": "assets/w.png", "fit": "tile" } },
               "rules": [{ "select": "app.window", "style": { "background": "$bg.main" } }]"#,
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let bg = theme
            .style_for(NodeId::AppWindow, &ctx())
            .background
            .unwrap();
        assert_eq!(bg.fit, Fit::Tile);
        assert!(bg.has_image());
    }

    // ------------------------------------------------ ABI

    #[test]
    fn newer_abi_warns_but_still_applies() {
        let src = r#"{
          "manifest": { "id": "com.example.t", "name": "T", "version": "1.0.0", "abi": 99 },
          "rules": [{ "select": "chat.message", "style": { "radius": 8 } }]
        }"#;
        let (theme, diags) = parse_ok(src);
        assert_eq!(
            theme.style_for(NodeId::ChatMessage, &ctx()).radius,
            Some(8.0),
            "既知のルールは適用する"
        );
        assert!(diags.iter().any(|d| d.path == "manifest.abi"));
    }

    // ------------------------------------------------ Sample themes

    /// The samples must parse without a single diagnostic. The schema check is
    /// a separate implementation, so a disagreement shows up at once.
    #[test]
    fn official_sample_midnight() {
        let src = include_str!("../../../examples/themes/midnight/theme.json");
        let r = Theme::parse(src);
        let theme = r.theme.expect("公式サンプルが適用できない");
        assert!(
            r.diagnostics.is_empty(),
            "公式サンプルに診断が出た: {:?}",
            r.diagnostics
        );
        assert!(theme.rules().len() >= 40, "{}", theme.rules().len());
        assert!(theme.affects(NodeId::AppWindow));
    }

    #[test]
    fn official_sample_wallpaper() {
        let src = include_str!("../../../examples/themes/wallpaper/theme.json");
        let r = Theme::parse(src);
        let theme = r.theme.expect("公式サンプルが適用できない");
        assert!(
            r.diagnostics.is_empty(),
            "公式サンプルに診断が出た: {:?}",
            r.diagnostics
        );
        let bg = theme
            .style_for(NodeId::AppWindow, &ctx())
            .background
            .expect("壁紙テーマなので app.window に背景がある");
        assert!(bg.has_image(), "背景画像が読めていない");
    }

    /// A node with no background stays transparent.
    #[test]
    fn nodes_without_background_stay_transparent() {
        let src = include_str!("../../../examples/themes/wallpaper/theme.json");
        let theme = Theme::parse(src).theme.unwrap();
        assert!(
            theme
                .style_for(NodeId::ChatMessageList, &ctx())
                .background
                .is_none(),
            "chat.message_list に背景を置くと app.window の画像が透けなくなる"
        );
    }

    /// The fetcher sees every background image once, with fit and blur.
    #[test]
    fn background_images_are_collected_once_each() {
        let (theme, _) = parse_ok(&wrap(
            r##" "rules": [
              { "select": "app.window", "style": { "background": { "image": "assets/a.png" } } },
              { "select": "app.window", "style": { "background": { "image": "assets/a.png", "fit": "contain", "blur": 4 } } },
              { "select": "chat.header", "style": { "background": "#111" } }
            ]"##,
        ));
        let got = theme.background_images();
        assert_eq!(got.len(), 2);
        assert!(matches!(
            got[0].0,
            gumicord_uitree::value::AssetRef::Bundled(_)
        ));
        assert_eq!(got[1].2, 4.0);
    }
}
