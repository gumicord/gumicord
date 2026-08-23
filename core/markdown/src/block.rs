//! Block-level parsing.
//!
//! Fenced code comes out first, before splitting into lines: a fence can open
//! mid-line, and leaving it until later would read `# ` and `> ` *inside* code
//! as headings and quotes. Code contents must not be interpreted at all.
//!
//! The cost is that a fenced block inside a `> ` quote escapes the quote,
//! since the fence is extracted before the `>` markers are stripped. Rare
//! enough to accept for now.

use crate::inline;
use crate::model::{Block, Item, Marker};

/// Spaces per indent level.
const INDENT: usize = 2;
/// Bounded so malformed input cannot nest forever.
const MAX_DEPTH: u8 = 4;
/// Bounded for the same reason as MAX_DEPTH.
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

/// Splits fenced code from everything else.
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
        // An unclosed fence is not code; leave it as literal text.
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

/// Decides whether the line after the fence is an info string or content.
fn split_lang(body: &str) -> (Option<String>, String) {
    let (head, rest) = match body.split_once('\n') {
        Some((h, r)) => (h, r),
        // With no newline there is no info string: ```js``` renders `js`.
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

/// Strips one leading and trailing newline. Blank lines inside are content.
fn trim_code(s: &str) -> String {
    let s = s.strip_prefix('\n').unwrap_or(s);
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.to_owned()
}

/// Splits into lines and stacks them. `depth` is the quote nesting level.
fn lines(text: &str, depth: u32, out: &mut Vec<Block>) {
    let all: Vec<&str> = text.split('\n').collect();
    let mut para: Vec<&str> = Vec::new();
    let mut i = 0;

    // Flush the pending paragraph.
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

        // `>>> ` quotes everything from here on.
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

        // Consecutive `> ` lines merge into one quote.
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

        // Consecutive list items merge into one list.
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

/// The content of a `> ` line. A bare `>` quotes a blank line.
fn quoted(line: &str) -> Option<&str> {
    line.strip_prefix("> ")
        .or_else(|| (line == ">" || line == "> ").then_some(""))
}

/// `# `, `## `, `### `. The space is required, so `#tag` is not a heading.
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
        // Without a digit cap a long run of digits overflows u32.
        if digits == 0 || digits > 9 {
            return None;
        }
        let b = rest[digits..].strip_prefix(". ")?;
        (Marker::Number(rest[..digits].parse().ok()?), b)
    };

    // A bare `- ` is not a list item, or sentences starting with `- ` vanish.
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
