# Bundled fonts

Fonts embedded in the binary; `gumicord-render` reads them with
`include_bytes!`.

**Everything here is redistributed.** Always ship the licence alongside and
record where the file came from.

| File | Use | Licence | Source |
|---|---|---|---|
| `Inter.ttf` | Body and UI (Latin) | SIL Open Font License 1.1 ([`Inter-OFL.txt`](Inter-OFL.txt)) | [google/fonts `ofl/inter`](https://github.com/google/fonts/tree/main/ofl/inter), upstream [rsms/inter](https://github.com/rsms/inter) |
| `NotoSansJP-VF.ttf` | Body and UI (Japanese) | SIL Open Font License 1.1 ([`NotoSansJP-OFL.txt`](NotoSansJP-OFL.txt)) | [Noto Sans JP subset variable](https://github.com/googlefonts/noto-cjk/raw/main/Sans/Variable/TTF/Subset/NotoSansJP-VF.ttf) ([notofonts/noto-cjk](https://github.com/googlefonts/noto-cjk)) |

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

## CJK: Noto Sans JP variable

Japanese renders from the bundled subset variable font (9.1 MB), first in
the CJK fallback order; system fonts cover the rest. Fullwidth forms no
longer depend on which system fonts a machine happens to have (bold on
some machines, missing on others). Non-Japanese scripts still fall back
to system fonts. Decided in [ADR-0012](../../spec/adr/0012-bundle-noto-sans-jp.md).

## Adding a font

1. Confirm the licence permits redistribution (OFL, Apache-2.0, …).
2. Put the full licence text in this directory.
3. Add a row above, **with the source URL**.
4. Say why that font, in `spec/06-renderer.md`.
