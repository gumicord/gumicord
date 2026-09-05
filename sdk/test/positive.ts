// Code that must type-check. It is in the tsconfig include, so
// npm run typecheck covers it too.
import { ui, log, storage } from "../src/index.js";

// ctx.data is typed from the stable ID.
ui.patch("chat.message.header.author", (node, ctx) => {
  if (!ctx.data.author.bot) return node;
  return ui.after(node, ui.badge({ text: "BOT", tone: "accent" }));
});

ui.patch("nav.channel_list.item", (node, ctx) => {
  if (ctx.data.mentionCount === 0) return node;
  return ui.after(node, ui.badge({ text: String(ctx.data.mentionCount) }));
});

// A node without data gives undefined.
ui.patch("chat.input.field", (node, ctx) => {
  const _: undefined = ctx.data;
  return node;
});

// primitive.* and layout.* can wrap.
ui.patch("chat.message.content", (node) =>
  ui.wrap(node, { id: "layout.column", props: { gap: 4 } }));

// A plugin's own namespace works too.
ui.patch("chat.message.content", (node) =>
  ui.stack([node, ui.node("plugin.com_example_translate.button", { label: "翻訳" })]));

// exists allows branching beforehand.
if (ui.exists("chrome.titlebar")) {
  ui.patch("chrome.titlebar.title", (node) => node);
}

log.info("ok");
storage.setJSON("config", { enabled: true });

// A settings page registers a node factory.
ui.settings(() => ui.stack([ui.text("見出し"), ui.text("説明")]));
