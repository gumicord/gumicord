use super::*;

/// Renders a parse into a readable one-line form: `**a**` becomes `[B]a[/]`.
fn flat(text: &str) -> String {
    let mut s = String::new();
    for b in parse(text) {
        write_block(&b, &mut s);
    }
    s
}

fn write_block(b: &Block, s: &mut String) {
    match b {
        Block::Paragraph(c) => write_inlines(c, s),
        Block::Heading { level, content } => {
            s.push_str(&format!("[H{level}]"));
            write_inlines(content, s);
        }
        Block::Subtext(c) => {
            s.push_str("[sub]");
            write_inlines(c, s);
        }
        Block::Quote(inner) => {
            s.push_str("[quote{");
            for b in inner {
                write_block(b, s);
            }
            s.push_str("}]");
        }
        Block::List(items) => {
            s.push_str("[list{");
            for it in items {
                let m = match it.marker {
                    Marker::Bullet => "*".to_owned(),
                    Marker::Number(n) => format!("{n}."),
                };
                s.push_str(&format!("{}{m}", "  ".repeat(it.depth as usize)));
                write_inlines(&it.content, s);
                s.push(';');
            }
            s.push_str("}]");
        }
        Block::Code { lang, text } => {
            s.push_str(&format!(
                "[code{}{{{text}}}]",
                lang.as_deref().unwrap_or("")
            ));
        }
    }
    s.push('\n');
}

fn write_inlines(c: &[Inline], s: &mut String) {
    for i in c {
        let mut tag = String::new();
        for (flag, ch) in [
            (Deco::BOLD, 'B'),
            (Deco::ITALIC, 'I'),
            (Deco::UNDERLINE, 'U'),
            (Deco::STRIKE, 'S'),
            (Deco::SPOILER, 'X'),
        ] {
            if i.deco.contains(flag) {
                tag.push(ch);
            }
        }
        if !tag.is_empty() {
            s.push_str(&format!("[{tag}]"));
        }
        match &i.kind {
            InlineKind::Text(t) => s.push_str(t),
            InlineKind::Code(t) => s.push_str(&format!("`{t}`")),
            InlineKind::Link { url, label } => match label {
                Some(l) => s.push_str(&format!("[{l}→{url}]")),
                None => s.push_str(&format!("[→{url}]")),
            },
            InlineKind::Mention(m) => s.push_str(&format!("[{m:?}]")),
            InlineKind::Emoji { name, id, animated } => {
                s.push_str(&format!(
                    "[emoji{name}:{id}{}]",
                    if *animated { "*" } else { "" }
                ));
            }
            InlineKind::Timestamp { at, format } => s.push_str(&format!("[time{at}:{format}]")),
            InlineKind::Break => s.push('⏎'),
        }
        // The closer is written too: without it the extent of a decoration
        // never shows up, and a wrong range still passes.
        if !tag.is_empty() {
            s.push_str("[/]");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Decoration

#[test]
fn bold_italic_underline_strike_and_spoiler() {
    assert_eq!(flat("**a**"), "[B]a[/]\n");
    assert_eq!(flat("*a*"), "[I]a[/]\n");
    assert_eq!(flat("_a_"), "[I]a[/]\n");
    assert_eq!(flat("__a__"), "[U]a[/]\n");
    assert_eq!(flat("~~a~~"), "[S]a[/]\n");
    assert_eq!(flat("||a||"), "[X]a[/]\n");
}

#[test]
fn decorations_stack() {
    assert_eq!(flat("***a***"), "[BI]a[/]\n");
    assert_eq!(flat("**~~a~~**"), "[BS]a[/]\n");
    assert_eq!(flat("||**a**||"), "[BX]a[/]\n");
    assert_eq!(flat("___a___"), "[IU]a[/]\n");
}

#[test]
fn a_decoration_joins_the_text_around_it() {
    assert_eq!(flat("前**中**後"), "前[B]中[/]後\n");
}

/// Getting this wrong makes the rest of the body disappear.
#[test]
fn an_unclosed_mark_is_literal_text() {
    assert_eq!(flat("**a"), "**a\n");
    assert_eq!(flat("a ** b"), "a ** b\n");
    assert_eq!(flat("||隠し"), "||隠し\n");
    assert_eq!(flat("~~"), "~~\n");
}

/// `a * b * c` is multiplication, not italic.
#[test]
fn a_mark_does_not_open_on_whitespace() {
    assert_eq!(flat("2 * 3 * 4"), "2 * 3 * 4\n");
    assert_eq!(flat("a ** b ** c"), "a ** b ** c\n");
}

#[test]
fn a_mark_does_not_close_after_whitespace() {
    assert_eq!(flat("*a *b"), "*a *b\n");
}

/// Without this rule `snake_case_word` becomes italic.
#[test]
fn snake_case_words_are_not_italic() {
    assert_eq!(flat("snake_case_word"), "snake_case_word\n");
    assert_eq!(flat("a_b_c"), "a_b_c\n");
    // Outside a word it does open.
    assert_eq!(flat("_a_ b"), "[I]a[/] b\n");
    assert_eq!(flat("(_a_)"), "([I]a[/])\n");
}

#[test]
fn an_asterisk_opens_mid_word() {
    assert_eq!(flat("a*b*c"), "a[I]b[/]c\n");
}

#[test]
fn the_same_decoration_does_not_nest() {
    // The inner `**` closes.
    assert_eq!(flat("**a**b**"), "[B]a[/]b**\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Escapes

#[test]
fn punctuation_can_be_escaped() {
    assert_eq!(flat(r"\*\*a\*\*"), "**a**\n");
    assert_eq!(flat(r"\|\|a\|\|"), "||a||\n");
}

/// Escaping the `n` of `\n` would silently swallow the backslash.
#[test]
fn non_punctuation_is_not_escaped() {
    assert_eq!(flat(r"\n"), "\\n\n");
    assert_eq!(flat(r"C:\Users"), "C:\\Users\n");
}

#[test]
fn an_escaped_mark_cannot_close() {
    assert_eq!(flat(r"**a\*b**"), "[B]a*b[/]\n");
    // One from `\*`, one from the leftover `*`; neither can close.
    assert_eq!(flat(r"**a\**b**"), "[B]a**b[/]\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Code

#[test]
fn inline_code() {
    assert_eq!(flat("`a`"), "`a`\n");
    assert_eq!(flat("``a`b``"), "`a`b`\n");
    assert_eq!(flat("前 `a` 後"), "前 `a` 後\n");
}

/// Marks inside code are literal.
#[test]
fn code_contents_are_not_parsed() {
    assert_eq!(flat("`**a**`"), "`**a**`\n");
    assert_eq!(flat("`<@1>`"), "`<@1>`\n");
}

/// Without this, a mark inside code closes the outer one.
#[test]
fn a_mark_inside_code_does_not_close_the_outer_one() {
    assert_eq!(flat("**a `**` b**"), "[B]a [/][B]`**`[/][B] b[/]\n");
}

#[test]
fn an_unclosed_code_span_is_literal() {
    assert_eq!(flat("`a"), "`a\n");
}

#[test]
fn fenced_code() {
    assert_eq!(flat("```\na\n```"), "[code{a}]\n");
    assert_eq!(flat("```js\na\n```"), "[codejs{a}]\n");
    // Without a newline there is no info string.
    assert_eq!(flat("```js```"), "[code{js}]\n");
}

/// A fence opens mid-line and ends the paragraph there.
#[test]
fn a_fence_can_open_mid_line() {
    assert_eq!(flat("見て ```js\na\n``` ね"), "見て \n[codejs{a}]\n ね\n");
}

/// A `# ` inside a fence is not a heading.
#[test]
fn nothing_inside_a_fence_is_interpreted() {
    assert_eq!(
        flat("```\n# a\n> b\n**c**\n```"),
        "[code{# a\n> b\n**c**}]\n"
    );
}

#[test]
fn an_unclosed_fence_is_literal() {
    assert_eq!(flat("```js\na"), "```js⏎a\n");
}

#[test]
fn blank_lines_inside_a_fence_survive() {
    assert_eq!(flat("```\na\n\nb\n```"), "[code{a\n\nb}]\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Line-leading markers

#[test]
fn headings() {
    assert_eq!(flat("# a"), "[H1]a\n");
    assert_eq!(flat("## a"), "[H2]a\n");
    assert_eq!(flat("### a"), "[H3]a\n");
}

/// `#tag` is not a heading; the space is required.
#[test]
fn a_hash_without_a_space_is_not_a_heading() {
    assert_eq!(flat("#tag"), "#tag\n");
    assert_eq!(flat("#### a"), "#### a\n");
    assert_eq!(flat("# "), "# \n");
}

#[test]
fn subtext() {
    assert_eq!(flat("-# a"), "[sub]a\n");
}

#[test]
fn quotes() {
    assert_eq!(flat("> a"), "[quote{a\n}]\n");
    // Consecutive lines merge.
    assert_eq!(flat("> a\n> b"), "[quote{a⏎b\n}]\n");
    // A non-consecutive line falls outside.
    assert_eq!(flat("> a\nb"), "[quote{a\n}]\nb\n");
}

#[test]
fn a_bare_angle_quotes_a_blank_line() {
    assert_eq!(flat("> a\n>\n> b"), "[quote{a\nb\n}]\n");
}

#[test]
fn a_triple_angle_quotes_the_rest() {
    assert_eq!(flat(">>> a\nb"), "[quote{a⏎b\n}]\n");
}

#[test]
fn quotes_nest() {
    assert_eq!(flat("> > a"), "[quote{[quote{a\n}]\n}]\n");
}

/// Without a depth cap, a thousand `>` characters recurse to the bottom.
#[test]
fn quote_nesting_is_bounded() {
    let deep = "> ".repeat(500) + "a";
    let _ = flat(&deep);
}

#[test]
fn lists() {
    assert_eq!(flat("- a\n- b"), "[list{*a;*b;}]\n");
    assert_eq!(flat("* a"), "[list{*a;}]\n");
    assert_eq!(flat("1. a\n2. b"), "[list{1.a;2.b;}]\n");
}

#[test]
fn indentation_nests_a_list() {
    assert_eq!(flat("- a\n  - b"), "[list{*a;  *b;}]\n");
}

/// Without a digit cap this overflows u32.
#[test]
fn an_overlong_number_is_not_a_list_item() {
    assert_eq!(flat("99999999999. a"), "99999999999. a\n");
}

#[test]
fn a_marker_with_no_content_is_not_a_list_item() {
    assert_eq!(flat("- "), "- \n");
    assert_eq!(flat("-"), "-\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Paragraphs

#[test]
fn a_newline_inside_a_paragraph_survives() {
    assert_eq!(flat("a\nb"), "a⏎b\n");
}

#[test]
fn a_blank_line_ends_a_paragraph() {
    assert_eq!(flat("a\n\nb"), "a\nb\n");
}

#[test]
fn empty_input_yields_nothing() {
    assert_eq!(parse(""), vec![]);
    assert_eq!(parse("\n\n"), vec![]);
}

// ═══════════════════════════════════════════════════════════════════════
//  Mentions, emoji, timestamps

#[test]
fn mentions() {
    assert_eq!(flat("<@1>"), "[User(1)]\n");
    assert_eq!(flat("<@!1>"), "[User(1)]\n");
    assert_eq!(flat("<@&1>"), "[Role(1)]\n");
    assert_eq!(flat("<#1>"), "[Channel(1)]\n");
    assert_eq!(flat("@everyone"), "[Everyone]\n");
    assert_eq!(flat("@here"), "[Here]\n");
}

#[test]
fn a_non_numeric_mention_is_literal() {
    assert_eq!(flat("<@abc>"), "<@abc>\n");
    assert_eq!(flat("<@>"), "<@>\n");
    assert_eq!(flat("a < b > c"), "a < b > c\n");
}

#[test]
fn emoji() {
    assert_eq!(flat("<:neko:1>"), "[emojineko:1]\n");
    assert_eq!(flat("<a:neko:1>"), "[emojineko:1*]\n");
}

#[test]
fn timestamps() {
    assert_eq!(flat("<t:1700000000:R>"), "[time1700000000:R]\n");
    // No format suffix means the default.
    assert_eq!(flat("<t:1700000000>"), "[time1700000000:f]\n");
    // An unknown format stays literal.
    assert_eq!(flat("<t:1:z>"), "<t:1:z>\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Links

#[test]
fn bare_links() {
    assert_eq!(flat("https://example.com"), "[→https://example.com]\n");
    assert_eq!(
        flat("見て http://a.b/c だよ"),
        "見て [→http://a.b/c] だよ\n"
    );
}

/// Swallowing sentence-final punctuation breaks the link.
#[test]
fn sentence_final_punctuation_stays_out_of_a_link() {
    assert_eq!(flat("https://a.b/c。"), "[→https://a.b/c]。\n");
    assert_eq!(flat("(https://a.b/c)"), "([→https://a.b/c])\n");
    assert_eq!(flat("https://a.b/c!"), "[→https://a.b/c]!\n");
}

#[test]
fn a_link_does_not_start_mid_word() {
    assert_eq!(flat("ahttps://a.b"), "ahttps://a.b\n");
}

#[test]
fn masked_links() {
    assert_eq!(flat("[ここ](https://a.b)"), "[ここ→https://a.b]\n");
}

#[test]
fn a_masked_link_needs_a_url_target() {
    assert_eq!(flat("[a](b)"), "[a](b)\n");
    assert_eq!(flat("[a]"), "[a]\n");
}

#[test]
fn angle_wrapped_links() {
    assert_eq!(flat("<https://a.b>"), "[→https://a.b]\n");
    // Not split in the middle.
    assert_eq!(flat("<https://a.b/x_y>"), "[→https://a.b/x_y]\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Malformed input

/// Bodies are strings other people typed; not panicking is the requirement.
#[test]
fn malformed_input_does_not_panic() {
    let ugly = [
        "***",
        "****",
        "```",
        "``````",
        "||||",
        "~~~~",
        "<@",
        "<:::>",
        "[](",
        "> > > > > > > > > > a",
        "- - - - -",
        "\\",
        "*_~|`<>[]()#-",
        "**a*b**c*",
        "`` ` ``",
        "\u{0}\u{1}\u{feff}",
    ];
    for s in ugly {
        let _ = parse(s);
    }
}

/// Never split inside a multi-byte character.
#[test]
fn multibyte_text_is_never_split_mid_character() {
    assert_eq!(flat("**あい**うえ"), "[B]あい[/]うえ\n");
    assert_eq!(flat("🎉**あ**🎉"), "🎉[B]あ[/]🎉\n");
    assert_eq!(flat("あ_い_う"), "あ_い_う\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  Combinations

#[test]
fn a_realistic_message_body() {
    let src = "# お知らせ\n\
               <@123> さん、**明日** の会議は <#456> です。\n\
               -# 変更は <t:1700000000:R>\n\
               \n\
               - `cargo test` を通す\n\
               - ~~急ぐ~~ 落ち着いてやる\n\
               \n\
               > 詳しくは https://example.com/doc\n\
               \n\
               ```rust\n\
               fn main() {}\n\
               ```";
    assert_eq!(
        flat(src),
        "[H1]お知らせ\n\
         [User(123)] さん、[B]明日[/] の会議は [Channel(456)] です。\n\
         [sub]変更は [time1700000000:R]\n\
         [list{*`cargo test` を通す;*[S]急ぐ[/] 落ち着いてやる;}]\n\
         [quote{詳しくは [→https://example.com/doc]\n}]\n\
         [coderust{fn main() {}}]\n"
    );
}
