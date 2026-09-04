import { log, ui } from "@gumicord/sdk";

log.info("hello plugin loaded");

// Puts a greeting badge beside every author name. Pure structure: no
// ctx.data needed, so this runs before data resolution exists.
ui.patch("chat.message.header.author", (node) =>
  ui.after(node, ui.badge({ text: "hi" })),
);
