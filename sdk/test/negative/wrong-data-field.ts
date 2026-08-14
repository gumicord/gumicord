// ノード種別に存在しない data フィールドは通ってはいけない
import { ui } from "../../src/index.js";
ui.patch("nav.guild_list.item", (node, ctx) => {
  const _ = ctx.data.author;
  return node;
});
