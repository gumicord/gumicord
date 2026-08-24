# iOS entry point

**Empty for now; work starts in M1.2, and needs a macOS machine.**

This will hold the Xcode project wrapper, and nothing else.

## Decide before starting

| | Why |
|---|---|
| Implementing `UITextInput` | Large and unverified — the biggest risk here. Try the platform's standard path before concluding a custom one is needed |
| `accesskit_ios` | Still at 0.1.2; try it early, since its maturity is unknown |
| Distribution | Passing App Store review is unlikely, so assume sideloading |

See [`spec/07-roadmap.md`](../../spec/07-roadmap.md) (written in Japanese).
