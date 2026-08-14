// data は読み取り専用。書き換えは通ってはいけない
import { ui } from "../../src/index.js";
ui.patch("chat.message", (node, ctx) => {
  ctx.data.content = "書き換え";
  return node;
});
