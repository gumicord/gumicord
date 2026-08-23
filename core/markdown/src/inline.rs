//! 行の中の解析。
//!
//! # ⚠️ 開いた印が必ず閉じるとは限らない
//!
//! `**` を見た瞬間に「ここから太字」と決めてはいけない。閉じる `**` が
//! 無ければ、それは**ただの二文字**である。`2 ** 3` を太字の開始と読むと、
//! そこから先の本文が丸ごと消える。
//!
//! なので印を見たら、**先に閉じ先を探してから**入る。見つからなければ
//! 文字として置く。
//!
//! # ⚠️ 閉じ先を決めたら、そこで止めること
//!
//! 探した閉じ先と、内側の解析が実際に止まる場所は**別物になりうる**。
//! `a ** b ** c` で `*` の閉じ先を 8 文字目と決めたのに、内側が 3 文字目の
//! `*` を見て止まると、**間の `b` が誰にも読まれないまま捨てられる**。
//!
//! だから内側には[`Scanner::limit`] を渡し、**そこから先を見せない**。
//! 「どこで止まるか」を二度計算しない、というのがここの決まりである。
//!
//! # ⚠️ 閉じ先を探すときに、コードの中を覗いてはいけない
//!
//! `` `**` `` の中の `**` は文字である。ここを飛ばさないと、
//! **コードの中身が外の太字を閉じてしまう**。
//!
//! # 空白と語の境目の規則 (Discord 固有)
//!
//! - `a * b * c` は斜体にならない。**中身が空白で始まっても終わってもいけない**。
//!   この一行が無いと、掛け算の式が全部斜体になる。
//! - `snake_case_word` も斜体にならない。`_` は**前が英数字なら開かない**。
//!   `*` にこの規則は無い (`a*b*c` は斜体になる)。
//! - `**` が閉じないとき、**同じ場所で `*` を試さない**。試すと
//!   `a ** b ** c` が「`*` で開いて `*` で閉じた」に化ける。
//!
//! 出典: Discord が使っている `simple-markdown` の規則
//! (`^\*(?=\S)…[^\s\*\\]\*(?!\*)` と `^\b_…_\b`)

use crate::model::{Deco, Inline, InlineKind, Mention};

/// 印と、それが足す飾り。**長いものから試す** — `**` は `*` より先
const MARKS: &[(&str, Deco)] = &[
    ("~~", Deco::STRIKE),
    ("||", Deco::SPOILER),
    ("__", Deco::UNDERLINE),
    ("**", Deco::BOLD),
    ("*", Deco::ITALIC),
    ("_", Deco::ITALIC),
];

/// 裸の URL の終わりから削る文字。
///
/// ⚠️ 文末の `.` まで URL に入れると**リンクが切れる**。
/// 「詳しくは https://example.com/a。」の `。` も同じ
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
    /// ここから先は**無いものとして扱う**。
    ///
    /// 印の内側を読むときに閉じ先を入れる。文字列の終わりと同じ扱いになるので、
    /// 内側の解析が外の閉じを食べたり、外を跨いで別の印を開いたりしなくなる
    limit: usize,
    out: Vec<Inline>,
    /// まだ [`Inline`] にしていない文字。**同じ飾りの間は繋げて溜める**
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

    /// ⚠️ [`Scanner::limit`] から先は `None` である。**終わりと同じ扱い。**
    /// 先読みもここを通すこと — 通さないと外側の文字を覗いてしまう
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

    /// 何か特別なものを 1 つ食べたら `true`。
    fn step(&mut self, deco: Deco) -> bool {
        let i = self.i;
        match self.c[i] {
            '\\' => {
                // ⚠️ **記号だけ**が逃がせる。`\n` の `n` を文字にすると、
                // 打った `\` が黙って消える
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

    /// `` `…` `` と ``` ``…`` ```。**中身は解析しない。**
    fn code(&mut self, deco: Deco) -> bool {
        let open = self.run_of('`', self.i);
        // ⚠️ 3 つ以上はコードブロックの印である。ここでは扱わない
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
        // Discord は中身の前後の空白を 1 つだけ削る
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

    /// `[名前](url)`
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

    /// 裸の `https://…`
    fn bare_url(&mut self, deco: Deco) -> bool {
        // ⚠️ 語の途中から始めない。`ahttps://x` はリンクではない
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
        // 文末の記号は URL ではない
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
            // ⚠️ `**` が閉じなかったとき、同じ場所で `*` を試さない。
            // 試すと `a ** b ** c` が「`*` で開いて次の `*` で閉じた」に化け、
            // 間の `b` が消える
            if pat.len() == 1 && self.starts(self.i, &pat.repeat(2)) {
                continue;
            }
            // `_` は語の後ろでは開かない (`snake_case`)
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

    /// 印の閉じ先。**中身の空白の規則をここで見る。**
    fn find_close(&self, from: usize, pat: &str) -> Option<usize> {
        // 中身が空白で始まってはいけない
        if !self.at(from).is_some_and(|c| !c.is_whitespace()) {
            return None;
        }
        let mut i = from;
        while let Some(close) = self.find_raw(i, pat) {
            let last = close.checked_sub(1).and_then(|k| self.at(k));
            let after = self.at(close + pat.chars().count());
            let ok = close > from
                // 中身が空白で終わってはいけない
                && last.is_some_and(|c| !c.is_whitespace())
                // `*a**` の閉じは `**` であって `*` ではない
                && after != pat.chars().next()
                // `_a_b` は斜体にしない
                && (pat != "_" || !after.is_some_and(char::is_alphanumeric));
            if ok {
                return Some(close);
            }
            i = close + 1;
        }
        None
    }

    /// `pat` をそのまま探す。**逃がした文字とコードの中は飛ばす。**
    ///
    /// ⚠️ コードを飛ばさないと、`` `**` `` の中の印が外の印を閉じる
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
                    // 閉じないコードは、ただの記号として素通りさせる
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

/// ⚠️ **どの枝も、失敗したら次を試せること。** 途中で `?` を使って
/// 関数ごと抜けると、後ろの枝 (URL) に一生たどり着かない
fn parse_angle(inner: &str) -> Option<InlineKind> {
    if let Some(rest) = inner.strip_prefix("@&") {
        return id(rest).map(|i| InlineKind::Mention(Mention::Role(i)));
    }
    if let Some(rest) = inner.strip_prefix('@') {
        // `<@!123>` は昔の書き方。いまも来る
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
    // `<https://…>` — 埋め込みを抑えるだけで、行き先は同じ
    is_url(inner).then(|| InlineKind::Link {
        url: inner.to_owned(),
        label: None,
    })
}

fn timestamp(rest: &str) -> Option<InlineKind> {
    let (at, format) = match rest.split_once(':') {
        Some((a, f)) => (a, f.chars().next().filter(|c| "tTdDfFR".contains(*c))?),
        // 書式が無ければ既定の書き方である
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

/// 語を作る文字か。`_` も語の一部である
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
}
