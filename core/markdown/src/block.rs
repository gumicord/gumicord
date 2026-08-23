//! 縦に積まれるものの解析。
//!
//! # 順番: コードブロックが最初
//!
//! ```` ``` ```` は**行の途中からでも始まる**。`見て ```js` と打てば、
//! そこで段落が切れてコードになる。だから行に切る前に、まずコードブロックを
//! 抜き出して残りを分ける。
//!
//! ⚠️ **これは行の解析より先でなければならない。** 後回しにすると、
//! コードの中の `# ` や `> ` を見出しや引用として読んでしまう。
//! **コードの中身は何も解釈しない**のが唯一の正しい扱いである。
//!
//! ## 引き換えに諦めたこと
//!
//! `> ` を付けた引用の中のコードブロックは、引用の外へ出る。
//! 先にコードを抜くので、`>` が剥がれる前に切れてしまうためである。
//! 珍しい書き方なので M1 では許す。

use crate::inline;
use crate::model::{Block, Item, Marker};

/// 字下げ何文字で 1 段とするか
const INDENT: usize = 2;
/// 箇条書きの深さの上限。**壊れた入力で無限に潜らないため**
const MAX_DEPTH: u8 = 4;
/// 引用の入れ子の上限。同じ理由である
const MAX_QUOTE: u32 = 4;

pub fn parse(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    for seg in segments(text) {
        match seg {
            Seg::Code { lang, text } => out.push(Block::Code { lang, text }),
            Seg::Text(t) => lines(&t, 0, &mut out),
        }
    }
    out
}

enum Seg {
    Text(String),
    Code { lang: Option<String>, text: String },
}

/// コードブロックとそれ以外に分ける。
fn segments(text: &str) -> Vec<Seg> {
    const FENCE: &str = "```";
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    while i < c.len() {
        if !starts(&c, i, FENCE) {
            plain.push(c[i]);
            i += 1;
            continue;
        }
        // 閉じないものはコードではない。**ただの記号として置く**
        let Some(close) = find(&c, i + FENCE.len(), FENCE) else {
            plain.push(c[i]);
            i += 1;
            continue;
        };

        if !plain.is_empty() {
            out.push(Seg::Text(std::mem::take(&mut plain)));
        }
        let body: String = c[i + FENCE.len()..close].iter().collect();
        let (lang, text) = split_lang(&body);
        out.push(Seg::Code { lang, text });
        i = close + FENCE.len();
    }

    if !plain.is_empty() {
        out.push(Seg::Text(plain));
    }
    out
}

/// 開きの直後の 1 行が言語名か、それとも中身の 1 行目か。
fn split_lang(body: &str) -> (Option<String>, String) {
    let (head, rest) = match body.split_once('\n') {
        Some((h, r)) => (h, r),
        // 改行が無ければ言語名ではない。```js``` は `js` を出す
        None => return (None, trim_code(body)),
    };
    let is_lang = !head.is_empty()
        && head
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "+-#._".contains(ch));
    if is_lang {
        (Some(head.to_owned()), trim_code(rest))
    } else {
        (None, trim_code(body))
    }
}

/// 前後の改行を 1 つだけ削る。**中の空行は中身である**
fn trim_code(s: &str) -> String {
    let s = s.strip_prefix('\n').unwrap_or(s);
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.to_owned()
}

/// 行に切って積む。`depth` は引用の入れ子の深さ
fn lines(text: &str, depth: u32, out: &mut Vec<Block>) {
    let all: Vec<&str> = text.split('\n').collect();
    let mut para: Vec<&str> = Vec::new();
    let mut i = 0;

    // 溜めていた段落を吐く
    macro_rules! flush {
        () => {
            if !para.is_empty() {
                let joined = std::mem::take(&mut para).join("\n");
                let content = inline::parse(&joined);
                if !content.is_empty() {
                    out.push(Block::Paragraph(content));
                }
            }
        };
    }

    while i < all.len() {
        let line = all[i];

        if line.trim().is_empty() {
            flush!();
            i += 1;
            continue;
        }

        // `>>> ` はここから先を全部引用にする
        if depth < MAX_QUOTE
            && let Some(rest) = line.strip_prefix(">>> ")
        {
            flush!();
            let mut body = vec![rest];
            body.extend_from_slice(&all[i + 1..]);
            let mut inner = Vec::new();
            lines(&body.join("\n"), depth + 1, &mut inner);
            out.push(Block::Quote(inner));
            return;
        }

        // `> ` は続く限り 1 つの引用にまとめる
        if depth < MAX_QUOTE && quoted(line).is_some() {
            flush!();
            let mut body = Vec::new();
            while i < all.len()
                && let Some(rest) = quoted(all[i])
            {
                body.push(rest);
                i += 1;
            }
            let mut inner = Vec::new();
            lines(&body.join("\n"), depth + 1, &mut inner);
            out.push(Block::Quote(inner));
            continue;
        }

        if let Some((level, rest)) = heading(line) {
            flush!();
            out.push(Block::Heading {
                level,
                content: inline::parse(rest),
            });
            i += 1;
            continue;
        }

        if let Some(rest) = line.strip_prefix("-# ") {
            flush!();
            out.push(Block::Subtext(inline::parse(rest)));
            i += 1;
            continue;
        }

        // 箇条書きも続く限り 1 つにまとめる
        if bullet(line).is_some() {
            flush!();
            let mut items = Vec::new();
            while i < all.len()
                && let Some(it) = bullet(all[i])
            {
                items.push(it);
                i += 1;
            }
            out.push(Block::List(items));
            continue;
        }

        para.push(line);
        i += 1;
    }
    flush!();
}

/// `> 中身` の中身。`>` だけの行は空行の引用である
fn quoted(line: &str) -> Option<&str> {
    line.strip_prefix("> ")
        .or_else(|| (line == ">" || line == "> ").then_some(""))
}

/// `# ` `## ` `### `。**空白が要る** — `#tag` は見出しではない
fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if !(1..=3).contains(&hashes) {
        return None;
    }
    let rest = line[hashes..].strip_prefix(' ')?;
    (!rest.trim().is_empty()).then_some((hashes as u8, rest))
}

/// `- ` `* ` `1. `
fn bullet(line: &str) -> Option<Item> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];

    let (marker, body) = if let Some(b) = rest.strip_prefix("- ").or(rest.strip_prefix("* ")) {
        (Marker::Bullet, b)
    } else {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        // ⚠️ 桁を数えないと、長い数字で `u32` が溢れる
        if digits == 0 || digits > 9 {
            return None;
        }
        let b = rest[digits..].strip_prefix(". ")?;
        (Marker::Number(rest[..digits].parse().ok()?), b)
    };

    // `- ` だけの行は箇条書きにしない。`- ` で始まる文が消える
    if body.trim().is_empty() {
        return None;
    }
    Some(Item {
        depth: ((indent / INDENT) as u8).min(MAX_DEPTH),
        marker,
        content: inline::parse(body),
    })
}

fn starts(c: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, ch)| c.get(i + k) == Some(&ch))
}

fn find(c: &[char], from: usize, pat: &str) -> Option<usize> {
    (from..c.len()).find(|&i| starts(c, i, pat))
}
