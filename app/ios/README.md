# iOS entry point

The Xcode wrapper, and nothing else. Everything beyond lifecycle and
passing the Documents and Caches directories across lives in `app/core`.

## Layout

| | |
|---|---|
| `Cargo.toml` / `src/lib.rs` | The `gumicord-ios` staticlib. `gumicord_ios_main(documents_dir, caches_dir)` runs the shared loop; the pointers are copied before returning |
| `Gumicord/` | Swift entry (`main.swift`), bridging header, `Info.plist` |
| `Gumicord.xcodeproj/` | Hand-written minimal project. No Xcodegen, no CocoaPods, no SPM |
| `lib/` | Staging for `libgumicord_ios.a`, copied here by CI. Git-ignored |

## Decisions

- **winit owns the lifecycle.** Swift is only `main.swift` handing over the
  Documents and Caches directories; `EventLoop::run` calls `UIApplicationMain` itself. A
  Swift `@main` entry calls it first and makes winit abort at startup.
  LiveContainer also jumps to the guest's main, so this suits both.
- **Text input through a hidden `UITextInput` editor** (`render/platform`
  `ios_text`, ADR-0011). winit's view only speaks `UIKeyInput`; the 1px
  editor beside it gives conversion, candidates and autocorrect while pixels
  stay ours. Every field edits through it, with content types set per
  field. Keyboard height
  comes from `UIKeyboardWillShow/Hide` notices and shrinks the layout
  viewport (`PLT-040`).
- **No signing.** `CODE_SIGNING_ALLOWED=NO`; CI zips the unsigned `.app` as `Payload/` into an `.ipa` for sideloading. Passing App Store review is unlikely anyway.
- **Files-visible Documents.** `UIFileSharingEnabled` + `LSSupportsOpeningDocumentsInPlace`, so themes and logs get on and off the phone through the Files app. The message database and image files live in Caches instead: the sandbox forbids the `HOME/.cache` fallback, and history refetches.
- **Logs are files.** `logs/gumicord.log` (plus `panic.log`) sits in Documents for the same reason: no Mac is needed to read a crash.
- **Staticlib, not a framework.** One archive, linked with `-lgumicord_ios`; no module maps or umbrella headers to maintain.

## Still open

| | |
|---|---|
| `UITextInput` | Done (ADR-0011): hidden editor in `render/platform`. Field verification pending |
| `accesskit_ios` | Still at 0.1.2; try it early, since its maturity is unknown |
| Safe area | `PLT-041`. The surface spans the full view and the root content is inset by `safeAreaInsets`; keyboard tracking (`PLT-040`) is in |
| First-device run | The Xcode project, lifecycle order and Metal backend have never run on hardware. Expect a shake-out pass |

## Building (macOS only)

```bash
rustup target add aarch64-apple-ios
GUMICORD_CHANNEL=nightly GUMICORD_COMMIT=$(git rev-parse --short HEAD) \
  cargo build --release --target aarch64-apple-ios -p gumicord-ios
mkdir -p app/ios/lib
cp target/aarch64-apple-ios/release/libgumicord_ios.a app/ios/lib/
xcodebuild -project app/ios/Gumicord.xcodeproj -scheme Gumicord \
  -configuration Release -destination 'generic/platform=iOS' \
  CODE_SIGNING_ALLOWED=NO build
# Unsigned IPA:
cd build-dir && mkdir Payload && cp -r Gumicord.app Payload/ && zip -r Gumicord.ipa Payload
```

`GUMICORD_CHANNEL=nightly` defaults the file log to debug (phones cannot
set `GUMICORD_LOG`), and `GUMICORD_COMMIT` stamps the run log's first line
with the build. Omit both for a quiet local build.

See [`spec/07-roadmap.md`](../../spec/07-roadmap.md) (written in Japanese).
