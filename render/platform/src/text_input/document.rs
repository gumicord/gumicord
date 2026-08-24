//! The text being edited. Touches no OS API.
//!
//! Windows TSF, Android's `InputConnection` and iOS's `UITextInput` all end up
//! asking to read and write a document holding a string and a selection. That
//! document lives here, once, and each platform layer drives it.
//!
//! Every position is a UTF-8 byte offset. All three talk in UTF-16 units, but
//! storing UTF-16 would make every Rust operation precarious, so converting at
//! the boundary is the platform layer's job.
//!
//! The caret moves by grapheme. By `char` it would stop inside a combining
//! sequence or a ZWJ emoji: 👨‍👩‍👧‍👦 is seven `char`s and one character to the
//! person typing.

use std::ops::Range;

use unicode_segmentation::GraphemeCursor;

/// One editable text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextDocument {
    text: String,
    /// Where the selection began; equal to `caret` means none.
    anchor: usize,
    caret: usize,
    /// The composing range, holding what is not committed yet.
    composing: Option<Range<usize>>,
}

impl TextDocument {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selection, ordered.
    pub fn selection(&self) -> Range<usize> {
        if self.anchor <= self.caret {
            self.anchor..self.caret
        } else {
            self.caret..self.anchor
        }
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// The composing range, underlined while it is uncommitted.
    pub fn composing(&self) -> Option<Range<usize>> {
        self.composing.clone()
    }

    pub fn is_composing(&self) -> bool {
        self.composing.is_some()
    }

    // ─────────────────────────────────────────────── Editing

    /// Inserts, replacing the selection if there is one.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.anchor = self.caret;
        self.composing = None;
    }

    /// Sets the composing text, replacing any previous one. Not a commit.
    /// `cursor` is the IME's caret, a byte offset within that text.
    pub fn set_composition(&mut self, s: &str, cursor: Option<usize>) {
        let start = match self.composing.take() {
            Some(r) => {
                self.text.replace_range(r.clone(), "");
                r.start
            }
            None => {
                self.delete_selection();
                self.caret
            }
        };

        self.text.insert_str(start, s);
        let end = start + s.len();
        self.composing = (!s.is_empty()).then_some(start..end);

        // Without one from the IME, the caret goes to the end.
        self.caret = cursor.map_or(end, |c| (start + c).min(end));
        self.anchor = self.caret;
    }

    /// Commits. An empty `s` cancels the composition.
    pub fn commit_composition(&mut self, s: &str) {
        if let Some(r) = self.composing.take() {
            self.text.replace_range(r.clone(), s);
            self.caret = r.start + s.len();
        } else {
            self.text.insert_str(self.caret, s);
            self.caret += s.len();
        }
        self.anchor = self.caret;
    }

    /// Drops the composition, as Esc does.
    pub fn cancel_composition(&mut self) {
        if let Some(r) = self.composing.take() {
            self.text.replace_range(r.clone(), "");
            self.caret = r.start;
            self.anchor = self.caret;
        }
    }

    /// Deletes backwards.
    pub fn delete_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(prev) = self.prev_boundary(self.caret) {
            self.text.replace_range(prev..self.caret, "");
            self.caret = prev;
            self.anchor = prev;
        }
    }

    /// Deletes forwards.
    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(next) = self.next_boundary(self.caret) {
            self.text.replace_range(self.caret..next, "");
        }
    }

    /// Deletes the selection, reporting whether there was one.
    fn delete_selection(&mut self) -> bool {
        let sel = self.selection();
        if sel.is_empty() {
            return false;
        }
        self.text.replace_range(sel.clone(), "");
        self.caret = sel.start;
        self.anchor = sel.start;
        true
    }

    // ─────────────────────────────────────────────── Movement

    /// Left, extending the selection when asked.
    pub fn move_left(&mut self, extend: bool) {
        // An unextended move collapses to the start of the selection.
        if self.has_selection() && !extend {
            self.caret = self.selection().start;
            self.anchor = self.caret;
            return;
        }
        if let Some(prev) = self.prev_boundary(self.caret) {
            self.caret = prev;
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        if self.has_selection() && !extend {
            self.caret = self.selection().end;
            self.anchor = self.caret;
            return;
        }
        if let Some(next) = self.next_boundary(self.caret) {
            self.caret = next;
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        self.caret = 0;
        if !extend {
            self.anchor = 0;
        }
    }

    pub fn move_end(&mut self, extend: bool) {
        self.caret = self.text.len();
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Takes the text and empties this, as sending does.
    pub fn take(&mut self) -> String {
        self.composing = None;
        self.anchor = 0;
        self.caret = 0;
        std::mem::take(&mut self.text)
    }

    // ─────────────────────────────────────────────── Grapheme boundaries

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        GraphemeCursor::new(at, self.text.len(), true)
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        GraphemeCursor::new(at, self.text.len(), true)
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> TextDocument {
        let mut d = TextDocument::new();
        d.insert(s);
        d
    }

    #[test]
    fn inserting_moves_the_caret_to_the_end_of_what_was_inserted() {
        let d = doc("こんにちは");
        assert_eq!(d.text(), "こんにちは");
        assert_eq!(d.caret(), "こんにちは".len());
        assert!(!d.has_selection());
    }

    /// The caret moves by grapheme, never stopping inside a ZWJ emoji.
    #[test]
    fn the_caret_steps_over_a_whole_emoji() {
        let family = "👨‍👩‍👧‍👦";
        assert!(family.chars().count() > 1, "結合された絵文字であること");

        let mut d = doc(family);
        d.move_left(false);
        assert_eq!(d.caret(), 0, "1 回で先頭まで戻る");

        d.move_right(false);
        assert_eq!(d.caret(), family.len(), "1 回で末尾まで進む");
    }

    #[test]
    fn backspace_deletes_a_whole_grapheme() {
        let mut d = doc("あ👨‍👩‍👧‍👦");
        d.delete_back();
        assert_eq!(d.text(), "あ");
        d.delete_back();
        assert_eq!(d.text(), "");
        // Deleting in an empty document is harmless.
        d.delete_back();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn deleting_a_selection_replaces_it() {
        let mut d = doc("あいうえお");
        d.select_all();
        assert!(d.has_selection());
        d.insert("か");
        assert_eq!(d.text(), "か");
        assert!(!d.has_selection());
    }

    /// Composing text is uncommitted and can be replaced.
    #[test]
    fn a_composition_is_replaced_not_appended() {
        let mut d = doc("送信: ");
        d.set_composition("にほn", None);
        assert_eq!(d.text(), "送信: にほn");
        assert!(d.is_composing());

        d.set_composition("にほん", None);
        assert_eq!(d.text(), "送信: にほん", "前の変換中の文字列は消える");

        d.commit_composition("日本");
        assert_eq!(d.text(), "送信: 日本");
        assert!(!d.is_composing());
        assert_eq!(d.caret(), d.text().len());
    }

    #[test]
    fn cancelling_a_composition_leaves_no_trace() {
        let mut d = doc("あ");
        d.set_composition("かんじ", None);
        assert_eq!(d.text(), "あかんじ");

        d.cancel_composition();
        assert_eq!(d.text(), "あ");
        assert_eq!(d.caret(), "あ".len());
        assert!(!d.is_composing());
    }

    /// The IME places the caret within the composing text.
    #[test]
    fn the_ime_can_place_the_caret_inside_the_composition() {
        let mut d = TextDocument::new();
        d.set_composition("にほんご", Some("にほ".len()));
        assert_eq!(d.caret(), "にほ".len());

        // An out-of-range position is harmless.
        d.set_composition("にほんご", Some(9999));
        assert_eq!(d.caret(), "にほんご".len());
    }

    #[test]
    fn selection_is_ordered_regardless_of_direction() {
        let mut d = doc("あいうえお");
        d.move_left(true);
        d.move_left(true);
        let sel = d.selection();
        assert!(sel.start < sel.end);
        assert_eq!(&d.text()[sel], "えお");
    }

    /// An arrow with a selection collapses it rather than moving.
    #[test]
    fn an_arrow_key_collapses_a_selection() {
        let mut d = doc("あいうえお");
        d.select_all();
        d.move_left(false);
        assert_eq!(d.caret(), 0);
        assert!(!d.has_selection());

        d.select_all();
        d.move_right(false);
        assert_eq!(d.caret(), d.text().len());
    }

    #[test]
    fn taking_the_text_empties_the_document() {
        let mut d = doc("送信する");
        assert_eq!(d.take(), "送信する");
        assert!(d.is_empty());
        assert_eq!(d.caret(), 0);
    }
}
