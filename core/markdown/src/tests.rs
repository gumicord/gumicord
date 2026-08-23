use super::*;

/// 飾りの付いた文だけを、読める形にして並べる。
///
/// `**a**` → `[B]a`。飾りの無い文字は素で出る
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
            s.push_str("[小]");
            write_inlines(c, s);
        }
        Block::Quote(inner) => {
            s.push_str("[引用{");
            for b in inner {
                write_block(b, s);
            }
            s.push_str("}]");
        }
        Block::List(items) => {
            s.push_str("[箇条{");
            for it in items {
                let m = match it.marker {
                    Marker::Bullet => "・".to_owned(),
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
                "[コード{}{{{text}}}]",
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
                    "[絵{name}:{id}{}]",
                    if *animated { "*" } else { "" }
                ));
            }
            InlineKind::Timestamp { at, format } => s.push_str(&format!("[時{at}:{format}]")),
            InlineKind::Break => s.push('⏎'),
        }
        // ⚠️ **閉じも書く。** 書かないと「どこで太字が終わったか」が
        // 出力に現れず、飾りの範囲が間違っていてもテストが通ってしまう
        if !tag.is_empty() {
            s.push_str("[/]");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  飾り

#[test]
fn 太字と斜体と下線と打ち消しとスポイラー() {
    assert_eq!(flat("**a**"), "[B]a[/]\n");
    assert_eq!(flat("*a*"), "[I]a[/]\n");
    assert_eq!(flat("_a_"), "[I]a[/]\n");
    assert_eq!(flat("__a__"), "[U]a[/]\n");
    assert_eq!(flat("~~a~~"), "[S]a[/]\n");
    assert_eq!(flat("||a||"), "[X]a[/]\n");
}

#[test]
fn 飾りは重なる() {
    assert_eq!(flat("***a***"), "[BI]a[/]\n");
    assert_eq!(flat("**~~a~~**"), "[BS]a[/]\n");
    assert_eq!(flat("||**a**||"), "[BX]a[/]\n");
    assert_eq!(flat("___a___"), "[IU]a[/]\n");
}

#[test]
fn 飾りの外と中が繋がる() {
    assert_eq!(flat("前**中**後"), "前[B]中[/]後\n");
}

/// ⚠️ **閉じない印は文字である。** ここが崩れると本文が丸ごと消える
#[test]
fn 閉じない印はただの文字() {
    assert_eq!(flat("**a"), "**a\n");
    assert_eq!(flat("a ** b"), "a ** b\n");
    assert_eq!(flat("||隠し"), "||隠し\n");
    assert_eq!(flat("~~"), "~~\n");
}

/// `a * b * c` は掛け算であって斜体ではない
#[test]
fn 中身が空白で始まる印は開かない() {
    assert_eq!(flat("2 * 3 * 4"), "2 * 3 * 4\n");
    assert_eq!(flat("a ** b ** c"), "a ** b ** c\n");
}

#[test]
fn 中身が空白で終わる印も開かない() {
    assert_eq!(flat("*a *b"), "*a *b\n");
}

/// ⚠️ ここが無いと `snake_case_word` が斜体になる
#[test]
fn 下線付きの語は斜体にならない() {
    assert_eq!(flat("snake_case_word"), "snake_case_word\n");
    assert_eq!(flat("a_b_c"), "a_b_c\n");
    // 語の外なら開く
    assert_eq!(flat("_a_ b"), "[I]a[/] b\n");
    assert_eq!(flat("(_a_)"), "([I]a[/])\n");
}

#[test]
fn 星は語の途中でも開く() {
    assert_eq!(flat("a*b*c"), "a[I]b[/]c\n");
}

#[test]
fn 同じ飾りは入れ子にしない() {
    // 内側の `**` は閉じに使われる
    assert_eq!(flat("**a**b**"), "[B]a[/]b**\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  逃がし

#[test]
fn 記号は逃がせる() {
    assert_eq!(flat(r"\*\*a\*\*"), "**a**\n");
    assert_eq!(flat(r"\|\|a\|\|"), "||a||\n");
}

/// ⚠️ `\n` の `n` を文字にすると、打った `\` が黙って消える
#[test]
fn 記号でない字は逃がさない() {
    assert_eq!(flat(r"\n"), "\\n\n");
    assert_eq!(flat(r"C:\Users"), "C:\\Users\n");
}

#[test]
fn 逃がした印は閉じに使われない() {
    assert_eq!(flat(r"**a\*b**"), "[B]a*b[/]\n");
    // `\*` で 1 つ、余った `*` で 1 つ。**どちらも閉じには使わない**
    assert_eq!(flat(r"**a\**b**"), "[B]a**b[/]\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  コード

#[test]
fn 行の中のコード() {
    assert_eq!(flat("`a`"), "`a`\n");
    assert_eq!(flat("``a`b``"), "`a`b`\n");
    assert_eq!(flat("前 `a` 後"), "前 `a` 後\n");
}

/// ⚠️ コードの中の印は文字である
#[test]
fn コードの中は解析しない() {
    assert_eq!(flat("`**a**`"), "`**a**`\n");
    assert_eq!(flat("`<@1>`"), "`<@1>`\n");
}

/// ⚠️ ここが無いと、コードの中の印が外の印を閉じる
#[test]
fn コードの中の印は外を閉じない() {
    assert_eq!(flat("**a `**` b**"), "[B]a [/][B]`**`[/][B] b[/]\n");
}

#[test]
fn 閉じないコードはただの記号() {
    assert_eq!(flat("`a"), "`a\n");
}

#[test]
fn コードブロック() {
    assert_eq!(flat("```\na\n```"), "[コード{a}]\n");
    assert_eq!(flat("```js\na\n```"), "[コードjs{a}]\n");
    // 改行が無ければ言語名ではない
    assert_eq!(flat("```js```"), "[コード{js}]\n");
}

/// ⚠️ **行の途中から始まる。** ここで段落が切れる
#[test]
fn コードブロックは行の途中からでも始まる() {
    assert_eq!(flat("見て ```js\na\n``` ね"), "見て \n[コードjs{a}]\n ね\n");
}

/// ⚠️ コードブロックの中の `# ` を見出しにしてはいけない
#[test]
fn コードブロックの中は何も解釈しない() {
    assert_eq!(
        flat("```\n# a\n> b\n**c**\n```"),
        "[コード{# a\n> b\n**c**}]\n"
    );
}

#[test]
fn 閉じないコードブロックはただの記号() {
    assert_eq!(flat("```js\na"), "```js⏎a\n");
}

#[test]
fn コードブロックの中の空行は残る() {
    assert_eq!(flat("```\na\n\nb\n```"), "[コード{a\n\nb}]\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  行の頭の印

#[test]
fn 見出し() {
    assert_eq!(flat("# a"), "[H1]a\n");
    assert_eq!(flat("## a"), "[H2]a\n");
    assert_eq!(flat("### a"), "[H3]a\n");
}

/// ⚠️ `#tag` は見出しではない。空白が要る
#[test]
fn 空白の無い井桁は見出しにしない() {
    assert_eq!(flat("#tag"), "#tag\n");
    assert_eq!(flat("#### a"), "#### a\n");
    assert_eq!(flat("# "), "# \n");
}

#[test]
fn 小さい注釈() {
    assert_eq!(flat("-# a"), "[小]a\n");
}

#[test]
fn 引用() {
    assert_eq!(flat("> a"), "[引用{a\n}]\n");
    // 続く行はまとまる
    assert_eq!(flat("> a\n> b"), "[引用{a⏎b\n}]\n");
    // 続かない行は外へ出る
    assert_eq!(flat("> a\nb"), "[引用{a\n}]\nb\n");
}

#[test]
fn 大なりだけの行は引用の空行() {
    assert_eq!(flat("> a\n>\n> b"), "[引用{a\nb\n}]\n");
}

#[test]
fn 三連の大なりは以降ぜんぶ() {
    assert_eq!(flat(">>> a\nb"), "[引用{a⏎b\n}]\n");
}

#[test]
fn 引用は入れ子になる() {
    assert_eq!(flat("> > a"), "[引用{[引用{a\n}]\n}]\n");
}

/// ⚠️ 深さに上限が無いと、`>` を千個並べた本文で潜り切る
#[test]
fn 引用の深さには上限がある() {
    let deep = "> ".repeat(500) + "a";
    let _ = flat(&deep);
}

#[test]
fn 箇条書き() {
    assert_eq!(flat("- a\n- b"), "[箇条{・a;・b;}]\n");
    assert_eq!(flat("* a"), "[箇条{・a;}]\n");
    assert_eq!(flat("1. a\n2. b"), "[箇条{1.a;2.b;}]\n");
}

#[test]
fn 箇条書きは字下げで潜る() {
    assert_eq!(flat("- a\n  - b"), "[箇条{・a;  ・b;}]\n");
}

/// ⚠️ 桁を数えないと `u32` が溢れる
#[test]
fn 長すぎる番号は箇条書きにしない() {
    assert_eq!(flat("99999999999. a"), "99999999999. a\n");
}

#[test]
fn 中身の無い印は箇条書きにしない() {
    assert_eq!(flat("- "), "- \n");
    assert_eq!(flat("-"), "-\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  段落

#[test]
fn 段落の中の改行は残る() {
    assert_eq!(flat("a\nb"), "a⏎b\n");
}

#[test]
fn 空行は段落を切る() {
    assert_eq!(flat("a\n\nb"), "a\nb\n");
}

#[test]
fn 空の本文からは何も出ない() {
    assert_eq!(parse(""), vec![]);
    assert_eq!(parse("\n\n"), vec![]);
}

// ═══════════════════════════════════════════════════════════════════════
//  メンションと絵文字と時刻

#[test]
fn メンション() {
    assert_eq!(flat("<@1>"), "[User(1)]\n");
    assert_eq!(flat("<@!1>"), "[User(1)]\n");
    assert_eq!(flat("<@&1>"), "[Role(1)]\n");
    assert_eq!(flat("<#1>"), "[Channel(1)]\n");
    assert_eq!(flat("@everyone"), "[Everyone]\n");
    assert_eq!(flat("@here"), "[Here]\n");
}

#[test]
fn 番号でないメンションは文字() {
    assert_eq!(flat("<@abc>"), "<@abc>\n");
    assert_eq!(flat("<@>"), "<@>\n");
    assert_eq!(flat("a < b > c"), "a < b > c\n");
}

#[test]
fn 絵文字() {
    assert_eq!(flat("<:neko:1>"), "[絵neko:1]\n");
    assert_eq!(flat("<a:neko:1>"), "[絵neko:1*]\n");
}

#[test]
fn 時刻() {
    assert_eq!(flat("<t:1700000000:R>"), "[時1700000000:R]\n");
    // 書式が無ければ既定
    assert_eq!(flat("<t:1700000000>"), "[時1700000000:f]\n");
    // 知らない書式は文字のまま
    assert_eq!(flat("<t:1:z>"), "<t:1:z>\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  リンク

#[test]
fn 裸のリンク() {
    assert_eq!(flat("https://example.com"), "[→https://example.com]\n");
    assert_eq!(
        flat("見て http://a.b/c だよ"),
        "見て [→http://a.b/c] だよ\n"
    );
}

/// ⚠️ 文末の記号まで URL に入れるとリンクが切れる
#[test]
fn 文末の記号はリンクに入れない() {
    assert_eq!(flat("https://a.b/c。"), "[→https://a.b/c]。\n");
    assert_eq!(flat("(https://a.b/c)"), "([→https://a.b/c])\n");
    assert_eq!(flat("https://a.b/c!"), "[→https://a.b/c]!\n");
}

#[test]
fn 語の途中はリンクにしない() {
    assert_eq!(flat("ahttps://a.b"), "ahttps://a.b\n");
}

#[test]
fn 名前付きリンク() {
    assert_eq!(flat("[ここ](https://a.b)"), "[ここ→https://a.b]\n");
}

#[test]
fn 行き先が_url_でなければ名前付きリンクにしない() {
    assert_eq!(flat("[a](b)"), "[a](b)\n");
    assert_eq!(flat("[a]"), "[a]\n");
}

#[test]
fn 山括弧で囲んだリンク() {
    assert_eq!(flat("<https://a.b>"), "[→https://a.b]\n");
    // 中で切らない
    assert_eq!(flat("<https://a.b/x_y>"), "[→https://a.b/x_y]\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  壊れた入力で落ちない

/// ⚠️ 本文は他人が打った文字列である。**落ちないことが要件である**
#[test]
fn 壊れた入力でも落ちない() {
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

/// 多バイト文字の途中で切らない
#[test]
fn 日本語と絵文字で境目を割らない() {
    assert_eq!(flat("**あい**うえ"), "[B]あい[/]うえ\n");
    assert_eq!(flat("🎉**あ**🎉"), "🎉[B]あ[/]🎉\n");
    assert_eq!(flat("あ_い_う"), "あ_い_う\n");
}

// ═══════════════════════════════════════════════════════════════════════
//  組み合わせ

#[test]
fn 実際に来そうな本文() {
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
         [小]変更は [時1700000000:R]\n\
         [箇条{・`cargo test` を通す;・[S]急ぐ[/] 落ち着いてやる;}]\n\
         [引用{詳しくは [→https://example.com/doc]\n}]\n\
         [コードrust{fn main() {}}]\n"
    );
}
