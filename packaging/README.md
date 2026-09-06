# Packaging sources

Built by CI (the `dist` job), never by hand. Everything generated lands in
the runner's temp dirs; only sources live here.

| | |
|---|---|
| `make-icon.py` | Dependency-free icon generator (pure stdlib). Sizes are rendered directly, not scaled |
| `linux/gumicord.desktop` | Desktop entry for the AppImage |
| `macos/Info.plist` | Bundle metadata for `Gumicord.app` |

The Linux AppImage is assembled with `linuxdeploy` (continuous build), the
macOS icon with `sips` + `iconutil`, the dmg with `hdiutil`. All three ship
with the stock OS tooling; nothing extra is installed for icons.
Unsigned, like the mobile builds: signing and notarization are separate work.
