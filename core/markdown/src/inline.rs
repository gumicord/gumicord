//! Inline parsing.
//!
//! An opening mark is not assumed to close. `2 ** 3` read as the start of
//! bold would swallow the rest of the body, so a mark is entered only after
//! its closer has been located; otherwise it stays literal text.
//!
//! Once a closer is chosen, the inner parse must stop exactly there. In
//! `a ** b ** c`, choosing offset 8 as the closer for `*` while the inner
//! parse stops at offset 3 would drop `b` entirely. [`Scanner::limit`] hides
//! everything past the closer, so where to stop is never computed twice.
//!
//! Searching for a closer skips code spans, or the marks inside a code span
//! would close the emphasis around it.
//!
//! Discord-specific boundary rules, from the `simple-markdown` patterns it
//! uses:
//!
//! - Content may neither start nor end with whitespace, so `a * b * c` is not
//!   italic. Without this every multiplication becomes italic.
//! - `_` does not open after an alphanumeric, so `snake_case_word` is not
//!   italic. `*` has no such rule: `a*b*c` is.
//! - When `**` fails to close, `*` is not retried at the same position, or
//!   `a ** b ** c` collapses into one `*` pair and loses `b`.

use crate::model::{Deco, Inline, InlineKind, Mention};

/// Marks and the decoration they add. Longest first: `**` before `*`.
const MARKS: &[(&str, Deco)] = &[
    ("~~", Deco::STRIKE),
    ("||", Deco::SPOILER),
    ("__", Deco::UNDERLINE),
    ("**", Deco::BOLD),
    ("*", Deco::ITALIC),
    ("_", Deco::ITALIC),
];

/// Trimmed from the end of a bare URL: swallowing sentence-final
/// punctuation breaks the link.
const TRAILING: &[char] = &[
    '.', ',', '!', '?', ';', ':', ')', ']', '}', '\'', '"', '。', '、', '」', '）',
];

pub fn parse(text: &str) -> Vec<Inline> {
    let chars: Vec<char> = text.chars().collect();
    let mut s = Scanner {
        c: &chars,
        i: 0,
        limit: chars.len(),
        out: Vec::new(),
        buf: String::new(),
    };
    s.run(Deco::NONE);
    s.flush(Deco::NONE);
    s.out
}

struct Scanner<'a> {
    c: &'a [char],
    i: usize,
    /// Everything at or past this offset is invisible.
    ///
    /// Set to the closer when parsing inside a mark, so the inner parse can
    /// neither consume the outer closer nor open a mark across it.
    limit: usize,
    out: Vec<Inline>,
    /// Characters not yet turned into an [`Inline`], accumulated while the
    /// decoration is unchanged.
    buf: String,
}

impl Scanner<'_> {
    fn flush(&mut self, deco: Deco) {
        if !self.buf.is_empty() {
            let text = std::mem::take(&mut self.buf);
            self.out.push(Inline::text(text, deco));
        }
    }

    fn push(&mut self, deco: Deco, kind: InlineKind) {
        self.flush(deco);
        self.out.push(Inline { deco, kind });
    }

    /// `None` at or past [`Scanner::limit`]. Lookahead must go through here
    /// too, or it peeks outside the mark.
    fn at(&self, i: usize) -> Option<char> {
        (i < self.limit).then(|| self.c[i])
    }

    fn starts(&self, i: usize, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(k, ch)| self.at(i + k) == Some(ch))
    }

    fn run(&mut self, deco: Deco) {
        while self.i < self.limit {
            if self.step(deco) {
                continue;
            }
            let ch = self.c[self.i];
            self.i += 1;
            self.buf.push(ch);
        }
    }

    /// True when one special construct was consumed.
    fn step(&mut self, deco: Deco) -> bool {
        let i = self.i;
        match self.c[i] {
            '\\' => {
                // Only punctuation escapes; escaping the `n` of `\n` would
                // silently swallow the backslash the author typed.
                if let Some(n) = self.at(i + 1)
                    && is_punct(n)
                {
                    self.i = i + 2;
                    self.buf.push(n);
                    return true;
                }
                false
            }
            '\n' => {
                self.i = i + 1;
                self.push(deco, InlineKind::Break);
                true
            }
            '`' => self.code(deco),
            '<' => self.angle(deco),
            '@' => self.at_mention(deco),
            '[' => self.masked_link(deco),
            'h' => self.bare_url(deco),
            _ => self.mark(deco),
        }
    }

    /// Code spans. Contents are not parsed.
    fn code(&mut self, deco: Deco) -> bool {
        let open = self.run_of('`', self.i);
        // Three or more is a fence, handled at block level.
        if open >= 3 {
            return false;
        }
        let fence: String = "`".repeat(open);
        let body = self.i + open;
        let Some(close) = self.find_raw(body, &fence) else {
            return false;
        };
        if close == body {
            return false;
        }
        let raw: String = self.c[body..close].iter().collect();
        self.i = close + open;
        // Discord strips exactly one space from each side.
        let text = raw
            .strip_prefix(' ')
            .and_then(|t| t.strip_suffix(' '))
            .map_or(raw.clone(), str::to_owned);
        self.push(deco, InlineKind::Code(text));
        true
    }

    /// `<@1>` `<#1>` `<@&1>` `<:n:1>` `<a:n:1>` `<t:1:R>` `<url>`
    fn angle(&mut self, deco: Deco) -> bool {
        let Some(close) = self.find_raw(self.i + 1, ">") else {
            return false;
        };
        let inner: String = self.c[self.i + 1..close].iter().collect();
        let Some(kind) = parse_angle(&inner) else {
            return false;
        };
        self.i = close + 1;
        self.push(deco, kind);
        true
    }

    fn at_mention(&mut self, deco: Deco) -> bool {
        for (word, m) in [("@everyone", Mention::Everyone), ("@here", Mention::Here)] {
            if self.starts(self.i, word) {
                self.i += word.chars().count();
                self.push(deco, InlineKind::Mention(m));
                return true;
            }
        }
        false
    }

    /// `[label](url)`
    fn masked_link(&mut self, deco: Deco) -> bool {
        let Some(rb) = self.find_raw(self.i + 1, "]") else {
            return false;
        };
        if self.at(rb + 1) != Some('(') {
            return false;
        }
        let Some(rp) = self.find_raw(rb + 2, ")") else {
            return false;
        };
        let label: String = self.c[self.i + 1..rb].iter().collect();
        let url: String = self.c[rb + 2..rp].iter().collect();
        if label.is_empty() || !is_url(&url) {
            return false;
        }
        self.i = rp + 1;
        self.push(
            deco,
            InlineKind::Link {
                url,
                label: Some(label),
            },
        );
        true
    }

    /// A bare `https://…`
    fn bare_url(&mut self, deco: Deco) -> bool {
        // Must not start mid-word: `ahttps://x` is not a link.
        if self.i > 0 && is_word(self.c[self.i - 1]) {
            return false;
        }
        if !["https://", "http://"]
            .iter()
            .any(|s| self.starts(self.i, s))
        {
            return false;
        }
        let mut end = self.i;
        while end < self.limit && !self.c[end].is_whitespace() && self.c[end] != '<' {
            end += 1;
        }
        // Sentence-final punctuation is not part of the URL.
        while end > self.i && TRAILING.contains(&self.c[end - 1]) {
            end -= 1;
        }
        let url: String = self.c[self.i..end].iter().collect();
        if !is_url(&url) {
            return false;
        }
        self.i = end;
        self.push(deco, InlineKind::Link { url, label: None });
        true
    }

    /// `**` `*` `__` `_` `~~` `||`
    fn mark(&mut self, deco: Deco) -> bool {
        for (pat, add) in MARKS {
            if !self.starts(self.i, pat) || deco.contains(*add) {
                continue;
            }
            // When `**` fails to close, do not retry `*` here: it would make
            // `a ** b ** c` one `*` pair and lose `b`.
            if pat.len() == 1 && self.starts(self.i, &pat.repeat(2)) {
                continue;
            }
            // `_` does not open after a word character (`snake_case`).
            if *pat == "_" && self.i > 0 && self.c[self.i - 1].is_alphanumeric() {
                continue;
            }
            let body = self.i + pat.chars().count();
            let Some(close) = self.find_close(body, pat) else {
                continue;
            };

            self.flush(deco);
            let outer = self.limit;
            self.i = body;
            self.limit = close;
            self.run(deco.with(*add));
            self.flush(deco.with(*add));
            self.limit = outer;
            self.i = close + pat.chars().count();
            return true;
        }
        false
    }

    /// Finds a mark's closer, applying the whitespace rules.
    fn find_close(&self, from: usize, pat: &str) -> Option<usize> {
        // Content must not start with whitespace.
        if !self.at(from).is_some_and(|c| !c.is_whitespace()) {
            return None;
        }
        let mut i = from;
        while let Some(close) = self.find_raw(i, pat) {
            let last = close.checked_sub(1).and_then(|k| self.at(k));
            let after = self.at(close + pat.chars().count());
            let ok = close > from
                // Content must not end with whitespace.
                && last.is_some_and(|c| !c.is_whitespace())
                // `*a**` closes with `**`, not `*`.
                && after != pat.chars().next()
                // `_a_b` is not italic.
                && (pat != "_" || !after.is_some_and(char::is_alphanumeric));
            if ok {
                return Some(close);
            }
            i = close + 1;
        }
        None
    }

    /// Finds `pat` literally, skipping escapes and code spans.
    ///
    /// Without skipping code, a mark inside a code span closes the outer one.
    fn find_raw(&self, from: usize, pat: &str) -> Option<usize> {
        let mut i = from;
        while i < self.limit {
            match self.c[i] {
                '\\' if self.at(i + 1).is_some_and(is_punct) => {
                    i += 2;
                    continue;
                }
                '`' if !pat.starts_with('`') => {
                    let n = self.run_of('`', i);
                    let fence = "`".repeat(n);
                    // An unclosed code span is literal; walk past it.
                    match self.find_raw(i + n, &fence) {
                        Some(end) => i = end + n,
                        None => i += n,
                    }
                    continue;
                }
                _ => {}
            }
            if self.starts(i, pat) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn run_of(&self, ch: char, from: usize) -> usize {
        let mut n = 0;
        while self.at(from + n) == Some(ch) {
            n += 1;
        }
        n
    }
}

/// Every branch must fall through on failure. An early `?` here would exit
/// the whole function and the later branches would never be reached.
fn parse_angle(inner: &str) -> Option<InlineKind> {
    if let Some(rest) = inner.strip_prefix("@&") {
        return id(rest).map(|i| InlineKind::Mention(Mention::Role(i)));
    }
    if let Some(rest) = inner.strip_prefix('@') {
        // `<@!123>` is the old form and still arrives.
        let rest = rest.strip_prefix('!').unwrap_or(rest);
        return id(rest).map(|i| InlineKind::Mention(Mention::User(i)));
    }
    if let Some(rest) = inner.strip_prefix('#') {
        return id(rest).map(|i| InlineKind::Mention(Mention::Channel(i)));
    }
    if let Some(rest) = inner.strip_prefix("t:") {
        return timestamp(rest);
    }
    if let Some(e) = emoji(inner) {
        return Some(e);
    }
    // `<https://…>` only suppresses the embed; the target is the same.
    is_url(inner).then(|| InlineKind::Link {
        url: inner.to_owned(),
        label: None,
    })
}

fn timestamp(rest: &str) -> Option<InlineKind> {
    let (at, format) = match rest.split_once(':') {
        Some((a, f)) => (a, f.chars().next().filter(|c| "tTdDfFR".contains(*c))?),
        // No format suffix means the default one.
        None => (rest, 'f'),
    };
    Some(InlineKind::Timestamp {
        at: at.parse().ok()?,
        format,
    })
}

/// `<a:name:123>` / `<:name:123>`
fn emoji(inner: &str) -> Option<InlineKind> {
    let (animated, rest) = match inner.strip_prefix("a:") {
        Some(r) => (true, r),
        None => (false, inner.strip_prefix(':')?),
    };
    let (name, num) = rest.split_once(':')?;
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(InlineKind::Emoji {
        name: name.to_owned(),
        id: id(num)?,
        animated,
    })
}

fn id(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn is_url(s: &str) -> bool {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/'))
}

/// Whether this is a word character. `_` counts as one.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
}
