// 存在しない安定 ID (typo) は通ってはいけない
import { ui } from "../../src/index.js";
ui.patch("chat.message.header.autor", (node) => node);
