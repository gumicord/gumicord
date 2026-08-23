//! 本文を UITree にする (`FR-021`)。
//!
//! 解析そのものは [`gumicord_markdown`] が済ませている。ここがやるのは
//! **その結果を、テーマが決めた見た目に着せ替えてノードにする**ことだけである。
//!
//! # ⚠️ 行の中の飾りはノードにしない
//!
//! 太字を別のノードにして横に並べる、は動かない。並べたノードは
//! **それぞれが独立して折り返す**ので、`これは **とても長い** 文章` の
//! 行末が合わなくなる ([`spec/06-renderer.md`] 6.6)。
//!
//! だから行の中は [`Span`] の並びにして 1 つのノードへ入れ、
//! **縦に積まれるもの** — 段落・見出し・引用・箇条書き・コードブロック —
//! だけをノードにする。
//!
//! # ⚠️ 太字を何で表すかを、ここで決めない
//!
//! 「太字である」は解析の結果だが、「太さ 700 である」はテーマの判断である。
//! ここは `primitive.text` の `when.slot` を引いて、**テーマが書いた値を
//! そのまま運ぶ**。テーマが何も書かなければ何も変わらない。
//!
//! ```json
//! { "select": "primitive.text", "when": { "slot": "bold" },
//!   "style": { "font": { "weight": 700 } } }
//! ```
//!
//! | slot | いつ |
//! |---|---|
//! | `bold` `italic` `underline` `strike` | `**` `*` `__` `~~` |
//! | `spoiler` | `\|\|` |
//! | `code` | `` ` `` (行の中) |
//! | `link` | リンクと裸の URL |
//! | `mention` | `<@1>` `<#1>` `<@&1>` `@everyone` |
//! | `h1` `h2` `h3` `subtext` | 見出しと `-# ` |
//! | `quote_bar` | 引用の左の線 |
//! | `bullet` | 箇条書きの印 |
//!
//! # ⚠️ 名前が引けなくても、番号を出さない
//!
//! `<@123>` の相手を知らないことは普通にある。そのときに `<@123>` と
//! 出すのは**打った人が書いた文字ではない**し、`@123` は嘘である。
//! `@不明なユーザー` と出す。

use gumicord_markdown::{Block, Deco, Inline, InlineKind, Item, Marker, Mention};
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::{Content, Key, Line, NodeId, Span, Style, UiNode};

/// 箇条書きの深さごとの slot。
///
/// ⚠️ **字下げの幅をここで決めない。** どれだけ下げるかは見た目の判断で
/// あり、テーマの領分である。ここが渡すのは「何段目か」だけ。
/// slot は静的な文字列でなければならないので、並べて持つ
const DEPTH: &[&str] = &["li0", "li1", "li2", "li3", "li4"];

/// 飾りを、テーマが決めた見た目へ翻訳するところ。
pub struct Ink<'a> {
    theme: Option<&'a Theme>,
    ctx: MatchContext,
    /// スポイラーを開けてあるか。**メッセージ単位である**
    ///
    /// ⚠️ 走りごとに開けるには走りごとの当たり判定が要り、それは走りを
    /// ノードにすることを意味する。それはできない (モジュールの説明を見よ)。
    /// M1 は「押したらそのメッセージのスポイラーが全部開く」で通す
    revealed: bool,
}

/// 名前を引く相手。**アプリの一覧を知っているのは呼ぶ側である。**
pub trait Names {
    fn user(&self, id: u64) -> Option<String>;
    fn channel(&self, id: u64) -> Option<String>;
    fn role(&self, id: u64) -> Option<String>;
}

impl<'a> Ink<'a> {
    pub fn new(theme: Option<&'a Theme>, ctx: MatchContext, revealed: bool) -> Self {
        Ink {
            theme,
            ctx,
            revealed,
        }
    }

    /// `primitive.text` のその slot に、テーマが書いたもの。
    ///
    /// テーマが無ければ空である。**空は「何も変えない」という意味**であり、
    /// 誤りではない
    fn slot(&self, slot: &'static str) -> Style {
        match self.theme {
            Some(t) => t.style_for(NodeId::PrimitiveText, &self.ctx.with_slot(Some(slot))),
            None => Style::default(),
        }
    }

    /// 本文を、縦に積むノードの並びにする。
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
            // ⚠️ **中身は飾らない。** コードの中の `**` は文字である
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
        // ⚠️ 字下げは印ではなく**行そのもの**に付ける。印に付けると、
        // 折り返した 2 行目が印の下へ潜り込む
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

    /// 行の中を [`Span`] の並びにする。
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
            // 絵文字は絵である。**取ってくるまでは名前で出す** —
            // 何も出さないと、絵文字だけの本文が空になる
            InlineKind::Emoji { name, .. } => (format!(":{name}:"), Some("mention")),
            InlineKind::Timestamp { at, format } => (stamp(*at, *format), Some("mention")),
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
        // 種類は飾りより後に重ねる。リンクの色が太字に消されないため
        if let Some(slot) = extra {
            style.overlay(&self.slot(slot));
        }
        span_of(text, &style, hidden)
    }

    /// 単独の走りを 1 つ作る。箇条書きの印のように、飾りが 1 つだけのとき
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

/// ```` ```rust ```` の `rust` を slot にする。
///
/// ⚠️ **静的な名前しか slot になれない。** 知らない言語は `code` に落ちる。
/// 言語ごとに色を変えたいテーマは、知っている言語ぶんだけ書けばよい
fn lang_slot(lang: Option<&str>) -> &'static str {
    const KNOWN: &[&str] = &[
        "rust", "js", "ts", "python", "json", "html", "css", "sh", "sql", "go", "java", "c", "cpp",
        "diff", "yaml", "toml",
    ];
    let Some(l) = lang else { return "code" };
    let l = l.to_ascii_lowercase();
    KNOWN.iter().find(|k| **k == l).copied().unwrap_or("code")
}

/// ⚠️ **引けなかったときに番号を出さない。** `<@123>` は打った人が
/// 書いた文字ではないし、`@123` は嘘である
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

/// `<t:…>` の表示。
///
/// ⚠️ **いまは書式を区別しない。** 相対表示 (`R`) は「3 分前」のように
/// 時間で変わるので、変わるたびに描き直す仕組みが要る。それまでは
/// 絶対時刻で出す — 止まった相対時刻を出すよりは正しい
fn stamp(at: i64, _format: char) -> String {
    let local = at + gumicord_platform::local_utc_offset_minutes() as i64 * 60;
    // ⚠️ **負の余りにならないよう `div_euclid` を使う。** 1970 年より前の
    // 時刻も打てるし、時差で 1 日戻ることもある
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}/{m:02}/{d:02} {:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60
    )
}

/// 1970-01-01 からの日数を年月日に直す。
///
/// Howard Hinnant の `civil_from_days`。**閏年の規則を自前で書かないため**に
/// 既知のものをそのまま使う。3 月始まりに座標をずらして、閏日を年の末尾へ
/// 追いやるのが要点である。
///
/// 出典: <https://howardhinnant.github.io/date_algorithms.html>
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

    fn ink_with(json: &str) -> Theme {
        let r = Theme::parse(json);
        assert!(r.errors().next().is_none(), "テーマが読めない");
        r.theme.expect("テーマ")
    }

    fn spans_of(theme: Option<&Theme>, src: &str, names: &dyn Names) -> Vec<Span> {
        let ink = Ink::new(theme, MatchContext::new(1000.0), false);
        let blocks = gumicord_markdown::parse(src);
        match blocks.first() {
            Some(gumicord_markdown::Block::Paragraph(c)) => ink.spans(c, names),
            other => panic!("段落ではない {other:?}"),
        }
    }

    /// ⚠️ **テーマが何も書かなければ、何も変わらない。**
    /// クライアントが太字の見た目を持っていないことの確認である
    #[test]
    fn テーマが無ければ飾りは何も付かない() {
        let spans = spans_of(None, "**太い**", &NoNames);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "太い");
        assert_eq!(spans[0].font, None, "クライアントが書体を決めている");
        assert_eq!(spans[0].color, None);
        assert!(!spans[0].line.any());
    }

    /// 太さを決めるのはテーマである
    #[test]
    fn 太字の太さはテーマが決める() {
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
    fn 下線を引くかはテーマが決める() {
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
    fn 重なった飾りは両方乗る() {
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
    fn 名前が引けなければ番号ではなく不明と出す() {
        let spans = spans_of(None, "<@1> <#2> <@&3>", &NoNames);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "@不明なユーザー #不明なチャンネル @不明な役職");
        assert!(!text.contains('1'), "番号が出ている: {text}");
    }

    #[test]
    fn 名前が引ければ名前で出す() {
        let spans = spans_of(None, "<@1> <#2> <@&3>", &Known);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "@みどり #雑談 @管理");
    }

    /// スポイラーは**場所を空けたまま**隠す。詰めて描くと、開いた瞬間に
    /// 行の折り返しが変わって本文が飛び跳ねる
    #[test]
    fn スポイラーは開けるまで隠す() {
        let hidden = Ink::new(None, MatchContext::new(1000.0), false);
        let shown = Ink::new(None, MatchContext::new(1000.0), true);
        let blocks = gumicord_markdown::parse("||秘密||");
        let gumicord_markdown::Block::Paragraph(c) = &blocks[0] else {
            panic!("段落ではない");
        };

        let h = hidden.spans(c, &NoNames);
        assert!(h[0].hidden);
        assert_eq!(h[0].text, "秘密", "隠しても場所は空けたままである");

        assert!(!shown.spans(c, &NoNames)[0].hidden);
    }

    /// コードブロックの中身は飾らない
    #[test]
    fn コードブロックは中身をそのまま持つ() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false);
        let nodes = ink.blocks(&gumicord_markdown::parse("```rust\n**a**\n```"), &NoNames);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, NodeId::PrimitiveCodeBlock);
        assert_eq!(nodes[0].content.as_text(), Some("**a**"));
    }

    /// 知らない言語でも落ちず、`code` に落ちること
    #[test]
    fn 知らない言語は既定の枠に落ちる() {
        assert_eq!(lang_slot(Some("rust")), "rust");
        assert_eq!(lang_slot(Some("RUST")), "rust");
        assert_eq!(lang_slot(Some("brainfuck")), "code");
        assert_eq!(lang_slot(None), "code");
    }

    /// ⚠️ 1970 年より前と閏日で崩れないこと
    #[test]
    fn 日付の変換は閏年と負の日数で崩れない() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2000 年は閏年 (400 で割り切れる)
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
        // 1900 年は閏年ではない (100 で割り切れて 400 で割り切れない)
        assert_eq!(civil_from_days(-25508), (1900, 3, 1));
        assert_eq!(civil_from_days(19584), (2023, 8, 15));
    }
}
