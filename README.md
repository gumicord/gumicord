# Gumicord

Gumicord は、Discord の公式クライアントを改造したものではなく、**ゼロから書き起こした独立した Discord クライアント**です。
Windows / macOS / Linux / Android / iOS で動作し、**同一のテーマとプラグインがすべてのデバイスでそのまま動きます。**

> [!WARNING]
> Discord の利用規約はサードパーティクライアントの使用を認めていません。Gumicord の利用によりアカウントが停止される可能性があります。
> 本プロジェクトはこのリスクを前提に、自動化機能を実装せず、公式クライアントと同等の通信挙動・レート制限遵守を設計目標としています。
> 詳細は [spec/00-vision.md](spec/00-vision.md#リスクと前提) を参照してください。

## 何が違うのか

| | 公式 Discord | BetterDiscord / Vencord | **Gumicord** |
|---|---|---|---|
| 実装 | Electron | Electron + 内部パッチ | ネイティブ (Rust) |
| テーマ | 限定的 | CSS (デスクトップのみ) | **JSON (全プラットフォーム)** |
| プラグイン | なし | JS (デスクトップのみ) | **TypeScript (全プラットフォーム)** |
| 拡張の安定性 | — | 内部構造依存で頻繁に壊れる | **公開された安定 ABI** |
| モバイル | 機能制限あり | 不可 | **デスクトップと同等** |

## 設計の中核: セマンティック UI ツリー

BetterDiscord が強力なのは React ツリーを直接パッチできるからであり、脆いのは対象が非公開の内部構造だからです。
Gumicord は自前クライアントなので、**最初から「パッチされる前提の公開 UI ツリー」**を持ちます。

```
        ┌───────────────────────────────┐
        │  gumicord-core (Rust)         │
        │  Gateway / REST / Store       │
        │  + UITree ビルダー             │
        └──────────────┬────────────────┘
                       │ UITree (安定IDを持つセマンティックノード)
      ┌────────────────┼────────────────┐
  Theme (JSON)   Plugin (TS→QuickJS)    │  ← どちらもここに介入する
      └────────────────┼────────────────┘
                       ▼
        ┌───────────────────────────────┐
        │  gumicord-render (Rust/wgpu)  │
        │  全プラットフォーム共通の描画    │
        └───────────────────────────────┘
```

レンダラが全プラットフォームで単一なので、**テーマとプラグインの挙動差が原理的に発生しません。**

## リポジトリ構成

```
gumicord/
├─ spec/          仕様書 — 単一の真実の源。実装より先にここが変わる
│  ├─ 00-vision.md          ビジョン・非目標・リスク
│  ├─ 01-requirements.md    要件定義 (ID付き)
│  ├─ 02-architecture.md    アーキテクチャ
│  ├─ 03-uitree.md          セマンティックUIツリー仕様 (拡張ABI)
│  ├─ 04-theme.md           テーマ仕様
│  ├─ 05-plugin-api.md      プラグインAPI仕様
│  ├─ 06-renderer.md        レンダラ仕様
│  ├─ 07-roadmap.md         マイルストーン
│  ├─ 08-spike-plan.md      技術検証計画 (完了)
│  ├─ 09-discord-protocol.md
│  ├─ schema/               JSON Schema と検証ツール
│  └─ adr/                  アーキテクチャ決定記録 0001-0005
│
├─ core/          Rust
│  ├─ model/        ドメイン型
│  ├─ gateway/      Gateway (WS / zstd / resume)
│  ├─ rest/         REST + レート制限
│  ├─ store/        状態 + SQLite
│  ├─ uitree/       UITree — 安定 ID の唯一の定義元
│  ├─ theme/        テーマ解決
│  └─ plugin/       QuickJS ホスト
│
├─ render/        Rust
│  ├─ render/       wgpu レンダラ (プラットフォーム非依存)
│  └─ platform/     OS 統合 — IME / a11y / 通知 (PF 依存はここだけ)
│
├─ app/           Rust
│  ├─ core/         画面遷移・アプリ状態・結線
│  ├─ desktop/      Windows / macOS / Linux の入口
│  ├─ android/      (M1.2)
│  └─ ios/          (M1.2)
│
├─ sdk/           TypeScript — プラグイン SDK
├─ examples/      公式サンプルテーマ
├─ spike/         技術検証コード (捨てる前提。結果は adr/ に記録済み)
└─ xtask/         タスクランナー
```

## 開発

```bash
npm install           # 初回のみ
(cd sdk && npm install)

cargo xtask check-light  # ビルドを伴わない検査だけ (数十秒)
cargo xtask check        # すべての検査 (ビルドを伴う)
cargo xtask fmt          # 整形
cargo xtask lint         # clippy
cargo xtask test         # テスト
cargo xtask schema       # JSON Schema と公式サンプルの検証
cargo xtask sdk          # SDK の型レベルの保証を検証
cargo xtask abi          # 安定 ID の後方互換性検査 (EXT-003)
```

`just` や `make` ではなく `cargo xtask` にしているのは、**貢献者に追加のツール導入を要求しないため**です。cargo があれば動きます。

### CI

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) が 3 つのジョブを回します。

| ジョブ | 内容 | 速さ |
|---|---|---|
| **仕様** | ABI の後方互換性 / JSON Schema / SDK 型検査 / 生成物の鮮度 | ワークスペースをビルドしないので数十秒 |
| **Rust** | fmt / `clippy --all-targets` / テスト (Linux + Windows) | 依存のビルドを含む |
| **MSRV** | `rust-version = "1.97"` が嘘でないことの確認 | — |

**手元で重いビルドを回す必要はありません。** 日常は `cargo xtask check-light` で足り、`--all-targets` の clippy は CI が担保します。

### ビルドの資源消費

日常の `cargo build --release` は thin LTO で、控えめな機械でも通ります。

配布物と性能計測には `dist` プロファイルを使います。

```bash
cargo build --profile dist
```

⚠️ `dist` は fat LTO のためリンク時に**数 GB のメモリ**を使い、`target/` も数 GB に育ちます。メモリの少ない機械では他を閉じてから実行してください。

不要になったビルド成果物は `cargo clean` で消せます。スパイクの成果物は `rm -rf spike/*/target` で消して構いません（コードは残り、測定結果は [`spec/adr/`](spec/adr/) に記録済みです）。

## 開発方針: 仕様駆動開発

1. **仕様が先。** 実装は `spec/` に書かれた要件 ID を参照する
2. **決定は ADR に残す。** なぜその選択をしたか、何を却下したかを記録する
3. **拡張 ABI は不可侵。** `spec/03-uitree.md` の安定 ID は破壊的変更をしない
4. **推測で決めない。** 性能・IME・プラットフォーム制約はスパイクで実測してから仕様に落とす

## 現在地

**スパイク (技術検証) は完了しました。** Windows 上で以下を実測済みです。

| | 実測 | 比較 |
|---|---|---|
| バイナリサイズ | **4.66 MB** | 公式 Discord は 150〜300MB 程度 |
| 常駐メモリ | **69.7 MB** | 公式 Discord は 1,273 MB (5 プロセス) |
| コールドスタート | **332 ms** | — |
| 描画性能 | 20,000 インスタンスまで 60fps | Intel HD Graphics 520 (2015 年世代) |
| プラグイン 1 個のメモリ | **11.3 KB** | 完全隔離した状態で |

同時に、**自前レンダラのコストは性能ではなくプラットフォーム統合にある**ことが分かりました。とくに Windows では変換候補ウィンドウを出すために TSF テキストストアの自前実装が必要です ([ADR-0005](spec/adr/0005-ime-strategy.md))。

次は M1.1 (Windows のみで縦に通す) です。詳細は [spec/07-roadmap.md](spec/07-roadmap.md)。

## ライセンス

MIT License — [LICENSE](LICENSE)
