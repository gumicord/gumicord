# iOS entry point

The Xcode wrapper, and nothing else. Everything beyond lifecycle and
passing the Documents directory across lives in `app/core`.

## Layout

| | |
|---|---|
| `Cargo.toml` / `src/lib.rs` | The `gumicord-ios` staticlib. `gumicord_ios_main(documents_dir)` runs the shared loop; the pointer is copied before returning |
| `Gumicord/` | Swift bootstrap (`AppDelegate`), bridging header, `Info.plist` |
| `Gumicord.xcodeproj/` | Hand-written minimal project. No Xcodegen, no CocoaPods, no SPM |
| `lib/` | Staging for `libgumicord_ios.a`, copied here by CI. Git-ignored |

## Decisions

- **No signing.** `CODE_SIGNING_ALLOWED=NO`; CI zips the unsigned `.app` as `Payload/` into an `.ipa` for sideloading. Passing App Store review is unlikely anyway.
- **Files-visible Documents.** `UIFileSharingEnabled` + `LSSupportsOpeningDocumentsInPlace`, so themes, logs and the database get on and off the phone through the Files app.
- **Staticlib, not a framework.** One archive, linked with `-lgumicord_ios`; no module maps or umbrella headers to maintain.

## Still open

| | |
|---|---|
| `UITextInput` | The biggest mobile risk (roadmap I2). Try the platform's standard path first |
| `accesskit_ios` | Still at 0.1.2; try it early, since its maturity is unknown |
| Safe area and keyboard tracking | PLT-040/PLT-041. The shell draws edge to edge today |
| First-device run | The Xcode project, lifecycle order and Metal backend have never run on hardware. Expect a shake-out pass |

## Building (macOS only)

```bash
rustup target add aarch64-apple-ios
cargo build --release --target aarch64-apple-ios -p gumicord-ios
mkdir -p app/ios/lib
cp target/aarch64-apple-ios/release/libgumicord_ios.a app/ios/lib/
xcodebuild -project app/ios/Gumicord.xcodeproj -scheme Gumicord \
  -configuration Release -destination 'generic/platform=iOS' \
  CODE_SIGNING_ALLOWED=NO build
# Unsigned IPA:
cd build-dir && mkdir Payload && cp -r Gumicord.app Payload/ && zip -r Gumicord.ipa Payload
```

See [`spec/07-roadmap.md`](../../spec/07-roadmap.md) (written in Japanese).
