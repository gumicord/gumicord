// スパイク S3: @gumicord/sdk の最小形。
//
// ホスト (Rust) が注入するのは `__gumicord_host` ただ 1 つ。
// SDK はその薄いラッパであり、プラグイン作者はホストの生 API を直接触らない。
// この間接層があることで、ホスト API を変えても SDK 側で吸収できる。

/** ホストが注入する唯一のオブジェクト。プラグインからは見せない。 */
declare const __gumicord_host: {
  log(level: string, msg: string): void;
  storage_get(key: string): string | null;
  storage_set(key: string, value: string): void;
};

// ---------------------------------------------------------------- UITree

/**
 * UITree のノード。
 * `id` は spec/03-uitree.md が定める安定 ID。実装では文字列リテラル型にして
 * 存在しない ID をビルド時に落とす (EXT-002)。
 */
export interface UINode {
  id: string;
  props?: Record<string, unknown>;
  children?: UINode[];
}

export type PatchFn = (node: UINode, ctx: PatchContext) => UINode;

export interface PatchContext {
  /** そのノードが表現しているドメインオブジェクト */
  data?: Record<string, unknown>;
}

// ---------------------------------------------------------------- ランタイム

/**
 * 登録されたパッチ。ホストはこの Map を直接は触らず、`__apply` 経由で呼ぶ。
 * ホスト側から巨大な UITree を毎回渡すのを避けるため、走査は JS 側で行う。
 */
const patches = new Map<string, PatchFn[]>();

export const ui = {
  /** 安定 ID に対してノード変換を登録する (EXT-032) */
  patch(id: string, fn: PatchFn): void {
    const list = patches.get(id);
    if (list) list.push(fn);
    else patches.set(id, [fn]);
  },

  /** ノードを別のノードで包む */
  wrap(node: UINode, wrapper: UINode): UINode {
    return { ...wrapper, children: [node] };
  },

  /** ノードの直後に兄弟を足す (親が受け取る想定の簡易版) */
  after(node: UINode, sibling: UINode): UINode {
    return { id: "layout.row", children: [node, sibling] };
  },

  /** 単純なテキストノード */
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

// ---------------------------------------------------------------- ホスト接点

/**
 * ホストが呼ぶ入口。部分木を受け取り、登録済みパッチを適用して返す。
 *
 * ADR-0002 の帰結: UITree 全体を毎フレーム受け渡すと Rust ↔ QuickJS の
 * 値変換で破綻する。実装では変更のあった部分木のみを渡す。
 * S3 ではその前提が正しいかを実測する。
 */
function applyPatches(node: UINode, ctx: PatchContext): UINode {
  // ■ S3 の発見 (2026-08-14)
  //
  //   当初は「自分にパッチ → 結果の子へ再帰」の順で書いていたが、これは無限再帰する。
  //   ui.wrap は元ノードを子として持つ新ノードを返すため、その子が同じ安定 ID で
  //   再びマッチし、また包まれる、を繰り返すからである。
  //
  //   正しい意味論は「子を先に処理 → 最後に自分へパッチ → 結果には再帰しない」。
  //   すなわち走査はボトムアップで、パッチの出力は最終形として扱う。
  //   これは spec/05-plugin-api.md で明文化する必要がある。

  // 1. 子を先に処理する
  const current: UINode =
    node.children && node.children.length > 0
      ? { ...node, children: node.children.map((c) => applyPatches(c, ctx)) }
      : node;

  // 2. 自分にパッチを当てる。照合は「元の」安定 ID で行う
  const list = patches.get(node.id);
  if (!list) return current;

  // 3. 複数のパッチは登録順に適用し、後のパッチは前のパッチの出力を受け取る。
  //    パッチの出力に対して再帰はしない。
  let out = current;
  for (const fn of list) out = fn(out, ctx);
  return out;
}

// ホストから到達させるための唯一の出口
(globalThis as any).__gumicord_apply = (node: UINode, ctx: PatchContext) =>
  applyPatches(node, ctx ?? {});
(globalThis as any).__gumicord_patch_count = () =>
  [...patches.values()].reduce((n, l) => n + l.length, 0);
