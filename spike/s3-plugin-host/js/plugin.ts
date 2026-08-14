// スパイク S3: サンプルプラグイン。
// BetterDiscord / Vencord の作者がそのまま書ける形になっているかを見る。

import { ui, log, storage, type UINode } from "./sdk";

log.info("plugin loaded");

let patched = 0;

// 送信者名の後ろに BOT バッジを足す
ui.patch("chat.message.header.author", (node, ctx) => {
  patched++;
  const author = ctx.data?.author as { bot?: boolean } | undefined;
  if (!author?.bot) return node;
  return ui.after(node, ui.badge({ text: "BOT", tone: "accent" }));
});

// メッセージ本文を包んで翻訳ボタンを付ける
ui.patch("chat.message.content", (node) => {
  patched++;
  return ui.wrap(node, { id: "layout.column", props: { gap: 4 } });
});

// 起動回数を永続ストレージに記録する (EXT-036)
const n = Number(storage.get("launches") ?? "0") + 1;
storage.set("launches", String(n));
log.info(`launch #${n}`);

// ホストが統計を取れるようにする
(globalThis as any).__plugin_stats = () => ({ patched });

// --- サンドボックス検証用 (SEC-015) ---
// これらが到達可能ならサンドボックスが破れている
(globalThis as any).__probe_globals = (): string[] => {
  const dangerous = [
    "require",
    "process",
    "fs",
    "child_process",
    "eval",
    "Function",
    "WebAssembly",
    "fetch",
    "XMLHttpRequest",
    "importScripts",
    "Deno",
    "Bun",
  ];
  return dangerous.filter((k) => typeof (globalThis as any)[k] !== "undefined");
};

(globalThis as any).__all_globals = (): string[] =>
  Object.getOwnPropertyNames(globalThis).sort();

// --- 暴走プラグインの検証用 (EXT-051) ---
(globalThis as any).__infinite_loop = () => {
  let x = 0;
  // eslint-disable-next-line no-constant-condition
  while (true) x++;
};

// --- 例外隔離の検証用 (EXT-050) ---
(globalThis as any).__throw = () => {
  throw new Error("プラグインが投げた例外");
};

export type { UINode };
