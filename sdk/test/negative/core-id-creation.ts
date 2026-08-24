// A plugin must not manufacture a core ID.
import { ui } from "../../src/index.js";
ui.patch("chat.message.content", (node) => ui.wrap(node, { id: "chat.message" }));
