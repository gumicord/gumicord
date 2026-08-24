// data is read-only; writing to it must not type-check.
import { ui } from "../../src/index.js";
ui.patch("chat.message", (node, ctx) => {
  ctx.data.content = "overwritten";
  return node;
});
