/**
 * `@gumicord/sdk` — the Gumicord plugin SDK.
 *
 * This module is all a plugin touches; the raw object the host injects is
 * never exposed. The indirection lets the host API change without the
 * plugins changing with it.
 * See `spec/05-plugin-api.md`.
 */

export type {
  NodeId,
  PluginNodeId,
  CreatableNodeId,
  NodeState,
  UINode,
  NewUINode,
  PatchContext,
  PatchFn,
  UserData,
  MessageData,
  GuildData,
  ChannelData,
  DataByNode,
} from "./uitree.js";

import type { CreatableNodeId, NewUINode, NodeId, PatchFn, UINode } from "./uitree.js";
import { registerPatch } from "./runtime.js";

/**
 * The only object the host injects.
 *
 * Never shown to a plugin. What is not here cannot be reached: a
 * capability is implemented by not injecting it, not by a permission
 * check.
 */
declare const __gumicord_host: {
  log(level: string, msg: string): void;
  storage_get(key: string): string | null;
  storage_set(key: string, value: string): void;
  storage_remove(key: string): void;
  node_exists(id: string): boolean;
};

export const ui = {
  /**
   * Registers a node transform against a stable ID.
   *
   * How it applies:
   * - traversal is bottom-up, children before their parent
   * - matching uses the stable ID as it was before any patch
   * - a patch's output is not recursed into; it is final
   * - several patches on one node chain in registration order
   *
   * So a patch runs exactly once per node.
   *
   * Registering against a node that does not exist here (`chrome.*` on
   * mobile) is not an error; it simply never runs. To branch beforehand,
   * use {@link exists}.
   * Virtualisation means offscreen nodes are never visited (rule V1), so
   * nothing can walk every message. Use Gateway event middleware instead.
   *
   * `fn` must be pure (rule P7): how many times it runs for one message is
   * not defined, since it runs again each time the node leaves the screen
   * and comes back, and a side effect would not add up.
   *
   * `ctx.data` is typed from `id`, so registering against
   * `chat.message.header.author` types `ctx.data.author.bot`.
   *
   * @example
   * ```ts
   * ui.patch("chat.message.header.author", (node) =>
   *   ui.after(node, ui.badge({ text: "hi" })),
   * );
   * ```
   */
  patch<Id extends NodeId>(id: Id, fn: PatchFn<Id>): void {
    registerPatch(id, fn as PatchFn);
  },

  /**
   * Whether the node can exist in this environment.
   *
   * `chrome.*` does not exist on mobile. Patching a missing ID is harmless,
   * so this is only for branching beforehand.
   */
  exists(id: NodeId): boolean {
    return __gumicord_host.node_exists(id);
  },

  /**
   * Wraps a node as the child of another.
   *
   * `wrapper` cannot be a core ID such as `chat.*`: a plugin transforms the
   * nodes it is given, it does not manufacture core ones. Only its own
   * namespace and `primitive.*` / `layout.*` may wrap.
   */
  wrap(node: UINode, wrapper: Omit<NewUINode, "children">): UINode {
    return { ...wrapper, children: [node] };
  },

  /** Adds a sibling after the node. */
  after(node: UINode, sibling: UINode): UINode {
    return { id: "layout.row", children: [node, sibling] };
  },

  /** Adds a sibling before the node. */
  before(node: UINode, sibling: UINode): UINode {
    return { id: "layout.row", children: [sibling, node] };
  },

  /**
   * Provides this plugin's settings page, shown in the client's settings
   * screen when the manifest declares a `settings` entry.
   *
   * Display-only for now: controls sit inert until the settings event
   * channel arrives, so describe, do not operate.
   */
  settings(fn: () => UINode): void {
    (globalThis as Record<string, unknown>)["__gumicord_settings"] = fn;
  },

  /** Stacks nodes vertically. */
  stack(nodes: UINode[]): UINode {
    return { id: "layout.column", children: nodes };
  },

  /** Creates any creatable node, for a plugin's own IDs. */
  node(id: CreatableNodeId, props?: Record<string, unknown>, children?: UINode[]): NewUINode {
    const n: NewUINode = { id };
    if (props) n.props = props;
    if (children) n.children = children;
    return n;
  },

  /** Creates a `primitive.text` node holding the string. */
  text(value: string): NewUINode {
    return { id: "primitive.text", props: { value } };
  },

  /**
   * Creates a `primitive.badge` node.
   *
   * `tone` is theme-owned and drops; the badge keeps its text.
   */
  badge(opts: { text: string; tone?: string }): NewUINode {
    return { id: "primitive.badge", props: { ...opts } };
  },

  /**
   * Creates a `primitive.button` node.
   *
   * Functions cannot cross the host boundary, so `onPress` drops silently
   * and the button is inert on settings screens.
   */
  button(opts: { label: string; onPress: () => void }): NewUINode {
    return { id: "primitive.button", props: { ...opts } };
  },

  /** Creates a `primitive.icon` node showing the named picture. */
  icon(name: string): NewUINode {
    return { id: "primitive.icon", props: { name } };
  },
} as const;

/** Logging under the `log` capability. Everything lands in the host log. */
export const log = {
  /** Records an informational message. */
  info: (msg: string): void => __gumicord_host.log("info", msg),
  /** Records a warning. */
  warn: (msg: string): void => __gumicord_host.log("warn", msg),
  /** Records an error. */
  error: (msg: string): void => __gumicord_host.log("error", msg),
};

/**
 * Persistent storage, separate per plugin.
 *
 * It lives in the host, so it survives a plugin reload.
 */
export const storage = {
  /** Reads a key. Yields `null` when missing. */
  get: (key: string): string | null => __gumicord_host.storage_get(key),
  /** Writes a key. Persists immediately; never call from inside a patch. */
  set: (key: string, value: string): void => __gumicord_host.storage_set(key, value),
  /** Deletes a key. Persists immediately; never call from inside a patch. */
  remove: (key: string): void => __gumicord_host.storage_remove(key),

  /**
   * Reads JSON. Returns `fallback` for both missing and broken JSON.
   *
   * Never call from inside a patch.
   */
  getJSON<T>(key: string, fallback: T): T {
    const raw = __gumicord_host.storage_get(key);
    if (raw === null) return fallback;
    try {
      return JSON.parse(raw) as T;
    } catch {
      return fallback;
    }
  },
  /** Writes JSON-encoded. Never call from inside a patch. */
  setJSON(key: string, value: unknown): void {
    __gumicord_host.storage_set(key, JSON.stringify(value));
  },
};

export type { PatchContext as Context } from "./uitree.js";
