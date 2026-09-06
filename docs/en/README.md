# Gumicord developer docs

A map of the monorepo. The specifications in [`spec/`](../../spec/README.md)
are the single source of truth; these pages are guides. When they disagree,
the spec wins.

## Contents

- [Architecture](architecture.md) — big picture, data flow, building
- [core: protocol and state](core.md) — `model` / `rest` / `gateway` / `store`
- [core: UI data](uidata.md) — `uitree` / `theme` / `markdown`
- [core: plugin runtime](plugin-runtime.md) — `plugin`
- [Renderer](render.md) — `render/render` / `render/platform`
- [App](app.md) — `app/core` and the platform shells
- [Tooling](tooling.md) — `xtask` / `sdk` / `tools/screenshot` / `packaging`
- [Theme API](https://github.com/gumicord/api-docs/blob/main/en/theme.md) — for theme authors (separate repo)
- [Plugin API](https://github.com/gumicord/api-docs/blob/main/en/plugins.md) — for plugin authors (separate repo)
