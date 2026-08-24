/**
 * The UITree types.
 *
 * The stable IDs (`NodeId`) and their `data` (`DataByNode`) are generated
 * into `ids.ts` from `core/uitree/src/ids.rs`. Only what is written by
 * hand lives here.
 */

import type { CoreCreatableNodeId, DataByNode, NodeId } from "./ids.js";

export type { NodeId, DataByNode } from "./ids.js";
export type * from "./data.js";

/** A node state, matching what a theme can condition on. */
export type NodeState =
  | "hover"
  | "active"
  | "focus"
  | "selected"
  | "disabled"
  | "unread"
  | "mentioned"
  | "loading"
  | "grouped"
  | "collapsed";

/**
 * An ID a plugin creates in its own namespace.
 *
 * The prefix is `plugin.` followed by the plugin ID with `.` replaced by
 * `_`. It is a hook for themes to aim at, and its compatibility is the
 * plugin author's to keep.
 */
export type PluginNodeId = `plugin.${string}`;

/**
 * The IDs a plugin may create.
 *
 * Not `app.*`, `chrome.*`, `nav.*` or `chat.*`: those are tied to real
 * domain objects, and forging one would make the accessibility tree lie
 * and let another plugin's selector match a node that is not there.
 *
 * A plugin transforms the nodes it is given; it does not manufacture core
 * ones.
 */
export type CreatableNodeId = CoreCreatableNodeId | PluginNodeId;

export interface UINode {
  /** The stable ID. */
  id: NodeId | PluginNodeId;
  /** Distinguishes siblings sharing an id under one parent. Read-only. */
  readonly key?: string;
  /** The states currently held. */
  readonly states?: readonly NodeState[];
  /**
   * The colour the data carries (`#RRGGBB`): a role colour, a folder colour.
   *
   * Not a style. Where it lands is the theme's choice, and it only fills a
   * property written as `$data.tint`.
   */
  readonly tint?: string;
  props?: Record<string, unknown>;
  children?: UINode[];
}

/** A node a plugin creates. Core IDs are not allowed. */
export interface NewUINode extends UINode {
  id: CreatableNodeId;
}

/**
 * The context a patch receives.
 *
 * `data` is typed from the stable ID, so `ctx.data.author.bot` is type
 * safe. It is `undefined` on a node that carries none.
 */
export interface PatchContext<Id extends NodeId = NodeId> {
  readonly data: Id extends keyof DataByNode ? Readonly<DataByNode[Id]> : undefined;
}

/**
 * A node transform.
 *
 * It must be pure (rule P7). Virtualisation leaves it undefined how many
 * times it runs for one message — again each time the node leaves the
 * screen and comes back — so a side effect is unpredictable. To react to
 * something happening, use Gateway event middleware.
 */
export type PatchFn<Id extends NodeId = NodeId> = (
  node: UINode,
  ctx: PatchContext<Id>,
) => UINode;
