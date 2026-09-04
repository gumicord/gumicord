/**
 * The plugin-side runtime, and where the host meets it.
 *
 * A plugin author never uses this directly: `ui.patch` in `index.ts`
 * registers here, and the host calls `__gumicord_apply`.
 *
 * Walking the tree in JS is deliberate. Asking the host node by node
 * multiplies the Rust/QuickJS round trips; S3 measured 1601 nodes at
 * 5.242 ms. Passing a whole subtree and walking it here costs one.
 */

import type { NodeId, PatchContext, PatchFn, UINode } from "./uitree.js";

const patches = new Map<string, PatchFn[]>();

export function registerPatch(id: NodeId, fn: PatchFn): void {
  const list = patches.get(id);
  if (list) list.push(fn);
  else patches.set(id, [fn]);
}

/**
 * Applies the patches to a subtree.
 *
 * This function's order is part of the extension ABI; changing it changes
 * how existing plugins behave.
 *
 * The rules:
 * - **P1** traversal is bottom-up: children first, then the parent
 * - **P2** matching uses the stable ID as it was before any patch
 * - **P3** a patch's output is not recursed into; it is final
 * - **P4** patches apply in registration order, each seeing the last
 *
 * Without P1 and P3 this recurses forever: `ui.wrap` returns a new node
 * holding the original as a child, so patching a node and then recursing
 * into the result matches that child on the same stable ID and wraps it
 * again. S3 hit exactly that.
 */
function applyPatches(node: UINode, ctx: PatchContext): UINode {
  // P1: children first.
  const current: UINode =
    node.children && node.children.length > 0
      ? { ...node, children: node.children.map((c) => applyPatches(c, ctx)) }
      : node;

  // P2: matched on the original stable ID.
  const list = patches.get(node.id);
  if (!list) return current;

  // P3, P4: applied in order, without recursing into the output.
  let out = current;
  for (const fn of list) {
    try {
      out = fn(out, ctx);
    } catch (e) {
      // One failing patch must not break the whole subtree. The host
      // isolates per plugin as well, and counts failures towards
      // disabling a chronically broken plugin.
      hostLog("error", `patch on ${node.id} threw: ${String(e)}`);
      reportFailure(node.id);
      return current;
    }
  }
  return out;
}

declare const __gumicord_host: {
  log(level: string, msg: string): void;
  patch_failed(nodeId: string): void;
};
function hostLog(level: string, msg: string): void {
  try {
    __gumicord_host.log(level, msg);
  } catch {
    /* Silently dropped when the host has injected nothing, as in tests. */
  }
}
function reportFailure(nodeId: string): void {
  try {
    __gumicord_host.patch_failed(nodeId);
  } catch {
    /* Silently dropped when the host has injected nothing, as in tests. */
  }
}

// The one way in from the host.
(globalThis as Record<string, unknown>)["__gumicord_apply"] = (
  node: UINode,
  ctx: PatchContext,
): UINode => applyPatches(node, ctx ?? {});

(globalThis as Record<string, unknown>)["__gumicord_patch_count"] = (): number =>
  [...patches.values()].reduce((n, l) => n + l.length, 0);
