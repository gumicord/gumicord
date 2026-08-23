//! Semantic UI tree — the single definition site for stable IDs.
//!
//! This crate is the extension ABI. Stable IDs in [`ids`] can never be removed
//! or renamed within a major version; only added. `spec/03-uitree.md` and
//! `sdk/src/ids.ts` are generated from here by `cargo xtask gen`.

pub mod ids;
pub mod node;
pub mod style;
pub mod value;

pub use ids::{DataKind, NodeId, Origin, UnknownNodeId};
pub use node::{Anchor, Content, DataRef, Editable, Line, Span, UiNode};
pub use style::{Decoration, Style};

/// Node state a theme can select on. Several can hold at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    Hover,
    Active,
    Focus,
    Selected,
    Disabled,
    Unread,
    Mentioned,
    Loading,
    /// The same author posted the previous message.
    Grouped,
    /// Folded away; contents are not shown.
    Collapsed,
}

impl State {
    pub const ALL: &'static [State] = &[
        State::Hover,
        State::Active,
        State::Focus,
        State::Selected,
        State::Disabled,
        State::Unread,
        State::Mentioned,
        State::Loading,
        State::Grouped,
        State::Collapsed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hover => "hover",
            Self::Active => "active",
            Self::Focus => "focus",
            Self::Selected => "selected",
            Self::Disabled => "disabled",
            Self::Unread => "unread",
            Self::Mentioned => "mentioned",
            Self::Loading => "loading",
            Self::Grouped => "grouped",
            Self::Collapsed => "collapsed",
        }
    }
}

/// The set of states that hold.
///
/// A bitset because selector matching runs per node and must not allocate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StateSet(u16);

impl StateSet {
    pub const EMPTY: StateSet = StateSet(0);

    pub const fn with(self, s: State) -> Self {
        StateSet(self.0 | (1u16 << s as u16))
    }

    pub const fn contains(self, s: State) -> bool {
        self.0 & (1u16 << s as u16) != 0
    }

    /// Whether every state in `other` holds. A theme's `when.state` array
    /// requires all of them.
    pub const fn contains_all(self, other: StateSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = State> {
        State::ALL
            .iter()
            .copied()
            .filter(move |s| self.contains(*s))
    }
}

impl FromIterator<State> for StateSet {
    fn from_iter<I: IntoIterator<Item = State>>(iter: I) -> Self {
        iter.into_iter().fold(StateSet::EMPTY, |acc, s| acc.with(s))
    }
}

/// Distinguishes siblings that share a stable ID, and identifies nodes across
/// diffs. Plugins can read it but cannot select on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A Discord snowflake.
    Id(u64),
    /// Position, for things like window control buttons.
    Slot(&'static str),
    Index(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_set_basics() {
        let s = StateSet::EMPTY.with(State::Hover).with(State::Unread);
        assert!(s.contains(State::Hover));
        assert!(s.contains(State::Unread));
        assert!(!s.contains(State::Focus));
        assert_eq!(s.iter().count(), 2);
    }

    #[test]
    fn contains_all_requires_every_state() {
        let node = StateSet::EMPTY.with(State::Hover).with(State::Unread);
        let both: StateSet = [State::Hover, State::Unread].into_iter().collect();
        let with_focus: StateSet = [State::Hover, State::Focus].into_iter().collect();

        assert!(node.contains_all(both));
        assert!(!node.contains_all(with_focus));
        assert!(node.contains_all(StateSet::EMPTY));
    }

    /// A seventeenth state would overflow the bitset.
    #[test]
    fn state_count_fits_in_bitset() {
        assert!(
            State::ALL.len() <= 16,
            "more than 16 states; widen StateSet to u32"
        );
    }
}
