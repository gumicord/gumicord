# Renderer

The platform-independent draw core (`render/render`) and the OS
integration layer (`render/platform`). Governing spec:
[`spec/06-renderer.md`](../../spec/06-renderer.md).

# gumicord-render (`render/render`)

> Takes a UITree and issues GPU draw commands. No OS-specific code.

## Files

### `src/lib.rs` — The `Renderer` drawing one frame: scrolling, hit
testing, link/spoiler press resolution, missing-image collection. Key
items: `Renderer`, `Renderer::new()`, `Renderer::headless()`,
`Renderer::render()`, `FrameStats`, `Hit`. Hit testing answers against
the previous frame's layout. Headless drawing with readback
(`Gpu::read_pixels`) serves screenshots.

### `src/gpu.rs` — wgpu setup and submit. Two pipelines (rect, text),
surface/headless output, delta uploads, ordered backend candidates. Key
items: `Gpu`, `GpuError`, `Gpu::new()`, `Gpu::headless()`,
`Gpu::submit()`. Windows tries GL first (about 1/16th the resident size
of DX12 by measurement); candidates are pre-verified in `probe` child
processes. Headless prefers fallback adapters for deterministic output.

### `src/shader.wgsl` — Hand-written shared rect/text shader. SDF rounded
rects plus textured quads. No compute shaders (keeps GLES viable).

### `src/text.rs` — Shaping and the glyph atlas. cosmic-text shaping plus
a shelf-packed RGBA8 atlas (glyphs top-down, images bottom-up, up to 4
pages). Key items: `TextEngine`, `Shaper`, `Shaper::shape_rich()`,
`TextEngine::put_image()`. Japanese fallback order is hand-rolled around
Han unification; system fonts enumerate on a background thread and fold
in later.

### `src/draw.rs` — The single logical-to-physical pixel conversion from
layout results. Stacks backgrounds, text, icons, images, and QR, merging
runs per pipeline and scissor. Key items: `DrawList`, `Run`, `RunKind`,
`build()`. No depth buffer: draw order is overlap order. Missing images
are not drawn but reported.

### `src/layout.rs` — Constraint layout. Three axes only: Row, Column,
Stack. Key items: `layout()`, `LayoutResult`, `Placed`, `ScrollState`.
End-pinned lists remember intent, not position; scrolled children clip
and stop hitting.

### `src/geom.rs` — Logical-pixel geometry types. Physical conversion
happens once just before drawing. Key items: `Rect`, `Size`,
`Rect::contains()`, `Rect::intersect()`. `contains` excludes the far edge.

### `src/backgrounds.rs` — One texture per theme background image. Never
in the atlas; uploaded with CPU-generated mip chains. Key items:
`Backgrounds`, `Backgrounds::put()`, `mip_levels()`. Switching themes
forgets the old pictures via `clear()`.

### `src/font_cache.rs` — On-disk cache for system font enumeration.
Verifies by file identity and re-parses only the unproven. Key items:
`Stats`, `populate()`. Also counts CJK families for diagnostics.

### `src/icon.rs` — Icons drawn as textures, not fonts. Polylines on a
unit square rasterized at the requested size into the glyph atlas. Key
items: `IconDef`, `ICONS`, `lookup()`. Unknown names draw nothing, never
error.

### `src/intrinsic.rs` — Default layout table per stable ID. Widths, axes,
scrollability the theme does not write are decided here; explicit theme
values win. Not part of the extension ABI; changes only move pixels.

### `src/motion.rs` — Time-driven animation of resolved style values
toward targets. First-seen nodes do not move; unused tracks are dropped.
Key items: `Motion`, `Motion::new()`, `Motion::apply()`.

### `src/probe.rs` — Child-process GPU backend verification. A broken
driver kills the process while the instance is created, so the same
binary boots as `--probe-gpu=<backend>` and only answering backends
survive. Key items: `surviving_backends()`, `run_probe()`.

# gumicord-platform (`render/platform`)

> OS-touching integration: window and event loop, input and IME,
> clipboard, secret storage, URL opening, captcha. Drawing itself belongs
> to `gumicord-render`.

## Files

### `src/lib.rs` — Entry, re-exports, panic hook, file logger. Key items:
`install_panic_hook()`, `init_file_logging()`, `prepare_run_log()`,
`write_diag_file()`, `Application`, `Waker`. The IME candidate area takes the whole input
field.

### `src/file_dialog.rs` — Desktop native file picker. Key items:
`PickOptions`, `FileFilter`, `FileDialogError`, `pick_file()`.
Cancellation comes back empty. Phones stay unsupported.

### `src/window.rs` — Decoration-less window and on-demand-redraw event
loop. Titlebar-area drag moves, 6px edge resize, press/release-split
control buttons, scrollbar grabs, link/spoiler-first press resolution,
IME and key delivery, blink and next-frame waits. Key items:
`Application`, `Waker`, `FrameCx`, `PlatformError`, `run()`,
`RevealRequest`, `ImeProxy`. Maximized state is asked, never kept;
resizes to the real size just before drawing. Mobile leaves window size
to the OS with no titlebar.

### `src/text_input/mod.rs` — OS-independent text input interface. Edit
keys, hidden keys, and clipboard ops without OS types. Key items:
`TextInputHost`, `EditKey`, `HiddenKey`, `ClipboardOp`, `TextDocument`.
Enter/Escape belong to the caller, not the document.

### `src/text_input/document.rs` — The editable text itself, no OS APIs.
All positions are UTF-8 byte offsets; caret moves by grapheme. Key
items: `TextDocument`, `insert()`, `set_composition()`, `selection()`,
`take()`. Holds the uncommitted composition range apart.

### `src/touch.rs` — Pure gesture recognition from raw touch points. Tap,
scroll deltas, swipes; second fingers are ignored. Key items: `Tracker`,
`TouchAction`, `Swipe`, `Tracker::press()`, `release()`.

### `src/clipboard.rs` — Text and image clipboard. Win32 on Windows,
`arboard` on Linux/macOS, `UIPasteboard` on iOS, Android not yet. Key
items: `ClipboardImage`, `ClipboardError`, `set_text()`, `text()`,
`set_image()`, `image()`. `Busy` never hides failure; open always pairs
with close.

### `src/secret.rs` — The OS secure store. Never writes plaintext where
encryption is unavailable; unsupported platforms log in again each
start. Key items: `SecretStore`, `SecretError`, `store()`, `load()`,
`clear()`. Windows is DPAPI, Linux/macOS is keyring, Android/iOS are not
yet.

### `src/proxy.rs` — Invisible iOS login fields for password autofill
(iOS only). winit only speaks `UIKeyInput`, so hidden username+password
`UITextField` twins receive the fill and poll it back into documents.
Key items: `Proxy`, `ProxyEvent`, `set_active()`, `poll()`. Visible
editing stays in our own fields.

### `src/clock.rs` — OS time. Key items: `local_utc_offset_minutes()`,
`now_unix()`, `caret_blink_interval()`.

### `src/dirs.rs` — App data directory resolution. Key items:
`app_data_dir()`. `GUMICORD_DATA_DIR` (empty counts as unset) wins; the
mobile shells set it at startup.

### `src/url.rs` — OS handoff for URLs. Key items: `open_url()`. Only
http/https; no scheme is guessed.

### `src/captcha/mod.rs` — Captcha presentation abstraction. The app only
handles raw data. Key items: `CaptchaChallenge`, `CaptchaHost`,
`WebView2Captcha`. Non-Windows `solve()` is an `Unsupported` stub.

### `src/captcha/webview2.rs` — Windows-only WebView2 captcha host. Opens
the hCaptcha page in a child window and receives the token over IPC.
