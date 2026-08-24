// An unknown stable ID must not type-check.
import { ui } from "../../src/index.js";
ui.patch("chat.message.header.autor", (node) => node);
