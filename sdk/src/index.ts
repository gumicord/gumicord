/**
 * `@gumicord/sdk` — Gumicord プラグイン SDK。
 *
 * プラグインが触れるのはこのモジュールだけである。ホストが注入する生の
 * オブジェクトは公開しない。この間接層があることで、ホスト API を変えても
 * SDK 側で吸収できる。
 *
 * 仕様: `spec/05-plugin-api.md`
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
 * ホストが注入する唯一のオブジェクト。
 *
 * ⚠️ **プラグインからは見せない。** ここに無いものはプラグインから到達
 * できない。ケイパビリティは権限チェックのコードではなく「注入しないこと」で
 * 実装されている (`SEC-010`, `SEC-015`)。
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
   * 安定 ID に対してノード変換を登録する (`EXT-032`)。
   *
   * 適用の意味論 (`spec/05-plugin-api.md` 2 章):
   * - 走査は**ボトムアップ**。子が先、自分が後
   * - 照合は**パッチ適用前の**安定 ID に対して行う
   * - **パッチの出力に再帰しない。** 出力は最終形として扱う
   * - 同一ノードへの複数パッチは**登録順**に連鎖する
   *
   * したがって自分のパッチは、そのノード 1 つにつき**ちょうど 1 回**呼ばれる。
   *
   * ⚠️ 存在しないノード (モバイルにおける `chrome.*` など) に登録しても
   * エラーにはならず、単に呼ばれない。事前に分岐したいときは {@link exists}。
   *
   * ⚠️ 仮想化により、**画面外のノードには呼ばれない** (規則 V1)。全メッセージを
   * 走査するような処理は書けない。代わりに Gateway イベントのミドルウェアを使う。
   *
   * ⚠️ **`fn` は純粋関数でなければならない (規則 P7)。**
   * 同じメッセージに対して何度呼ばれるかは決まっていない。画面外へ出て戻る
   * たびに呼び直されるため、副作用を書くと数が合わなくなる。
   *
   * `ctx.data` の型は `id` から決まる。`chat.message.header.author` に
   * 登録すれば `ctx.data.author.bot` が型安全に読める。
   */
  patch<Id extends NodeId>(id: Id, fn: PatchFn<Id>): void {
    registerPatch(id, fn as PatchFn);
  },

  /**
   * そのノードが現在の環境に存在しうるかを返す。
   *
   * `chrome.*` はモバイルに存在しない。存在しない ID にパッチを登録しても
   * エラーにはならず単に呼ばれないが、事前に分岐したいときに使う。
   */
  exists(id: NodeId): boolean {
    return __gumicord_host.node_exists(id);
  },

  /**
   * ノードを別のノードの子として包む。
   *
   * ⚠️ `wrapper` に中核 ID (`chat.*` など) は使えない。
   * プラグインは受け取ったノードを変形するのであって、中核ノードを製造しない
   * (`spec/03-uitree.md` 8.2)。
   */
  wrap(node: UINode, wrapper: Omit<NewUINode, "children">): UINode {
    return { ...wrapper, children: [node] };
  },

  /** ノードの直後に兄弟を足す */
  after(node: UINode, sibling: UINode): UINode {
    return { id: "layout.row", children: [node, sibling] };
  },

  /** ノードの直前に兄弟を足す */
  before(node: UINode, sibling: UINode): UINode {
    return { id: "layout.row", children: [sibling, node] };
  },

  /** ノードを縦に並べる */
  stack(nodes: UINode[]): UINode {
    return { id: "layout.column", children: nodes };
  },

  /** 任意の生成可能ノードを作る。プラグイン固有の ID を使うときに */
  node(id: CreatableNodeId, props?: Record<string, unknown>, children?: UINode[]): NewUINode {
    const n: NewUINode = { id };
    if (props) n.props = props;
    if (children) n.children = children;
    return n;
  },

  text(value: string): NewUINode {
    return { id: "primitive.text", props: { value } };
  },

  badge(opts: { text: string; tone?: string }): NewUINode {
    return { id: "primitive.badge", props: { ...opts } };
  },

  button(opts: { label: string; onPress: () => void }): NewUINode {
    return { id: "primitive.button", props: { ...opts } };
  },

  icon(name: string): NewUINode {
    return { id: "primitive.icon", props: { name } };
  },
} as const;

export const log = {
  info: (msg: string): void => __gumicord_host.log("info", msg),
  warn: (msg: string): void => __gumicord_host.log("warn", msg),
  error: (msg: string): void => __gumicord_host.log("error", msg),
};

/**
 * プラグインごとに分離された永続ストレージ (`EXT-036`, `SEC-014`)。
 *
 * ホスト側にあるため、プラグインの再読み込み (`Context` の再生成) を跨いで残る。
 */
export const storage = {
  get: (key: string): string | null => __gumicord_host.storage_get(key),
  set: (key: string, value: string): void => __gumicord_host.storage_set(key, value),
  remove: (key: string): void => __gumicord_host.storage_remove(key),

  getJSON<T>(key: string, fallback: T): T {
    const raw = __gumicord_host.storage_get(key);
    if (raw === null) return fallback;
    try {
      return JSON.parse(raw) as T;
    } catch {
      return fallback;
    }
  },
  setJSON(key: string, value: unknown): void {
    __gumicord_host.storage_set(key, JSON.stringify(value));
  },
};

export type { PatchContext as Context } from "./uitree.js";
