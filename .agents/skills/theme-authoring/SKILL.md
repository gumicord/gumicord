---
name: theme-authoring
description: Create or improve Gumicord theme.json files, or investigate and implement Rust UI layout changes in Gumicord's native renderer. Use for consistent theme design, layout diagnostics, and renderer layout fixes.
---

# Gumicord theme authoring

Create coherent, readable Gumicord themes and help diagnose or implement layout behavior in the native renderer. A theme is a JSON data file; the app's UI tree and Rust renderer own structural layout.

## Start with the source of truth

Before editing, read `spec/README.md`, `spec/04-theme.md`, `spec/03-uitree.md`, `spec/06-renderer.md` (especially section 8), `spec/schema/theme.schema.json`, and the closest example under `examples/themes/`. Read the relevant parts of `spec/10-login-screen-ui.md` when designing the login screen. Specifications are Japanese and authoritative; follow their current contents if this skill's summary differs.

Use `spec/03-uitree.md` as the catalog of selectable nodes. Selectors are exact stable IDs, at most four segments, with no wildcard or descendant matching. Use only listed IDs; do not invent IDs or assume styling a parent automatically styles its children. Inspect the node's meaning and sibling IDs before choosing rules.

## Understand the renderer

- `theme.json` contains `manifest`, `tokens`, and ordered `rules`. Rules match stable UITree IDs and set supported style properties. Use `spec/schema/theme.schema.json` for exact accepted shapes and values.
- This is a native Rust renderer with its own semantic UI tree and layout algorithm. There is no DOM, HTML, CSS, selector cascade, browser box model, CSS flexbox, grid, media query, or arbitrary markup.
- The app constructs the UI tree and its `row`, `column`, `stack`, and `scroll` layout behavior. Themes cannot add, remove, reorder, or wrap UI nodes, nor change a node's layout axis. Theme dimensions and spacing can adjust the existing composition, but do not turn them into arbitrary positioning.
- The renderer applies rules in file order, and later matching values override earlier values property by property. There is no CSS specificity. Put broad/base rules before state-specific overrides.
- Layout is constraint based: fixed-size children are measured before `grow` children receive the remaining main-axis space. In scroll regions, content stacks to its measured size. Avoid assigning widths/heights to every node: renderer defaults provide important layout behavior. A mistaken width or height can consume the sidebar/chat space or collapse a content area.
- Lengths are logical pixels; DPI scaling is automatic. Padding/margin edges follow the schema's four-number convention and sample themes. Do not assume CSS shorthand semantics beyond what the schema/spec documents.
- `when.state` arrays mean all listed states at once; `when.platform` arrays mean any listed platform. Conditions in one `when` object are ANDed. `when.slot` is a positional semantic slot, not an item/server/user identifier.
- Text formatting inside message content is represented by `primitive.text` slots; target those slots rather than inventing markup or separate inline nodes.

## Design for consistency and usable layout

Translate the user's visual direction into a small, reusable token system before writing rules:

- Define semantic color roles for app/background layers, primary/secondary/muted text, accent, danger, and borders. Keep contrast sufficient for text and preserve distinct hover, selected, unread, mentioned, disabled, and focus cues.
- Use a restrained spacing and radius scale, and shared body/title/small/monospace font tokens. Prefer bundled/default font behavior unless a supplied font asset is part of the request.
- Apply tokens consistently to the major regions and common controls, then add component and state rules. Avoid unrelated one-off colors and dimensions that make the interface feel stitched together.
- Design the whole composition: app frame, navigation/sidebar, channel or member lists, chat header, message rows/content, input, settings and overlays where relevant. Maintain visual hierarchy, alignment, readable text, and breathing room.
- Keep desktop and narrow/mobile layouts usable using supported `when.platform` and width conditions. The app's responsive tree and overlays still determine what exists; theme conditions only alter styles.
- Preserve semantic/user data colors through `$data.tint` when the node supports it, unless the user explicitly requests otherwise.
- For background images or font files, use paths within the theme folder and follow the theme asset and remote-asset rules in `spec/04-theme.md`. Do not rely on arbitrary network URLs.

When the request calls a layout “broken,” inspect the existing rules, stable IDs, renderer defaults, and relevant layout code/spec behavior before changing dimensions. Explain limits imposed by the tree. If the desired composition requires different nodes or ordering, identify it as an application/UI-tree change rather than pretending JSON can implement it.

## Visual quality bar

Treat user-facing UI work as a design task as well as an implementation task. Do not stop when the UI compiles, the controls work, or the theme schema passes. Aim for a deliberate, finished client interface with clear hierarchy and consistent craft, rather than a collection of individually styled widgets.

Before implementing, identify the screen's purpose, primary action, secondary actions, and likely content density. Inspect the current rendered screen and the closest relevant reference available in the repository or provided by the user. Useful repository references include `render/tests/screenshots/` and, for login-specific work, the design findings and node constraints in `spec/10-login-screen-ui.md`. Treat references as evidence about composition and quality; do not copy details that conflict with the current product, platform behavior, or specification. If no suitable reference exists, state a concise visual direction and use it consistently across the screen.

Review the composition as a whole: hierarchy, alignment, spacing rhythm, proportion, text scale and line length, grouping, contrast, information density, and how primary actions stand out. Check real content lengths and relevant states, including hover, focus, selected/unread, loading, and error where the UI supports them. Check both a normal desktop viewport and a narrow viewport when relevant. Avoid generic centered stacks of controls, equal emphasis on every element, excessive empty space, decorative effects without purpose, and oversized panels that waste the available area.

After implementation, inspect an actual render or screenshot at the relevant viewport sizes. Compare it with the chosen reference or visual direction, name the most visible shortcomings, and make another correction pass. For Rust UI changes, inspect the screenshot/blessed-image workflow and update baselines only when the new appearance is intentional and reviewed; do not treat geometry tests as a visual sign-off. For theme-only changes, schema validity is necessary but does not establish design quality. If a usable render cannot be produced in the environment, say which visual checks remain unverified instead of describing the result as polished.

## Author and validate

1. Preserve the existing theme identity and intent when editing; when creating one, use a unique reverse-domain manifest ID, valid semver, ABI from the current spec, and a clear name/description.
2. Make the smallest coherent set of edits. Keep JSON strict (no comments or trailing commas), avoid duplicate/conflicting rules, and put state overrides after base rules.
3. Validate JSON parsing and the theme schema. For themes under `examples/themes/`, run `npm run validate:schema` (or `cargo xtask schema`); these validate the checked-in sample themes and assets. For a theme elsewhere, validate that exact file against `spec/schema/theme.schema.json` with the repository's Node/Ajv dependencies, and check every referenced bundled asset relative to the theme directory. Do not claim the example validator checked an unrelated file.
4. Inspect rule ordering, token references and cycles, selector spelling against `spec/03-uitree.md`, `when` values, style shapes, asset references, and dimensions likely to starve or collapse sibling regions.
5. Follow the visual quality bar above: inspect rendered results at a typical desktop size and a narrow/mobile size where relevant, identify the largest visual shortcomings, and correct them. Do not infer visual success from valid JSON alone. If runtime rendering is unavailable, report that limitation clearly.

Do not change specifications or application UI structure as a side effect of theme work. If the user wants a capability the theme format does not expose, state the precise constraint and propose the appropriate UI-tree or renderer follow-up.

## Rust renderer layout work

Use this mode when the requested fix changes how nodes are measured, constrained, sized, positioned, clipped, or scrolled. Before inspecting implementation or editing code, read `spec/README.md`, `spec/03-uitree.md`, and `spec/06-renderer.md` sections 8 and 12, plus any directly related requirement or ADR. The specification is authoritative. If it does not define the requested behavior or conflicts with another spec, resolve that at the spec level before making a significant implementation choice.

Then trace the affected path through `render/render/src/intrinsic.rs` (stable-ID layout defaults), `render/render/src/layout.rs` (measurement/placement), and the UITree/style construction that supplies the nodes. Inspect nearby tests before changing code. Keep the fix local to the responsible layer; do not compensate for a renderer defect with arbitrary theme dimensions or unrelated UI changes.

Preserve these layout invariants unless the specification is deliberately changed:

- Parent constraints flow down and measured sizes flow back up in one layout pass.
- The supported axes are row, column, stack, and scroll. Stable-ID intrinsic defaults supply axis, grow, cross-axis behavior, fixed size, scrolling, and related policies; these defaults are not a general CSS layout system.
- In a row or column, measure non-growing children first, then divide the remaining main-axis space among growing children. Text wrapping must receive the actual remaining width after fixed siblings are accounted for.
- Scroll content is measured along its stacking axis without distributing the viewport's leftover space to children. Preserve the intended scroll anchor behavior where relevant.
- `follows_cross`, content hugging, clipping, overlays, and theme-specified width/height interact with parent sizing. Check sibling allocations and zero-sized/overflow cases, not only the node being changed.

Use `gumicord_render::layout_for_test` for focused geometry diagnostics where it fits: it lays out a constructed UITree without a GPU and returns node rectangles. Add or adjust focused layout tests for behavior changes, covering the relevant constraint, grow, wrapping, scroll, or narrow-viewport edge case. Prefer assertions on meaningful rectangles and invariants over implementation details. Run the narrowest useful validation first, such as `cargo test -p gumicord-render <focused_test>` and/or `cargo check -p gumicord-render`; follow `CONTRIBUTING.md` for broader scope. For changes to rendering output, inspect the relevant screenshot/blessed-image path and viewport sizes; renderer geometry tests alone do not prove visual correctness. Run `cargo xtask check` at a meaningful completion checkpoint for substantial renderer or cross-package changes, not after each edit.

When the available UI tree has no node or semantic slot for the requested arrangement, explain that the tree must change and identify the relevant UI construction path. Do not silently expand stable IDs or alter their meaning: `spec/03-uitree.md` treats them as a compatibility surface. If a spec update is required, make that relationship explicit and use the repository's spec/generation workflow where applicable.
