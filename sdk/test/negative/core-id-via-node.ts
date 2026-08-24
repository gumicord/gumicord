// ui.node cannot make a core ID either.
import { ui } from "../../src/index.js";
ui.patch("chat.message.content", (node) => ui.stack([node, ui.node("nav.guild_list.item")]));
