//! Android text input through GameTextInput.
//!
//! Design in ADR-0011. `winit` only shows and hides the soft keyboard; text
//! never reaches `WindowEvent`. GameActivity already integrates
//! GameTextInput, and `android-activity` exposes it, so this layer polls that
//! state on the event-loop thread and reconciles it into the focused
//! document. No Java of our own, no raw JNI `InputConnection`.
//!
//! While an IME is connected it owns the text: its state overwrites the
//! document ([`TextDocument::replace_all`]). Our own edits (hardware keys)
//! mirror back only when they differ from the last mirror, so typing never
//! fights the sync. Positions cross the boundary as UTF-16 units and convert
//! to byte offsets here ([`offsets`]).
//!
//! The public `AndroidApp` API exposes no take-flags, so freshness is change
//! detection against the last snapshot. It exposes no editor-action
//! accessor either, so the action button is `None`: return arrives as a
//! newline inside the text and single-line fields act on it, like the iOS
//! login proxies.

use winit::platform::android::activity::AndroidApp;
use winit::platform::android::activity::input::{
    ImeOptions, InputType, TextInputAction, TextInputState, TextSpan,
};

use crate::text_input::offsets::{bytes_to_utf16, utf16_to_bytes};
use crate::text_input::{ImeField, ImeKind, TextDocument};

/// A snapshot for change detection: text plus ordered byte offsets.
type Snapshot = (String, usize, usize, Option<(usize, usize)>);

/// The editor contract for one focused field.
pub struct AndroidText {
    /// Whether the IME has a live connection for our focus.
    live: bool,
    /// Whether the field takes newlines; otherwise a committed return acts.
    multiline: bool,
    /// Last state seen, from either direction. Our own mirror updates it,
    /// so its echo never counts as a change.
    last_seen: Snapshot,
}

impl Default for AndroidText {
    fn default() -> Self {
        Self::new()
    }
}

impl AndroidText {
    pub fn new() -> Self {
        AndroidText {
            live: false,
            multiline: false,
            last_seen: (String::new(), 0, 0, None),
        }
    }
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Focus gained: tell the IME what the field is and seed it with the
    /// document, so conversion and predictions start from real text.
    pub fn focus(&mut self, app: &AndroidApp, field: &ImeField, doc: &TextDocument) {
        let mut input_type = match field.kind {
            ImeKind::Text => InputType::TYPE_CLASS_TEXT,
            ImeKind::Email => {
                InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_VARIATION_EMAIL_ADDRESS
            }
            ImeKind::Password => {
                InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_VARIATION_PASSWORD
            }
            ImeKind::Number => InputType::TYPE_CLASS_NUMBER,
        };
        if field.multiline {
            input_type |= InputType::TYPE_TEXT_FLAG_MULTI_LINE;
        }
        // No action button: without an accessor its presses are invisible,
        // while return-as-newline is detectable below.
        app.set_ime_editor_info(
            input_type,
            TextInputAction::None,
            ImeOptions::IME_FLAG_NO_FULLSCREEN,
        );
        self.multiline = field.multiline;
        self.mirror(app, doc);
        self.live = true;
    }

    /// Focus lost.
    pub fn blur(&mut self) {
        self.live = false;
    }

    /// Polls once. Returns whether the document changed (needs a redraw)
    /// and whether a single-line field committed a newline (advance or
    /// submit). Call on the event-loop thread only: the underlying query is
    /// not thread-safe.
    pub fn poll(&mut self, app: &AndroidApp, doc: &mut TextDocument) -> (bool, bool) {
        if !self.live {
            return (false, false);
        }
        let mut changed = false;
        let mut newline = false;
        changed |= self.apply_ime_state(doc, &app.text_input_state());
        // A return in a single-line field acts instead of staying: by
        // invariant such documents never end in a newline, so any trailing
        // ones are new.
        if !self.multiline && doc.text().contains('\n') {
            let stripped = doc.text().trim_end_matches('\n').to_owned();
            if stripped.len() != doc.text().len() {
                let end = stripped.len();
                doc.replace_all(&stripped, end..end, None);
                newline = true;
                changed = true;
            }
        }
        // Our own edits since the last mirror go back to the IME, so its
        // surrounding text stays true. An IME state in the same cycle wins:
        // it is the fresher keystroke.
        if self.doc_differs(doc) {
            self.mirror(app, doc);
        }
        (changed, newline)
    }

    /// Overwrites the document from the IME state. True when anything moved.
    fn apply_ime_state(&mut self, doc: &mut TextDocument, state: &TextInputState) -> bool {
        let text = &state.text;
        let start = utf16_to_bytes(text, state.selection.start);
        let end = utf16_to_bytes(text, state.selection.end);
        let composing = state.compose_region.map(|r| {
            let s = utf16_to_bytes(text, r.start);
            let e = utf16_to_bytes(text, r.end);
            if s <= e { s..e } else { e..s }
        });
        // Direction-only selection changes ride along with the next content
        // change; comparing ordered keeps the loop quiet.
        let (ordered_start, ordered_end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let composing = composing.filter(|r| !r.is_empty());
        if doc.text() == text
            && doc.selection() == (ordered_start..ordered_end)
            && doc.composing() == composing
        {
            self.last_seen = snapshot(doc);
            return false;
        }
        doc.replace_all(text, start..end, composing);
        self.last_seen = snapshot(doc);
        true
    }

    fn doc_differs(&self, doc: &TextDocument) -> bool {
        snapshot(doc) != self.last_seen
    }

    fn mirror(&mut self, app: &AndroidApp, doc: &TextDocument) {
        let text = doc.text();
        let sel = doc.selection();
        let state = TextInputState {
            text: text.to_owned(),
            selection: TextSpan {
                start: bytes_to_utf16(text, sel.start),
                end: bytes_to_utf16(text, sel.end),
            },
            compose_region: doc.composing().map(|r| TextSpan {
                start: bytes_to_utf16(text, r.start),
                end: bytes_to_utf16(text, r.end),
            }),
        };
        app.set_text_input_state(state);
        self.last_seen = snapshot(doc);
    }
}

/// (text, ordered selection start/end, composing) for change detection.
fn snapshot(doc: &TextDocument) -> Snapshot {
    let sel = doc.selection();
    (
        doc.text().to_owned(),
        sel.start,
        sel.end,
        doc.composing().map(|r| (r.start, r.end)),
    )
}
