// A data field the node kind does not have must not type-check.
import { ui } from "../../src/index.js";
ui.patch("nav.guild_list.item", (node, ctx) => {
  const _ = ctx.data.author;
  return node;
});
