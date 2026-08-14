// 通らなければならないコード。tsconfig の include に入っており、
// npm run typecheck で一緒に検査される。
import { ui, log, storage } from "../src/index.js";

// ctx.data の型が安定 ID から決まる
ui.patch("chat.message.header.author", (node, ctx) => {
  if (!ctx.data.author.bot) return node;
  return ui.after(node, ui.badge({ text: "BOT", tone: "accent" }));
});

ui.patch("nav.channel_list.item", (node, ctx) => {
  if (ctx.data.mentionCount === 0) return node;
  return ui.after(node, ui.badge({ text: String(ctx.data.mentionCount) }));
});

// data を持たないノードでは undefined
ui.patch("chat.input.field", (node, ctx) => {
  const _: undefined = ctx.data;
  return node;
});

// primitive.* / layout.* で包める
ui.patch("chat.message.content", (node) =>
  ui.wrap(node, { id: "layout.column", props: { gap: 4 } }));

// プラグイン自身の名前空間の ID も使える
ui.patch("chat.message.content", (node) =>
  ui.stack([node, ui.node("plugin.com_example_translate.button", { label: "翻訳" })]));

// exists で事前に分岐できる
if (ui.exists("chrome.titlebar")) {
  ui.patch("chrome.titlebar.title", (node) => node);
}

log.info("ok");
storage.setJSON("config", { enabled: true });
