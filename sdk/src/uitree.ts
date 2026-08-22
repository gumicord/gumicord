/**
 * UITree の型。
 *
 * 安定 ID (`NodeId`) と `data` の対応 (`DataByNode`) は
 * `core/uitree/src/ids.rs` から生成された `ids.ts` にある。
 * **このファイルには手書きの部分だけを置く。**
 */

import type { CoreCreatableNodeId, DataByNode, NodeId } from "./ids.js";

export type { NodeId, DataByNode } from "./ids.js";
export type * from "./data.js";

/** ノードの状態。テーマの条件分岐に対応する (`spec/03-uitree.md` 2.3) */
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
 * プラグインが自分の名前空間に作る ID。
 *
 * 接頭辞は `plugin.` + プラグイン ID の `.` を `_` に置換したもの。
 * テーマから狙えるようにするためのフックであり、**この ID の後方互換性は
 * プラグイン作者が負う** (`spec/03-uitree.md` 8.3)。
 */
export type PluginNodeId = `plugin.${string}`;

/**
 * プラグインが**生成してよい** ID。
 *
 * ⚠️ `app.*` / `chrome.*` / `nav.*` / `chat.*` は含まれない。
 * これらは実在するドメインオブジェクトと結びついており、偽物を作ると
 * アクセシビリティツリーが嘘をつき、他プラグインのセレクタが実体のない
 * ノードにマッチする (`spec/03-uitree.md` 8.2)。
 *
 * **プラグインは受け取ったノードを変形するのであって、中核ノードを製造しない。**
 */
export type CreatableNodeId = CoreCreatableNodeId | PluginNodeId;

export interface UINode {
  /** 安定 ID */
  id: NodeId | PluginNodeId;
  /** 同じ親の下で同じ id を持つノードを区別する鍵。読み取り専用 */
  readonly key?: string;
  /** 現在立っている状態 */
  readonly states?: readonly NodeState[];
  props?: Record<string, unknown>;
  children?: UINode[];
}

/** プラグインが新しく作るノード。中核 ID は使えない */
export interface NewUINode extends UINode {
  id: CreatableNodeId;
}

/**
 * パッチに渡される文脈。
 *
 * `data` の型は安定 ID から決まるため、`ctx.data.author.bot` のような
 * アクセスが**型安全**になる。`data` を持たないノードでは `undefined` になる。
 */
export interface PatchContext<Id extends NodeId = NodeId> {
  readonly data: Id extends keyof DataByNode ? Readonly<DataByNode[Id]> : undefined;
}

/**
 * ノード変換。
 *
 * ⚠️ **純粋関数でなければならない (規則 P7)。**
 * 仮想化により、同じメッセージに対して何度呼ばれるかは決まっていない。
 * 画面外へ出て戻るたびに呼び直される。副作用を書くと予測不能になる。
 * 出来事に反応したいときは Gateway イベントのミドルウェアを使う。
 */
export type PatchFn<Id extends NodeId = NodeId> = (
  node: UINode,
  ctx: PatchContext<Id>,
) => UINode;
