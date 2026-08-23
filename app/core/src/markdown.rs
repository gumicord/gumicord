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
    /// いま何時か (UTC 秒)。**フレームの頭で 1 回読んだもの**
    ///
    /// ⚠️ ここで時計を読まない。組んでいる最中に時刻が動くと、隣り合う
    /// 相対表示が食い違う
    now: i64,
    /// 組んだ結果が持つ秒数のうち、**一番短いもの**。
    ///
    /// 相対表示が 1 つも無ければ `None` = 描き直す理由が無い (`NFR-005`)。
    /// `&self` のまま組むので [`Cell`](std::cell::Cell) で溜める
    holds: std::cell::Cell<Option<i64>>,
}

/// 名前を引く相手。**アプリの一覧を知っているのは呼ぶ側である。**
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

    /// 組んだ結果が、**あと何秒そのままでよいか**。
    ///
    /// `None` は「時間で変わるものが無い」= 寝たままでよい (`NFR-005`)。
    /// 呼ぶのは [`Self::blocks`] の後である
    pub fn holds_for(&self) -> Option<i64> {
        self.holds.get()
    }

    /// 「あと何秒持つか」を溜める。**一番短いものが残る**
    fn hold(&self, secs: i64) {
        let next = match self.holds.get() {
            Some(cur) => cur.min(secs),
            None => secs,
        };
        self.holds.set(Some(next));
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
            InlineKind::Timestamp { at, format } => {
                let (text, holds) = stamp(self.now, *at, *format);
                // 時間で変わるものがあれば、そのぶんで起き直す
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

/// `<t:…>` の表示と、**その表示が変わるまでの秒数**。
///
/// # なぜ 2 つ返すのか
///
/// 相対表示 (`R`) は「3 分前」のように**時間で変わる**。出したきり寝て
/// しまうと、開きっぱなしの画面で「たった今」が何時間も残る。かといって
/// 毎秒描き直すのは `NFR-005` (非アクティブ時に描画を停止する) に反する。
///
/// **次に文字が変わる時刻が分かれば、そこまで寝ていられる。** 「3 分前」
/// なら次は 1 分後、「3 日前」なら次は明日である。だから表示と一緒に
/// 「いつまで持つか」を返す。
///
/// 絶対表示は変わらないので `None` を返す。**寝たままでよい。**
///
/// ⚠️ **`now` を引数で受ける。** ここで時計を読むと、同じフレームの中で
/// 時刻が動き、隣り合う相対表示が食い違う。試験もできなくなる
fn stamp(now: i64, at: i64, format: char) -> (String, Option<i64>) {
    if format == 'R' {
        let (text, holds) = relative(now, at);
        return (text, Some(holds));
    }

    let local = at + gumicord_platform::local_utc_offset_minutes() as i64 * 60;
    // ⚠️ **負の余りにならないよう `div_euclid` を使う。** 1970 年より前の
    // 時刻も打てるし、時差で 1 日戻ることもある
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (secs / 3600, (secs / 60) % 60, secs % 60);

    // ⚠️ **知らない書式は既定 (`f`) として出す。** 解析側が通した以上、
    // ここで空文字にすると本文から時刻だけが消える
    let text = match format {
        // 時刻だけ
        't' => format!("{h:02}:{min:02}"),
        'T' => format!("{h:02}:{min:02}:{s:02}"),
        // 日付だけ
        'd' => format!("{y:04}/{m:02}/{d:02}"),
        'D' => format!("{y}年{m}月{d}日"),
        // 日付と時刻。**曜日が付くのは `F` だけ**
        'F' => format!(
            "{y}年{m}月{d}日({}) {h:02}:{min:02}",
            WEEKDAYS[weekday(days)]
        ),
        _ => format!("{y}年{m}月{d}日 {h:02}:{min:02}"),
    };
    (text, None)
}

/// 曜日の名前。**日曜始まり** ([`weekday`] がそう返す)
const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

/// 1970-01-01 からの日数を曜日にする。`0` が日曜。
///
/// ⚠️ **1970-01-01 は木曜である。** そこから逆算するので `+4` する
fn weekday(days: i64) -> usize {
    (days + 4).rem_euclid(7) as usize
}

/// `<t:…:R>` の相対表示と、**その表示が持つ秒数**。
///
/// ⚠️ **持つ秒数は必ず 1 以上を返す。** ちょうど境目のときに 0 を返すと、
/// 描いては起き描いては起きで回り続ける
///
/// # ⚠️ 月と年はおおよそである
///
/// 30 日を 1 か月、365 日を 1 年として数える。**暦の上の「1 か月前」とは
/// ずれる。** 相対表示は「だいたいいつか」を伝えるものであり、正確な日付が
/// 要るなら `<t:…:D>` を使うべきである。ここで暦を持ち出すと、閏年と月の
/// 大小のために「1 か月前」が 28〜31 日のどれかに揺れる
fn relative(now: i64, at: i64) -> (String, i64) {
    /// これ以上先は寝ていてよい上限 (秒)。**1 時間。**
    ///
    /// 「3 年前」の次の変化は 1 年後だが、そこまで待つ約束をする意味は
    /// 無い。窓が閉じるほうが先である
    const MAX_SLEEP: i64 = 3_600;

    let diff = now - at;
    let ago = diff >= 0;
    let n = diff.unsigned_abs() as i64;

    // (1 単位の秒数, 単位の名前)
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

    // 次に文字が変わるのは、いまの単位の切れ目である。
    // ⚠️ **未来は逆向きに減っていく。** 「3 分後」は 1 分経つと「2 分後」に
    // なるので、切れ目の取り方が過去と違う
    let holds = if ago {
        unit - (n % unit)
    } else {
        // 「0 秒後」の次は「0 秒前」である。境目をまたぐまで
        let r = n % unit;
        if r == 0 { unit } else { r }
    };
    (text, holds.clamp(1, MAX_SLEEP))
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

    /// 試験の中の「いま」。2023-11-15 06:13:20 UTC。
    ///
    /// ⚠️ **時計を読まない。** 読むと、走らせる時刻によって結果が変わる
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

    /// コードブロックの中身は飾らない
    #[test]
    fn コードブロックは中身をそのまま持つ() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
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

    // ═══════════════════════════════════════════════════════════════
    //  `<t:…>` (`FR-021`)

    /// ⚠️ **1970-01-01 は木曜である。** ここを間違えると全部の曜日がずれる
    #[test]
    fn 曜日は木曜から数える() {
        assert_eq!(WEEKDAYS[weekday(0)], "木");
        assert_eq!(WEEKDAYS[weekday(1)], "金");
        // 1969-12-31 は水曜。**負の日数でも崩れないこと**
        assert_eq!(WEEKDAYS[weekday(-1)], "水");
        // 2023-11-15 は水曜
        assert_eq!(WEEKDAYS[weekday(19_676)], "水");
    }

    /// 書式ごとに出るものが違うこと。
    ///
    /// ⚠️ **時差で結果が変わるので、中身の時刻までは見ない。**
    /// この機械の時間帯に依らず言えることだけを見る
    #[test]
    fn 書式ごとに出す形が変わる() {
        let at = |f| stamp(NOW, NOW, f).0;

        // 時刻だけ。日付は出さない
        assert!(!at('t').contains('年'), "{}", at('t'));
        assert_eq!(at('t').len(), 5, "hh:mm のはず: {}", at('t'));
        assert_eq!(at('T').len(), 8, "hh:mm:ss のはず: {}", at('T'));

        // 日付だけ。時刻は出さない
        assert!(!at('d').contains(':'), "{}", at('d'));
        assert!(!at('D').contains(':'), "{}", at('D'));
        assert!(at('D').contains('年') && at('D').contains('日'));

        // 日付と時刻の両方
        assert!(at('f').contains('年') && at('f').contains(':'));
        assert!(at('F').contains('年') && at('F').contains(':'));

        // ⚠️ **曜日が付くのは `F` だけ**
        let 曜日あり = |s: &str| WEEKDAYS.iter().any(|w| s.contains(&format!("({w})")));
        assert!(曜日あり(&at('F')), "{}", at('F'));
        assert!(!曜日あり(&at('f')), "{}", at('f'));
    }

    /// ⚠️ **知らない書式でも時刻を消さない。** 解析側が通した以上、
    /// ここで空にすると本文から時刻だけが消える
    #[test]
    fn 知らない書式は既定として出す() {
        assert_eq!(stamp(NOW, NOW, 'z').0, stamp(NOW, NOW, 'f').0);
    }

    /// 絶対表示は時間で変わらない。**寝たままでよい** (`NFR-005`)
    #[test]
    fn 絶対表示は描き直しを要求しない() {
        for f in ['t', 'T', 'd', 'D', 'f', 'F', 'z'] {
            assert_eq!(stamp(NOW, NOW, f).1, None, "{f} が描き直しを求めている");
        }
    }

    /// 相対表示は単位を切り替えながら出る
    #[test]
    fn 相対表示は単位が上がっていく() {
        let 前 = |secs: i64| relative(NOW, NOW - secs).0;

        assert_eq!(前(0), "0 秒前");
        assert_eq!(前(59), "59 秒前");
        assert_eq!(前(60), "1 分前");
        assert_eq!(前(3_599), "59 分前");
        assert_eq!(前(3_600), "1 時間前");
        assert_eq!(前(86_399), "23 時間前");
        assert_eq!(前(86_400), "1 日前");
        assert_eq!(前(2_591_999), "29 日前");
        assert_eq!(前(2_592_000), "1 か月前");
        assert_eq!(前(31_535_999), "12 か月前");
        assert_eq!(前(31_536_000), "1 年前");
    }

    /// 未来の時刻は「後」で出る。**Discord は未来も打てる**
    #[test]
    fn 未来は後ろ向きに出る() {
        assert_eq!(relative(NOW, NOW + 90).0, "1 分後");
        assert_eq!(relative(NOW, NOW + 86_400 * 3).0, "3 日後");
    }

    /// ⚠️ **次に文字が変わる頃に起きる。** 早すぎると `NFR-005` に反し、
    /// 遅すぎると「たった今」が何時間も残る
    #[test]
    fn 次に変わる頃まで寝る() {
        let 持つ = |secs: i64| relative(NOW, NOW - secs).1;

        // 秒の桁は毎秒変わる
        assert_eq!(持つ(0), 1);
        assert_eq!(持つ(59), 1);
        // 「1 分前」は次の分まで持つ
        assert_eq!(持つ(60), 60);
        assert_eq!(持つ(90), 30);
        // 「1 時間前」は次の時まで持つ
        assert_eq!(持つ(3_600), 3_600);
        assert_eq!(持つ(3_610), 3_590);
    }

    /// ⚠️ **必ず 1 秒以上を返す。** 0 を返すと、描いては起き描いては起きで
    /// 回り続ける
    #[test]
    fn 持つ秒数は必ず一以上() {
        for d in [-100_000i64, -3_600, -60, -1, 0, 1, 59, 60, 3_600, 86_400] {
            let (_, holds) = relative(NOW, NOW - d);
            assert!(holds >= 1, "差 {d} 秒で {holds} 秒を返した");
        }
    }

    /// ⚠️ **遠い過去のために長い約束をしない。** 「3 年前」の次の変化は
    /// 1 年後だが、そこまで待つ意味は無い。窓が閉じるほうが先である
    #[test]
    fn 遠い過去でも一時間で見直す() {
        let (_, holds) = relative(NOW, NOW - 86_400 * 400);
        assert_eq!(holds, 3_600);
    }

    /// 相対表示が 1 つでもあれば、木は「あと何秒持つか」を知っている
    #[test]
    fn 相対表示があると描き直しを求める() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        assert_eq!(ink.holds_for(), None, "組む前から要求している");

        ink.blocks(
            &gumicord_markdown::parse(&format!("<t:{}:R>", NOW - 90)),
            &NoNames,
        );
        assert_eq!(ink.holds_for(), Some(30), "分の切れ目まで");
    }

    /// ⚠️ **一番早く変わるものに合わせる。** 遅いほうに合わせると、
    /// 速いほうが止まって見える
    #[test]
    fn 一番早く変わるものに合わせる() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        ink.blocks(
            &gumicord_markdown::parse(&format!(
                "<t:{}:R> と <t:{}:R>",
                NOW - 3_610, // 3590 秒持つ
                NOW - 90,    // 30 秒持つ
            )),
            &NoNames,
        );
        assert_eq!(ink.holds_for(), Some(30));
    }

    /// ⚠️ **絶対表示だけなら寝たままでよい** (`NFR-005`)
    #[test]
    fn 絶対表示だけなら起きない() {
        let ink = Ink::new(None, MatchContext::new(1000.0), false, NOW);
        ink.blocks(
            &gumicord_markdown::parse(&format!("<t:{NOW}:f> と <t:{NOW}> と ふつうの文")),
            &NoNames,
        );
        assert_eq!(ink.holds_for(), None);
    }
}
