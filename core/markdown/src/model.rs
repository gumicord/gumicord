//! Parse results. Nothing here knows how anything is drawn — "this is bold"
//! is a parse fact, "bold is weight 700" is the theme's.

/// Decoration applied to text. Stackable.
///
/// Spoilers live here rather than as their own element: `||` wraps arbitrary
/// content the way the other decorations do, and a separate element would
/// stop that content from wrapping with the rest of the line. Whether a
/// spoiler is revealed is the renderer's state.
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

/// Who or what is being referred to.
///
/// Carries the id only: `<@123>` contains no name, and resolving it needs a
/// local directory. What to show when the lookup fails is the renderer's
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mention {
    /// `<@123>` or `<@!123>`
    User(u64),
    /// `<#123>`
    Channel(u64),
    /// `<@&123>`
    Role(u64),
    Everyone,
    Here,
}

/// One item within a line.
#[derive(Debug, Clone, PartialEq)]
pub struct Inline {
    /// Nesting is already flattened: `**~~a~~**` becomes one item.
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
    /// `` `…` ``. Contents are not parsed.
    Code(String),
    /// `[label](url)` and bare URLs. A `None` label shows the URL itself.
    Link {
        url: String,
        label: Option<String>,
    },
    Mention(Mention),
    /// `<:name:123>` or `<a:name:123>`
    Emoji {
        name: String,
        id: u64,
        animated: bool,
    },
    /// `<t:1700000000:R>`. `format` defaults to `'f'`.
    Timestamp {
        at: i64,
        format: char,
    },
    /// A newline inside a paragraph; does not end it.
    Break,
}

/// The marker at the head of a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `-`, `*`, or `+`
    Bullet,
    /// `1.`, carrying the number as written.
    Number(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Indent depth, two spaces per level.
    pub depth: u8,
    pub marker: Marker,
    pub content: Vec<Inline>,
}

/// One vertically stacked element.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// `#`, `##`, `###`. `level` is 1 to 3.
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// `-# `, Discord's subtext.
    Subtext(Vec<Inline>),
    /// `> ` and `>>> `. Nests.
    Quote(Vec<Block>),
    /// Consecutive items collapsed into one list.
    List(Vec<Item>),
    /// A fenced block. `lang` is the fence's info string verbatim.
    Code {
        lang: Option<String>,
        text: String,
    },
}
