//! Turns a parsed message body into UITree nodes.
//!
//! Parsing already happened in [`gumicord_markdown`]; this only dresses the
//! result in whatever the theme decided and emits nodes.
//!
//! Inline decoration does not become nodes. Separate nodes wrap
//! independently, so a bold run inside a sentence would break the line at the
//! wrong place. Inline content becomes a list of [`Span`]s inside one node,
//! and only vertically stacked things — paragraphs, headings, quotes, lists,
//! code blocks — become nodes of their own.
//!
//! What bold *looks* like is not decided here. "This is bold" is a parse
//! fact; "weight 700" is the theme's. This looks up `primitive.text` by
//! `when.slot` and carries whatever the theme wrote; a theme that writes
//! nothing changes nothing.
//!
//! | slot | applies to |
//! |---|---|
//! | `bold` `italic` `underline` `strike` | `**` `*` `__` `~~` |
//! | `spoiler` | `\|\|` |
//! | `code` | inline `` ` `` |
//! | `link` | links and bare URLs |
//! | `mention` | `<@1>` `<#1>` `<@&1>` `@everyone` |
//! | `h1` `h2` `h3` `subtext` | headings and `-# ` |
//! | `quote_bar` | the rule beside a quote |
//! | `bullet` | list markers |
//!
//! An unresolvable `<@123>` renders as a Japanese "unknown user" label rather
//! than the raw id: the id is not what the author typed, and `@123` is a lie.

use gumicord_markdown::{Block, Deco, Inline, InlineKind, Item, Marker, Mention};
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::{Content, Key, Line, NodeId, Span, Style, UiNode};

/// Slot per list depth.
///
/// How far to indent is the theme's decision; this only reports the level.
/// Listed out because a slot must be a `&'static str`.
const DEPTH: &[&str] = &["li0", "li1", "li2", "li3", "li4"];

/// Translates parse facts into whatever the theme decided they look like.
pub struct Ink<'a> {
    theme: Option<&'a Theme>,
    ctx: MatchContext,
    /// Whether spoilers are revealed, per message.
    ///
    /// Per-span revealing would need per-span hit testing, which would mean
    /// making spans into nodes — which this module cannot do. For now,
    /// pressing one spoiler reveals all of them in that message.
    revealed: bool,
    /// The time, in UTC seconds, read once at the head of the frame.
    ///
    /// Never read the clock here: time moving mid-build makes adjacent
    /// relative timestamps disagree.
    now: i64,
    /// The shortest time any rendered timestamp stays valid for.
    ///
    /// `None` when nothing here changes with time, so there is no reason to
    /// redraw. A [`Cell`](std::cell::Cell) because building takes `&self`.
    holds: std::cell::Cell<Option<i64>>,
}

/// Resolves ids to names. Only the caller knows the directories.
pub trait Names {
    fn user(&self, id: u64) -> Option<String>;
    fn channel(&self, id: u64) -> Option<String>;
    fn role(&self, id: u64) -> Option<String>;
}

impl<'a> Ink<'a> {
    pub fn new(theme: Option<&'a Theme>, ctx: MatchContext, revealed: bool, now: i64) -> Self {
        Ink {
            theme,
            ctx,
            revealed,
            now,
            holds: std::cell::Cell::new(None),
        }
    }

    /// How many seconds the built nodes stay valid for.
    ///
    /// `None` means nothing changes with time. Read after [`Self::blocks`].
    pub fn holds_for(&self) -> Option<i64> {
        self.holds.get()
    }

    /// Accumulates validity; the shortest wins.
    fn hold(&self, secs: i64) {
        let next = match self.holds.get() {
            Some(cur) => cur.min(secs),
            None => secs,
        };
        self.holds.set(Some(next));
    }

    /// What the theme wrote for `primitive.text` in that slot.
    ///
    /// Empty with no theme, which means "change nothing" rather than an
    /// error.
    fn slot(&self, slot: &'static str) -> Style {
        match self.theme {
            Some(t) => t.style_for(NodeId::PrimitiveText, &self.ctx.with_slot(Some(slot))),
            None => Style::default(),
        }
    }

    /// Turns a body into vertically stacked nodes.
    pub fn blocks(&self, blocks: &[Block], names: &dyn Names) -> Vec<UiNode> {
        blocks.iter().map(|b| self.block(b, names)).collect()
    }

    fn block(&self, b: &Block, names: &dyn Names) -> UiNode {
        match b {
            Block::Paragraph(c) => self.rich(self.spans(c, names), None),
            Block::Heading { level, content } => {
                let slot = match level {
                    1 => "h1",
                    2 => "h2",
                    _ => "h3",
                };
                self.rich(self.spans(content, names), Some(slot))
            }
            Block::Subtext(c) => self.rich(self.spans(c, names), Some("subtext")),
            Block::Quote(inner) => UiNode::new(NodeId::LayoutRow)
                .child(UiNode::new(NodeId::PrimitiveDivider).with_key(Key::Slot("quote_bar")))
                .child(
                    UiNode::new(NodeId::LayoutColumn)
                        .children(self.blocks(inner, names))
                        .with_key(Key::Slot("quote_body")),
                ),
            Block::List(items) => UiNode::new(NodeId::LayoutColumn)
                .children(
                    items
                        .iter()
                        .map(|i| self.item(i, names))
                        .collect::<Vec<_>>(),
                )
                .with_key(Key::Slot("list")),
            // Contents are not decorated: `**` inside code is literal.
            Block::Code { lang, text } => UiNode::new(NodeId::PrimitiveCodeBlock)
                .with_content(Content::Text(text.clone()))
                .with_key(Key::Slot(lang_slot(lang.as_deref()))),
        }
    }

    fn item(&self, it: &Item, names: &dyn Names) -> UiNode {
        let marker = match it.marker {
            Marker::Bullet => "•".to_owned(),
            Marker::Number(n) => format!("{n}."),
        };
        // Indent the row, not the marker: indenting the marker tucks the
        // wrapped second line underneath it.
        let depth = DEPTH
            .get(it.depth as usize)
            .or(DEPTH.last())
            .copied()
            .unwrap_or("li0");
        UiNode::new(NodeId::LayoutRow)
            .with_key(Key::Slot(depth))
            .child(self.rich(vec![self.dressed(marker, "bullet", false)], None))
            .child(self.rich(self.spans(&it.content, names), None))
    }

    fn rich(&self, spans: Vec<Span>, slot: Option<&'static str>) -> UiNode {
        let mut n = UiNode::new(NodeId::PrimitiveText).with_content(Content::Rich(spans));
        if let Some(s) = slot {
            n = n.with_key(Key::Slot(s));
        }
        n
    }

    /// Turns inline content into [`Span`]s.
    pub fn spans(&self, inlines: &[Inline], names: &dyn Names) -> Vec<Span> {
        inlines
            .iter()
            .map(|i| self.span(i, names))
            .filter(|s| !s.text.is_empty())
            .collect()
    }

    fn span(&self, i: &Inline, names: &dyn Names) -> Span {
        let (text, extra) = match &i.kind {
            InlineKind::Text(t) => (t.clone(), None),
            InlineKind::Code(t) => (t.clone(), Some("code")),
            InlineKind::Link { url, label } => {
                (label.clone().unwrap_or_else(|| url.clone()), Some("link"))
            }
            InlineKind::Mention(m) => (mention_text(*m, names), Some("mention")),
            // Emoji are images, but until one is fetched the name stands in;
            // showing nothing would empty an emoji-only message.
            InlineKind::Emoji { name, .. } => (format!(":{name}:"), Some("mention")),
            InlineKind::Timestamp { at, format } => {
                let (text, holds) = stamp(self.now, *at, *format);
                // Anything that changes with time schedules a redraw.
                if let Some(secs) = holds {
                    self.hold(secs);
                }
                (text, Some("mention"))
            }
            InlineKind::Break => ("\n".to_owned(), None),
        };

        let hidden = i.deco.contains(Deco::SPOILER) && !self.revealed;
        let mut style = Style::default();
        for (flag, slot) in [
            (Deco::BOLD, "bold"),
            (Deco::ITALIC, "italic"),
            (Deco::UNDERLINE, "underline"),
            (Deco::STRIKE, "strike"),
            (Deco::SPOILER, "spoiler"),
        ] {
            if i.deco.contains(flag) {
                style.overlay(&self.slot(slot));
            }
        }
        // Kind is layered after decoration, so bold cannot erase a link's
        // colour.
        if let Some(slot) = extra {
            style.overlay(&self.slot(slot));
        }
        span_of(text, &style, hidden)
    }

    /// One standalone span, for cases with a single decoration such as a
    /// list marker.
    fn dressed(&self, text: String, slot: &'static str, hidden: bool) -> Span {
        span_of(text, &self.slot(slot), hidden)
    }
}

fn span_of(text: String, style: &Style, hidden: bool) -> Span {
    let d = style.decoration.unwrap_or_default();
    Span {
        text,
        font: style.font.clone(),
        color: style.color,
        line: Line {
            under: d.underline,
            through: d.strikethrough,
        },
        hidden,
    }
}

/// Maps a fence's info string to a slot.
///
/// Slots must be static names, so unknown languages fall back to `code`. A
/// theme colouring by language only writes rules for the ones it knows.
fn lang_slot(lang: Option<&str>) -> &'static str {
    const KNOWN: &[&str] = &[
        "rust", "js", "ts", "python", "json", "html", "css", "sh", "sql", "go", "java", "c", "cpp",
        "diff", "yaml", "toml",
    ];
    let Some(l) = lang else { return "code" };
    let l = l.to_ascii_lowercase();
    KNOWN.iter().find(|k| **k == l).copied().unwrap_or("code")
}

/// An unresolved mention never shows the raw id: `<@123>` is not what the
/// author typed, and `@123` is a lie.
fn mention_text(m: Mention, names: &dyn Names) -> String {
    match m {
        Mention::User(id) => match names.user(id) {
            Some(n) => format!("@{n}"),
            None => "@不明なユーザー".to_owned(),
        },
        Mention::Channel(id) => match names.channel(id) {
            Some(n) => format!("#{n}"),
            None => "#不明なチャンネル".to_owned(),
        },
        Mention::Role(id) => match names.role(id) {
            Some(n) => format!("@{n}"),
            None => "@不明な役職".to_owned(),
        },
        Mention::Everyone => "@everyone".to_owned(),
        Mention::Here => "@here".to_owned(),
    }
}

/// Renders a `<t:…>` and reports how long that rendering stays valid.
///
/// The relative form changes on its own: rendered once and then left alone,
/// "just now" would sit there for hours, while redrawing every second would
/// mean never sleeping. Knowing when the text next changes allows sleeping
/// until exactly then — a minute for "3 minutes ago", a day for "3 days ago".
///
/// Absolute forms return `None`: nothing to wake for.
///
/// `now` is an argument. Reading the clock here would let time move within a
/// frame, making adjacent timestamps disagree, and would make this untestable.
fn stamp(now: i64, at: i64, format: char) -> (String, Option<i64>) {
    if format == 'R' {
        let (text, holds) = relative(now, at);
        return (text, Some(holds));
    }

    let local = at + gumicord_platform::local_utc_offset_minutes() as i64 * 60;
    // `div_euclid` to avoid negative remainders: timestamps before 1970 are
    // typeable, and a time zone can push the date back a day.
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (secs / 3600, (secs / 60) % 60, secs % 60);

    // Unknown formats fall back to `f`: the parser accepted it, so emitting
    // nothing would delete the timestamp from the body.
    let text = match format {
        't' => format!("{h:02}:{min:02}"),
        'T' => format!("{h:02}:{min:02}:{s:02}"),
        'd' => format!("{y:04}/{m:02}/{d:02}"),
        'D' => format!("{y}年{m}月{d}日"),
        // Only `F` carries the weekday.
        'F' => format!(
            "{y}年{m}月{d}日({}) {h:02}:{min:02}",
            WEEKDAYS[weekday(days)]
        ),
        _ => format!("{y}年{m}月{d}日 {h:02}:{min:02}"),
    };
    (text, None)
}

/// Weekday names, Sunday first, matching [`weekday`].
const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

/// Days since 1970-01-01 to a weekday, `0` being Sunday.
///
/// The `+4` is because 1970-01-01 was a Thursday.
fn weekday(days: i64) -> usize {
    (days + 4).rem_euclid(7) as usize
}

/// The relative rendering of a `<t:…:R>`, and how long it stays valid.
///
/// Validity is always at least one second: returning zero exactly on a
/// boundary would spin between drawing and waking.
///
/// Months and years are approximated as 30 and 365 days. A relative timestamp
/// conveys roughly when, and `<t:…:D>` exists for exact dates; using a real
/// calendar here would make "a month ago" mean anything from 28 to 31 days.
fn relative(now: i64, at: i64) -> (String, i64) {
    /// Longest sleep to promise. "3 years ago" next changes in a year, but
    /// the window will close long before that.
    const MAX_SLEEP: i64 = 3_600;

    let diff = now - at;
    let ago = diff >= 0;
    let n = diff.unsigned_abs() as i64;

    // (seconds per unit, unit name)
    let (unit, name) = match n {
        0..60 => (1, "秒"),
        60..3_600 => (60, "分"),
        3_600..86_400 => (3_600, "時間"),
        86_400..2_592_000 => (86_400, "日"),
        2_592_000..31_536_000 => (2_592_000, "か月"),
        _ => (31_536_000, "年"),
    };

    let count = n / unit;
    let text = format!("{count} {name}{}", if ago { "前" } else { "後" });

    // The text next changes at the current unit's boundary. Future times
    // count down instead of up, so the boundary is taken the other way.
    let holds = if ago {
        unit - (n % unit)
    } else {
        // "in 0 seconds" becomes "0 seconds ago" at the crossing.
        let r = n % unit;
        if r == 0 { unit } else { r }
    };
    (text, holds.clamp(1, MAX_SLEEP))
}

/// Days since 1970-01-01 to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, used verbatim rather than writing leap
/// year rules by hand. It shifts the origin to March so leap days fall at the
/// end of the year.
///
/// <https://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_theme::Theme;

    struct NoNames;
    impl Names for NoNames {
        fn user(&self, _: u64) -> Option<String> {
            None
        }
        fn channel(&self, _: u64) -> Option<String> {
            None
        }
        fn role(&self, _: u64) -> Option<String> {
            None
        }
    }

    struct Known;
    impl Names for Known {
        fn user(&self, _: u64) -> Option<String> {
            Some("みどり".to_owned())
        }
        fn channel(&self, _: u64) -> Option<String> {
            Some("雑談".to_owned())
        }
        fn role(&self, _: u64) -> Option<String> {
            Some("管理".to_owned())
        }
    }

    /// "Now" for the tests: 2023-11-15 06:13:20 UTC. Fixed, so results do
    /// not depend on when the suite runs.
    const NOW: i64 = 1_700_000_000;

    fn ink_with(json: &str) -> Theme {
        let r = Theme::parse(json);
        assert!(r.errors().next().is_none(), "テーマが読めない");
        r.theme.expect("テーマ")
    }

    fn spans_of(theme: Option<&Theme>, src: &str, names: &dyn Names) -> Vec<Span> {
        let ink = Ink::new(theme, MatchContext::new(1000.0), false, NOW);
        let blocks = gumicord_markdown::parse(src);
        match blocks.first() {
            Some(gumicord_markdown::Block::Paragraph(c)) => ink.spans(c, names),
            other => panic!("段落ではない {other:?}"),
        }
    }

    /// Confirms the client holds no opinion about what bold looks like.
    #[test]
    fn without_a_theme_nothing_is_decorated() {
        let spans = spans_of(None, "**太い**", &NoNames);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "太い");
        assert_eq!(spans[0].font, None, "クライアントが書体を決めている");
        assert_eq!(spans[0].color, None);
        assert!(!spans[0].line.any());
    }

    /// 太さを決めるのはテーマである
    #[test]
    fn the_theme_decides_how_heavy_bold_is() {
        let t = ink_with(
            r#"{ "manifest": { "id": "t.t", "name": "T", "version": "1.0.0", "abi": 1 }, "rules": [
                { "select": "primitive.text", "when": { "slot": "bold" },
                  "style": { "font": { "weight": 900 } } }
            ]}"#,
        );
        let spans = spans_of(Some(&t), "**太い**", &NoNames);
        assert_eq!(spans[0].font.as_ref().and_then(|f| f.weight), Some(900));
    }

    /// ⚠️ 線を引くかどうかもテーマの判断である。
    /// `__a__` を色で表すテーマがあってよい
    #[test]
    fn the_theme_decides_whether_to_underline() {
        let none = ink_with(
            r#"{ "manifest": { "id": "t.t", "name": "T", "version": "1.0.0", "abi": 1 }, "rules": [] }"#,
        );
        assert!(!spans_of(Some(&none), "__a__", &NoNames)[0].line.under);

        let lined = ink_with(
            r#"{ "manifest": { "id": "t.t", "name": "T", "version": "1.0.0", "abi": 1 }, "rules": [
                { "select": "primitive.text", "when": { "slot": "underline" },
                  "style": { "decoration": "underline" } }
            ]}"#,
        );
        assert!(spans_of(Some(&lined), "__a__", &NoNames)[0].line.under);
    }

    /// 重なった飾りは、両方のルールが乗ること
    #[test]
    fn stacked_decorations_both_apply() {
        let t = ink_with(
            r#"{ "manifest": { "id": "t.t", "name": "T", "version": "1.0.0", "abi": 1 }, "rules": [
                { "select": "primitive.text", "when": { "slot": "bold" },
                  "style": { "font": { "weight": 700 } } },
                { "select": "primitive.text", "when": { "slot": "strike" },
                  "style": { "decoration": "strikethrough" } }
            ]}"#,
        );
        let spans = spans_of(Some(&t), "**~~a~~**", &NoNames);
        assert_eq!(spans[0].font.as_ref().and_then(|f| f.weight), Some(700));
        assert!(spans[0].line.through);
    }

    /// ⚠️ **引けなかったときに番号を出さない。**
    /// `<@1>` は打った人が書いた文字ではないし、`@1` は嘘である
    #[test]
    fn an_unresolved_mention_shows_unknown_not_a_number() {
        let spans = spans_of(None, "<@1> <#2> <@&3>", &NoNames);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "@不明なユーザー #不明なチャンネル @不明な役職");
        assert!(!text.contains('1'), "番号が出ている: {text}");
    }

    #[test]
    fn a_resolved_mention_shows_the_name() {
        let spans = spans_of(None, "<@1> <#2> <@&3>", &Known);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "@みどり #雑談 @管理");
    }

    /// スポイラーは**場所を空けたまま**隠す。詰めて描くと、開いた瞬間に
    /// 行の折り返しが変わって本文が飛び跳ねる
    #[test]
    fn a_spoiler_stays_hidden_until_revealed() {
        let hidden = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        let shown = Ink::new(None, MatchContext::new(1000.0), true, NOW);
        let blocks = gumicord_markdown::parse("||秘密||");
        let gumicord_markdown::Block::Paragraph(c) = &blocks[0] else {
            panic!("段落ではない");
        };

        let h = hidden.spans(c, &NoNames);
        assert!(h[0].hidden);
        assert_eq!(h[0].text, "秘密", "隠しても場所は空けたままである");

        assert!(!shown.spans(c, &NoNames)[0].hidden);
    }

    /// Fenced contents are not decorated.
    #[test]
    fn a_fenced_block_keeps_its_contents_verbatim() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        let nodes = ink.blocks(&gumicord_markdown::parse("```rust\n**a**\n```"), &NoNames);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, NodeId::PrimitiveCodeBlock);
        assert_eq!(nodes[0].content.as_text(), Some("**a**"));
    }

    /// An unknown language must fall back to `code` rather than fail.
    #[test]
    fn an_unknown_language_falls_back_to_the_default_slot() {
        assert_eq!(lang_slot(Some("rust")), "rust");
        assert_eq!(lang_slot(Some("RUST")), "rust");
        assert_eq!(lang_slot(Some("brainfuck")), "code");
        assert_eq!(lang_slot(None), "code");
    }

    /// Survives dates before 1970 and leap days.
    #[test]
    fn the_date_conversion_survives_leap_years_and_negative_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2000 年は閏年 (400 で割り切れる)
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
        // 1900 年は閏年ではない (100 で割り切れて 400 で割り切れない)
        assert_eq!(civil_from_days(-25508), (1900, 3, 1));
        assert_eq!(civil_from_days(19584), (2023, 8, 15));
    }

    // ═══════════════════════════════════════════════════════════════
    //  `<t:…>` (`FR-021`)

    /// 1970-01-01 was a Thursday; getting this wrong shifts every weekday.
    #[test]
    fn the_weekday_counts_from_a_thursday() {
        assert_eq!(WEEKDAYS[weekday(0)], "木");
        assert_eq!(WEEKDAYS[weekday(1)], "金");
        // 1969-12-31 は水曜。**負の日数でも崩れないこと**
        assert_eq!(WEEKDAYS[weekday(-1)], "水");
        // 2023-11-15 は水曜
        assert_eq!(WEEKDAYS[weekday(19_676)], "水");
    }

    /// Only what holds regardless of this machine's time zone is asserted.
    #[test]
    fn each_format_renders_differently() {
        let at = |f| stamp(NOW, NOW, f).0;

        // Time only.
        assert!(!at('t').contains('年'), "{}", at('t'));
        assert_eq!(at('t').len(), 5, "hh:mm のはず: {}", at('t'));
        assert_eq!(at('T').len(), 8, "hh:mm:ss のはず: {}", at('T'));

        // Date only.
        assert!(!at('d').contains(':'), "{}", at('d'));
        assert!(!at('D').contains(':'), "{}", at('D'));
        assert!(at('D').contains('年') && at('D').contains('日'));

        // Both.
        assert!(at('f').contains('年') && at('f').contains(':'));
        assert!(at('F').contains('年') && at('F').contains(':'));

        // Only `F` carries the weekday.
        let has_weekday = |s: &str| WEEKDAYS.iter().any(|w| s.contains(&format!("({w})")));
        assert!(has_weekday(&at('F')), "{}", at('F'));
        assert!(!has_weekday(&at('f')), "{}", at('f'));
    }

    /// The parser accepted it, so emitting nothing would delete the
    /// timestamp from the body.
    #[test]
    fn an_unknown_format_renders_as_the_default() {
        assert_eq!(stamp(NOW, NOW, 'z').0, stamp(NOW, NOW, 'f').0);
    }

    /// Absolute forms never change, so nothing has to wake for them.
    #[test]
    fn an_absolute_timestamp_asks_for_no_redraw() {
        for f in ['t', 'T', 'd', 'D', 'f', 'F', 'z'] {
            assert_eq!(stamp(NOW, NOW, f).1, None, "{f} asked for a redraw");
        }
    }

    /// The relative form steps up through units.
    #[test]
    fn a_relative_timestamp_steps_up_through_units() {
        let ago = |secs: i64| relative(NOW, NOW - secs).0;

        assert_eq!(ago(0), "0 秒前");
        assert_eq!(ago(59), "59 秒前");
        assert_eq!(ago(60), "1 分前");
        assert_eq!(ago(3_599), "59 分前");
        assert_eq!(ago(3_600), "1 時間前");
        assert_eq!(ago(86_399), "23 時間前");
        assert_eq!(ago(86_400), "1 日前");
        assert_eq!(ago(2_591_999), "29 日前");
        assert_eq!(ago(2_592_000), "1 か月前");
        assert_eq!(ago(31_535_999), "12 か月前");
        assert_eq!(ago(31_536_000), "1 年前");
    }

    /// Discord allows future timestamps.
    #[test]
    fn a_future_timestamp_reads_as_from_now() {
        assert_eq!(relative(NOW, NOW + 90).0, "1 分後");
        assert_eq!(relative(NOW, NOW + 86_400 * 3).0, "3 日後");
    }

    /// Too early wastes frames; too late leaves "just now" up for hours.
    #[test]
    fn it_sleeps_until_the_text_would_change() {
        let holds = |secs: i64| relative(NOW, NOW - secs).1;

        // Seconds change every second.
        assert_eq!(holds(0), 1);
        assert_eq!(holds(59), 1);
        // "1 minute ago" holds until the next minute.
        assert_eq!(holds(60), 60);
        assert_eq!(holds(90), 30);
        // "1 hour ago" holds until the next hour.
        assert_eq!(holds(3_600), 3_600);
        assert_eq!(holds(3_610), 3_590);
    }

    /// Zero would spin between drawing and waking.
    #[test]
    fn it_never_reports_less_than_one_second() {
        for d in [-100_000i64, -3_600, -60, -1, 0, 1, 59, 60, 3_600, 86_400] {
            let (_, holds) = relative(NOW, NOW - d);
            assert!(
                holds >= 1,
                "a {d} second difference reported {holds} seconds"
            );
        }
    }

    /// "3 years ago" next changes in a year, but the window will close long
    /// before that.
    #[test]
    fn even_the_distant_past_is_revisited_within_an_hour() {
        let (_, holds) = relative(NOW, NOW - 86_400 * 400);
        assert_eq!(holds, 3_600);
    }

    /// One relative timestamp is enough for the tree to know its validity.
    #[test]
    fn a_relative_timestamp_asks_for_a_redraw() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        assert_eq!(
            ink.holds_for(),
            None,
            "asked for a redraw before anything was built"
        );

        ink.blocks(
            &gumicord_markdown::parse(&format!("<t:{}:R>", NOW - 90)),
            &NoNames,
        );
        assert_eq!(
            ink.holds_for(),
            Some(30),
            "should hold until the minute boundary"
        );
    }

    /// Following the slower one would leave the faster one looking frozen.
    #[test]
    fn it_follows_whichever_changes_soonest() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        ink.blocks(
            &gumicord_markdown::parse(&format!(
                "<t:{}:R> と <t:{}:R>",
                NOW - 3_610, // holds 3590 seconds
                NOW - 90,    // holds 30 seconds
            )),
            &NoNames,
        );
        assert_eq!(ink.holds_for(), Some(30));
    }

    /// Absolute timestamps alone give no reason to wake.
    #[test]
    fn absolute_timestamps_alone_never_wake_it() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        ink.blocks(
            &gumicord_markdown::parse(&format!("<t:{NOW}:f> と <t:{NOW}> と ふつうの文")),
            &NoNames,
        );
        assert_eq!(ink.holds_for(), None);
    }
}
