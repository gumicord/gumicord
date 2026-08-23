//! Discord-flavoured Markdown.
//!
//! This is not CommonMark. Off-the-shelf parsers drop syntax Discord has
//! (`__underline__`, `||spoiler||`, `-# subtext`, `<@123>`, `>>> `) and add
//! syntax it does not. Emphasis boundaries differ too — see [`inline`].
//!
//! Returns [`Block`]s only: no colours, no sizes, no name lookup. Unparseable
//! syntax is emitted as literal text, because dropping it makes body content
//! silently disappear and the reader cannot notice.
//!
//! See `spec/03-uitree.md`.

mod block;
mod inline;
mod model;

pub use model::{Block, Deco, Inline, InlineKind, Item, Marker, Mention};

/// Parses a message body.
pub fn parse(text: &str) -> Vec<Block> {
    block::parse(text)
}

/// Parses inline syntax only, for contexts where the block is already known.
pub fn parse_inline(text: &str) -> Vec<Inline> {
    inline::parse(text)
}

#[cfg(test)]
mod tests;
