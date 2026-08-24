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

pub use document::TextDocument;

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
