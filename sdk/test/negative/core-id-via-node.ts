// ui.node でも中核 ID は作れない
import { ui } from "../../src/index.js";
ui.patch("chat.message.content", (node) => ui.stack([node, ui.node("nav.guild_list.item")]));
