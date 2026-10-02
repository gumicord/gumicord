//! Text input.
//!
//! `winit` reports only that the composing text changed or was committed.
//! Where that text goes, and how the caret and selection follow, is the app's
//! to hold.
//!
//! | | |
//! |---|---|
//! | [`TextDocument`] | done, shared, touches no OS API |
//! | Windows input | done, `winit`'s `Ime` events suffice |
//! | Android `InputConnection` | to come |
//! | iOS `UITextInput` | to come |
//!
//! The document model came first so every platform drives the same thing:
//! `InputConnection` and `UITextInput` both end up asking to read and write a
//! string with a selection.
//!
//! Windows needed no TSF text store. The earlier conclusion that a candidate
//! window requires `ITextStoreACP` was wrong (see ADR-0006); the real cause was
//! the rectangle passed to `set_ime_cursor_area`. `winit` sets `CANDIDATEFORM`
//! with `CFS_EXCLUDE`, so that rectangle is the area to avoid, not where to put
//! the candidates. Pass the whole input field: a caret-width rectangle leaves
//! the IME nowhere to place them.

mod document;
pub mod offsets;

pub use document::TextDocument;

/// What the focused field wants from the soft keyboard (mobile only).
/// Desktop ignores it; `winit` IME events carry no field metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeKind {
    Text,
    Email,
    Password,
    Number,
}

/// One focused field's keyboard contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImeField {
    pub kind: ImeKind,
    pub multiline: bool,
}

impl Default for ImeField {
    fn default() -> Self {
        ImeField {
            kind: ImeKind::Text,
            multiline: false,
        }
    }
}

/// Which document the platform's single IME connection is currently serving.
///
/// Every mobile OS gives the app one text connection, so moving from one field
/// to another has to reset both the editor kind and the text. A bridge that
/// only seeds on the first focus leaves the second field holding the first
/// one's content type and content, and the next poll writes that into the new
/// document. Tracking the identity here keeps that decision in one place and
/// testable without either OS.
#[derive(Debug, Default)]
pub struct ImeServing {
    doc: Option<*const ()>,
    /// A field was just seeded, so the next poll must not read the IME: the
    /// push we just made has not been applied yet, and the reported state can
    /// still be the previous field's.
    fresh: bool,
}

impl ImeServing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the IME must be seeded afresh for this document, and records it.
    pub fn seed_for(&mut self, doc: &TextDocument) -> bool {
        let key = std::ptr::from_ref(doc).cast::<()>();
        let changed = self.doc != Some(key);
        self.doc = Some(key);
        changed
    }

    /// Whether this tick must not read the IME. One-shot.
    pub fn skips_this_tick(&mut self) -> bool {
        std::mem::take(&mut self.fresh)
    }

    /// Marks the connection as just seeded.
    pub fn mark_seeded(&mut self) {
        self.fresh = true;
    }

    /// Forgets the field, so the next one starts clean.
    pub fn forget(&mut self) {
        self.doc = None;
    }
}

#[cfg(test)]
mod text_input_tests {
    use super::*;

    #[test]
    fn enter_and_escape_are_left_to_the_caller() {
        let mut d = TextDocument::new();
        d.insert("あ");
        assert!(!EditKey::Enter.apply(&mut d, false));
        assert!(!EditKey::Escape.apply(&mut d, false));
        assert_eq!(d.text(), "あ", "文書は変わらない");
    }

    #[test]
    fn shift_extends_the_selection() {
        let mut d = TextDocument::new();
        d.insert("あいう");
        assert!(EditKey::Left.apply(&mut d, true));
        assert!(d.has_selection());
    }
}

#[cfg(test)]
mod ime_serving_tests {
    use super::*;

    /// The first field seeds; the next frame on it must not.
    #[test]
    fn only_a_field_change_seeds_again() {
        let mut s = ImeServing::new();
        let email = TextDocument::new();
        assert!(s.seed_for(&email), "最初の欄は種を蒔く");
        assert!(!s.seed_for(&email), "同じ欄を種蒔きした");
    }

    /// Moving between the login fields seeds again. This is the whole point:
    /// one connection, two fields.
    #[test]
    fn moving_between_login_fields_seeds_again() {
        let mut s = ImeServing::new();
        let email = TextDocument::new();
        let password = TextDocument::new();
        assert!(s.seed_for(&email));
        assert!(
            s.seed_for(&password),
            "パスワード欄がメール欄の IME 状態を引き継いだ"
        );
    }

    /// Losing focus forgets the field.
    #[test]
    fn forgetting_makes_the_next_field_seed() {
        let mut s = ImeServing::new();
        let email = TextDocument::new();
        s.seed_for(&email);
        s.forget();
        assert!(s.seed_for(&email));
    }

    /// The skip after a seed is one-shot, or the field would never sync.
    #[test]
    fn the_skip_after_seeding_is_one_shot() {
        let mut s = ImeServing::new();
        assert!(!s.skips_this_tick());
        s.mark_seeded();
        assert!(s.skips_this_tick());
        assert!(!s.skips_this_tick());
    }
}

/// Where text input goes.
///
/// Input reaches one focused document. Which one is the app's choice; the
/// platform layer only hands it over.
pub trait TextInputHost {
    /// The document receiving input; `None` means no text input is happening.
    fn focused_document(&mut self) -> Option<&mut TextDocument>;

    /// Something was committed, which may trigger a send.
    fn on_commit(&mut self) {}
}

/// The keys that edit text.
///
/// No OS type appears here: passing `winit` key codes through would drag a
/// different type in on Android and iOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Cancels a composition, or drops focus.
    Escape,
    /// Send.
    Enter,
    SelectAll,
}

impl EditKey {
    /// Applies this to a document, reporting whether anything changed.
    ///
    /// `Enter` and `Escape` are not handled here: sending and focus live
    /// outside the document.
    pub fn apply(self, doc: &mut TextDocument, shift: bool) -> bool {
        match self {
            EditKey::Backspace => doc.delete_back(),
            EditKey::Delete => doc.delete_forward(),
            EditKey::Left => doc.move_left(shift),
            EditKey::Right => doc.move_right(shift),
            EditKey::Home => doc.move_home(shift),
            EditKey::End => doc.move_end(shift),
            EditKey::SelectAll => doc.select_all(),
            // The caller's business.
            EditKey::Enter | EditKey::Escape => return false,
        }
        true
    }
}

/// Keys that drive the hidden login code only (the QR screen's konami
/// sequence). A deliberately tiny set: the arrows and B/A do nothing else
/// once no field is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenKey {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
}

/// A clipboard operation on the focused text field, from a Ctrl shortcut or a
/// context-menu item. The app decides which field it lands on and where the
/// text comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOp {
    Copy,
    Cut,
    Paste,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_escape_are_left_to_the_caller() {
        let mut d = TextDocument::new();
        d.insert("あ");
        assert!(!EditKey::Enter.apply(&mut d, false));
        assert!(!EditKey::Escape.apply(&mut d, false));
        assert_eq!(d.text(), "あ", "文書は変わらない");
    }

    #[test]
    fn shift_extends_the_selection() {
        let mut d = TextDocument::new();
        d.insert("あいう");
        assert!(EditKey::Left.apply(&mut d, true));
        assert!(d.has_selection());
    }
}
