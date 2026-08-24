// Spike S3: the smallest @gumicord/sdk.
//
// The Rust host injects only `__gumicord_host`. The SDK is a thin wrapper
// over it, so a plugin author never touches the host's raw API and the
// host API can change without the plugins changing with it.

/** The only object the host injects. Never shown to a plugin. */
declare const __gumicord_host: {
  log(level: string, msg: string): void;
  storage_get(key: string): string | null;
  storage_set(key: string, value: string): void;
};

// ---------------------------------------------------------------- UITree

/**
 * A UITree node.
 * `id` is a stable ID from spec/03-uitree.md. The real one uses string
 * literal types, so an unknown ID fails to build.
 */
export interface UINode {
  id: string;
  props?: Record<string, unknown>;
  children?: UINode[];
}

export type PatchFn = (node: UINode, ctx: PatchContext) => UINode;

export interface PatchContext {
  /** The domain object this node stands for. */
  data?: Record<string, unknown>;
}

// ---------------------------------------------------------------- Runtime

/**
 * The registered patches. The host reaches them through `__apply`, not
 * this map, and the walking happens here so a whole UITree need not cross
 */
const patches = new Map<string, PatchFn[]>();

export const ui = {
  /** Registers a node transform against a stable ID. */
  patch(id: string, fn: PatchFn): void {
    const list = patches.get(id);
    if (list) list.push(fn);
    else patches.set(id, [fn]);
  },

  /** Wraps a node in another. */
  wrap(node: UINode, wrapper: UINode): UINode {
    return { ...wrapper, children: [node] };
  },

  /** Adds a sibling after the node; simplified, assuming the parent takes it. */
  after(node: UINode, sibling: UINode): UINode {
    return { id: "layout.row", children: [node, sibling] };
  },

  /** A plain text node. */
  text(value: string): UINode {
    return { id: "primitive.text", props: { value } };
  },

  badge(opts: { text: string; tone?: string }): UINode {
    return { id: "primitive.badge", props: { ...opts } };
  },
};

export const log = {
  info: (msg: string) => __gumicord_host.log("info", msg),
  warn: (msg: string) => __gumicord_host.log("warn", msg),
};

export const storage = {
  get: (key: string) => __gumicord_host.storage_get(key),
  set: (key: string, value: string) => __gumicord_host.storage_set(key, value),
};

// ---------------------------------------------------------------- The host

/**
 * What the host calls: takes a subtree, applies the patches, returns it.
 *
 * Per ADR-0002, passing a whole UITree each frame breaks down on the
 * Rust/QuickJS conversion, so only the changed subtree is passed. S3
 * measures whether that holds.
 */
function applyPatches(node: UINode, ctx: PatchContext): UINode {
  // What S3 found (2026-08-14):
  //
  //   patching a node and then recursing into the result recurses forever.
  //   ui.wrap returns a new node holding the original as a child, so that
  //   child matches the same stable ID and gets wrapped again.
  //
  //   The right order is children first, then the node, and never into the
  //   result: bottom-up, with a patch's output treated as final.
  //   spec/05-plugin-api.md needs to say so.

  // 1. Children first.
  const current: UINode =
    node.children && node.children.length > 0
      ? { ...node, children: node.children.map((c) => applyPatches(c, ctx)) }
      : node;

  // 2. Then the node, matched on the original stable ID.
  const list = patches.get(node.id);
  if (!list) return current;

  // 3. Patches apply in registration order, each seeing the last output,
  //    and never recursing into it.
  let out = current;
  for (const fn of list) out = fn(out, ctx);
  return out;
}

// The one way in from the host.
(globalThis as any).__gumicord_apply = (node: UINode, ctx: PatchContext) =>
  applyPatches(node, ctx ?? {});
(globalThis as any).__gumicord_patch_count = () =>
  [...patches.values()].reduce((n, l) => n + l.length, 0);
