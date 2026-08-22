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
│  └─ adr/                  アーキテクチャ決定記録 0001-0007
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

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) が 4 つのジョブを回します。

| ジョブ | 内容 | 速さ |
|---|---|---|
| **仕様** | ABI の後方互換性 / JSON Schema / SDK 型検査 / 生成物の鮮度 | ワークスペースをビルドしないので数十秒 |
| **Rust** | fmt / `clippy --all-targets` / テスト (Linux + Windows) | 依存のビルドを含む |
| **配布物** | Windows 版をビルドして Actions の artifact に上げる | LTO を含むので長い |
| **MSRV** | `rust-version = "1.97"` が嘘でないことの確認 | — |

**すべての実行で動くバイナリが残ります。** Actions の実行ページ下部の `gumicord-windows-<sha>` を落とすと、ビルドせずにその時点の状態を触れます。テーマ (`themes/`) が同梱してあるので `GUMICORD_THEME` で差し替えて見比べられます。

**手元で重いビルドを回す必要はありません。** 日常は `cargo xtask check-light` で足ります。ワークスペース全体のテストと `--all-targets` の clippy、そして配布物のビルドは CI が担保します。

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

同時に、**自前レンダラのコストは性能ではなくプラットフォーム統合にある**ことが分かりました。とくに日本語入力は、`winit` がテキスト入力の面倒を見ないため自前の層が要ります ([ADR-0006](spec/adr/0006-windows-ime-via-winit.md))。

### M1.1 — 描画が縦に通りました

**UITree → テーマ解決 → レイアウト → 描画** が一本につながり、ウィンドウが出ます。

```bash
cargo run -p gumicord-desktop

# テーマを差し替えて見比べる
GUMICORD_THEME=examples/themes/midnight/theme.json cargo run -p gumicord-desktop
```

画面に出ているものに**ハードコードされた色は 1 つもありません。** 見えている色・角丸・余白・書体はすべて [`examples/themes/midnight/theme.json`](examples/themes/midnight/theme.json) が決めています。

| | 状態 |
|---|---|
| ウィンドウ + 独自タイトルバー (P1) | ✅ ドラッグ移動・端のリサイズ・最小化/最大化/閉じる |
| レイアウト (R2) | ✅ row / column / stack / scroll |
| SDF 角丸矩形バッチャ (R1) | ✅ 差分バッファ転送はまだ |
| テキスト (R3) | ✅ CJK・折り返し・カラー絵文字。アトラスは 1 ページのみ |
| テーマ解決 (E1) | ✅ トークン・セレクタ・`when.state`・継承 |
| レスポンシブ (X1) | ✅ 幅で 1/2/3 ペインを出し分け |
| **日本語入力 (P2)** | ✅ インライン変換・変換候補ウィンドウとも動く |
| **ログイン (C4a, P4)** | ✅ QR でログインし、トークンは OS の鍵束へ |
| **通信 (C2, C3)** | ✅ 本物のサーバ・チャンネル・発言が出る。送れる |
| 状態の保持 (C5) | ❌ まだ。**再起動で全部消えます** |
| プラグイン (E4〜E7) | ❌ まだ |

### 見積もりが一番大きかったものは、そもそも要りませんでした

P2 (日本語入力) は **XL・M1.1 のクリティカルパス**と見積もっていました。Windows の TSF テキストストア (`ITextStoreACP`) を自前実装しないと変換候補ウィンドウが出ない、と結論していたためです。

**それは誤りでした。** 原因は `set_ime_cursor_area` に渡していた矩形で、`winit` はそれを `CFS_EXCLUDE` (避けるべき領域) として扱います。キャレット幅ではなく**入力欄全体**を渡せば、TSF なしで候補ウィンドウが正しく出ます。Google 日本語入力と Microsoft IME で確認済みです。

経緯は [ADR-0006](spec/adr/0006-windows-ime-via-winit.md) に、**誤った過程は [ADR-0005](spec/adr/0005-ime-strategy.md) (廃止) にそのまま残して**あります。決定より、なぜ誤ったかのほうが次に効くためです。

これで **M1.1 の XL は 0 になりました。**

### 本物の Discord として動きます

QR でログインすると、**自分のサーバとチャンネルが並び、発言が読め、打った文字が届きます。**

```bash
cargo run -p gumicord-desktop
```

スマホの Discord で QR を読むだけです。パスワードもトークンも我々のプロセスを通りません ([ADR-0007](spec/adr/0007-login-paths-and-captcha.md))。トークンは DPAPI で暗号化して保存するので、2 回目からは QR も出ません。

⚠️ **暗号化できない環境では保存しません。** macOS / Linux はまだ実装が無いので、起動のたびに聞きます。平文でディスクに置くよりましだと判断しています。

```bash
# ログインを飛ばして固定データで画面だけ見る (レンダラやテーマを触るとき)
GUMICORD_SKIP_LOGIN=1 cargo run -p gumicord-desktop
```

**まだ状態を保持していません (C5)。** メッセージは開いているチャンネルのぶんだけを持っていて、再起動すると全部消えます。未読も既読位置もありません。

次は C5 (Store) です。詳細は [spec/07-roadmap.md](spec/07-roadmap.md)。

## ライセンス

MIT License — [LICENSE](LICENSE)
