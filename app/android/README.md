# Android entry point

**Empty for now; work starts in M1.2.**

This will hold the Gradle + NDK wrapper, and nothing else. Everything beyond
lifecycle and passing native handles across lives in `app/core`.

## Decide before starting

| | Why |
|---|---|
| Use `android-game-activity` | `accesskit`'s Android backend supports GameActivity only; it does not work on NativeActivity |
| JNI bridge for `InputConnection` | Large and unverified — the biggest risk here. Try the platform's standard path before concluding a custom one is needed |
| GLES backend | Rendering avoids compute shaders, so GLES is enough |

See [`spec/07-roadmap.md`](../../spec/07-roadmap.md) (written in Japanese).
