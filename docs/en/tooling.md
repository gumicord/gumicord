# Tooling

The machinery around development: task runner (`xtask`), plugin SDK
(`sdk`), conformance screenshots (`tools/screenshot`), distribution
sources (`packaging/`).

# xtask (`xtask`)

> The `cargo xtask` task runner. No `just` or `make`; checks, generation,
> and compatibility live here. Depends only on `gumicord-uitree`.

## Files

### `src/main.rs` — Command dispatch and tasks. `check-fast` (fast
workspace compile check), `check` (full validation), `check-light`
(build-free checks only), `fmt`, `lint`, `test`, `schema` (JSON Schema
check via `spec/schema/validate.mjs`), `sdk` (type-guarantee check), `abi` (stable-ID compatibility,
`--accept` updates the snapshot), `gen` (regenerate from spec).

### `src/uitree.rs` — Generation and ABI checks from stable IDs. The only
source is `core/uitree/src/ids.rs`; `spec/03-uitree.md` and
`sdk/src/ids.ts` are overwritten from it. Removals and renames are
refused as breaking.

# sdk (`sdk`)

> The TypeScript SDK plugin authors import. A thin façade over host
> injection.

## Files

### `src/index.ts` — Public façade. `ui.patch/exists/wrap/after/before/settings/stack/node/text/badge/button/icon`,
`log`, `storage`. `patch` runs once, bottom-up, in registration order;
`wrap` cannot make core IDs.

### `src/uitree.ts` — UITree types. `data` is typed from the ID and
read-only.

### `src/runtime.ts` — Plugin-side runtime. Order and exception handling
are part of the ABI; exceptions return to their branch and count failures.

### `src/ids.ts` — Generated. From `cargo xtask gen`; never hand-edit.
Unknown IDs fail type checking.

### `src/data.ts` — `data` field types (`UserData`/`MessageData`/...). No
raw Discord payloads surface.

### `package.json` — `@gumicord/sdk` (`build`/`typecheck` and friends).

### `test/run.mjs` — Type-guarantee runner. Positive cases must pass,
negative cases must fail, checked by esbuild bundling plus direct `tsc`.

### `test/positive.ts` — Examples that must pass.

### `test/negative/` — Examples that must fail: `typo-node-id.ts` (unknown IDs), `wrong-data-field.ts` (off-type
`data`), `data-mutation.ts` (`data` writes), `core-id-via-node.ts` and `core-id-creation.ts` (core-ID creation).

### `bin/gumicord-plugin.mjs` — `gumicord-plugin build/dev`. Bundles to a
self-contained IIFE.

# tools/screenshot (`tools/screenshot`)

> Headless conformance screenshots. Renders fixed scenes and
> compares against blessed images with tolerance.

## Files

### `Cargo.toml` — `gumicord-screenshot` (binary `screenshot`).
`gumicord-render/theme/uitree` plus `png`.

### `src/main.rs` — The comparer. Three scenes (`login`/`chat`/`chat-hidpi`)
in ASCII with bundled fonts only; `--rebless` refreshes blessed images;
mismatches keep actuals and diffs. Skips successfully with no GPU.
Blessed images live in `render/tests/screenshots/<os>/`.

# packaging/ (`packaging/`)

> Distribution sources assembled by CI's `dist` job.

## Files

### `README.md` — Source list and assembly (unsigned).

### `linux/gumicord.desktop` — Desktop entry for the AppImage.

### `macos/Info.plist` — The `Gumicord.app` bundle definition.

### `icons/app-icon.png` — 4096px icon master (macOS `sips` source).

### `icons/gumicord.png` — Same mark at 512px (desktop-entry sized).
