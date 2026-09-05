//! The window and the event loop.
//!
//! The OS title bar is not used, so a theme's reach extends to the window's
//! edge on every platform. What it provided has to be replaced: dragging the
//! title bar area moves the window, a 6px border resizes it, the control
//! buttons are told apart by their slot, and the cursor follows the zone.
//! Snap layouts, which Windows attaches to a real maximise button, are not
//! handled.
//!
//! If the title bar disappears and the drawing area looks cropped, suspect a
//! surface larger than the window: the GL origin is bottom-left, so the top of
//! the UI lands above the visible area and the right edge overflows. Missing
//! one resize notification is enough to cause it, so [`Host::redraw`] resizes
//! the surface from the window's real size immediately before drawing.
//!
//! Maximising a borderless window used to overflow the work area. Do not try
//! to fix that here: winit already clamps the client rect in `WM_NCCALCSIZE`,
//! and correcting it again makes the window smaller by the border width. Nor
//! should the maximised state be tracked locally — see [`Host::maximized`].
//!
//! Redraws are on demand. Drawing continuously cannot coexist with stopping
//! while inactive, so the loop waits and only redraws when something changed.

use std::sync::Arc;

use crate::captcha::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha, WebView2Captcha};
use crate::text_input::{ClipboardOp, EditKey, HiddenKey, TextDocument};
use gumicord_render::{Hit, Presented, Renderer, ScrollGrab, Size};
use gumicord_uitree::{Key, NodeId, UiNode};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

/// Width of the resize grip at the window's edge.
const RESIZE_BORDER: f32 = 6.0;
/// Distance per wheel notch, for platforms reporting lines.
const LINE_SCROLL: f32 = 48.0;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("cannot create the event loop: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    #[error("cannot create the window: {0}")]
    Window(#[from] winit::error::OsError),
    #[error("GPU setup failed: {0}")]
    Gpu(#[from] gumicord_render::GpuError),
}

/// What is known while building one frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameCx {
    /// The viewport, which themes also match `when.maxWidth` against.
    pub viewport: Size,
    pub scale: f32,
}

/// Scrolls a scroll region so a node becomes visible.
#[derive(Debug, Clone, PartialEq)]
pub struct RevealRequest {
    pub region: NodeId,
    pub id: NodeId,
    pub key: Option<Key>,
}

/// The application, as the platform layer sees it. No OS types appear here,
/// so winit never leaks into the app crate.
pub trait Application {
    /// Builds the tree, with the style already resolved.
    fn build(&mut self, cx: &FrameCx) -> UiNode;

    /// The node under the pointer changed. `hits` runs front to back, and is
    /// empty when the pointer leaves the window.
    fn hover_changed(&mut self, hits: &[Hit]) -> bool;

    /// The primary button was pressed. Title bar presses are handled before
    /// this.
    fn pressed(&mut self, hits: &[Hit]) -> bool;

    /// A link run was pressed. Returning true opens it with the system
    /// handler; returning false leaves the press to [`Application::pressed`],
    /// which is how a press meant for an open menu declines the link.
    ///
    /// `url` is whatever the tree carried; only http and https reach the OS.
    fn link_pressed(&mut self, _url: &str) -> bool {
        false
    }

    /// A covered spoiler run was pressed, named by its message and which of
    /// that message's spoiler runs it is. The app keeps that state; this
    /// layer only knows where the run landed.
    ///
    /// Returning true takes the press whole, like [`Application::link_pressed`];
    /// false hands it back down.
    fn spoiler_pressed(&mut self, _owner: u64, _run: usize) -> bool {
        false
    }

    /// The secondary button was pressed.
    ///
    /// `at` is where the press happened, not where a menu should go: that
    /// needs the menu's size and the window's.
    fn context_menu(&mut self, _hits: &[Hit], _at: (f32, f32)) -> bool {
        false
    }

    /// A scroll region moved. Deciding what that means is the app's job; this
    /// layer does not know what the list holds.
    fn scrolled(&mut self, _id: NodeId, _at: f32, _max: f32) {}

    /// Redraw after this long even with no input.
    ///
    /// A relative timestamp changes on its own, and left alone "just now"
    /// would sit there for hours. Waking every second is not an option
    /// either, so knowing when the text next changes allows sleeping until
    /// exactly then.
    ///
    /// `None` means there is no reason to wake, not zero. Asked once after
    /// each draw.
    fn next_frame_in(&self) -> Option<std::time::Duration> {
        None
    }

    /// A list that grew at the top and should hold its scroll position.
    /// Consumed once; only the app knows which end grew.
    fn keep_place(&mut self) -> Option<NodeId> {
        None
    }

    /// Scrolls a region so a node becomes visible, on the next frame.
    /// Consumed once; the app knows what was pressed, the renderer knows
    /// where it landed.
    fn take_reveal(&mut self) -> Option<RevealRequest> {
        None
    }

    fn title(&self) -> String;

    /// The document receiving input, if any. This layer has no notion of
    /// focus; the app decides.
    fn focused_document(&mut self) -> Option<&mut TextDocument> {
        None
    }

    /// Commits and sends, on enter.
    ///
    /// Never called while composing: mistaking the enter that commits an IME
    /// candidate for send makes Japanese unusable.
    fn submit(&mut self) -> bool {
        false
    }

    /// Leaves text input, on escape.
    fn cancel_input(&mut self) -> bool {
        false
    }

    /// A hidden-code key on the login screen (the konami sequence on the QR
    /// screen). Nothing else uses the arrows or B/A once no field is focused,
    /// so these are the only keys handed here. Returning true takes the press.
    fn hidden_key(&mut self, _key: HiddenKey) -> bool {
        false
    }

    /// A clipboard operation against the focused field: the Ctrl+C/X/V
    /// shortcuts, and the cut/copy/paste items on a field's menu. Returning
    /// true means the shortcut was consumed.
    fn clipboard(&mut self, _op: ClipboardOp) -> bool {
        false
    }

    /// A captcha challenge waiting to be solved, if any.
    ///
    /// Checked by the platform layer after a wake: when this returns a
    /// challenge it opens the captcha modal, and either hands the solution
    /// back through [`Application::captcha_solved`] or abandons the pending
    /// login through [`Application::captcha_cancelled`].
    fn pending_captcha(&mut self) -> Option<CaptchaChallenge> {
        None
    }

    /// The token produced by a solved captcha, to forward to the login API.
    fn captcha_solved(&mut self, _solved: SolvedCaptcha) {}

    /// The captcha was cancelled; drop the pending login and go back.
    fn captcha_cancelled(&mut self) {}

    /// Called once before the window opens; background work starts here. The
    /// [`Waker`] is the only way another thread can wake the loop.
    fn start(&mut self, _waker: Waker) {}

    /// The atlas evicted images; put back what was fetched. They are still on
    /// disk, so no round trip happens.
    fn images_dropped(&mut self) {}

    /// Requests images that were about to draw and were missing.
    ///
    /// Only what survived clipping arrives, so a 300-row list asks for the
    /// dozen or so actually on screen.
    fn request_images(&mut self, _urls: &[String]) {}

    /// Requests theme background images that were about to draw and were
    /// missing. Separate from `request_images`: those go to the CDN fetcher,
    /// these to the theme asset resolver.
    fn request_backgrounds(&mut self, _keys: &[String]) {}

    /// Which theme the background images currently drawing belong to.
    fn theme_namespace(&self) -> Option<&str> {
        None
    }

    /// Takes fetched images.
    ///
    /// The renderer never touches the network: fetching and decoding are the
    /// app's, and only pixels cross here. The app can drop them afterwards.
    fn take_images(&mut self) -> Vec<gumicord_render::ImageData> {
        Vec::new()
    }

    /// Takes arrived background images, for their own textures.
    fn take_backgrounds(&mut self) -> Vec<gumicord_render::ImageData> {
        Vec::new()
    }

    /// Builds the screen-reader tree from the just-built UITree.
    fn accesskit_update(
        &mut self,
        _tree: &gumicord_uitree::UiNode,
    ) -> Option<accesskit::TreeUpdate> {
        None
    }

    /// Woken by [`Waker::wake`]; drain everything pending. Wakes coalesce, so
    /// the count is not meaningful.
    fn wake(&mut self) -> bool {
        false
    }
}

/// Wakes the event loop from another thread.
///
/// The loop sleeps, and async work happens elsewhere, so without this the
/// screen would sleep through it.
///
/// Carries no payload, only "something happened": the app moves the contents
/// over its own channel. Putting them here would mix winit's types into the
/// app's.
#[derive(Clone, Debug)]
pub struct Waker(winit::event_loop::EventLoopProxy<LoopEvent>);

/// The loop's user events: wakes from other threads, and screen-reader
/// requests delivered through the accesskit adapter.
#[derive(Debug)]
enum LoopEvent {
    Wake,
    AccessKit(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for LoopEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        LoopEvent::AccessKit(event)
    }
}

impl Waker {
    /// Wakes the main thread, from any thread. A no-op once the loop has
    /// ended, which happens normally during shutdown.
    pub fn wake(&self) {
        let _ = self.0.send_event(LoopEvent::Wake);
    }

    /// A second sender for the screen-reader adapter. The adapter needs its
    /// own because each proxy delivers one event type mapping.
    fn proxy(&self) -> winit::event_loop::EventLoopProxy<LoopEvent> {
        self.0.clone()
    }
}

/// Opens the window and runs until exit.
pub fn run(mut app: impl Application + 'static) -> Result<(), PlatformError> {
    let event_loop = EventLoop::<LoopEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // Before the window exists: login may start early, and earlier means the
    // QR appears sooner.
    let waker = Waker(event_loop.create_proxy());
    app.start(waker.clone());

    let mut host = Host {
        app: Box::new(app),
        waker,
        window: None,
        renderer: None,
        adapter: None,
        captcha: WebView2Captcha,
        cursor: (0.0, 0.0),
        zone: Zone::Client,
        hovering_link: false,
        hovering_spoiler: false,
        scroll_grab: None,
        control_pending: None,
        modifiers: ModifiersState::empty(),
        ime_allowed: false,
        first_frame: true,
        started: std::time::Instant::now(),
        blink: crate::clock::caret_blink_interval(),
        caret_on: true,
        next_blink: std::time::Instant::now(),
        next_frame: None,
        motion: gumicord_render::Motion::new(),
    };
    event_loop.run_app(&mut host)?;
    Ok(())
}

/// What the pointer is over, as far as window management is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Client,
    Titlebar,
    Control(&'static str),
    Resize(ResizeDirection),
}

struct Host {
    app: Box<dyn Application>,
    /// Wakes the loop from another thread; handed to the renderer so system
    /// fonts can ask for a redraw once the background thread has them.
    waker: Waker,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// Speaks to the OS screen reader. Created before the window first
    /// shows; without it Narrator never connects.
    adapter: Option<accesskit_winit::Adapter>,
    /// Presents a captcha challenge as a modal over the window (ADR-0007).
    captcha: WebView2Captcha,
    /// Pointer position.
    cursor: (f32, f32),
    zone: Zone,
    /// Whether the pointer is over a link run; picks the hand cursor.
    hovering_link: bool,
    /// Whether the pointer is over a covered spoiler; the hand fits there
    /// too, since an open one can be pressed back shut.
    hovering_spoiler: bool,
    /// The scrollbar being dragged, while held.
    scroll_grab: Option<ScrollGrab>,
    /// A title-bar control button armed on press; acted on on release.
    ///
    /// Acting on press lets Windows hand the release to whatever is now under
    /// the pointer, so a disappearing window, a losing focus (minimize) or a
    /// moving layout (maximize) all triggered the window underneath or a
    /// stray point. Waiting for the release keeps the action and its release
    /// in this window. The stored slot is the armed button, so a press that
    /// drags away and releases elsewhere is cancelled like any button.
    control_pending: Option<&'static str>,
    /// Held modifiers.
    modifiers: ModifiersState,
    /// Whether IME is allowed; only changes are told to the OS.
    ime_allowed: bool,
    first_frame: bool,
    /// When `run` started, for measuring time to first frame.
    started: std::time::Instant,
    /// Blink interval; `None` means blinking is switched off.
    blink: Option<std::time::Duration>,
    /// Whether the caret is currently visible.
    caret_on: bool,
    /// When it next toggles.
    next_blink: std::time::Instant,
    /// When to redraw without input; `None` means never.
    /// ([`Application::next_frame_in`])
    next_frame: Option<std::time::Instant>,
    /// Styles in motion; the loop sleeps once they settle.
    motion: gumicord_render::Motion,
}

impl Host {
    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Restarts the blink from visible. A dark phase mid-keystroke loses the
    /// caret, so every input and focus change goes through here.
    fn restart_caret(&mut self) {
        self.caret_on = true;
        if let Some(interval) = self.blink {
            self.next_blink = std::time::Instant::now() + interval;
        }
    }

    /// Nodes under the pointer, front to back.
    fn hits(&self) -> Vec<Hit> {
        let Some(r) = &self.renderer else {
            return Vec::new();
        };
        r.hit_test(self.cursor.0, self.cursor.1).cloned().collect()
    }

    /// The link run under the pointer, if any.
    fn link_under_cursor(&self) -> Option<String> {
        let r = self.renderer.as_ref()?;
        r.link_at(self.cursor.0, self.cursor.1).map(str::to_owned)
    }

    /// The covered spoiler run under the pointer, if any.
    fn spoiler_under_cursor(&self) -> Option<(u64, usize)> {
        self.renderer
            .as_ref()?
            .spoiler_at(self.cursor.0, self.cursor.1)
    }

    /// Opens a link with the system handler; the scheme was already checked
    /// on the way in, and is checked again there.
    fn open_link(&self, url: &str) {
        // Not logged: what someone pressed is theirs, and the error says
        // enough to diagnose a refusal.
        if let Err(e) = crate::url::open_url(url) {
            tracing::warn!(%e, "could not open the link");
        }
    }

    /// Whether the window is maximised. Asked, never remembered: it also
    /// happens through the keyboard, edge snapping, the taskbar and
    /// double-clicking, and a stale flag makes the border grab a resize while
    /// maximised and wastes a button press.
    fn maximized(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.is_maximized())
    }

    /// Which window-management zone the pointer is in.
    ///
    /// The resize border is outside the tree, so it goes by coordinates;
    /// everything else goes by stable ID, so a theme moving the title bar
    /// does not break it.
    fn zone_at(&self, hits: &[Hit]) -> Zone {
        if !self.maximized()
            && let Some(r) = &self.renderer
        {
            let v = r.viewport();
            let (x, y) = self.cursor;
            let left = x < RESIZE_BORDER;
            let right = x > v.w - RESIZE_BORDER;
            let top = y < RESIZE_BORDER;
            let bottom = y > v.h - RESIZE_BORDER;
            let dir = match (top, bottom, left, right) {
                (true, _, true, _) => Some(ResizeDirection::NorthWest),
                (true, _, _, true) => Some(ResizeDirection::NorthEast),
                (_, true, true, _) => Some(ResizeDirection::SouthWest),
                (_, true, _, true) => Some(ResizeDirection::SouthEast),
                (true, ..) => Some(ResizeDirection::North),
                (_, true, ..) => Some(ResizeDirection::South),
                (_, _, true, _) => Some(ResizeDirection::West),
                (_, _, _, true) => Some(ResizeDirection::East),
                _ => None,
            };
            if let Some(d) = dir {
                return Zone::Resize(d);
            }
        }

        for h in hits {
            if h.id == NodeId::ChromeTitlebarControl
                && let Some(Key::Slot(slot)) = h.key
            {
                return Zone::Control(slot);
            }
            if h.id == NodeId::ChromeTitlebar {
                return Zone::Titlebar;
            }
        }
        Zone::Client
    }

    fn apply_cursor(&self) {
        let Some(w) = &self.window else { return };
        let icon = match self.zone {
            Zone::Resize(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            Zone::Resize(ResizeDirection::East | ResizeDirection::West) => CursorIcon::EwResize,
            Zone::Resize(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => {
                CursorIcon::NwseResize
            }
            Zone::Resize(ResizeDirection::NorthEast | ResizeDirection::SouthWest) => {
                CursorIcon::NeswResize
            }
            // A hand over a link run or a covered spoiler; the colour alone
            // does not say it presses.
            _ if self.hovering_link || self.hovering_spoiler => CursorIcon::Pointer,
            _ => CursorIcon::Default,
        };
        w.set_cursor(icon);
    }

    /// Feeds IME notifications to the document. On Windows this is enough; no
    /// TSF text store is needed.
    fn on_ime(&mut self, ime: Ime) -> bool {
        let Some(doc) = self.app.focused_document() else {
            return false;
        };
        match ime {
            Ime::Preedit(s, cursor) => {
                // Byte offsets; only the start is used.
                doc.set_composition(&s, cursor.map(|(a, _)| a));
                true
            }
            Ime::Commit(s) => {
                doc.commit_composition(&s);
                true
            }
            // Start and end of composition; the document is unchanged.
            Ime::Enabled | Ime::Disabled => false,
        }
    }

    /// Feeds key input to the document.
    fn on_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};

        let shift = self.modifiers.shift_key();
        let ctrl = self.modifiers.control_key();

        // With no field focused there is nothing to edit, so the arrows and
        // the B/A keys are free for the QR screen's hidden login code.
        if self.app.focused_document().is_none() {
            let hidden = match &event.logical_key {
                Key::Named(NamedKey::ArrowUp) => Some(HiddenKey::Up),
                Key::Named(NamedKey::ArrowDown) => Some(HiddenKey::Down),
                Key::Named(NamedKey::ArrowLeft) => Some(HiddenKey::Left),
                Key::Named(NamedKey::ArrowRight) => Some(HiddenKey::Right),
                Key::Character(c) if c.eq_ignore_ascii_case("b") => Some(HiddenKey::B),
                Key::Character(c) if c.eq_ignore_ascii_case("a") => Some(HiddenKey::A),
                _ => None,
            };
            if let Some(key) = hidden {
                return self.app.hidden_key(key);
            }
        }

        // Enter and escape mean different things while composing.
        let composing = self
            .app
            .focused_document()
            .is_some_and(|d| d.is_composing());

        let edit = match &event.logical_key {
            Key::Named(NamedKey::Backspace) => Some(EditKey::Backspace),
            Key::Named(NamedKey::Delete) => Some(EditKey::Delete),
            Key::Named(NamedKey::ArrowLeft) => Some(EditKey::Left),
            Key::Named(NamedKey::ArrowRight) => Some(EditKey::Right),
            Key::Named(NamedKey::Home) => Some(EditKey::Home),
            Key::Named(NamedKey::End) => Some(EditKey::End),
            Key::Named(NamedKey::Enter) if !composing => Some(EditKey::Enter),
            Key::Named(NamedKey::Escape) => Some(EditKey::Escape),
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => Some(EditKey::SelectAll),
            _ => None,
        };

        match edit {
            Some(EditKey::Enter) => return self.app.submit(),
            Some(EditKey::Escape) => {
                if let Some(doc) = self.app.focused_document()
                    && doc.is_composing()
                {
                    doc.cancel_composition();
                    return true;
                }
                return self.app.cancel_input();
            }
            Some(key) => {
                if let Some(doc) = self.app.focused_document() {
                    return key.apply(doc, shift);
                }
                return false;
            }
            None => {}
        }

        // Composing text arrives as preedit; taking it here duplicates it.
        if composing || ctrl {
            if ctrl {
                let op = match &event.logical_key {
                    Key::Character(c) if c.eq_ignore_ascii_case("c") => Some(ClipboardOp::Copy),
                    Key::Character(c) if c.eq_ignore_ascii_case("x") => Some(ClipboardOp::Cut),
                    Key::Character(c) if c.eq_ignore_ascii_case("v") => Some(ClipboardOp::Paste),
                    _ => None,
                };
                if let Some(op) = op {
                    // Only a focused field takes clipboard input; the app
                    // decides and returns true when the shortcut is consumed.
                    return self.app.clipboard(op);
                }
            }
            return false;
        }
        // Tab and newline can arrive as literal characters.
        match event
            .text
            .as_ref()
            .filter(|t| !t.is_empty() && !t.chars().any(|c| c.is_control()))
        {
            Some(t) => match self.app.focused_document() {
                Some(doc) => {
                    doc.insert(t);
                    true
                }
                None => false,
            },
            None => false,
        }
    }

    /// Tells the IME where the field is, which is what positions the
    /// candidate window; without it, it appears in a corner of the screen.
    fn update_ime_area(&mut self) {
        let (Some(w), Some(r)) = (&self.window, &self.renderer) else {
            return;
        };
        let has_input = self.app.focused_document().is_some();

        // No IME events arrive until this is allowed; winit defaults to off.
        //
        // Only on change: per frame this keeps resetting the IME context.
        if has_input != self.ime_allowed {
            self.ime_allowed = has_input;
            w.set_ime_allowed(has_input);
            tracing::debug!(allowed = has_input, "toggled IME");
        }
        if !has_input {
            return;
        }

        // The field's position comes from the hit record.
        let Some(field) = r
            .hit_boxes()
            .iter()
            .find(|h| h.id == NodeId::ChatInputField)
        else {
            return;
        };
        w.set_ime_cursor_area(
            winit::dpi::LogicalPosition::new(field.rect.x, field.rect.y),
            winit::dpi::LogicalSize::new(field.rect.w, field.rect.h),
        );
    }

    fn redraw(&mut self) {
        // Resize the surface from the window's real size, right before
        // drawing.
        //
        // Missing a `Resized` leaves the surface stale, the swapchain
        // permanently outdated, and reconfiguring only rebuilds it at the old
        // size — the window stays blank. This happened. One `inner_size()` per
        // frame, and resizing to the same size is a no-op.
        let size = self.window.as_ref().map(|w| w.inner_size());
        if let (Some(size), Some(r)) = (size, &mut self.renderer) {
            r.resize(size.width, size.height);
        }

        // Fold in the system fonts the background thread collected before
        // building the tree, so the redraw they woke shows them. The result
        // is not needed: whatever the draw below re-shapes will use the new
        // set either way.
        let _ = self
            .renderer
            .as_mut()
            .is_some_and(Renderer::process_font_update);

        let caret_on = self.caret_on;
        // Before drawing, so this frame can use them.
        // Report evictions first, or the replacements miss this frame.
        if self
            .renderer
            .as_mut()
            .is_some_and(Renderer::took_image_recycle)
        {
            self.app.images_dropped();
        }
        // What the previous frame was missing; only visible things.
        if let Some(r) = &self.renderer {
            let want: Vec<String> = r.missing_images().to_vec();
            if !want.is_empty() {
                self.app.request_images(&want);
            }
            let want: Vec<String> = r.missing_backgrounds().to_vec();
            if !want.is_empty() {
                self.app.request_backgrounds(&want);
            }
        }
        let images = self.app.take_images();
        let backgrounds = self.app.take_backgrounds();
        // Holds the scroll position for one frame after a prepend.
        let keep_place = self.app.keep_place();
        let reveal = self.app.take_reveal();
        let moving;

        let (stats, backend) = {
            let Some(r) = &mut self.renderer else { return };
            r.set_caret_visible(caret_on);
            r.set_theme_namespace(self.app.theme_namespace());
            if let Some(id) = keep_place {
                r.keep_place(id);
            }
            if let Some(req) = reveal {
                r.reveal(req.region, req.id, req.key.as_ref());
            }
            for image in &images {
                r.put_image(image);
            }
            for image in &backgrounds {
                r.put_background(image);
            }
            let cx = FrameCx {
                viewport: r.viewport(),
                scale: r.scale(),
            };
            tracing::trace!(w = cx.viewport.w, h = cx.viewport.h, "drawing");
            let mut tree = self.app.build(&cx);

            // Move the resolved styles towards their targets.
            //
            // The tree's shape is untouched; only settled values move.
            // ([`gumicord_render::motion`])
            moving = self.motion.apply(&mut tree, std::time::Instant::now());

            if let Some(adapter) = self.adapter.as_mut()
                && let Some(update) = self.app.accesskit_update(&tree)
            {
                adapter.update_if_active(|| update);
            }

            (r.render(&tree), r.backend())
        };

        // Only while something moves; once settled, stop asking and sleep.
        if moving {
            self.request_redraw();
        }

        // Asked every frame: after "3 minutes ago" the next change is a
        // minute away, but after "59 seconds ago" it is one second.
        self.next_frame = self
            .app
            .next_frame_in()
            .map(|d| std::time::Instant::now() + d);

        // A failed present asks again: the loop waits, so giving up here
        // leaves the window blank until the next input. Being merely hidden
        // does not, or it would spin for as long as the window is minimised.
        if stats.presented == Presented::Failed {
            self.request_redraw();
            return;
        }

        // Hover changes with layout, not only with the pointer: scrolling
        // moves rows under a stationary cursor, and hit testing answers
        // against the previous frame. Without rechecking here the highlight
        // sticks to the row that moved away.
        if self.app.hover_changed(&self.hits()) {
            self.request_redraw();
        }

        // The field's position is only known after layout, and it is what
        // positions the IME candidate window.
        self.update_ime_area();

        if self.first_frame && stats.presented == Presented::Yes {
            self.first_frame = false;
            tracing::info!(
                ?backend,
                nodes = stats.nodes,
                rects = stats.rects,
                glyphs = stats.glyphs,
                draw_calls = stats.draw_calls,
                // Cold start has a budget; without measuring it there is no
                // way to tell whether it is met.
                ms = self.started.elapsed().as_millis() as u64,
                "最初のフレームを描いた"
            );
        }
    }

    /// Open the captcha modal if the app is asking for one, and route its
    /// outcome back to the app. The solve blocks this thread while the OS
    /// message queue is pumped, like a native dialog.
    fn pump_captcha(&mut self) {
        let Some(challenge) = self.app.pending_captcha() else {
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        match self.captcha.solve(&window, challenge) {
            Ok(solved) => self.app.captcha_solved(solved),
            // A failed or cancelled solve leaves the flow mid-login; abandon
            // it rather than leave a stuck form, and let the app go back.
            Err(e) => {
                if !matches!(e, CaptchaError::Cancelled) {
                    tracing::error!(%e, "captcha could not be solved");
                }
                self.app.captcha_cancelled();
            }
        }
        self.request_redraw();
    }
}

impl ApplicationHandler<LoopEvent> for Host {
    /// Decides what to wait for; the caret blink is driven from here.
    ///
    /// Nothing wakes for a blink when no field has focus, since there is
    /// nothing to blink. The rate follows the OS setting, which can also be
    /// "never", in which case the caret stays lit.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = std::time::Instant::now();

        // Sleep until the earliest deadline; watching only one stalls the
        // other.
        let mut until: Option<std::time::Instant> = None;
        let mut soonest = |at: std::time::Instant| {
            until = Some(until.map_or(at, |cur: std::time::Instant| cur.min(at)));
        };

        // Caret blink.
        //
        // Nothing to blink without a focused field.
        match self.blink.filter(|_| self.app.focused_document().is_some()) {
            Some(interval) => {
                if now >= self.next_blink {
                    self.caret_on = !self.caret_on;
                    self.next_blink = now + interval;
                    self.request_redraw();
                }
                soonest(self.next_blink);
            }
            // Reset so the next focus starts lit.
            None => self.caret_on = true,
        }

        // Time-dependent display.
        if let Some(at) = self.next_frame {
            if now >= at {
                // Redrawing settles the next deadline too.
                self.next_frame = None;
                self.request_redraw();
            } else {
                soonest(at);
            }
        }

        // With no reason, sleep: a stale deadline wakes for no change.
        event_loop.set_control_flow(match until {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }

    /// Woken by [`Waker::wake`]; the payload comes over the app's own
    /// channel. Screen-reader requests arrive here too, through the adapter.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: LoopEvent) {
        match event {
            LoopEvent::Wake => {
                // Fonts arriving from their background thread also wake the loop; a
                // redraw folds them in if the app has nothing to draw.
                let fonts = self.renderer.as_ref().is_some_and(Renderer::fonts_pending);
                let woke = self.app.wake() || fonts;
                // A wake may be the login flow asking for a captcha; the modal runs
                // here, on the thread with the window handle.
                self.pump_captcha();
                if woke {
                    self.request_redraw();
                }
            }
            LoopEvent::AccessKit(event) => {
                use accesskit_winit::WindowEvent as AccessKitEvent;
                match event.window_event {
                    // The tree goes out on the next redraw, which this asks
                    // for; building it anywhere else would split the frame.
                    AccessKitEvent::InitialTreeRequested => self.request_redraw(),
                    AccessKitEvent::ActionRequested(request) => {
                        tracing::debug!(?request, "screen-reader actions are not handled yet");
                    }
                    AccessKitEvent::AccessibilityDeactivated => {}
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        // No OS title bar.
        let attrs = Window::default_attributes()
            .with_title(self.app.title())
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0))
            .with_decorations(false)
            // The screen-reader adapter must exist before the first show.
            .with_visible(false);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!(%e, "could not create the window");
                event_loop.exit();
                return;
            }
        };
        self.adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.waker.proxy(),
        ));
        window.set_visible(true);

        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        // The renderer starts with the bundled font and unfolds system fonts
        // on a background thread; a wake lets a sleeping loop know they are
        // ready. The window must exist before the first frame measures text.
        let wake = {
            let w = self.waker.clone();
            Box::new(move || w.wake()) as Box<dyn Fn() + Send + Sync + 'static>
        };
        match Renderer::new(
            window.clone().into(),
            size.width,
            size.height,
            scale,
            wake,
            crate::app_data_dir().map(|d| d.join("fonts")),
            crate::app_data_dir().map(|d| d.join("gpu")),
        ) {
            Ok(r) => self.renderer = Some(r),
            Err(e) => {
                tracing::error!(%e, "could not initialise the GPU");
                event_loop.exit();
                return;
            }
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                // Log the screen size too, so overflow can be shown by
                // subtraction rather than guessed at: the expected value is
                // the screen minus the taskbar.
                let screen = self
                    .window
                    .as_ref()
                    .and_then(|w| w.current_monitor())
                    .map(|m| m.size());
                tracing::debug!(
                    w = size.width,
                    h = size.height,
                    max = self.maximized(),
                    screen_w = screen.map(|s| s.width),
                    screen_h = screen.map(|s| s.height),
                    "リサイズ"
                );
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                self.request_redraw();
            }

            // Follows DPI changes and moves between displays.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(r) = &mut self.renderer {
                    r.set_scale(scale_factor as f32);
                }
                self.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.renderer.as_ref().map_or(1.0, Renderer::scale) as f64;
                self.cursor = ((position.x / scale) as f32, (position.y / scale) as f32);

                // While dragging a scrollbar nothing else is consulted; the
                // grip is kept even when the pointer leaves the thumb.
                if let (Some(grab), Some(r)) = (self.scroll_grab, &mut self.renderer) {
                    let moved = r.drag_scrollbar(&grab, self.cursor.1);
                    let owner = grab.owner();
                    let (at, max) = r.scroll_place(owner);
                    self.app.scrolled(owner, at, max);
                    if moved {
                        self.request_redraw();
                    }
                    return;
                }

                let hits = self.hits();
                let zone = self.zone_at(&hits);
                if zone != self.zone {
                    self.zone = zone;
                    self.apply_cursor();
                }
                // Asked against the last frame's layout, so a run that just
                // scrolled under the pointer is noticed one frame late.
                let on_link = self.link_under_cursor().is_some();
                let on_spoiler = self.spoiler_under_cursor().is_some();
                if on_link != self.hovering_link || on_spoiler != self.hovering_spoiler {
                    self.hovering_link = on_link;
                    self.hovering_spoiler = on_spoiler;
                    self.apply_cursor();
                }
                if self.app.hover_changed(&hits) {
                    self.request_redraw();
                }
            }

            WindowEvent::CursorLeft { .. } => {
                // Dragging past the window edge is ordinary; treating it as a
                // release makes the scrollbar vanish mid-drag.
                if self.scroll_grab.is_some() {
                    return;
                }
                if self.hovering_link || self.hovering_spoiler {
                    self.hovering_link = false;
                    self.hovering_spoiler = false;
                    self.apply_cursor();
                }
                if self.app.hover_changed(&[]) {
                    self.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * LINE_SCROLL,
                    MouseScrollDelta::PixelDelta(p) => {
                        let scale = self.renderer.as_ref().map_or(1.0, Renderer::scale) as f64;
                        -(p.y / scale) as f32
                    }
                };
                let hits = self.hits();
                let Some(r) = &mut self.renderer else { return };
                // The frontmost scroll region under the pointer.
                let target = hits
                    .iter()
                    .find(|h| gumicord_render::intrinsic(h.id).scroll)
                    .map(|h| h.id);
                let Some(id) = target else { return };
                let moved = r.scroll_by(id, dy);
                let (at, max) = r.scroll_place(id);
                // Reported even when nothing moved: scrolling further at the
                // top is the request for more history, and by then the
                // position no longer changes.
                self.app.scrolled(id, at, max);
                if moved {
                    self.request_redraw();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(w) = self.window.clone() else { return };
                match self.zone {
                    // Drag to move.
                    Zone::Titlebar => {
                        if let Err(e) = w.drag_window() {
                            tracing::warn!(%e, "could not drag the window");
                        }
                    }
                    // Drag the edge to resize.
                    Zone::Resize(dir) => {
                        if let Err(e) = w.drag_resize_window(dir) {
                            tracing::warn!(%e, "could not resize the window");
                        }
                    }
                    // Acted on release, not press: see `control_pending`. The
                    // unknown-slot path is still logged here, on press.
                    Zone::Control(slot @ ("minimize" | "maximize" | "close")) => {
                        self.control_pending = Some(slot);
                    }
                    Zone::Control(other) => {
                        tracing::debug!(slot = other, "unknown title bar button");
                    }
                    Zone::Client => {
                        // Scrollbars come before the app: they overlap the
                        // list, and only one can answer.
                        let grabbed = self
                            .renderer
                            .as_mut()
                            .and_then(|r| r.grab_scrollbar(self.cursor.0, self.cursor.1));
                        if let Some(g) = grabbed {
                            self.scroll_grab = Some(g);
                            self.request_redraw();
                            return;
                        }

                        let hits = self.hits();

                        // A link takes the press whole: opening it and
                        // toggling whatever is underneath at the same time
                        // would be two answers to one press. The app may
                        // decline, which hands the press back below.
                        if let Some(url) = self.link_under_cursor()
                            && self.app.link_pressed(&url)
                        {
                            self.open_link(&url);
                            return;
                        }

                        // A spoiler run likewise: covered text presses as a
                        // whole, and the app may decline while a menu floats.
                        // A run with a link under it already went to the link.
                        if let Some((owner, run)) = self.spoiler_under_cursor()
                            && self.app.spoiler_pressed(owner, run)
                        {
                            self.request_redraw();
                            return;
                        }

                        if self.app.pressed(&hits) {
                            // A dark caret right after pressing the field
                            // leaves it unclear whether the press landed.
                            self.restart_caret();
                            self.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.scroll_grab = None;
                // The action runs here, not on press, so the release is
                // consumed by this window and the one underneath is spared.
                // Only if the pointer is still over the armed button does it
                // count, matching how a dragged-off button press is cancelled.
                if let Some(slot) = self.control_pending.take()
                    && matches!(self.zone, Zone::Control(s) if s == slot)
                {
                    let Some(w) = self.window.clone() else { return };
                    match slot {
                        "minimize" => w.set_minimized(true),
                        "maximize" => w.set_maximized(!w.is_maximized()),
                        "close" => event_loop.exit(),
                        other => tracing::debug!(slot = other, "unknown title bar button"),
                    }
                }
            }

            // Secondary press. Not over window chrome: right-clicking a title
            // bar opens the OS window menu, not the app's.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if self.zone == Zone::Client {
                    let hits = self.hits();
                    if self.app.context_menu(&hits, self.cursor) {
                        self.request_redraw();
                    }
                }
            }

            // Focus is deliberately not consulted: an inactive window still
            // has to redraw when it resizes or its contents change, and a
            // window tiled beside another would otherwise stay blank. Not
            // drawing while inactive comes from waiting and redrawing on
            // change, not from here.
            // Composing text; not committed.
            WindowEvent::Ime(ime) => {
                if self.on_ime(ime) {
                    // A dark phase mid-keystroke loses the caret.
                    self.restart_caret();
                    self.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if self.on_key(&event) {
                    self.restart_caret();
                    self.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }

            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }
}
