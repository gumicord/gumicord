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
- **arm64-v8a only** for now; 32-bit ARM and x86 emulators when needed.
- **External storage first** for the data dir (`getExternalFilesDir`, USB-visible), internal as fallback. Set once as `GUMICORD_DATA_DIR` before the loop starts.
- **No Java/Kotlin of our own**: the manifest points at `GameActivity` directly.

## Still open

| | |
|---|---|
| JNI bridge for `InputConnection` | The biggest mobile risk (roadmap A2). Try the platform's standard path first |
| GLES backend tuning | Rendering avoids compute shaders, so GLES is enough; wgpu picks GL before Vulkan on Android like on Windows |
| Exact dependency pins | `games-activity`, `appcompat`, NDK and AGP versions are pinned to releases that exist at the time of writing; if Maven/CI says otherwise, bump and note why |

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
