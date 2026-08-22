//! セマンティック UI ツリー。**安定 ID の唯一の定義元。**
//!
//! ここが Gumicord の拡張 ABI そのものである。
//!
//! ⚠️ [`ids`] に定義された安定 ID は、**メジャーバージョン内で削除も改名も
//! できない** (`EXT-003`)。追加のみを許す。これは技術的な制約ではなく
//! **プロジェクトの約束**であり、破ると BetterDiscord のプラグインが壊れ
//! 続ける問題を解くという存在理由が消える。
//!
//! `spec/03-uitree.md` の安定 ID 一覧と `sdk/src/ids.ts` は、
//! **このクレートから `cargo xtask gen` で生成する**。手書きで同期しない。
//!
//! 要件: `EXT-001`〜`EXT-006`
//! 仕様: [`spec/03-uitree.md`]

pub mod ids;
pub mod node;
pub mod style;
pub mod value;

pub use ids::{DataKind, NodeId, Origin, UnknownNodeId};
pub use node::{Content, DataRef, UiNode};
pub use style::Style;

/// ノードの状態。テーマの条件分岐に使う (`EXT-013`)。
///
/// 複数が同時に立ちうる。
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
        }
    }
}

/// 立っている状態の集合。
///
/// 状態は 8 個しかないのでビットセットで持つ。テーマのセレクタ照合は
/// ノードごとに走るため、割り当てを避ける。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StateSet(u8);

impl StateSet {
    pub const EMPTY: StateSet = StateSet(0);

    pub const fn with(self, s: State) -> Self {
        StateSet(self.0 | (1 << s as u8))
    }

    pub const fn contains(self, s: State) -> bool {
        self.0 & (1 << s as u8) != 0
    }

    /// すべて含むか。テーマの `when.state` が配列のときに使う (`EXT-013`)。
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

/// 同じ親の下で同じ安定 ID を持つノードを区別する鍵。
///
/// 差分更新の同一性判定にも使う。**プラグインはこれを読めるがセレクタには
/// 使えない** (`spec/03-uitree.md` 2.2)。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Discord のスノーフレーク
    Id(u64),
    /// 位置による区別 (ウィンドウ操作ボタンなど)
    Slot(&'static str),
    /// 連番
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

    /// EXT-013: `when.state` が配列なら**すべて**成立が必要
    #[test]
    fn contains_all_requires_every_state() {
        let node = StateSet::EMPTY.with(State::Hover).with(State::Unread);
        let both: StateSet = [State::Hover, State::Unread].into_iter().collect();
        let with_focus: StateSet = [State::Hover, State::Focus].into_iter().collect();

        assert!(node.contains_all(both));
        assert!(!node.contains_all(with_focus));
        assert!(node.contains_all(StateSet::EMPTY));
    }

    /// State が 8 個を超えたら StateSet の u8 が溢れる。
    /// 状態を足すときはここで気づけるようにしておく。
    #[test]
    fn state_count_fits_in_bitset() {
        assert!(
            State::ALL.len() <= 8,
            "State が 8 個を超えた。StateSet の内部表現を u16 以上に広げること"
        );
    }
}
