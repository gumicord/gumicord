# Packaging sources

Built by CI (the `dist` job), never by hand.

| | |
|---|---|
| `icons/app-icon.png` | The application icon master (4096px). Scaled with `sips` for macOS |
| `icons/gumicord.png` | Same mark at 512px, named for the desktop entry. `linuxdeploy` rejects anything bigger |
| `linux/gumicord.desktop` | Desktop entry for the AppImage |
| `macos/Info.plist` | Bundle metadata for `Gumicord.app` |

The Linux AppImage is assembled with `linuxdeploy` (continuous build), the
macOS icon with `sips` + `iconutil`, the dmg with `hdiutil`. All three ship
with the stock OS tooling.
Unsigned, like the mobile builds: signing and notarization are separate work.
