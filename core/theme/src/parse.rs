//! JSON からテーマを読む。
//!
//! # なぜ JSON Schema をそのまま使わないのか
//!
//! `spec/schema/theme.schema.json` はテーマ作者のための検証器であり、
//! **文書全体の可否しか答えない**。しかし `EXT-016` が要求するのは
//! 「誤りのある**プロパティだけ**を無視して残りを適用する」であり、
//! 粒度が違う。したがって実行時の読み取りはここで手書きする。
//!
//! スキーマは CI (`cargo xtask schema`) が公式テーマに対して回し続けるので、
//! **2 つの実装が食い違えばそこで気づける**。
//!
//! 仕様: [`spec/04-theme.md`] 7 章

use serde_json::{Map, Value};

use gumicord_uitree::{NodeId, State, StateSet};

use crate::cond::{ColorScheme, PlatformSel, When};
use crate::diag::{Diagnostics, Ignored};
use crate::style::Style;
use crate::token::{TokenValue, Tokens};
use crate::value::{AssetKind, AssetRef, Background, Color, Edges, Fit, Font, Shadow};

/// テーマの manifest。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// 想定する UITree の ABI メジャーバージョン
    pub abi: u32,
    pub author: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    /// 外部アセットの取得を許すホスト (`SEC-022`)
    pub remote_assets: Vec<String>,
}

/// 1 本のルール。
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub select: NodeId,
    pub when: When,
    pub style: Style,
}

/// トークン表と `remoteAssets` — 値を読む間ずっと必要になるもの。
#[derive(Clone, Copy)]
struct Env<'a> {
    tokens: &'a Tokens,
    hosts: &'a [String],
}

/// `$参照` を 1 段解いた値。
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

/// `$name` ならトークンを引き、そうでなければ値をそのまま返す。
///
/// **未定義のトークンはここで診断になる。** 使用箇所ごとに同じ判定を
/// 書かないための一点集約である。
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

// ------------------------------------------------------------------ 値の読み取り

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

/// 長さ (論理 px)。負の値は受けない。
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

/// 0.0〜1.0 に収まる数値。
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
                // 字間だけは負の値に意味がある
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
            // オフセットは負にできる
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

/// 背景 (`EXT-021`〜`EXT-024`, `EXT-027`)。
///
/// 色の短縮記法とオブジェクトの両方を受ける。
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
                    // EXT-023: 読み込み時に一度だけ適用される。上限は
                    // 事故 (blur: 100000) でメモリを溶かさないための歯止め
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

/// アセット参照 (`EXT-017`, `SEC-022`)。
///
/// **読み込み失敗はテーマ全体を無効化しない** (`EXT-027`)。ここで弾かれた
/// 画像は単に `None` になり、`background.color` にフォールバックする。
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

/// `style` オブジェクトを読む。
///
/// **未知のプロパティは警告であって誤りではない。** 新しいクライアント
/// 向けに書かれたテーマを古いクライアントで開いたとき、知らないプロパティ
/// が出てくるのは正常な状況である ([`spec/04-theme.md`] 7.1)。
fn style(obj: &Map<String, Value>, env: Env<'_>, path: &str, diags: &mut Diagnostics) -> Style {
    let mut s = Style::default();
    for (key, v) in obj {
        let p = format!("{path}.{key}");
        match key.as_str() {
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
            _ => diags.warn(
                &p,
                Ignored::Property,
                format!("未知のスタイルプロパティ {key} を無視した"),
            ),
        }
    }
    s
}

// ------------------------------------------------------------------ when

/// `when` を読む。
///
/// 未知のキーや未知の値は**ルールごと無視する**。条件を落として適用すると、
/// 意図より広い範囲にスタイルが当たってしまうためである。無視するほうが安全側に倒れる。
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

/// `when` の数値。負のウィンドウ幅は意味を持たない。
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

/// 文字列、またはその配列を取り出す。それ以外はルールごと無視する。
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
        // 未知の安定 ID は「将来追加されたノード」かもしれない。
        // 誤りではなく警告として扱い、残りのルールは適用する (EXT-016)
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
        let st = style(st, env, &format!("{path}.style"), diags);

        // すべてのプロパティが無視された結果、何も残らないこともある
        if st.is_empty() {
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
        });
    }
    out
}

/// manifest を読む。**ここで失敗したらテーマ全体を適用しない。**
///
/// 他と違って寛容にできないのは、テーマの同一性・バージョン・外部アクセス
/// の許可範囲がすべてここで決まるためである。
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
