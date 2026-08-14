//! テーマの解決。
//!
//! 責務: JSON のパース / 検証 / トークン解決 / セレクタ照合 / スタイル確定。
//!
//! カスケード規則は 1 つだけである — **記述順に適用し、後のルールが前のルールを
//! プロパティ単位で上書きする**。CSS の詳細度は採用しない。テーマ作者が
//! 「なぜこのルールが効かないのか」を上から読んで必ず分かる状態を優先する。
//!
//! 検証に失敗しても**テーマ全体を捨てない**。誤りのあるルールやプロパティ
//! だけを無視して残りを適用する (`EXT-016`)。
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
//! let theme = loaded.theme.expect("適用できる");
//! let style = theme.style_for(NodeId::AppWindow, &MatchContext::new(1280.0));
//! assert!(style.background.is_some());
//! ```
//!
//! 要件: `EXT-010`〜`EXT-027`
//! 仕様: [`spec/04-theme.md`], [`spec/schema/theme.schema.json`]

pub mod cond;
pub mod diag;
mod parse;
pub mod style;
pub mod token;
pub mod value;

use std::collections::HashMap;

use serde_json::Value;

use gumicord_uitree::NodeId;

pub use crate::cond::{ColorScheme, MatchContext, Platform, PlatformSel, When};
pub use crate::diag::{Diagnostic, Diagnostics, Ignored, Severity};
pub use crate::parse::{Manifest, Rule};
pub use crate::style::Style;
pub use crate::token::{TokenValue, Tokens};
pub use crate::value::{AssetKind, AssetRef, Background, Color, Edges, Fit, Font, Shadow};

/// このクライアントが解釈できる UITree の ABI メジャーバージョン。
///
/// テーマの `manifest.abi` がこれより大きい場合、「このテーマは新しすぎる」と
/// 利用者に伝えた上で**既知のルールのみを適用する** ([`spec/04-theme.md`] 2.1)。
pub const CLIENT_ABI: u32 = 1;

/// 適用可能なテーマ。
///
/// セレクタは安定 ID の**完全一致のみ**である。ワイルドカードを持たないのは、
/// 安定 ID の追加が既存テーマの見た目を変えないようにするためである
/// (`03-uitree.md` の C3 と両立させる)。
#[derive(Debug, Clone)]
pub struct Theme {
    pub manifest: Manifest,
    rules: Vec<Rule>,
    /// 安定 ID から、その ID を選ぶルールの添字へ (記述順)
    ///
    /// ノードごとに全ルールを走査すると O(ノード数 × ルール数) になる。
    /// テーマは 1 度読めば変わらないので、読んだときに索引化しておく。
    index: HashMap<NodeId, Vec<u32>>,
}

/// [`Theme::parse`] の結果。
///
/// **`Result` ではない。** テーマが適用できたかどうかと、何が無視されたかは
/// 別の情報である。ルールが 3 本無視されてもテーマは適用される (`EXT-016`)。
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// 適用できるテーマ。`None` ならテーマ全体が適用されない
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
    /// テーマの JSON を読む。
    ///
    /// テーマ全体が捨てられるのは次の 2 つだけである ([`spec/04-theme.md`] 7 章)。
    ///
    /// - JSON として壊れている
    /// - `manifest` が欠けている、または不正
    pub fn parse(src: &str) -> ParseResult {
        let mut diags = Diagnostics::new();

        let root: Value = match serde_json::from_str(src) {
            Ok(v) => v,
            Err(e) => {
                // 構文誤りに限り、行番号を伝えられる
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

    /// 1 ノードのスタイルを確定する。
    ///
    /// **規則 K1 の実装そのものである。** 記述順に走査し、条件が成立した
    /// ルールをプロパティ単位で重ねていく。詳細度は計算しない。
    pub fn style_for(&self, node: NodeId, ctx: &MatchContext) -> Style {
        let mut out = Style::default();
        let Some(indices) = self.index.get(&node) else {
            return out;
        };
        for &i in indices {
            let rule = &self.rules[i as usize];
            if rule.when.matches(ctx) {
                out.overlay(&rule.style);
            }
        }
        out
    }

    /// このテーマが何らかのルールを持つ安定 ID か。
    ///
    /// レンダラが「テーマの影響を受けないノード」を早期に判定するために使う。
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

    /// manifest だけを補って、本体を包む
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
            platform: Platform::Windows,
            color_scheme: ColorScheme::Dark,
            window_width: 1280.0,
        }
    }

    fn solid(hex: &str) -> Option<Background> {
        Some(Background::solid(Color::parse(hex).unwrap()))
    }

    // ------------------------------------------------ テーマ全体を捨てる 2 つの場合

    #[test]
    fn broken_json_rejects_the_whole_theme() {
        let r = Theme::parse("{ not json");
        assert!(!r.is_applied());
        assert_eq!(r.errors().count(), 1);
        // 構文誤りなら行番号を伝えられる
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

    /// 逆に、これらは通る
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

    /// K1: 後のルールが勝つ
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

    /// 5.1: 上書きの単位はプロパティ
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

    /// 5 章の例: 順序を逆にすると hover が効かなくなる
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

    // ------------------------------------------------ EXT-016 の粒度

    /// 未知の安定 ID → **ルール**を無視し、**警告**
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

    /// 未知のプロパティ → **プロパティ**を無視し、**警告**
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

    /// 未知の when のキー → **ルール**を無視し、**警告**
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

    /// 未知の状態名も「将来追加された状態」でありうるので警告
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

    /// 未定義のトークン参照 → **プロパティ**を無視し、**エラー**
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

    /// 値の型が違う → **プロパティ**を無視し、**エラー**
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

    /// 循環参照したトークンを参照するルールは、そのプロパティだけ落ちる
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

    /// すべてのプロパティが落ちたルールは残さない
    #[test]
    fn rule_with_nothing_left_is_dropped() {
        let (theme, _) = parse_ok(&wrap(
            r#""rules": [{ "select": "chat.message", "style": { "color": "$missing" } }]"#,
        ));
        assert_eq!(theme.rules().len(), 0);
    }

    // ------------------------------------------------ トークン

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

    /// オブジェクトのトークンは使用箇所で型が決まる
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

    /// 同じオブジェクトを別の型として使うと、そこで型が合わずに落ちる
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

    // ------------------------------------------------ 背景 (EXT-021〜027)

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

    /// EXT-027: 画像が読めなくても色は残り、テーマ全体は生きる
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

    /// SEC-022: 宣言のないホストへは到達しない
    #[test]
    fn remote_image_requires_declaration() {
        let (theme, diags) = parse_ok(&wrap(
            r#""rules": [{
                "select": "app.window",
                "style": { "background": { "image": "https://cdn.example.com/bg.png" } }
            }]"#,
        ));
        // EXT-027: 画像は落ちるが、テーマもルールも生きたまま
        // background.color (ここでは未指定 = 透明) にフォールバックする
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

    /// 背景オブジェクトそのものをトークンにできる
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

    // ------------------------------------------------ 公式サンプル

    /// 公式サンプルは診断ゼロで通らなければならない。
    ///
    /// スキーマ検証 (`cargo xtask schema`) とこの試験は**別々の実装**である。
    /// 食い違えばどちらかが間違っているとすぐ分かる。
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

    /// 6.2 の要点: 背景色を指定しないノードは透明のまま残る
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
}
