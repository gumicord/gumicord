// プラグインが中核 ID を製造してはいけない (spec/03-uitree.md 8.2)
import { ui } from "../../src/index.js";
ui.patch("chat.message.content", (node) => ui.wrap(node, { id: "chat.message" }));
