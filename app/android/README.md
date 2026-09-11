# Android entry point

The Gradle + NDK wrapper, and nothing else. Everything beyond lifecycle
and passing native handles across lives in `app/core`.

## Layout

| | |
|---|---|
| `Cargo.toml` / `src/lib.rs` | The `gumicord-android` cdylib (`libmain.so`). `android_main` picks the data dir, then calls the shared `gumicord_platform::run_android` |
| `app/` | The Gradle app: manifest (GameActivity), theme, `build.gradle` |
| `settings.gradle`, `build.gradle` | Toolchain versions. No Gradle wrapper is vendored; CI installs Gradle via `gradle/actions/setup-gradle` |

## Decisions (from the README's table, settled)

- **GameActivity**, not NativeActivity: `accesskit`'s Android backend supports GameActivity only.
- **arm64-v8a plus x86_64** for now; the emulator is x86_64 and cannot run
  arm64 code without translation. 32-bit ARM comes back when someone needs it.
- **External storage first** for the data dir (`getExternalFilesDir`), internal as fallback. Set once as `GUMICORD_DATA_DIR` before the loop starts. External is *not* USB-visible anymore: modern Android hides the app's directory from the Files app and USB alike.
- **Logs are files, shared out.** `logs/gumicord-<stamp>.log` (plus `panic-<stamp>.log`, newest five each) sits next to the data; the settings screen's support page hands the newest to the share sheet through a FileProvider (`logs/` only). Every exit and every panic also ferries copies to Downloads. `logcat` works too, but nothing requires `adb`.
- **No Java/Kotlin of our own**: the manifest points at `GameActivity` directly.
- **RGBA window buffers**: GameActivity hands out an opaque RGBX window, but wgpu's GLES backend picks an EGL config with 8-bit alpha (the surface is sRGB). Strict drivers (Mali) answer `eglCreateWindowSurface` with `BadAlloc`, so `ensure_renderer` asks for `R8G8B8A8_UNORM` via `ANativeWindow_setBuffersGeometry` (size 0 keeps the size) before creating the surface.

## Still open

| | |
|---|---|
| JNI bridge for `InputConnection` | The biggest mobile risk (roadmap A2). Try the platform's standard path first |
| GLES backend tuning | Rendering avoids compute shaders, so GLES is enough; wgpu picks GL before Vulkan on Android like on Windows |
| Exact dependency pins | `games-activity`, `appcompat`, NDK and AGP versions are pinned to releases that exist at the time of writing; if Maven/CI says otherwise, bump and note why. `games-activity` must stay on the 4.x line: `android-activity` 0.6 only speaks that Java interface, and 2.x dies before any Rust runs |

## Building

Requires the Android SDK + NDK (CI does this). Then, from the repo root:

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
cargo ndk -t arm64-v8a -o app/android/app/src/main/jniLibs build --release -p gumicord-android
# libc++_shared.so next to libmain.so, from the NDK:
# $ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so
gradle -p app/android assembleDebug
```

See [`spec/07-roadmap.md`](../../spec/07-roadmap.md) (written in Japanese).
