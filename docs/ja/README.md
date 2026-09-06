# Gumicord 開発ドキュメント

モノレポの案内図。仕様そのものは [`spec/`](../../spec/README.md) が単一の
真実の源であり、ここは読み物である。食い違ったら仕様が正しい。

## 目次

- [アーキテクチャ](architecture.md) — 全体像、データの流れ、ビルド方法
- [core: 通信と状態](core.md) — `model` / `rest` / `gateway` / `store`
- [core: UI データ](uidata.md) — `uitree` / `theme` / `markdown`
- [core: プラグイン実行](plugin-runtime.md) — `plugin`
- [レンダラ](render.md) — `render/render` / `render/platform`
- [アプリ](app.md) — `app/core` と各プラットフォームの殻
- [道具](tooling.md) — `xtask` / `sdk` / `tools/screenshot` / `packaging`
- [テーマ API](https://github.com/gumicord/api-docs/blob/main/ja/theme.md) — テーマ作者向け (別リポジトリ)
- [プラグイン API](https://github.com/gumicord/api-docs/blob/main/ja/plugins.md) — プラグイン作者向け (別リポジトリ)
