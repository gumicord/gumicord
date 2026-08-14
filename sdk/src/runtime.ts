/**
 * プラグイン側のランタイム。ホストとの接点。
 *
 * プラグイン作者はこのファイルを直接使わない。`index.ts` の `ui.patch` が
 * ここに登録し、ホストが `__gumicord_apply` を呼ぶ。
 *
 * ⚠️ **走査を JS 側で行うのは意図的である。** ホストが 1 ノードずつ問い合わせると
 * Rust ↔ QuickJS の往復が跳ね上がる。S3 の実測では 1601 ノードの往復が 5.242 ms
 * だった。部分木ごと渡して JS 側で歩くことで、往復を 1 回に抑える。
 */

import type { NodeId, PatchContext, PatchFn, UINode } from "./uitree.js";

const patches = new Map<string, PatchFn[]>();

export function registerPatch(id: NodeId, fn: PatchFn): void {
  const list = patches.get(id);
  if (list) list.push(fn);
  else patches.set(id, [fn]);
}

/**
 * 部分木にパッチを適用する。
 *
 * ⚠️ **この関数の順序は拡張 ABI の一部である** (`spec/05-plugin-api.md` 2 章)。
 * 変更すると既存プラグインの挙動が変わる。
 *
 * 規則:
 * - **P1** 走査はボトムアップ。子を先に処理し、自分を後に処理する
 * - **P2** 照合はパッチ適用前の安定 ID に対して行う
 * - **P3** パッチの出力に再帰しない。出力は最終形として扱う
 * - **P4** 複数パッチは登録順に適用し、後のパッチは前のパッチの出力を受け取る
 *
 * P1 と P3 がないと**無限再帰する**。`ui.wrap` は元ノードを子として持つ
 * 新ノードを返すため、素朴に「自分にパッチ → 結果の子へ再帰」と書くと
 * その子が同じ安定 ID で再びマッチし、また包まれる、を繰り返す。
 * S3 で実際に踏んだ。
 */
function applyPatches(node: UINode, ctx: PatchContext): UINode {
  // P1: 子を先に処理する
  const current: UINode =
    node.children && node.children.length > 0
      ? { ...node, children: node.children.map((c) => applyPatches(c, ctx)) }
      : node;

  // P2: 照合は「元の」安定 ID で行う
  const list = patches.get(node.id);
  if (!list) return current;

  // P3, P4: 登録順に適用し、出力には再帰しない
  let out = current;
  for (const fn of list) {
    try {
      out = fn(out, ctx);
    } catch (e) {
      // 1 つのパッチの失敗で部分木ごと壊さない。
      // ホスト側でもプラグイン単位の隔離を行う (EXT-050)。
      hostLog("error", `patch on ${node.id} threw: ${String(e)}`);
      return current;
    }
  }
  return out;
}

declare const __gumicord_host: { log(level: string, msg: string): void };
function hostLog(level: string, msg: string): void {
  try {
    __gumicord_host.log(level, msg);
  } catch {
    /* ホストが未注入の場合 (テスト時など) は黙って捨てる */
  }
}

// ホストから到達させるための唯一の出口
(globalThis as Record<string, unknown>)["__gumicord_apply"] = (
  node: UINode,
  ctx: PatchContext,
): UINode => applyPatches(node, ctx ?? {});

(globalThis as Record<string, unknown>)["__gumicord_patch_count"] = (): number =>
  [...patches.values()].reduce((n, l) => n + l.length, 0);
