# core: UI data

UITree (`uitree`), themes (`theme`), Markdown (`markdown`). Governing
specs: [`spec/03-uitree.md`](../../spec/03-uitree.md),
[`spec/04-theme.md`](../../spec/04-theme.md).

# gumicord-uitree (`core/uitree`)

> The semantic UI tree and its stable IDs, the extension ABI. IDs may only
> ever be added; removal and renames are breaking and refused.

## Files

### `src/lib.rs` — Entry point. Re-exports `ids`, `node`, `style`, `value`
and defines the node states themes can match. `StateSet` is an allocation-free
u16 bitset; a `when.state` array requires all of them.

### `src/ids.rs` — The single definition site of the stable IDs. Generates
`NodeId` and metadata from `define_node_ids!`. Key items: `NodeId`,
`DataKind`, `Origin`, `as_str()`, `parent()`. Namespaces and parent IDs
derive from dotted names. At most 4 levels, `[a-z0-9_.]` naming.

### `src/node.rs` — Node bodies. Bind a stable ID to content, key, state,
reference, and children. Key items: `UiNode`, `Content`, `Span`,
`DataRef`, `UiNode::new()`, `walk()`. `with_data()` only attaches where
the ID declares a `DataKind`. Traversal is pre-order, matching draw order.

### `src/style.rs` — Resolved styles. Cascade is last-wins, per property,
no specificity. Unset stays `None`. Key items: `Style`, `Decoration`,
`Style::overlay()`, `inherit_from()`. Only `color` and `font` inherit.

### `src/value.rs` — Theme value types. Lengths are always logical px; DPI
conversion belongs to the renderer. Key items: `Color`, `AssetRef`,
`Background`, `Color::parse()`, `AssetRef::parse()`. Out-of-bundle
references and undeclared hosts are refused.

# gumicord-theme (`core/theme`)

> Validates theme JSON and resolves tokens, selector matches, and final
> styles.

## Files

### `src/lib.rs` — Takes JSON, returns a usable `Theme` plus diagnostics.
Drops only offenders and keeps applying the rest. Key items: `Theme`,
`ParseResult`, `Theme::parse()`, `style_for()`, `CLIENT_ABI`. Only broken
JSON syntax and missing/invalid manifests discard everything.

### `src/resolve.rs` — Theme application over the whole tree, flowing
inheritance down. Key items: `resolve()`, `clear()`. Only the `Slot` key
participates in matching; snowflakes never match.

### `src/parse.rs` — Reading. Unknown properties warn and drop just that
spot; unknown `when` keys or values drop the rule. `$data.tint` is kept as
a mark and filled per node. Key items: `Manifest`, `Rule`, `Tinted`.
`blur` applies at load with a 256 cap.

### `src/token.rs` — Design-token table. Colors and numbers resolve
eagerly; objects stay untyped until use. Reference chains resolve
iteratively. Key items: `Tokens`, `TokenValue`, `Tokens::build()`,
`get()`. Cycles and undefined names fall out of the table. No recursion,
however deep the chain.

### `src/diag.rs` — Diagnostics collection. One error never blanks the
screen. Key items: `Diagnostic`, `Diagnostics`, `Severity`,
`Diagnostics::error()`, `warn()`. Unknowns warn for forward compatibility,
author mistakes error, positions point at JSON paths.

### `src/cond.rs` — Rule conditions. AND across keys, but AND across the
state array and OR across the platform array. Key items: `When`,
`MatchContext`, `Platform`, `When::matches()`, `MatchContext::new()`.
Width bounds are inclusive; `slot` only tells same-ID siblings apart.

# gumicord-markdown (`core/markdown`)

> Parses Discord-flavored Markdown into `Block` lists that know nothing
> about drawing. Unreadable syntax comes out literally so no body is lost.

## Files

### `src/lib.rs` — Public entry. `parse()` for whole bodies,
`parse_inline()` for inline-only, plus model re-exports.

### `src/model.rs` — Parse result model. Decorations compose as a bitset.
Key items: `Block`, `Inline`, `InlineKind`, `Deco`, `Mention`. Spoilers
live on the decoration side so line wrapping survives.

### `src/block.rs` — Block parsing. Fences are cut out before line
splitting so `# ` and `> ` inside code are not misread. Key items:
`parse()`. Unclosed fences stay literal; quotes bundle `>>> ` and runs
of `> `.

### `src/inline.rs` — Inline parsing. An opener only counts once its
closer is found, otherwise it stays literal. Key items: `parse()`. Closer
search skips code spans, `_` does not open right after alphanumerics,
trailing punctuation stays out of bare URLs.

### `src/tests.rs` — Tests-only. Flattens to one line and compares.
