//! 解析の結果。**描き方は知らない。**
//!
//! ここに色も大きさも出てこない。それはテーマが決めることである
//! ([`spec/04-theme.md`])。ここが持つのは「太字である」「引用である」
//! までで、太字が何ポイントかは知らない。

/// 文字にかかっている飾り。**重ねられる。**
///
/// ⚠️ **スポイラーもここに入る。** `||` は行をまたぐ他の飾りと同じように
/// 任意の中身を包むので、別の要素にすると**包んだ中身が折り返せなくなる**。
/// 隠す・現すの状態は描く側が持つ
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Deco(u8);

impl Deco {
    pub const NONE: Deco = Deco(0);
    pub const BOLD: Deco = Deco(1 << 0);
    pub const ITALIC: Deco = Deco(1 << 1);
    pub const UNDERLINE: Deco = Deco(1 << 2);
    pub const STRIKE: Deco = Deco(1 << 3);
    pub const SPOILER: Deco = Deco(1 << 4);

    pub const fn contains(self, other: Deco) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn with(self, other: Deco) -> Deco {
        Deco(self.0 | other.0)
    }

    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl core::ops::BitOr for Deco {
    type Output = Deco;
    fn bitor(self, rhs: Deco) -> Deco {
        self.with(rhs)
    }
}

/// 誰・どこを指しているか。
///
/// ⚠️ **名前は入っていない。** `<@123>` に入っているのは番号だけで、
/// 名前は手元の一覧を引かないと分からない。引けなかったときにどう出すかは
/// 描く側の判断である ([`crate::Inline`] の注意も見よ)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mention {
    /// `<@123>` / `<@!123>`
    User(u64),
    /// `<#123>`
    Channel(u64),
    /// `<@&123>`
    Role(u64),
    /// `@everyone`
    Everyone,
    /// `@here`
    Here,
}

/// 行の中に並ぶもの 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct Inline {
    /// かかっている飾り。**入れ子は畳んである** — `**~~a~~**` は 1 つになる
    pub deco: Deco,
    pub kind: InlineKind,
}

impl Inline {
    pub fn text(s: impl Into<String>, deco: Deco) -> Inline {
        Inline {
            deco,
            kind: InlineKind::Text(s.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InlineKind {
    Text(String),
    /// `` `…` ``。**中身は解析しない**
    Code(String),
    /// `[名前](url)` と、裸の URL。
    ///
    /// `label` が `None` なら URL をそのまま見せる
    Link {
        url: String,
        label: Option<String>,
    },
    Mention(Mention),
    /// `<:name:123>` / `<a:name:123>`
    Emoji {
        name: String,
        id: u64,
        animated: bool,
    },
    /// `<t:1700000000:R>`。`format` は書かれていなければ `'f'`
    Timestamp {
        at: i64,
        format: char,
    },
    /// 段落の中の改行。**段落を切らない**
    Break,
}

/// 行の頭に付く印。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `-` `*` `+`
    Bullet,
    /// `1.` — 書かれていた数を持つ
    Number(u32),
}

/// 箇条書きの 1 項目。
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// 字下げの深さ。**空白 2 つで 1 段**とする
    pub depth: u8,
    pub marker: Marker,
    pub content: Vec<Inline>,
}

/// 縦に積まれるもの 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// `#` `##` `###`。`level` は 1〜3
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// `-# `。Discord の小さい注釈
    Subtext(Vec<Inline>),
    /// `> ` と `>>> `。**入れ子になる**
    Quote(Vec<Block>),
    /// 続いた項目をまとめて 1 つにする
    List(Vec<Item>),
    /// ```` ``` ````。`lang` は書かれていた文字列そのまま
    Code {
        lang: Option<String>,
        text: String,
    },
}
