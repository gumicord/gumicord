//! iOS text input through a hidden `UITextInput` editor.
//!
//! Design in ADR-0011. `winit`'s view only speaks `UIKeyInput`, which cannot
//! convert. A 1px editor view implementing `UITextInput` sits beside it: the
//! OS gets a real editor (conversion, candidates, autocorrect), while pixels
//! stay ours. The platform polls the editor state into the focused document,
//! mirroring Android's bridge.
//!
//! Login email/password keep the autofill proxies (`proxy`): the password
//! manager pairs real fields. Everything else edits here.

// ObjC selectors keep their spelling; renaming them would lie about the
// protocol conformance.
#![allow(non_snake_case)]

use std::sync::Mutex;

use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, msg_send};
use objc2_core_foundation::CGPoint;
use objc2_foundation::{
    NSArray, NSAttributedStringKey, NSComparisonResult, NSDictionary, NSInteger, NSNotification,
    NSNotificationCenter, NSRange, NSRect, NSString, NSValue,
};
use objc2_ui_kit::{
    NSWritingDirection, UIKeyInput, UIKeyboardType, UIReturnKeyType, UITextAutocapitalizationType,
    UITextAutocorrectionType, UITextInput, UITextInputDelegate, UITextInputStringTokenizer,
    UITextInputTokenizer, UITextInputTraits, UITextLayoutDirection, UITextPosition, UITextRange,
    UITextSelectionRect, UITextStorageDirection, UIView,
};

use crate::text_input::offsets::{bytes_to_utf16, utf16_to_bytes};
use crate::text_input::{ImeField, ImeKind, TextDocument};

/// Editor state, in UTF-16 units like the OS counts.
#[derive(Debug)]
struct EditorState {
    text: String,
    /// Directional (anchor, caret), as given.
    sel: (usize, usize),
    marked: Option<(usize, usize)>,
    keyboard: UIKeyboardType,
    return_key: UIReturnKeyType,
    secure: bool,
    autocorrect: bool,
    autocap_sentences: bool,
    content: Option<Retained<NSString>>,
    /// Field-anchored rect (points) for candidate/autocorrect placement.
    caret_rect: NSRect,
    delegate: Option<Retained<ProtocolObject<dyn UITextInputDelegate>>>,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            text: String::new(),
            sel: (0, 0),
            marked: None,
            keyboard: UIKeyboardType::Default,
            return_key: UIReturnKeyType::Default,
            secure: false,
            autocorrect: true,
            autocap_sentences: true,
            content: None,
            caret_rect: NSRect::new(
                objc2_foundation::NSPoint::new(0.0, 0.0),
                objc2_foundation::NSSize::new(2.0, 20.0),
            ),
            delegate: None,
        }
    }
}

define_class!(
    #[unsafe(super(UITextPosition))]
    #[thread_kind = MainThreadOnly]
    #[name = "GumicordImePosition"]
    #[ivars = Mutex<usize>]
    struct ImePosition;

    unsafe impl NSObjectProtocol for ImePosition {}
);

define_class!(
    #[unsafe(super(UITextRange))]
    #[thread_kind = MainThreadOnly]
    #[name = "GumicordImeRange"]
    #[ivars = Mutex<(usize, usize)>]
    struct ImeRange;

    unsafe impl NSObjectProtocol for ImeRange {}
);

define_class!(
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GumicordImeEditor"]
    #[ivars = Mutex<EditorState>]
    struct ImeEditor;

    unsafe impl NSObjectProtocol for ImeEditor {}

    impl ImeEditor {
        #[unsafe(method(canBecomeFirstResponder))]
        fn canBecomeFirstResponder(&self) -> bool {
            true
        }

        #[unsafe(method(canResignFirstResponder))]
        fn canResignFirstResponder(&self) -> bool {
            true
        }
    }

    unsafe impl UIKeyInput for ImeEditor {
        #[unsafe(method(hasText))]
        fn hasText(&self) -> bool {
            self.with_state(|s| !s.text.is_empty())
        }

        #[unsafe(method(insertText:))]
        fn insertText(&self, text: &NSString) {
            self.insert_str(&text.to_string());
        }

        #[unsafe(method(deleteBackward))]
        fn deleteBackward(&self) {
            self.update_state(|s| {
                let (a, b) = ordered_byte_range(s);
                if a != b {
                    s.text.replace_range(a..b, "");
                    s.sel = (a, a);
                } else if let Some(prev) = prev_char_start(&s.text, a) {
                    s.text.replace_range(prev..a, "");
                    s.sel = (prev, prev);
                }
                s.marked = None;
            });
        }
    }

    unsafe impl UITextInput for ImeEditor {
        #[unsafe(method_id(textInRange:))]
        fn textInRange(&self, range: &UITextRange) -> Option<Retained<NSString>> {
            let (a, b) = range_bounds(range);
            let range = self.byte_range(a, b);
            Some(NSString::from_str(&self.with_state(|s| s.text[range].to_owned())))
        }

        #[unsafe(method(replaceRange:withText:))]
        fn replaceRange_withText(&self, range: &UITextRange, text: &NSString) {
            let (a, b) = range_bounds(range);
            let text = text.to_string();
            self.update_state(|s| {
                let range = ordered(a, b, &s.text);
                s.text.replace_range(range.clone(), &text);
                let caret = range.start + text.len();
                s.sel = (caret, caret);
                s.marked = None;
            });
        }

        #[unsafe(method_id(selectedTextRange))]
        fn selectedTextRange(&self) -> Option<Retained<UITextRange>> {
            let (a, b) = self.with_state(|s| s.sel);
            Some(ImeEditor::make_range(a, b))
        }

        #[unsafe(method(setSelectedTextRange:))]
        fn setSelectedTextRange(&self, selected_text_range: Option<&UITextRange>) {
            if let Some(range) = selected_text_range {
                let (a, b) = range_bounds(range);
                let len = self.text_len_utf16();
                self.update_state(|s| {
                    s.sel = (a.min(len), b.min(len));
                });
            }
        }

        #[unsafe(method_id(markedTextRange))]
        fn markedTextRange(&self) -> Option<Retained<UITextRange>> {
            self.with_state(|s| s.marked)
                .map(|(a, b)| ImeEditor::make_range(a, b))
        }

        #[unsafe(method_id(markedTextStyle))]
        fn markedTextStyle(
            &self,
        ) -> Option<
            Retained<NSDictionary<NSAttributedStringKey, objc2::runtime::AnyObject>>,
        > {
            None
        }

        #[unsafe(method(setMarkedTextStyle:))]
        unsafe fn setMarkedTextStyle(
            &self,
            _marked_text_style: Option<
                &NSDictionary<NSAttributedStringKey, objc2::runtime::AnyObject>,
            >,
        ) {
        }

        #[unsafe(method(setMarkedText:selectedRange:))]
        fn setMarkedText_selectedRange(
            &self,
            marked_text: Option<&NSString>,
            selected_range: NSRange,
        ) {
            let marked_text = marked_text.map(|s| s.to_string()).unwrap_or_default();
            self.update_state(|s| {
                let range = ordered(s.sel.0, s.sel.1, &s.text);
                s.text.replace_range(range.start..range.end, &marked_text);
                let start = range.start;
                // The range counts in the marked text's own units.
                let caret = start
                    + utf16_to_bytes(&marked_text, selected_range.location)
                        .min(marked_text.len());
                s.sel = (caret, caret);
                let end = start + marked_text.len();
                s.marked = (!marked_text.is_empty()).then_some((start, end));
            });
        }

        #[unsafe(method(unmarkText))]
        fn unmarkText(&self) {
            self.update_state(|s| {
                s.marked = None;
            });
        }

        #[unsafe(method_id(beginningOfDocument))]
        fn beginningOfDocument(&self) -> Retained<UITextPosition> {
            ImeEditor::make_position(0)
        }

        #[unsafe(method_id(endOfDocument))]
        fn endOfDocument(&self) -> Retained<UITextPosition> {
            ImeEditor::make_position(self.text_len_utf16())
        }

        #[unsafe(method_id(textRangeFromPosition:toPosition:))]
        fn textRangeFromPosition_toPosition(
            &self,
            from_position: &UITextPosition,
            to_position: &UITextPosition,
        ) -> Option<Retained<UITextRange>> {
            Some(ImeEditor::make_range(
                position_offset(from_position),
                position_offset(to_position),
            ))
        }

        #[unsafe(method_id(positionFromPosition:offset:))]
        fn positionFromPosition_offset(
            &self,
            position: &UITextPosition,
            offset: NSInteger,
        ) -> Option<Retained<UITextPosition>> {
            let len = self.text_len_utf16() as NSInteger;
            let at = (position_offset(position) as NSInteger + offset).clamp(0, len);
            Some(ImeEditor::make_position(at as usize))
        }

        #[unsafe(method_id(positionFromPosition:inDirection:offset:))]
        fn positionFromPosition_inDirection_offset(
            &self,
            position: &UITextPosition,
            _direction: UITextLayoutDirection,
            offset: NSInteger,
        ) -> Option<Retained<UITextPosition>> {
            // No layout lives here; direction cannot move across lines.
            let len = self.text_len_utf16() as NSInteger;
            let at = (position_offset(position) as NSInteger + offset).clamp(0, len);
            Some(ImeEditor::make_position(at as usize))
        }

        #[unsafe(method(comparePosition:toPosition:))]
        fn comparePosition_toPosition(
            &self,
            position: &UITextPosition,
            other: &UITextPosition,
        ) -> NSComparisonResult {
            position_offset(position).cmp(&position_offset(other)).into()
        }

        #[unsafe(method(offsetFromPosition:toPosition:))]
        fn offsetFromPosition_toPosition(
            &self,
            position: &UITextPosition,
            other: &UITextPosition,
        ) -> NSInteger {
            position_offset(other) as NSInteger - position_offset(position) as NSInteger
        }

        #[unsafe(method_id(inputDelegate))]
        fn inputDelegate(
            &self,
        ) -> Option<Retained<ProtocolObject<dyn UITextInputDelegate>>> {
            self.with_state(|s| s.delegate.clone())
        }

        #[unsafe(method(setInputDelegate:))]
        fn setInputDelegate(
            &self,
            input_delegate: Option<&ProtocolObject<dyn UITextInputDelegate>>,
        ) {
            // The system retains its input delegate while editing, so
            // retaining here cannot outlive the use.
            self.update_state(|s| {
                s.delegate = input_delegate.map(|d| d.retain());
            });
        }

        #[unsafe(method_id(tokenizer))]
        fn tokenizer(&self) -> Retained<ProtocolObject<dyn UITextInputTokenizer>> {
            let tokenizer: Retained<UITextInputStringTokenizer> = unsafe {
                UITextInputStringTokenizer::initWithTextInput(
                    UITextInputStringTokenizer::alloc(mtm()),
                    self,
                )
            };
            ProtocolObject::from_retained(tokenizer)
        }

        #[unsafe(method_id(positionWithinRange:farthestInDirection:))]
        fn positionWithinRange_farthestInDirection(
            &self,
            range: &UITextRange,
            _direction: UITextLayoutDirection,
        ) -> Option<Retained<UITextPosition>> {
            let (a, b) = range_bounds(range);
            Some(ImeEditor::make_position(a.max(b)))
        }

        #[unsafe(method_id(characterRangeByExtendingPosition:inDirection:))]
        fn characterRangeByExtendingPosition_inDirection(
            &self,
            position: &UITextPosition,
            _direction: UITextLayoutDirection,
        ) -> Option<Retained<UITextRange>> {
            let at = position_offset(position);
            Some(ImeEditor::make_range(at, at))
        }

        #[unsafe(method(baseWritingDirectionForPosition:inDirection:))]
        fn baseWritingDirectionForPosition_inDirection(
            &self,
            _position: &UITextPosition,
            _direction: UITextStorageDirection,
        ) -> NSWritingDirection {
            NSWritingDirection(0)
        }

        #[unsafe(method(setBaseWritingDirection:forRange:))]
        fn setBaseWritingDirection_forRange(
            &self,
            _direction: NSWritingDirection,
            _range: &UITextRange,
        ) {
        }

        #[unsafe(method(firstRectForRange:))]
        fn firstRectForRange(&self, _range: &UITextRange) -> NSRect {
            self.with_state(|s| s.caret_rect)
        }

        #[unsafe(method(caretRectForPosition:))]
        fn caretRectForPosition(&self, _position: &UITextPosition) -> NSRect {
            self.with_state(|s| s.caret_rect)
        }

        #[unsafe(method_id(selectionRectsForRange:))]
        fn selectionRectsForRange(
            &self,
            _range: &UITextRange,
        ) -> Retained<NSArray<UITextSelectionRect>> {
            NSArray::new()
        }

        #[unsafe(method_id(closestPositionToPoint:))]
        fn closestPositionToPoint(
            &self,
            _point: CGPoint,
        ) -> Option<Retained<UITextPosition>> {
            Some(ImeEditor::make_position(0))
        }

        #[unsafe(method_id(closestPositionToPoint:withinRange:))]
        fn closestPositionToPoint_withinRange(
            &self,
            _point: CGPoint,
            range: &UITextRange,
        ) -> Option<Retained<UITextPosition>> {
            let (a, _) = range_bounds(range);
            Some(ImeEditor::make_position(a))
        }

        #[unsafe(method_id(characterRangeAtPoint:))]
        fn characterRangeAtPoint(
            &self,
            _point: CGPoint,
        ) -> Option<Retained<UITextRange>> {
            None
        }

        #[unsafe(method_id(textInputView))]
        fn textInputView(&self) -> Retained<UIView> {
            self.retain().into_super()
        }

        #[unsafe(method(selectionAffinity))]
        fn selectionAffinity(&self) -> UITextStorageDirection {
            UITextStorageDirection::Forward
        }

        #[unsafe(method(setSelectionAffinity:))]
        fn setSelectionAffinity(&self, _selection_affinity: UITextStorageDirection) {}
    }

    unsafe impl UITextInputTraits for ImeEditor {
        #[unsafe(method(keyboardType))]
        fn keyboardType(&self) -> UIKeyboardType {
            self.with_state(|s| s.keyboard)
        }

        #[unsafe(method(setKeyboardType:))]
        fn setKeyboardType(&self, keyboard_type: UIKeyboardType) {
            self.update_state(|s| s.keyboard = keyboard_type);
        }

        #[unsafe(method(returnKeyType))]
        fn returnKeyType(&self) -> UIReturnKeyType {
            self.with_state(|s| s.return_key)
        }

        #[unsafe(method(setReturnKeyType:))]
        fn setReturnKeyType(&self, return_key_type: UIReturnKeyType) {
            self.update_state(|s| s.return_key = return_key_type);
        }

        #[unsafe(method(isSecureTextEntry))]
        fn isSecureTextEntry(&self) -> bool {
            self.with_state(|s| s.secure)
        }

        #[unsafe(method(setSecureTextEntry:))]
        fn setSecureTextEntry(&self, secure_text_entry: bool) {
            self.update_state(|s| s.secure = secure_text_entry);
        }

        #[unsafe(method(autocorrectionType))]
        fn autocorrectionType(&self) -> UITextAutocorrectionType {
            if self.with_state(|s| s.autocorrect) {
                UITextAutocorrectionType::Default
            } else {
                UITextAutocorrectionType::No
            }
        }

        #[unsafe(method(setAutocorrectionType:))]
        fn setAutocorrectionType(&self, autocorrection_type: UITextAutocorrectionType) {
            self.update_state(|s| {
                s.autocorrect = !matches!(
                    autocorrection_type,
                    UITextAutocorrectionType::No
                )
            });
        }

        #[unsafe(method(autocapitalizationType))]
        fn autocapitalizationType(&self) -> UITextAutocapitalizationType {
            if self.with_state(|s| s.autocap_sentences) {
                UITextAutocapitalizationType::Sentences
            } else {
                UITextAutocapitalizationType::None
            }
        }

        #[unsafe(method(setAutocapitalizationType:))]
        fn setAutocapitalizationType(
            &self,
            autocapitalization_type: UITextAutocapitalizationType,
        ) {
            self.update_state(|s| {
                s.autocap_sentences = matches!(
                    autocapitalization_type,
                    UITextAutocapitalizationType::Sentences
                        | UITextAutocapitalizationType::Words
                        | UITextAutocapitalizationType::AllCharacters
                )
            });
        }

        #[unsafe(method_id(textContentType))]
        fn textContentType(&self) -> Option<Retained<NSString>> {
            self.with_state(|s| s.content.clone())
        }

        #[unsafe(method(setTextContentType:))]
        fn setTextContentType(&self, text_content_type: Option<&NSString>) {
            self.update_state(|s| {
                s.content = text_content_type.map(|t| NSString::from_str(&t.to_string()));
            });
        }
    }
);

/// Plain-Rust helpers. The `define_class!` methods above serve UIKit and
/// must not be called directly; these serve them and the poll loop.
impl ImeEditor {
    fn spawn() -> Retained<Self> {
        let this = Self::alloc(mtm()).set_ivars(Mutex::new(EditorState::default()));
        unsafe { msg_send![super(this), init] }
    }

    fn insert_str(&self, text: &str) {
        self.update_state(|s| {
            let range = ordered(s.sel.0, s.sel.1, &s.text);
            let (a, b) = (range.start, range.end);
            s.text.replace_range(a..b, text);
            let caret = a + text.len();
            s.sel = (caret, caret);
            // Typing commits the mark unless the caret stays inside it.
            let inside = s.marked.is_some_and(|(m, n)| {
                let (lo, hi) = if m <= n { (m, n) } else { (n, m) };
                let caret_units = bytes_to_utf16(&s.text, caret);
                lo <= caret_units && caret_units <= hi
            });
            if !inside {
                s.marked = None;
            }
        });
    }

    fn with_state<R>(&self, f: impl FnOnce(&EditorState) -> R) -> R {
        f(&self.ivars().lock().unwrap())
    }

    fn update_state(&self, f: impl FnOnce(&mut EditorState)) {
        let delegate = self.with_state(|s| s.delegate.clone());
        if let Some(delegate) = &delegate {
            let text_input: &ProtocolObject<dyn UITextInput> = ProtocolObject::from_ref(self);
            unsafe {
                let _: () = msg_send![delegate, textWillChange: text_input];
                let _: () = msg_send![delegate, selectionWillChange: text_input];
            }
        }
        f(&mut self.ivars().lock().unwrap());
        if let Some(delegate) = &delegate {
            let text_input: &ProtocolObject<dyn UITextInput> = ProtocolObject::from_ref(self);
            unsafe {
                let _: () = msg_send![delegate, textDidChange: text_input];
                let _: () = msg_send![delegate, selectionDidChange: text_input];
            }
        }
    }

    fn make_position(offset: usize) -> Retained<UITextPosition> {
        ImePosition::new(offset).into_super()
    }

    fn make_range(start: usize, end: usize) -> Retained<UITextRange> {
        ImeRange::new(start, end).into_super()
    }

    fn text_len_utf16(&self) -> usize {
        self.with_state(|s| s.text.encode_utf16().count())
    }

    /// Byte range of a UTF-16 span, snapped to boundaries.
    fn byte_range(&self, start: usize, end: usize) -> std::ops::Range<usize> {
        self.with_state(|s| {
            let a = utf16_to_bytes(&s.text, start);
            let b = utf16_to_bytes(&s.text, end);
            if a <= b { a..b } else { b..a }
        })
    }

    fn snapshot(&self) -> (String, (usize, usize), Option<(usize, usize)>) {
        self.with_state(|s| (s.text.clone(), s.sel, s.marked))
    }

    fn apply_snapshot(&self, text: &str, sel: (usize, usize), marked: Option<(usize, usize)>) {
        self.ivars().lock().unwrap().set_all(text, sel, marked);
    }

    fn configure(
        &self,
        keyboard: UIKeyboardType,
        return_key: UIReturnKeyType,
        secure: bool,
        autocorrect: bool,
        autocap_sentences: bool,
        content: Option<Retained<NSString>>,
    ) {
        let mut state = self.ivars().lock().unwrap();
        state.keyboard = keyboard;
        state.return_key = return_key;
        state.secure = secure;
        state.autocorrect = autocorrect;
        state.autocap_sentences = autocap_sentences;
        state.content = content;
    }

    fn set_field_rect(&self, x: f64, y: f64) {
        use objc2_foundation::{NSPoint, NSSize};
        let mut state = self.ivars().lock().unwrap();
        state.caret_rect = NSRect::new(NSPoint::new(x, y), NSSize::new(2.0, 20.0));
    }
}

impl ImePosition {
    fn new(offset: usize) -> Retained<Self> {
        let this = Self::alloc(mtm()).set_ivars(Mutex::new(offset));
        unsafe { msg_send![super(this), init] }
    }

    fn get(&self) -> usize {
        *self.ivars().lock().unwrap()
    }
}

impl ImeRange {
    fn new(start: usize, end: usize) -> Retained<Self> {
        let this = Self::alloc(mtm()).set_ivars(Mutex::new((start, end)));
        unsafe { msg_send![super(this), init] }
    }

    fn get(&self) -> (usize, usize) {
        *self.ivars().lock().unwrap()
    }
}
fn position_offset(obj: &UITextPosition) -> usize {
    obj.downcast_ref::<ImePosition>()
        .map(|p| p.get())
        .unwrap_or(0)
}

/// Bounds of one of our ranges; foreign ranges read as empty.
fn range_bounds(obj: &UITextRange) -> (usize, usize) {
    obj.downcast_ref::<ImeRange>()
        .map(|r| r.get())
        .unwrap_or((0, 0))
}

/// Ordered byte range of UTF-16 offsets, snapped to boundaries.
fn ordered(a: usize, b: usize, text: &str) -> std::ops::Range<usize> {
    let (mut x, mut y) = (utf16_to_bytes(text, a), utf16_to_bytes(text, b));
    if x > y {
        std::mem::swap(&mut x, &mut y);
    }
    x..y
}

/// Byte range of the directional selection, for deletions.
fn ordered_byte_range(s: &EditorState) -> (usize, usize) {
    let r = ordered(s.sel.0, s.sel.1, &s.text);
    (r.start, r.end)
}

/// Previous character start at or before a byte offset.
fn prev_char_start(text: &str, at: usize) -> Option<usize> {
    text[..at.min(text.len())]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("text input runs on the main thread")
}

impl EditorState {
    fn set_all(&mut self, text: &str, sel: (usize, usize), marked: Option<(usize, usize)>) {
        self.text = text.to_owned();
        self.sel = sel;
        self.marked = marked.filter(|(a, b)| a != b);
    }
}

/// Snapshot for change detection: text plus ordered UTF-16 offsets.
type Snapshot = (String, usize, usize, Option<(usize, usize)>);

fn snapshot_of(text: &str, sel: (usize, usize), marked: Option<(usize, usize)>) -> Snapshot {
    let (mut a, mut b) = sel;
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let marked = marked
        .map(|(mut m, mut n)| {
            if m > n {
                std::mem::swap(&mut m, &mut n);
            }
            (m, n)
        })
        .filter(|(m, n)| m != n);
    let len = text.encode_utf16().count();
    (text.to_owned(), a.min(len), b.min(len), marked)
}

/// The iOS editor contract for one focused field.
pub struct IosText {
    editor: Option<Retained<ImeEditor>>,
    parent: Option<std::ptr::NonNull<std::ffi::c_void>>,
    /// Which document the editor serves; a switch re-focuses.
    doc_ptr: Option<*const ()>,
    keyboard_px: std::sync::Arc<Mutex<f32>>,
    /// Kept alive: dropping unregisters the keyboard observations.
    _observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    last_seen: Snapshot,
    multiline: bool,
}

impl IosText {
    pub fn new(waker: crate::window::Waker) -> Self {
        let keyboard_px = std::sync::Arc::new(Mutex::new(0.0f32));
        let mut observers = Vec::new();
        // Keyboard notices post on the main thread; the block only records
        // the height and wakes the loop, and the redraw applies it.
        // External linkage: reading the statics needs unsafe.
        let names = unsafe {
            [
                (objc2_ui_kit::UIKeyboardWillShowNotification, false),
                (objc2_ui_kit::UIKeyboardWillHideNotification, true),
            ]
        };
        for (name, hide) in names {
            let keyboard_px = keyboard_px.clone();
            let waker = waker.clone();
            let block = block2::StackBlock::new(move |notif: std::ptr::NonNull<NSNotification>| {
                if hide {
                    *keyboard_px.lock().unwrap() = 0.0;
                } else {
                    let notif: &NSNotification = unsafe { notif.as_ref() };
                    if let Some(h) = keyboard_height(notif) {
                        *keyboard_px.lock().unwrap() = h;
                    }
                }
                waker.wake();
            });
            let block = block.copy();
            let center = NSNotificationCenter::defaultCenter();
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
            };
            observers.push(token);
        }
        IosText {
            editor: None,
            parent: None,
            doc_ptr: None,
            keyboard_px,
            _observers: observers,
            last_seen: (String::new(), 0, 0, None),
            multiline: false,
        }
    }

    pub fn is_live(&self) -> bool {
        self.editor.is_some()
    }

    /// Current keyboard height in points (0 when hidden).
    pub fn keyboard_height(&self) -> f32 {
        *self.keyboard_px.lock().unwrap()
    }

    /// Focus gained: configure the editor for the field, seed it from the
    /// document, park its pixel by the field, and take first responder.
    /// False when the view refuses, and the field stays unfocused for typing.
    pub fn focus(
        &mut self,
        parent: &UIView,
        field: &ImeField,
        doc: &TextDocument,
        x: f64,
        y: f64,
    ) -> bool {
        let editor = match &self.editor {
            Some(editor) => editor.clone(),
            None => ImeEditor::spawn(),
        };
        let (keyboard, secure, autocorrect, autocap, content) = match field.kind {
            ImeKind::Text => (UIKeyboardType::Default, false, true, true, None),
            ImeKind::Email => (
                UIKeyboardType::EmailAddress,
                false,
                false,
                false,
                // External linkage: reading the static needs unsafe.
                Some(unsafe { objc2_ui_kit::UITextContentTypeUsername.retain() }),
            ),
            ImeKind::Password => (
                UIKeyboardType::Default,
                true,
                false,
                false,
                Some(unsafe { objc2_ui_kit::UITextContentTypePassword.retain() }),
            ),
            ImeKind::Number => (
                UIKeyboardType::NumberPad,
                false,
                false,
                false,
                Some(NSString::from_str("one-time-code")),
            ),
        };
        editor.configure(
            keyboard,
            if field.multiline {
                UIReturnKeyType::Default
            } else {
                UIReturnKeyType::Done
            },
            secure,
            autocorrect,
            autocap,
            content,
        );
        let caret = bytes_to_utf16(doc.text(), doc.caret());
        editor.apply_snapshot(doc.text(), (caret, caret), None);
        editor.set_field_rect(x, y);
        place_editor(&editor, x, y);
        let ptr = std::ptr::NonNull::from(parent).cast::<std::ffi::c_void>();
        if self.parent != Some(ptr) || self.editor.is_none() {
            parent.addSubview(&editor);
            self.parent = Some(ptr);
        }
        let became = editor.becomeFirstResponder();
        self.editor = became.then_some(editor);
        self.multiline = field.multiline;
        if became {
            let (text, sel, marked) = self.snapshot();
            self.last_seen = snapshot_of(&text, sel, marked);
        }
        became
    }

    /// Focus lost.
    pub fn blur(&mut self) {
        if let Some(editor) = self.editor.take() {
            editor.resignFirstResponder();
            editor.removeFromSuperview();
        }
        self.parent = None;
        self.doc_ptr = None;
    }

    /// Makes sure the editor serves this document: a field switch blurs
    /// first, so typing never lands in the wrong document. True when live.
    pub fn ensure(
        &mut self,
        parent: &UIView,
        field: &ImeField,
        doc: &TextDocument,
        x: f64,
        y: f64,
    ) -> bool {
        let ptr = std::ptr::from_ref(doc).cast::<()>();
        if self.doc_ptr != Some(ptr) {
            self.blur();
        }
        if !self.is_live() && !self.focus(parent, field, doc, x, y) {
            return false;
        }
        self.doc_ptr = Some(ptr);
        self.track_field(x, y);
        true
    }

    /// Moves the parked pixel with the field. Cheap; the frame change does
    /// not disturb first responder.
    pub fn track_field(&self, x: f64, y: f64) {
        if let Some(editor) = &self.editor {
            place_editor(editor, x, y);
            editor.set_field_rect(x, y);
        }
    }

    fn snapshot(&self) -> (String, (usize, usize), Option<(usize, usize)>) {
        self.editor
            .as_ref()
            .map(|e| e.snapshot())
            .unwrap_or_default()
    }

    /// Polls once. Returns whether the document changed (needs a redraw)
    /// and whether a single-line field committed a newline (advance or
    /// submit).
    pub fn poll(&mut self, doc: &mut TextDocument) -> (bool, bool) {
        if self.editor.is_none() {
            return (false, false);
        }
        let (text, sel, marked) = self.snapshot();
        if snapshot_of(&text, sel, marked) == self.last_seen {
            // Our own edits since the last mirror go back to the editor.
            let caret = bytes_to_utf16(doc.text(), doc.caret());
            if sel != (caret, caret) || marked.is_some() || text != doc.text() {
                if let Some(editor) = &self.editor {
                    editor.apply_snapshot(doc.text(), (caret, caret), None);
                }
                let (text, sel, marked) = self.snapshot();
                self.last_seen = snapshot_of(&text, sel, marked);
            }
            return (false, false);
        }
        let start = utf16_to_bytes(&text, sel.0);
        let end = utf16_to_bytes(&text, sel.1);
        let marked = marked.map(|(m, n)| {
            let (mut a, mut b) = (utf16_to_bytes(&text, m), utf16_to_bytes(&text, n));
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            (a, b)
        });
        doc.replace_all(
            &text,
            start..end,
            marked.filter(|(a, b)| a != b).map(|(a, b)| a..b),
        );
        let mut changed = true;
        let mut newline = false;
        if !self.multiline && doc.text().contains('\n') {
            let stripped = doc.text().trim_end_matches('\n').to_owned();
            if stripped.len() != doc.text().len() {
                let end = stripped.len();
                doc.replace_all(&stripped, end..end, None);
                newline = true;
            }
        }
        // Echo back what stuck, so the next poll compares against reality.
        if let Some(editor) = &self.editor {
            let caret = bytes_to_utf16(doc.text(), doc.caret());
            editor.apply_snapshot(doc.text(), (caret, caret), None);
        }
        let (text, sel, marked) = self.snapshot();
        self.last_seen = snapshot_of(&text, sel, marked);
        changed |= newline;
        (changed, newline)
    }
}

/// Parks the 1px editor by the field: candidate and autocorrect UI anchor
/// near it, while no touch can meaningfully land on a pixel.
fn place_editor(editor: &ImeEditor, x: f64, y: f64) {
    use objc2_foundation::{NSPoint, NSSize};
    editor.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(1.0, 1.0)));
    editor.setAlpha(0.0);
}

/// winit's view, the only legal parent. `None` when the handle is missing.
pub fn parent_view(window: &winit::window::Window) -> Option<&UIView> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::UiKit(handle) => {
            Some(unsafe { &*handle.ui_view.as_ptr().cast::<UIView>() })
        }
        _ => None,
    }
}

/// Keyboard end-frame height in points, if the notice carries one.
fn keyboard_height(notif: &NSNotification) -> Option<f32> {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    let info = notif.userInfo()?;
    let value = unsafe { info.valueForKey(objc2_ui_kit::UIKeyboardFrameEndUserInfoKey) }?;
    let value: &NSValue = value.downcast_ref()?;
    // No `CGRectValue` binding exists; read the documented `{CGRect=...}`
    // bytes directly. iOS is 64-bit only, so four `f64`s.
    let mut rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 0.0,
            height: 0.0,
        },
    };
    unsafe {
        value.getValue_size(
            std::ptr::NonNull::from(&mut rect).cast::<std::ffi::c_void>(),
            std::mem::size_of::<CGRect>(),
        );
    }
    Some(rect.size.height as f32)
}
