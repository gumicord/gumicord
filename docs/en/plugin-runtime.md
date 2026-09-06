# core: plugin runtime

QuickJS plugin loading, isolation, permission enforcement, and UITree
patching (`core/plugin`). Governing spec:
[`spec/05-plugin-api.md`](../../spec/05-plugin-api.md). For writing
plugins, see the [Plugin API](https://github.com/gumicord/api-docs/blob/main/en/plugins.md).

## Files

### `src/lib.rs` — Host entry. Re-exports the modules and defines
developer-facing English error types. Permissions are enforced by
not injecting, and patches carry diffs only. Key items: `PluginError`,
`PluginHost`, `PluginManager`, `Manifest`, `Storage`.

### `src/manager.rs` — Loading, chaining, disabling. An ordered host set
runs the chain synchronously while a worker thread hands only the newest
tree to the main thread, turning runaway plugins into delayed effects.
Also persists approvals/denials/disables and computes settings-screen
rows. Key items: `PluginManager`, `ManagerEvent`,
`PluginManager::start()`, `drain()`. No capabilities loads immediately;
capabilities ask approval on first sight, and an empty grant stays a
denial. Chronic failures unload; chains over 8ms warn once a minute.

### `src/host.rs` — One plugin's QuickJS home. Pins a `Runtime` +
`Context` pair to a thread and confines all execution to closures.
Reloading discards the whole home and rebuilds. Key items: `PluginHost`,
`PluginSource`, `PluginHost::load()`, `apply_tree()`. Only granted
capabilities are injected; undeclared APIs are just `TypeError`. Settings
pages read in a read-only throwaway world, killed at 100ms. Failures count
100 per 60-second window.

### `src/manifest.rs` — `manifest.json` identity and declared-capability
checking. Hand-edited permission widening is rejected at load. Key items:
`Manifest`, `Manifest::load()`, `KNOWN_CAPABILITIES`. IDs are reverse-DNS,
versions are semantic, unknown or duplicate capabilities are refused.

### `src/convert.rs` — Two-way host-boundary conversion. Structure and
draw content cross; domain facts travel via `ctx`. Returns validate per
section; unknown IDs reject that output wholesale. Key items:
`PatchContext`, `node_to_js()`, `js_to_node()`, `data_key()`. Keys,
states, colors, and references inherit from the input tree; the JS side
is never trusted.

### `src/storage.rs` — Per-plugin host-side key-value storage. Survives
reloads; nobody else's keys are ever visible. Key items: `Storage`,
`Storage::load()`, `get()`, `set()`, `remove()`. Missing starts empty.
