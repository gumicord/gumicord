# Architecture

## Big picture

```
plugins (.js) ──patch──▶ UITree ──theme──▶ renderer ──▶ screen
                                ▲                    │
Discord REST/gateway ──▶ Store ─┘                    ▼
                                            platform layer
                                      (window, input, secrets)
```

- The only extension ABI is the [semantic UITree](../../spec/adr/0004-semantic-uitree-as-extension-abi.md). Themes and plugins only touch the UITree.
- The UI renderer is [hand-rolled Rust + wgpu](../../spec/adr/0001-native-rust-renderer.md).
- Plugin language is TypeScript, runtime is [QuickJS](../../spec/adr/0002-quickjs-plugin-runtime.md).
- [One monorepo](../../spec/adr/0003-monorepo.md).

## Crate map

| Layer | Crates | Page |
|---|---|---|
| Protocol and state | `core/model`, `core/rest`, `core/gateway`, `core/store` | [core.md](core.md) |
| UI data | `core/uitree`, `core/theme`, `core/markdown` | [uidata.md](uidata.md) |
| Extensions | `core/plugin` | [plugin-runtime.md](plugin-runtime.md) |
| Rendering | `render/render`, `render/platform` | [render.md](render.md) |
| Screens | `app/core`, `app/desktop`, `app/android`, `app/ios` | [app.md](app.md) |
| Tooling | `xtask`, `sdk`, `tools/screenshot`, `packaging` | [tooling.md](tooling.md) |

## Frame pipeline

1. The app builds a UITree (`app/core`)
2. Plugins patch it (diffs only)
3. The theme resolves styles
4. The renderer lays out and draws

## Building and checking

```bash
npm install
(cd sdk && npm install)

cargo check -p <affected-package>   # ordinary check
cargo test -p <package> <test_name> # when behavior changed

cargo xtask check-light  # spec-only changes
cargo xtask check        # full validation before submitting
cargo xtask fmt          # format
cargo xtask gen          # regenerate from spec/
```

No `just` or `make`. Project-specific operations go through `cargo xtask`
(see [tooling](tooling.md)); ordinary Rust work uses plain Cargo commands.

## Repository root

- `Cargo.toml` — Workspace definition and pinned dependency versions.
- `.cargo/config.toml` — The `cargo xtask` alias and build parallelism cap.
- `.github/workflows/ci.yml` — CI (checks, builds, distribution, screenshots, nightly).
- `LICENSE` — MIT.
- `.gitignore` — Build outputs and local settings like `local.properties`.
