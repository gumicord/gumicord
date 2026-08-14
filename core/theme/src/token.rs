//! デザイントークン (`EXT-011`)。
//!
//! **トークン名はテーマ作者が自由に決める。** クライアントが特定の名前を
//! 要求することはない ([`spec/04-theme.md`] 3.1)。
//!
//! # 型はここでは決めない
//!
//! `{ "color": "#000" }` というオブジェクトは、影としても背景としても
//! 読める。定義した時点でどちらか一方に決めてしまうと、テーマ作者が
//! 意図しない側に倒れる。
//!
//! したがってトークンは**オブジェクトのまま保持し、使われた場所で型が
//! 決まる**。`"shadow": "$x"` なら影として、`"background": "$x"` なら
//! 背景として読む。曖昧さは使用箇所が解消する。
//!
//! 色と数値は曖昧になりようがないので、その場で確定させる。

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::diag::{Diagnostics, Ignored};
use crate::value::Color;

/// 解決済みのトークンの値。
#[derive(Debug, Clone, PartialEq)]
pub enum TokenValue {
    Color(Color),
    /// 論理ピクセル、またはミリ秒 (`motion.*`)
    Length(f32),
    /// フォント / 影 / 背景。**型は使用箇所が決める**
    Object(Map<String, Value>),
}

impl TokenValue {
    /// 診断メッセージ用の型名。
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Color(_) => "色",
            Self::Length(_) => "数値",
            Self::Object(_) => "オブジェクト",
        }
    }
}

/// 解決済みのトークン表。
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    map: HashMap<String, TokenValue>,
}

/// 解決前の 1 エントリ
enum Raw {
    Value(TokenValue),
    /// `$other` — 他のトークンへの参照
    Ref(String),
}

impl Tokens {
    pub fn get(&self, name: &str) -> Option<&TokenValue> {
        self.map.get(name)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `tokens` オブジェクトから表を組み立て、参照をすべて解決する。
    ///
    /// 解決できなかったトークンは**表に入らない**。それを参照する
    /// プロパティは使用箇所で「未定義のトークン」として無視される
    /// ([`spec/04-theme.md`] 7 章)。
    pub fn build(src: &Map<String, Value>, diags: &mut Diagnostics) -> Tokens {
        let mut raw: HashMap<&str, Raw> = HashMap::with_capacity(src.len());

        for (name, value) in src {
            let path = format!("tokens.{name}");
            match parse_raw(value) {
                Some(entry) => {
                    raw.insert(name.as_str(), entry);
                }
                None => diags.error(
                    path,
                    Ignored::Token,
                    "トークンの値は 色 / 数値 / オブジェクト / $参照 のいずれかである必要がある",
                ),
            }
        }

        let mut map = HashMap::with_capacity(raw.len());
        for name in raw.keys().copied() {
            match resolve(name, &raw) {
                Ok(v) => {
                    map.insert(name.to_owned(), v.clone());
                }
                Err(e) => diags.error(format!("tokens.{name}"), Ignored::Token, e.message()),
            }
        }

        Tokens { map }
    }
}

/// 参照の解決に失敗した理由
enum ResolveError {
    /// 循環参照
    Cycle(Vec<String>),
    /// 存在しないトークンを参照している
    Undefined(String),
}

impl ResolveError {
    fn message(&self) -> String {
        match self {
            Self::Cycle(chain) => {
                format!("トークンが循環参照している: {}", chain.join(" -> "))
            }
            Self::Undefined(target) => {
                format!("参照先のトークン ${target} が定義されていない")
            }
        }
    }
}

/// 参照の連鎖をたどる。
///
/// 参照先はちょうど 1 つなので、木ではなく鎖である。訪問済み集合を持って
/// 線形にたどれば、再帰なしで循環を検出できる。**深く入れ子にした
/// テーマでスタックを溢れさせない**ためである。
fn resolve<'a>(
    start: &'a str,
    raw: &'a HashMap<&'a str, Raw>,
) -> Result<&'a TokenValue, ResolveError> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut chain: Vec<&str> = Vec::new();
    let mut cur = start;

    loop {
        chain.push(cur);
        if !seen.insert(cur) {
            return Err(ResolveError::Cycle(
                chain.iter().map(|s| (*s).to_owned()).collect(),
            ));
        }

        match raw.get_key_value(cur) {
            Some((_, Raw::Value(v))) => return Ok(v),
            Some((_, Raw::Ref(next))) => {
                // 参照先が表にない = 未定義、または値として不正だったもの
                let Some((key, _)) = raw.get_key_value(next.as_str()) else {
                    return Err(ResolveError::Undefined(next.clone()));
                };
                cur = key;
            }
            None => return Err(ResolveError::Undefined(cur.to_owned())),
        }
    }
}

fn parse_raw(value: &Value) -> Option<Raw> {
    match value {
        Value::String(s) => {
            if let Some(target) = s.strip_prefix('$') {
                if target.is_empty() {
                    return None;
                }
                Some(Raw::Ref(target.to_owned()))
            } else {
                Color::parse(s).map(|c| Raw::Value(TokenValue::Color(c)))
            }
        }
        Value::Number(n) => {
            let v = n.as_f64()? as f32;
            if v < 0.0 || !v.is_finite() {
                return None;
            }
            Some(Raw::Value(TokenValue::Length(v)))
        }
        Value::Object(o) => Some(Raw::Value(TokenValue::Object(o.clone()))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;

    fn build(json: &str) -> (Tokens, Diagnostics) {
        let v: Value = serde_json::from_str(json).expect("試験の JSON が壊れている");
        let mut diags = Diagnostics::new();
        let tokens = Tokens::build(v.as_object().unwrap(), &mut diags);
        (tokens, diags)
    }

    fn color(s: &str) -> TokenValue {
        TokenValue::Color(Color::parse(s).unwrap())
    }

    #[test]
    fn colors_and_lengths() {
        let (t, d) = build(r##"{ "color.accent": "#7c6cf0", "radius.md": 8 }"##);
        assert!(!d.has_errors());
        assert_eq!(t.get("color.accent"), Some(&color("#7c6cf0")));
        assert_eq!(t.get("radius.md"), Some(&TokenValue::Length(8.0)));
    }

    /// 3.2: トークンは他のトークンを参照できる
    #[test]
    fn reference_chain_resolves() {
        let (t, d) = build(
            r##"{
                "color.brand": "#7c6cf0",
                "color.accent": "$color.brand",
                "color.link": "$color.accent"
            }"##,
        );
        assert!(!d.has_errors(), "{:?}", d.items());
        assert_eq!(t.get("color.accent"), Some(&color("#7c6cf0")));
        assert_eq!(t.get("color.link"), Some(&color("#7c6cf0")));
    }

    /// 7 章: 循環参照は当該トークンを解決不能として扱う
    #[test]
    fn cycle_is_detected_and_reported() {
        let (t, d) = build(r##"{ "a": "$b", "b": "$a" }"##);
        assert!(d.has_errors());
        assert_eq!(t.get("a"), None, "循環したトークンは表に入らない");
        assert_eq!(t.get("b"), None);
        assert!(
            d.items().iter().any(|x| x.message.contains("循環参照")),
            "循環参照であることが利用者に伝わらなければならない: {:?}",
            d.items()
        );
    }

    #[test]
    fn self_reference_is_a_cycle() {
        let (t, d) = build(r##"{ "a": "$a" }"##);
        assert!(d.has_errors());
        assert_eq!(t.get("a"), None);
    }

    /// 長い鎖でも再帰せずに解決できる (スタックを溢れさせない)
    #[test]
    fn long_chain_does_not_overflow() {
        let n = 5000;
        let mut entries = vec![r##""t0": "#111""##.to_string()];
        for i in 1..n {
            entries.push(format!(r#""t{i}": "$t{}""#, i - 1));
        }
        let json = format!("{{ {} }}", entries.join(","));
        let (t, d) = build(&json);
        assert!(
            !d.has_errors(),
            "{:?}",
            &d.items()[..d.items().len().min(3)]
        );
        assert_eq!(t.get(&format!("t{}", n - 1)), Some(&color("#111")));
    }

    #[test]
    fn undefined_reference_is_reported() {
        let (t, d) = build(r#"{ "a": "$missing" }"#);
        assert!(d.has_errors());
        assert_eq!(t.get("a"), None);
        assert!(d.items().iter().any(|x| x.message.contains("$missing")));
    }

    /// 不正な値のトークンだけが落ち、他は生き残る (EXT-016)
    #[test]
    fn bad_token_does_not_kill_the_others() {
        let (t, d) = build(r##"{ "good": "#111", "bad": "notacolor", "also_good": 4 }"##);
        assert_eq!(d.count(Severity::Error), 1);
        assert!(t.get("good").is_some());
        assert!(t.get("also_good").is_some());
        assert_eq!(t.get("bad"), None);
    }

    #[test]
    fn objects_are_kept_untyped() {
        let (t, d) = build(r#"{ "font.body": { "family": "Inter", "size": 15 } }"#);
        assert!(!d.has_errors());
        assert!(matches!(t.get("font.body"), Some(TokenValue::Object(_))));
    }

    #[test]
    fn negative_length_is_rejected() {
        let (t, d) = build(r#"{ "space.neg": -4 }"#);
        assert!(d.has_errors());
        assert_eq!(t.get("space.neg"), None);
    }
}
