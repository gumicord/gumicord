# Packaging sources

Built by CI (the `dist` job) or by the manual Release workflow, never by hand.

| | |
|---|---|
| `icons/app-icon.png` | The application icon master (4096px). Scaled with `sips` for macOS |
| `icons/gumicord.png` | Same mark at 512px, named for the desktop entry. `linuxdeploy` rejects anything bigger |
| `linux/gumicord.desktop` | Desktop entry for the AppImage |
| `macos/Info.plist` | Bundle metadata for `Gumicord.app` |

The Linux AppImage is assembled with `linuxdeploy` (continuous build), the
macOS icon with `sips` + `iconutil`, the dmg with `hdiutil`. All three ship
with the stock OS tooling.

Every build validates `gumicord.desktop` with `desktop-file-validate` and
extracts the AppImage once to prove it is not truncated.

## macOS signing and notarization (opt-in)

Without secrets everything builds unsigned, like the mobile builds. The
Release workflow signs and notarizes only when these repository secrets
exist; forks stay green without them.

| Secret | Contents |
|---|---|
| `MACOS_CERT_P12` | Developer ID Application certificate, base64-encoded `.p12` |
| `MACOS_CERT_PASSWORD` | The `.p12` import password |
| `MACOS_SIGN_IDENTITY` | The signing identity, e.g. `Developer ID Application: Example (TEAMID)` |
| `MACOS_NOTARY_APPLE_ID` | The Apple ID enrolled in the Developer Program |
| `MACOS_NOTARY_PASSWORD` | An app-specific password for that Apple ID |
| `MACOS_NOTARY_TEAM_ID` | The 10-character Team ID |

The same steps by hand, after building `Gumicord.app` as CI does:

```bash
security import cert.p12 -k login.keychain -P "$MACOS_CERT_PASSWORD" -T /usr/bin/codesign
/usr/bin/codesign --deep --force --verify --verbose \
  --sign "$MACOS_SIGN_IDENTITY" --options runtime --timestamp \
  Gumicord.app
/usr/bin/codesign --verify --deep --strict Gumicord.app
hdiutil create -volname Gumicord -srcfolder dmg-staging -ov Gumicord.dmg
xcrun notarytool submit Gumicord.dmg \
  --apple-id "$MACOS_NOTARY_APPLE_ID" \
  --password "$MACOS_NOTARY_PASSWORD" \
  --team-id "$MACOS_NOTARY_TEAM_ID" \
  --wait
xcrun stapler staple Gumicord.dmg
```

The hardened runtime (`--options runtime`) and a trusted timestamp are
what notarization asks for; without them the ticket is refused.

## Windows: no console, no redistributable

Since v0.0.3 the Windows binary is a GUI-subsystem executable: launching
it opens no console window. All logging goes to
`%APPDATA%\gumicord\logs\gumicord-<stamp>.log` (newest five runs kept),
which is also where the panic hook writes.

The CRT is linked statically (`target-feature=+crt-static` in
`.cargo/config.toml`), so the binary runs without the VC++
redistributable. `llvm-readobj --coff-imports` on the binary must show
no `VCRUNTIME140` or `MSVCP140`.
