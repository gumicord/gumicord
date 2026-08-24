# Bundled fonts

Fonts embedded in the binary; `gumicord-render` reads them with
`include_bytes!`.

**Everything here is redistributed.** Always ship the licence alongside and
record where the file came from.

| File | Use | Licence | Source |
|---|---|---|---|
| `Inter.ttf` | Body and UI (Latin) | SIL Open Font License 1.1 ([`Inter-OFL.txt`](Inter-OFL.txt)) | [google/fonts `ofl/inter`](https://github.com/google/fonts/tree/main/ofl/inter), upstream [rsms/inter](https://github.com/rsms/inter) |

## Why bundle at all

Leaving it to the system font means the typeface changes per machine, which
makes identical rendering across platforms impossible to claim.

Enumerating system fonts was measured at 360 ms on a cold start. Getting that
off the startup path requires being able to shape text from a bundled font
alone.

The default sans-serif on each OS is also not designed for UI.

## Why one variable font

`Inter.ttf` carries `opsz` and `wght`, covering Thin (100) through Black (900)
in a single file, and cosmic-text sets the `wght` axis at rasterisation time.
Static instances would mean two files for the 400 and 600 the sample theme
uses, and another every time a theme reaches for a different weight.

## No CJK yet

Japanese still falls back to a system font.

The variable Noto Sans JP is around 5.7 MB, which would more than double the
current 4.66 MB binary. Bundling it is right for identical rendering, but it
is a trade against binary size and needs a decision of its own. Record it in
an ADR once made.

## Adding a font

1. Confirm the licence permits redistribution (OFL, Apache-2.0, …).
2. Put the full licence text in this directory.
3. Add a row above, **with the source URL**.
4. Say why that font, in `spec/06-renderer.md`.
