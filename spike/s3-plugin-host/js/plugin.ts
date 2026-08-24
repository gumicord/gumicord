// Spike S3: a sample plugin, written the way a
// BetterDiscord or Vencord author would, to see whether that works.

import { ui, log, storage, type UINode } from "./sdk";

log.info("plugin loaded");

let patched = 0;

// A BOT badge after the author name.
ui.patch("chat.message.header.author", (node, ctx) => {
  patched++;
  const author = ctx.data?.author as { bot?: boolean } | undefined;
  if (!author?.bot) return node;
  return ui.after(node, ui.badge({ text: "BOT", tone: "accent" }));
});

// Wraps the body and adds a translate button.
ui.patch("chat.message.content", (node) => {
  patched++;
  return ui.wrap(node, { id: "layout.column", props: { gap: 4 } });
});

// Counts starts in persistent storage.
const n = Number(storage.get("launches") ?? "0") + 1;
storage.set("launches", String(n));
log.info(`launch #${n}`);

// Lets the host collect statistics.
(globalThis as any).__plugin_stats = () => ({ patched });

// --- For the sandbox check ---
// Reaching any of these means the sandbox leaks.
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

// --- For the runaway-plugin check ---
(globalThis as any).__infinite_loop = () => {
  let x = 0;
  // eslint-disable-next-line no-constant-condition
  while (true) x++;
};

// --- For the exception-isolation check ---
(globalThis as any).__throw = () => {
  throw new Error("thrown by the plugin");
};

export type { UINode };
