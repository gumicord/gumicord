import { ui } from "@gumicord/sdk";

// The settings screen shows this when the manifest declares a `settings`
// entry. Display-only: controls sit inert, so this describes the badge
// instead of offering switches.
ui.settings(() =>
  ui.stack([
    ui.text("Hello の設定"),
    ui.text("送信者の横に挨拶バッジを付けます。文言は固定です。"),
  ]),
);
