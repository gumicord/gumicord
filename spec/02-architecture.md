# 02. アーキテクチャ

| | |
|---|---|
| ステータス | **暫定** — スパイク ([08-spike-plan.md](08-spike-plan.md)) の結果で確定させる |
| 最終更新 | 2026-08-14 |
| 前提となる決定 | [ADR-0001](adr/0001-native-rust-renderer.md), [ADR-0002](adr/0002-quickjs-plugin-runtime.md), [ADR-0004](adr/0004-semantic-uitree-as-extension-abi.md) |

> この文書は未検証の想定を含む。特にクレート選定・スレッドモデル・性能特性は、スパイクで実測するまで確定していない。
> 確定した項目には ✅、スパイクで検証中の項目には 🔬 を付す。

## 全体像

```
┌──────────────────────────────────────────────────────────────┐
│  app/{desktop,android,ios}   プラットフォームのエントリポイント │
│  薄いラッパ。ライフサイクルとネイティブハンドルの受け渡しのみ    │
└────────────────────────────┬─────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────┐
│  gumicord-app       画面遷移・アプリ状態・キーバインド          │
└────────────────────────────┬─────────────────────────────────┘
                             │
        ┌────────────────────┼────────────────────┐
        ▼                    ▼                    ▼
┌───────────────┐  ┌───────────────────┐  ┌──────────────────┐
│ gumicord-     │  │ gumicord-uitree   │  │ gumicord-render  │
│ store         │─▶│ UITree 構築・差分  │─▶│ レイアウト・描画  │
│ 状態 + SQLite │  └─────────┬─────────┘  └────────┬─────────┘
└───────▲───────┘            │                     │
        │           ┌────────┴────────┐            ▼
        │           ▼                 ▼   ┌──────────────────┐
        │  ┌────────────────┐ ┌─────────┐ │ gumicord-platform│
        │  │ gumicord-plugin│ │gumicord-│ │ winit / IME /    │
        │  │ QuickJS ホスト │ │ theme   │ │ a11y / 通知      │
        │  └────────────────┘ └─────────┘ └──────────────────┘
        │
┌───────┴───────────────────────┐
│ gumicord-gateway  WebSocket   │
│ gumicord-rest     HTTP        │
│ gumicord-model    ドメイン型   │
└───────────────────────────────┘
```

## クレート構成 🔬

| クレート | 責務 | 依存する主なクレート |
|---|---|---|
| `gumicord-model` | Snowflake, Guild, Channel, Message などのドメイン型とシリアライズ | `serde` |
| `gumicord-gateway` | Gateway 接続、ハートビート、再接続、イベント配信 (`NFR-010`, `NFR-020`, `NFR-023`) | `tokio-tungstenite`, `zstd` |
| `gumicord-rest` | REST クライアント、レート制限の事前抑制とバックオフ (`NFR-021`, `NFR-022`) | `reqwest` |
| `gumicord-store` | 正規化された状態のインメモリ保持と SQLite への永続化、全文検索 (`FR-035`), 暗号化 (`SEC-020`) | `rusqlite` (FTS5) |
| `gumicord-uitree` | UITree の型定義・構築・差分計算。**安定 ID の唯一の定義元** (`EXT-001`〜`EXT-005`) | — |
| `gumicord-theme` | テーマ JSON のパース・検証・トークン解決・セレクタ照合 (`EXT-010`〜`EXT-020`) | `serde_json`, `jsonschema` |
| `gumicord-plugin` | QuickJS ホスト、ケイパビリティ強制、隔離とタイムアウト (`EXT-030`〜`EXT-053`, `SEC-010`〜`SEC-015`) | `rquickjs` |
| `gumicord-render` | レイアウト計算、描画コマンド生成、wgpu によるラスタライズ、テキスト整形 | `wgpu`, `cosmic-text` |
| `gumicord-platform` | ウィンドウ、入力、IME、アクセシビリティ、通知、クリップボード、ファイル選択 (`PLT-*`) | `winit`, `accesskit` |
| `gumicord-app` | 画面遷移、アプリ状態、上記の結線 | — |

> **`gumicord-uitree` が安定 ID の唯一の定義元である。** 仕様書 [03-uitree.md](03-uitree.md) と SDK の `.d.ts` はここから生成する ([ADR-0004](adr/0004-semantic-uitree-as-extension-abi.md) の帰結 3)。手書きで同期しない。

## フレームのパイプライン 🔬

1フレームで何が起きるか。**この順序が拡張の意味論を決めるため、仕様として固定する。**

```
[1] 入力・イベント取り込み
    OS イベント (winit) / Gateway イベント / タイマー
        │
[2] 状態更新                                        gumicord-store
    Gateway イベントを正規化して状態へ反映
        │
[3] UITree 構築 (差分)                              gumicord-uitree
    変更された状態に対応する部分木のみ再構築
        │
[4] プラグインの構造介入                             gumicord-plugin
    insert / replace / wrap / remove
    ※ 変更のあった部分木のみをプラグインへ渡す
        │
[5] テーマ解決                                      gumicord-theme
    トークン解決 → セレクタ照合 → スタイル確定
    ※ [4] で挿入されたノードにもテーマが適用される
        │
[6] プラグインのスタイル介入                         gumicord-plugin
    テーマより後に適用される (プラグインが最終決定権を持つ)
        │
[7] レイアウト                                      gumicord-render
        │
[8] 描画コマンド生成 → GPU 送出                      gumicord-render
        │
[9] アクセシビリティツリー更新                       gumicord-platform
    UITree のセマンティクスをそのまま a11y に流す
```

### この順序の根拠

- **[4] が [5] より前**: プラグインが挿入したノードもテーマの対象になる。そうでないとプラグイン製 UI だけテーマから浮く
- **[6] が [5] より後**: プラグインはテーマを上書きできる。テーマとプラグインが衝突したときプラグインが勝つ
- **[9] が UITree から直接生成される**: UITree がセマンティックである以上、アクセシビリティ情報は追加コストなしに得られる。これは自前レンダラの数少ない構造的な利点であり、`PLT-003` の実現手段でもある

## スレッドモデル 🔬

```
メインスレッド            ワーカー (tokio)         プラグインスレッド
─────────────────       ──────────────────      ──────────────────
OS イベントループ    ◀──▶  Gateway (WS)          QuickJS コンテキスト
レイアウト                 REST                   (プラグインごとに分離)
描画 / present             SQLite I/O
a11y ツリー更新            画像デコード
        │                       │                        │
        └───────── チャネル経由でのみ通信 ────────────────┘
```

**原則: メインスレッドをブロックするものを置かない。**

- プラグインの実行はメインスレッドの外。暴走しても描画が止まらない (`EXT-050`, `EXT-051`)
- プラグインの介入結果はフレーム境界で取り込む。規定時間内に返らなければ**前フレームの結果を再利用**し、当該プラグインに警告を出す
- QuickJS のコンテキストはプラグインごとに独立させ、1つのクラッシュが他に波及しないようにする (`EXT-050`)

> 🔬 検証事項: プラグインを別スレッドに置くと、UITree の受け渡しでコピーが発生する。差分のみを渡す設計でコストが許容範囲に収まるか、スパイク S3 で測る。

## プラットフォーム統合 🔬

自前レンダラで最大のコストがかかる部分。**プラットフォーム固有コードはここに閉じ込める。**

| 機能 | Windows | macOS | Linux | Android | iOS |
|---|---|---|---|---|---|
| ウィンドウ | `winit` | `winit` | `winit` (X11/Wayland) | `winit` + `android-activity` | `winit` |
| GPU | D3D12 / Vulkan | Metal | Vulkan / GLES | Vulkan / GLES | Metal |
| カスタムタイトルバー | DWM 拡張フレーム + `WM_NCHITTEST` | `titlebarAppearsTransparent` + `fullSizeContentView` | CSD (Wayland) / `_GTK_FRAME_EXTENTS` (X11) | — | — |
| IME 🔬 | winit の `Ime` イベント | 同左 | 同左 | **要自前** (`InputConnection`) | **要自前** (`UITextInput`) |
| アクセシビリティ 🔬 | `accesskit` (UIA) | `accesskit` | `accesskit` (AT-SPI) | `accesskit` | **要確認** |
| 通知 | `ToastNotification` | `UNUserNotification` | D-Bus (`org.freedesktop.Notifications`) | `NotificationManager` + FCM | `UNUserNotification` + APNs |
| セキュアストレージ | DPAPI | Keychain | Secret Service | Keystore | Keychain |

> 🔬 **最大の未検証リスクはモバイルの IME である。** winit のデスクトップ実装は `Ime` イベントを提供するが、Android の `InputConnection` と iOS の `UITextInput` は自前で橋渡しする必要がある可能性が高い。`PLT-001`, `PLT-002` はこれに依存する。スパイク S2 の最優先項目。

## テーマとプラグインのデータフロー

```
テーマ:
  theme.json ──[JSON Schema 検証]──▶ トークン表 + ルール表
                                        │
                                  [セレクタ索引化]
                                        │
                        UITree の各ノードに対し O(1) 近傍で照合
                                        │
                                   確定スタイル

プラグイン:
  index.ts ──[esbuild]──▶ plugin.js ──[qjsc]──▶ plugin.qjsc
                                                    │
                                        [manifest のケイパビリティ検証]
                                                    │
                                          QuickJS コンテキスト起動
                                                    │
                              ホストが注入する API のみ到達可能
                              (SEC-010: 宣言外の API は存在しない)
```

## 未確定事項

スパイク完了後にこの文書を更新して確定させる。

| 項目 | 決めるための材料 |
|---|---|
| 2D 描画を自前バッチャにするか `vello` / `skia-safe` を使うか | S1 の描画品質と性能の比較 |
| モバイル IME をどう橋渡しするか | S2 の結果 |
| `accesskit` を採用するか OS を直接叩くか | S2 の iOS 対応状況 |
| プラグインを別スレッドに置くコストが許容範囲か | S3 の実測 |
| `NFR-001`, `NFR-002` の具体的な数値目標 | S1 の実測 + 公式クライアントとの比較 |
| M1 のスコープを維持するか縮小するか | 全スパイクの工数実績 |
