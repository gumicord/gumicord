# App

The shared screen-and-state logic (`app/core`) plus thin per-platform
shells. Governing specs:
[`spec/10-login-screen-ui.md`](../../spec/10-login-screen-ui.md),
[`spec/11-multi-account.md`](../../spec/11-multi-account.md).

# gumicord-app (`app/core`)

> Owns screens, state, and frame-pipeline order; called from every platform.

## Files

### `src/lib.rs` — The `Gumicord` app state binding screens, input, and
theme selection. Key items: `Gumicord::new()`, `Panes::for_width()`,
`Composing`, `LoginField`, `SettingsView`, the `Application` impl
(`start`/`wake`/`pressed`/`scrolled`). The clock reads once per frame
head. Mobile shows no titlebar and writes first-frame diagnostics to
`diag.log`.

### `src/live.rs` — Live data wiring. `Live` binds `Store`, gateway, REST,
and cache. Key items: `LiveEvent`, `Live::without_cache()`,
`open_channel()`, `extend_members()`, `send_message()`, `mark_read()`.
Cache first, REST replaces, gateway follows, in that order, so a late
cache never overwrites REST. Bots skip `MESSAGE_ACK` and take rosters
over op 8. Following chunks route by account kind, not index presence.

### `src/session.rs` — Login state and background progress.
`Session::Connecting/WaitingForScan/Password/PasswordTotp/Token/Exchanging/LoggedIn/Failed`
is what the screen reads. Key items: `Login::start()`,
`submit_password()`, `submit_totp()`, `cancel_password()`, `poll()`.
QR is default and saved tokens are tried first, discarded immediately on
failure.

### `src/account.rs` — Multi-account saving and switching. Tokens live
under per-account keys (`account_user_<id>` / `account_bot_<id>`) in the
OS secure store; the index holds no tokens. Key items:
`AccountsIndex::load()`, `remember()`, `load_token()`, `remove()`.
`remember` sweeps the legacy single keys.

### `src/a11y.rs` — UITree to screen-reader tree translation. QR payloads
and hidden spoilers are not read. Key items: `tree_update()`. Parent
qualified stable IDs keep frames aligned.

### `src/assets.rs` — Theme background asset resolution and fetching.
Unapproved remotes stay untouched until declared and approved; failures
return fallback colors with warnings. Key items:
`ThemeAssets::request()`, `poll_ask()`, `approve_hosts()`.

### `src/demo.rs` — Fixed dummy data for renderer and theme checks.
Japanese-heavy, covering wrapping, mentions, spoilers, links.

### `src/images.rs` — Avatar fetching, decoding, handoff. Six at a time,
128px longest side; pixels never ride the tree, only `take_images()`.

### `src/markdown.rs` — Parsed bodies to UITree tinting. Inline
decorations fold into `Span`; themes decide the look. Key items: `Ink`,
`Reveals`.

### `src/menu.rs` — Floating layer: menus, confirm dialogs, toasts.
Irreversible actions pass a dialog previewing the body; items address by
index. Key items: `Floating`, `Menu`, `Confirm`, `Action`.

### `src/time.rs` — ISO 8601 rendering with home-grown civil-date
routines only. Key items: `parse_unix()`, `continues()`.

# app/desktop (`app/desktop`)

> Desktop entry. Lifecycle and logging only; everything real lives in
> `gumicord-app`.

## Files

### `src/main.rs` — Startup. Probe children exit at once, then the shared
loop runs `gumicord_platform::run(Gumicord::new())`. `GUMICORD_LOG` is
`info` for our crates only; dependencies default to `warn` via
`GUMICORD_LOG_DEPS`. No console window on Windows: every line goes to
both the run log under `logs/` and stderr. Static CRT, no redistributable.

### `Cargo.toml` — The `gumicord` binary.

# app/android (`app/android`)

> Android entry. GameActivity lifecycle and data-directory choice only.

## Files

### `src/lib.rs` — The `cdylib` entry. `android_main()`, `data_dir()`,
`init_tls_verifier()`. External storage first, internal fallback; the
chosen path is set once as `GUMICORD_DATA_DIR`, the JVM is handed to the
TLS verifier, then the shared loop runs. Exits the process when the loop
ends so the next launch starts fresh under the one-loop rule.

### `Cargo.toml` — `gumicord-android` (lib name `main`, `cdylib`). Android-only
`android_logger` and friends.

### `settings.gradle` / `build.gradle` — Repositories and the AGP version
in one place.

### `app/build.gradle` — The app. `namespace/applicationId
dev.gumicord.app`, NDK version, SDK 34/min 26, `abiFilters
arm64-v8a/x86_64`, `games-activity:4.4.0` (paired with android-activity
0.6; the 2.x line dies before any Rust runs). No minify; `pickFirst` the
`libc++_shared.so`.

### `gradle.properties` — `android.useAndroidX=true` only.

### `app/src/main/AndroidManifest.xml` — Single `GameActivity` declaration.
Only `INTERNET`/`ACCESS_NETWORK_STATE`; no camera, mic, or storage
permissions.

### `app/src/main/res/values/themes.xml` — `GumicordTheme` extending
`Theme.AppCompat.NoActionBar`. Rust draws everything.

### `README.md` — Layout, ABI, external-first storage, file logging, build
steps.

# app/ios (`app/ios`)

> iOS entry. A thin static library the Xcode-owned bundle calls once.

## Files

### `src/lib.rs` — `gumicord_ios_main(documents_dir)`. Copies the C string
into `GUMICORD_DATA_DIR` and runs the shared loop. Never call
`UIApplicationMain` from Swift first: winit calls it itself.

### `Cargo.toml` — `gumicord-ios` (static library).

### `Gumicord/main.swift` — Swift entry. Fetches Documents and hands it over.

### `Gumicord/Gumicord-Bridging-Header.h` — The `gumicord_ios_main`
declaration only.

### `Gumicord/Info.plist` — Bundle definition. Files-visible Documents.

### `Gumicord.xcodeproj/project.pbxproj` — Hand-written minimal Xcode
project. Builds only `main.swift`, links `libgumicord_ios.a`, no signing,
deployment 17.0.

### `README.md` — Layout, winit ownership, unsigned builds, Files
visibility, build steps.
