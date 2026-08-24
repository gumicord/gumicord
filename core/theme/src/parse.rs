//! Reads a theme from JSON.
//!
//! The JSON Schema is a validator for theme authors and answers only whether
//! the whole document is valid. Runtime parsing needs to ignore one offending
//! property and keep the rest, so it is written out here instead. CI runs the
//! schema against the sample themes, so the two disagreeing shows up there.

use serde_json::{Map, Value};

use gumicord_uitree::{Decoration, NodeId, State, StateSet};

use crate::cond::{ColorScheme, PlatformSel, When};
use crate::diag::{Diagnostics, Ignored};
use crate::style::Style;
use crate::token::{TokenValue, Tokens};
use crate::value::{AssetKind, AssetRef, Background, Color, Edges, Fit, Font, Shadow};

/// A theme's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The UITree ABI version it expects.
    pub abi: u32,
    pub author: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    /// Hosts external assets may come from.
    pub remote_assets: Vec<String>,
}

/// One rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub select: NodeId,
    pub when: When,
    pub style: Style,
    /// Properties painted with a data-supplied colour.
    pub tinted: Tinted,
}

/// Properties painted with the colour the data carries.
///
/// The value is not decided in the theme: role and folder colours differ per
/// node, so a theme can only say where to use one.
///
/// A node without a colour changes nothing, leaving whatever the previous
/// rule wrote, which is what makes "the colour if there is one, the default
/// otherwise" expressible:
///
/// ```json
/// { "select": "nav.member_list.item.name", "style": { "color": "$color.text.secondary" } },
/// { "select": "nav.member_list.item.name", "style": { "color": "$data.tint" } }
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tinted {
    pub color: bool,
    pub background: bool,
    pub border_color: bool,
}

/// A reference to a data-supplied colour.
///
/// Not a token: a token looks up the theme's own table, while this has no
/// table and arrives on the node.
const DATA_TINT: &str = "$data.tint";

fn is_data_tint(v: &Value) -> bool {
    v.as_str() == Some(DATA_TINT)
}

/// The token table and declared hosts, needed throughout parsing.
#[derive(Clone, Copy)]
struct Env<'a> {
    tokens: &'a Tokens,
    hosts: &'a [String],
}

/// A `$` reference resolved by one level.
enum Resolved<'a> {
    Color(Color),
    Number(f32),
    Object(&'a Map<String, Value>),
    Array(&'a [Value]),
    Str(&'a str),
    Bool(bool),
}

impl Resolved<'_> {
    const fn kind_name(&self) -> &'static str {
        match self {
            Self::Color(_) => "色",
            Self::Number(_) => "数値",
            Self::Object(_) => "オブジェクト",
            Self::Array(_) => "配列",
            Self::Str(_) => "文字列",
            Self::Bool(_) => "真偽値",
        }
    }
}

/// Resolves a `$name` against the tokens, passing anything else through.
/// Undefined tokens become a diagnostic here rather than at every use.
fn resolve<'a>(
    v: &'a Value,
    env: Env<'a>,
    path: &str,
    diags: &mut Diagnostics,
) -> Option<Resolved<'a>> {
    if let Value::String(s) = v
        && let Some(name) = s.strip_prefix('$')
    {
        return match env.tokens.get(name) {
            Some(TokenValue::Color(c)) => Some(Resolved::Color(*c)),
            Some(TokenValue::Length(n)) => Some(Resolved::Number(*n)),
            Some(TokenValue::Object(o)) => Some(Resolved::Object(o)),
            None => {
                diags.error(
                    path,
                    Ignored::Property,
                    format!("未定義のトークン ${name} を参照している"),
                );
                None
            }
        };
    }

    match v {
        Value::String(s) => Some(Resolved::Str(s)),
        Value::Number(n) => {
            let f = n.as_f64()? as f32;
            if f.is_finite() {
                Some(Resolved::Number(f))
            } else {
                diags.error(path, Ignored::Property, "数値が有限でない");
                None
            }
        }
        Value::Object(o) => Some(Resolved::Object(o)),
        Value::Array(a) => Some(Resolved::Array(a)),
        Value::Bool(b) => Some(Resolved::Bool(*b)),
        Value::Null => {
            diags.error(path, Ignored::Property, "null は値として使えない");
            None
        }
    }
}

fn type_error(path: &str, diags: &mut Diagnostics, want: &str, got: &Resolved<'_>) {
    diags.error(
        path,
        Ignored::Property,
        format!("{want} が必要だが {} が書かれている", got.kind_name()),
    );
}

// ------------------------------------------------------------------ Values

fn color(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<Color> {
    match resolve(v, env, path, diags)? {
        Resolved::Color(c) => Some(c),
        Resolved::Str(s) => match Color::parse(s) {
            Some(c) => Some(c),
            None => {
                diags.error(
                    path,
                    Ignored::Property,
                    format!("色の書式が不正: {s} (#RGB / #RRGGBB / #RRGGBBAA)"),
                );
                None
            }
        },
        other => {
            type_error(path, diags, "色", &other);
            None
        }
    }
}

/// A length; negatives are rejected.
fn length(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<f32> {
    match resolve(v, env, path, diags)? {
        Resolved::Number(n) if n >= 0.0 => Some(n),
        Resolved::Number(n) => {
            diags.error(path, Ignored::Property, format!("長さが負: {n}"));
            None
        }
        other => {
            type_error(path, diags, "長さ (数値)", &other);
            None
        }
    }
}

/// A number within 0 to 1.
fn ratio(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<f32> {
    match resolve(v, env, path, diags)? {
        Resolved::Number(n) if (0.0..=1.0).contains(&n) => Some(n),
        Resolved::Number(n) => {
            diags.error(path, Ignored::Property, format!("0.0〜1.0 の範囲外: {n}"));
            None
        }
        other => {
            type_error(path, diags, "0.0〜1.0 の数値", &other);
            None
        }
    }
}

fn edges(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<Edges> {
    match resolve(v, env, path, diags)? {
        Resolved::Number(n) if n >= 0.0 => Some(Edges::all(n)),
        Resolved::Number(n) => {
            diags.error(path, Ignored::Property, format!("長さが負: {n}"));
            None
        }
        Resolved::Array(a) => {
            if a.len() != 4 {
                diags.error(
                    path,
                    Ignored::Property,
                    format!("[上, 右, 下, 左] の 4 要素が必要 (実際は {} 要素)", a.len()),
                );
                return None;
            }
            let mut out = [0.0f32; 4];
            for (i, item) in a.iter().enumerate() {
                out[i] = length(item, env, &format!("{path}[{i}]"), diags)?;
            }
            Some(Edges {
                top: out[0],
                right: out[1],
                bottom: out[2],
                left: out[3],
            })
        }
        other => {
            type_error(path, diags, "長さ、または 4 要素の配列", &other);
            None
        }
    }
}

fn font(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<Font> {
    let obj = match resolve(v, env, path, diags)? {
        Resolved::Object(o) => o,
        other => {
            type_error(path, diags, "フォント (オブジェクト)", &other);
            return None;
        }
    };

    let mut f = Font::default();
    for (key, value) in obj {
        let p = format!("{path}.{key}");
        match key.as_str() {
            "family" => match resolve(value, env, &p, diags) {
                Some(Resolved::Str(s)) => f.family = Some(s.to_owned()),
                Some(other) => type_error(&p, diags, "文字列", &other),
                None => {}
            },
            "size" => f.size = positive(value, env, &p, diags),
            "lineHeight" => f.line_height = positive(value, env, &p, diags),
            "weight" => f.weight = weight(value, env, &p, diags),
            "italic" => match resolve(value, env, &p, diags) {
                Some(Resolved::Bool(b)) => f.italic = Some(b),
                Some(other) => type_error(&p, diags, "真偽値", &other),
                None => {}
            },
            "letterSpacing" => match resolve(value, env, &p, diags) {
                // Letter spacing is the one place a negative means something.
                Some(Resolved::Number(n)) => f.letter_spacing = Some(n),
                Some(other) => type_error(&p, diags, "数値", &other),
                None => {}
            },
            _ => diags.warn(
                &p,
                Ignored::Property,
                format!("フォントの未知のキー {key} を無視した"),
            ),
        }
    }
    Some(f)
}

fn positive(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<f32> {
    match resolve(v, env, path, diags)? {
        Resolved::Number(n) if n > 0.0 => Some(n),
        Resolved::Number(n) => {
            diags.error(
                path,
                Ignored::Property,
                format!("0 より大きい必要がある: {n}"),
            );
            None
        }
        other => {
            type_error(path, diags, "数値", &other);
            None
        }
    }
}

fn weight(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<u16> {
    match resolve(v, env, path, diags)? {
        Resolved::Number(n) if (100.0..=900.0).contains(&n) => Some(n as u16),
        Resolved::Number(n) => {
            diags.error(path, Ignored::Property, format!("100〜900 の範囲外: {n}"));
            None
        }
        other => {
            type_error(path, diags, "数値", &other);
            None
        }
    }
}

fn shadow(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<Shadow> {
    let obj = match resolve(v, env, path, diags)? {
        Resolved::Object(o) => o,
        other => {
            type_error(path, diags, "影 (オブジェクト)", &other);
            return None;
        }
    };

    let mut s = Shadow::default();
    let mut has_color = false;
    for (key, value) in obj {
        let p = format!("{path}.{key}");
        match key.as_str() {
            // The offset may be negative.
            "x" | "y" | "spread" => match resolve(value, env, &p, diags) {
                Some(Resolved::Number(n)) => match key.as_str() {
                    "x" => s.x = n,
                    "y" => s.y = n,
                    _ => s.spread = n,
                },
                Some(other) => type_error(&p, diags, "数値", &other),
                None => {}
            },
            "blur" => {
                if let Some(n) = length(value, env, &p, diags) {
                    s.blur = n;
                }
            }
            "color" => {
                if let Some(c) = color(value, env, &p, diags) {
                    s.color = c;
                    has_color = true;
                }
            }
            _ => diags.warn(
                &p,
                Ignored::Property,
                format!("影の未知のキー {key} を無視した"),
            ),
        }
    }

    if !has_color {
        diags.error(path, Ignored::Property, "影には color が必要");
        return None;
    }
    Some(s)
}

/// A background, accepting both the colour shorthand and the object form.
fn background(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<Background> {
    let obj = match resolve(v, env, path, diags)? {
        // "background": "#111" / "$color.panel"
        Resolved::Color(c) => return Some(Background::solid(c)),
        Resolved::Str(s) => {
            return match Color::parse(s) {
                Some(c) => Some(Background::solid(c)),
                None => {
                    diags.error(
                        path,
                        Ignored::Property,
                        format!("色の書式が不正: {s}。画像を指定するにはオブジェクトを使う"),
                    );
                    None
                }
            };
        }
        Resolved::Object(o) => o,
        other => {
            type_error(path, diags, "色、または背景オブジェクト", &other);
            return None;
        }
    };

    let mut bg = Background::default();
    for (key, value) in obj {
        let p = format!("{path}.{key}");
        match key.as_str() {
            "color" => bg.color = color(value, env, &p, diags),
            "tint" => bg.tint = color(value, env, &p, diags),
            "image" => bg.image = asset(value, env, &p, diags),
            "fit" => match resolve(value, env, &p, diags) {
                Some(Resolved::Str(s)) => match Fit::parse(s) {
                    Some(f) => bg.fit = f,
                    None => diags.warn(
                        &p,
                        Ignored::Property,
                        format!(
                            "未知の fit {s} を無視した (cover / contain / stretch / tile / none)"
                        ),
                    ),
                },
                Some(other) => type_error(&p, diags, "文字列", &other),
                None => {}
            },
            "position" => {
                if let Some(pos) = position(value, env, &p, diags) {
                    bg.position = pos;
                }
            }
            "opacity" => {
                if let Some(n) = ratio(value, env, &p, diags) {
                    bg.opacity = n;
                }
            }
            "blur" => {
                if let Some(n) = length(value, env, &p, diags) {
                    // Applied once at load time; the cap stops an accidental
                    // huge value from exhausting memory.
                    if n > 256.0 {
                        diags.error(&p, Ignored::Property, format!("blur の上限は 256: {n}"));
                    } else {
                        bg.blur = n;
                    }
                }
            }
            _ => diags.warn(
                &p,
                Ignored::Property,
                format!("背景の未知のキー {key} を無視した"),
            ),
        }
    }
    Some(bg)
}

fn position(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<[f32; 2]> {
    let Some(Resolved::Array(a)) = resolve(v, env, path, diags) else {
        diags.error(
            path,
            Ignored::Property,
            "position は [x, y] の配列である必要がある",
        );
        return None;
    };
    if a.len() != 2 {
        diags.error(
            path,
            Ignored::Property,
            format!("position は 2 要素が必要 (実際は {} 要素)", a.len()),
        );
        return None;
    }
    let x = ratio(&a[0], env, &format!("{path}[0]"), diags)?;
    let y = ratio(&a[1], env, &format!("{path}[1]"), diags)?;
    Some([x, y])
}

/// An asset reference. A rejected image becomes `None` and falls back to the
/// background colour; it never invalidates the theme.
fn asset(v: &Value, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Option<AssetRef> {
    let s = match resolve(v, env, path, diags)? {
        Resolved::Str(s) => s,
        other => {
            type_error(path, diags, "アセット参照 (文字列)", &other);
            return None;
        }
    };
    match AssetRef::parse(s, AssetKind::Image, env.hosts) {
        Ok(a) => Some(a),
        Err(e) => {
            diags.error(path, Ignored::Property, e.message());
            None
        }
    }
}

// ------------------------------------------------------------------ style

/// Reads a style object.
///
/// An unknown property warns rather than failing: opening a theme written for
/// a newer client is an ordinary thing to do.
fn style(
    obj: &Map<String, Value>,
    env: Env<'_>,
    path: &str,
    diags: &mut Diagnostics,
) -> (Style, Tinted) {
    let mut s = Style::default();
    let mut t = Tinted::default();
    for (key, v) in obj {
        let p = format!("{path}.{key}");
        match key.as_str() {
            // A colour can be replaced by "use the data's colour"; the value
            // is decided per node.
            "background" if is_data_tint(v) => t.background = true,
            "color" if is_data_tint(v) => t.color = true,
            "borderColor" if is_data_tint(v) => t.border_color = true,

            "background" => s.background = background(v, env, &p, diags),
            "color" => s.color = color(v, env, &p, diags),
            "font" => s.font = font(v, env, &p, diags),
            "borderColor" => s.border_color = color(v, env, &p, diags),
            "borderWidth" => s.border_width = length(v, env, &p, diags),
            "radius" => s.radius = length(v, env, &p, diags),
            "padding" => s.padding = edges(v, env, &p, diags),
            "margin" => s.margin = edges(v, env, &p, diags),
            "gap" => s.gap = length(v, env, &p, diags),
            "width" => s.width = length(v, env, &p, diags),
            "height" => s.height = length(v, env, &p, diags),
            "minWidth" => s.min_width = length(v, env, &p, diags),
            "maxWidth" => s.max_width = length(v, env, &p, diags),
            "minHeight" => s.min_height = length(v, env, &p, diags),
            "maxHeight" => s.max_height = length(v, env, &p, diags),
            "opacity" => s.opacity = ratio(v, env, &p, diags),
            "shadow" => s.shadow = shadow(v, env, &p, diags),
            // How appearance changes, not appearance; draws nothing itself.
            "transition" => s.transition = length(v, env, &p, diags),
            "decoration" => s.decoration = decoration(v, &p, diags),
            _ => diags.warn(
                &p,
                Ignored::Property,
                format!("未知のスタイルプロパティ {key} を無視した"),
            ),
        }
    }
    (s, t)
}

// ------------------------------------------------------------------ when

/// Reads a `when`.
///
/// An unknown key or value drops the whole rule: dropping the condition and
/// applying anyway would style more than intended.
fn when(obj: &Map<String, Value>, path: &str, diags: &mut Diagnostics) -> Option<When> {
    let mut w = When::default();
    for (key, v) in obj {
        let p = format!("{path}.{key}");
        match key.as_str() {
            "state" => w.states = states(v, &p, diags)?,
            "platform" => w.platforms = platforms(v, &p, diags)?,
            "colorScheme" => {
                let scheme = v.as_str().and_then(ColorScheme::parse);
                match scheme {
                    Some(c) => w.color_scheme = Some(c),
                    None => {
                        diags.warn(
                            &p,
                            Ignored::Rule,
                            format!("colorScheme として解釈できない値 {v}。ルールごと無視した"),
                        );
                        return None;
                    }
                }
            }
            "minWidth" => w.min_width = Some(non_negative(v, &p, diags)?),
            "maxWidth" => w.max_width = Some(non_negative(v, &p, diags)?),
            "slot" => match v.as_str() {
                Some(s) => w.slot = Some(s.to_owned()),
                None => {
                    diags.warn(
                        &p,
                        Ignored::Rule,
                        format!("slot は文字列である。{v} を読めない。ルールごと無視した"),
                    );
                    return None;
                }
            },
            _ => {
                diags.warn(
                    &p,
                    Ignored::Rule,
                    format!("未知の when のキー {key}。ルールごと無視した"),
                );
                return None;
            }
        }
    }
    Some(w)
}

/// One or more decoration names, space separated.
///
/// One unknown word drops the property: applying the rest gives a
/// strikethrough without an underline, which hides the typo.
fn decoration(v: &Value, path: &str, diags: &mut Diagnostics) -> Option<Decoration> {
    let Some(text) = v.as_str() else {
        diags.warn(
            path,
            Ignored::Property,
            format!("decoration は文字列である。{v} を読めない"),
        );
        return None;
    };
    let mut d = Decoration::default();
    for word in text.split_whitespace() {
        match word {
            "underline" => d.underline = true,
            "strikethrough" => d.strikethrough = true,
            // Explicitly none, so a previous rule can be cancelled.
            "none" => {}
            _ => {
                diags.warn(
                    path,
                    Ignored::Property,
                    format!(
                        "未知の decoration {word}。underline / strikethrough / none のいずれかである"
                    ),
                );
                return None;
            }
        }
    }
    Some(d)
}

/// A number in a `when`; a negative width means nothing.
fn non_negative(v: &Value, path: &str, diags: &mut Diagnostics) -> Option<f32> {
    match v.as_f64() {
        Some(n) if n >= 0.0 && n.is_finite() => Some(n as f32),
        _ => {
            diags.warn(
                path,
                Ignored::Rule,
                format!("0 以上の数値である必要がある: {v}。ルールごと無視した"),
            );
            None
        }
    }
}

/// A string or an array of them; anything else drops the rule.
fn string_list<'a>(v: &'a Value, path: &str, diags: &mut Diagnostics) -> Option<Vec<&'a str>> {
    match v {
        Value::String(s) => Some(vec![s.as_str()]),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for item in a {
                let Some(s) = item.as_str() else {
                    diags.warn(
                        path,
                        Ignored::Rule,
                        format!("配列に文字列でない要素がある: {item}。ルールごと無視した"),
                    );
                    return None;
                };
                out.push(s);
            }
            Some(out)
        }
        _ => {
            diags.warn(
                path,
                Ignored::Rule,
                "文字列かその配列である必要がある。ルールごと無視した",
            );
            None
        }
    }
}

fn states(v: &Value, path: &str, diags: &mut Diagnostics) -> Option<StateSet> {
    let names = string_list(v, path, diags)?;
    let mut set = StateSet::EMPTY;
    for name in names {
        match parse_state(name) {
            Some(s) => set = set.with(s),
            None => {
                diags.warn(
                    path,
                    Ignored::Rule,
                    format!("未知の状態 {name}。ルールごと無視した"),
                );
                return None;
            }
        }
    }
    Some(set)
}

fn parse_state(name: &str) -> Option<State> {
    State::ALL.iter().copied().find(|s| s.as_str() == name)
}

fn platforms(v: &Value, path: &str, diags: &mut Diagnostics) -> Option<Vec<PlatformSel>> {
    let names = string_list(v, path, diags)?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        match PlatformSel::parse(name) {
            Some(p) => out.push(p),
            None => {
                diags.warn(
                    path,
                    Ignored::Rule,
                    format!("未知の platform {name}。ルールごと無視した"),
                );
                return None;
            }
        }
    }
    Some(out)
}

// ------------------------------------------------------------------ rules / manifest

pub(crate) fn rules(
    arr: &[Value],
    tokens: &Tokens,
    hosts: &[String],
    diags: &mut Diagnostics,
) -> Vec<Rule> {
    let env = Env { tokens, hosts };
    let mut out = Vec::with_capacity(arr.len());

    for (i, item) in arr.iter().enumerate() {
        let path = format!("rules[{i}]");
        let Some(obj) = item.as_object() else {
            diags.error(&path, Ignored::Rule, "ルールはオブジェクトである必要がある");
            continue;
        };

        for key in obj.keys() {
            if !matches!(key.as_str(), "select" | "when" | "style") {
                diags.warn(
                    format!("{path}.{key}"),
                    Ignored::Nothing,
                    format!("ルールの未知のキー {key} を無視した"),
                );
            }
        }

        let Some(select) = obj.get("select").and_then(Value::as_str) else {
            diags.error(&path, Ignored::Rule, "select が必要");
            continue;
        };
        // An unknown ID may be a node added later, so it warns and the rest
        // of the rules still apply.
        let Ok(select) = select.parse::<NodeId>() else {
            diags.warn(
                format!("{path}.select"),
                Ignored::Rule,
                format!("未知の安定 ID {select}。このクライアントには存在しないノードである"),
            );
            continue;
        };

        let cond = match obj.get("when") {
            None => When::default(),
            Some(Value::Object(w)) => match when(w, &format!("{path}.when"), diags) {
                Some(w) => w,
                None => continue,
            },
            Some(_) => {
                diags.error(
                    format!("{path}.when"),
                    Ignored::Rule,
                    "when はオブジェクトである必要がある",
                );
                continue;
            }
        };

        let Some(Value::Object(st)) = obj.get("style") else {
            diags.error(&path, Ignored::Rule, "style (オブジェクト) が必要");
            continue;
        };
        let (st, tinted) = style(st, env, &format!("{path}.style"), diags);

        // Every property may end up ignored. A rule with only a tint marker
        // is not empty: the value simply arrives per node.
        if st.is_empty() && tinted == Tinted::default() {
            diags.warn(
                &path,
                Ignored::Rule,
                "適用できるプロパティが 1 つも残らなかった",
            );
            continue;
        }

        out.push(Rule {
            select,
            when: cond,
            style: st,
            tinted,
        });
    }
    out
}

/// Reads the manifest; a failure here discards the theme.
///
/// It cannot be lenient like the rest, because a theme's identity, version
/// and permitted external access are all decided here.
pub(crate) fn manifest(v: &Value, diags: &mut Diagnostics) -> Option<Manifest> {
    let Some(obj) = v.as_object() else {
        diags.error(
            "manifest",
            Ignored::Theme,
            "manifest はオブジェクトである必要がある",
        );
        return None;
    };

    let required_str = |key: &str, diags: &mut Diagnostics| -> Option<String> {
        match obj.get(key).and_then(Value::as_str) {
            Some(s) if !s.is_empty() => Some(s.to_owned()),
            _ => {
                diags.error(
                    format!("manifest.{key}"),
                    Ignored::Theme,
                    format!("{key} が必要"),
                );
                None
            }
        }
    };

    let id = required_str("id", diags);
    let name = required_str("name", diags);
    let version = required_str("version", diags);

    let abi = match obj.get("abi").and_then(Value::as_u64) {
        Some(n) if n >= 1 => Some(n as u32),
        _ => {
            diags.error(
                "manifest.abi",
                Ignored::Theme,
                "abi (1 以上の整数) が必要。テーマが想定する UITree の ABI である",
            );
            None
        }
    };

    let (id, name, version, abi) = (id?, name?, version?, abi?);

    if !is_reverse_domain(&id) {
        diags.error(
            "manifest.id",
            Ignored::Theme,
            format!("id は逆ドメイン形式である必要がある: {id}"),
        );
        return None;
    }
    if !is_semver(&version) {
        diags.error(
            "manifest.version",
            Ignored::Theme,
            format!("version はセマンティックバージョニングである必要がある: {version}"),
        );
        return None;
    }

    let optional = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_owned);

    let mut remote_assets = Vec::new();
    match obj.get("remoteAssets") {
        None => {}
        Some(Value::Array(a)) => {
            for (i, host) in a.iter().enumerate() {
                match host.as_str() {
                    Some(h) if is_host_name(h) => remote_assets.push(h.to_ascii_lowercase()),
                    _ => diags.warn(
                        format!("manifest.remoteAssets[{i}]"),
                        Ignored::Nothing,
                        "ホスト名として解釈できない項目を無視した",
                    ),
                }
            }
        }
        Some(_) => diags.warn(
            "manifest.remoteAssets",
            Ignored::Nothing,
            "remoteAssets は配列である必要がある。宣言なしとして扱った",
        ),
    }

    Some(Manifest {
        id,
        name,
        version,
        abi,
        author: optional("author"),
        description: optional("description"),
        homepage: optional("homepage"),
        license: optional("license"),
        remote_assets,
    })
}

fn is_reverse_domain(s: &str) -> bool {
    let mut parts = s.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    if first.is_empty()
        || !first
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return false;
    }
    let mut count = 0;
    for p in parts {
        count += 1;
        let ok = !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !ok {
            return false;
        }
    }
    count >= 1
}

fn is_semver(s: &str) -> bool {
    let core = s.split('-').next().unwrap_or(s);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_host_name(s: &str) -> bool {
    if s.is_empty() || !s.contains('.') {
        return false;
    }
    s.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}
