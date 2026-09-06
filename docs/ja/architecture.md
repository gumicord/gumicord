# アーキテクチャ

## 全体像

```
プラグイン (.js) ──パッチ──▶ UITree ──テーマ解決──▶ レンダラ ──▶ 画面
                                ▲                        │
Discord REST/Gateway ──▶ Store ─┘                        ▼
                                              プラットフォーム層
                                          (窓・入力・鍵束・通知)
```

- 唯一の拡張 ABI はセマンティック UITree ([ADR-0004](../../spec/adr/0004-semantic-uitree-as-extension-abi.md))。テーマもプラグインも UITree にのみ触る。
- UI レンダラは Rust + wgpu の自前実装 ([ADR-0001](../../spec/adr/0001-native-rust-renderer.md))。
- プラグイン言語は TypeScript、実行は QuickJS ([ADR-0002](../../spec/adr/0002-quickjs-plugin-runtime.md))。
- 単一モノレポ ([ADR-0003](../../spec/adr/0003-monorepo.md))。

## クレート配置

| 層 | クレート | 役目 |
|---|---|---|
| 通信と状態 | `core/model`, `core/rest`, `core/gateway`, `core/store` | [core.md](core.md) |
| UI データ | `core/uitree`, `core/theme`, `core/markdown` | [uidata.md](uidata.md) |
| 拡張実行 | `core/plugin` | [plugin-runtime.md](plugin-runtime.md) |
| 描画 | `render/render`, `render/platform` | [render.md](render.md) |
| 画面 | `app/core`, `app/desktop`, `app/android`, `app/ios` | [app.md](app.md) |
| 道具 | `xtask`, `sdk`, `tools/screenshot`, `packaging` | [tooling.md](tooling.md) |

## フレームの流れ

1. アプリが UITree を組む (`app/core`)
2. プラグインがパッチを当てる (差分のみ)
3. テーマがスタイルを解決する
4. レンダラが配置・描画する

## ビルドと検証

```bash
npm install
(cd sdk && npm install)

cargo check -p <affected-package>   # 普段の確認はこれ
cargo test -p <package> <test_name> # 振る舞いが変わったら対象試験

cargo xtask check-light  # 仕様のみ変更時
cargo xtask check        # 提出前の完全検証
cargo xtask fmt          # 整形
cargo xtask gen          # spec/ から生成物を更新
```

`just` や `make` は使わない。プロジェクト固有の操作は `cargo xtask`
(詳細は [道具](tooling.md))、通常の Rust 開発は標準の Cargo コマンドを使う。

## リポジトリ直下

- `Cargo.toml` — ワークスペース定義と依存版の集約。版は S1〜S4 のスパイクで実際に動いたもの。
- `.cargo/config.toml` — `cargo xtask` 別名とビルド並列数の上限。
- `.github/workflows/ci.yml` — CI (検査・ビルド・配布・スクリーンショット・nightly)。
- `LICENSE` — MIT。
- `.gitignore` — ビルド成果物と `local.properties` 等の手元設定の除外。
