# 道具

開発を回す道具: タスクランナー (`xtask`)、プラグイン SDK (`sdk`)、
適合スクリーンショット (`tools/screenshot`)、配布素材 (`packaging`)。

# xtask (`xtask`)

> `cargo xtask` の独自タスクランナー。`just`・`make` 不要で、検査・生成・互換性検証を集約する。安定 ID の正本を直接使うため `gumicord-uitree` のみに依存する。

## Files

### `src/main.rs` — コマンド分岐と各タスクの実装。`check-fast` (高速コンパイル検査)、`check` (完全検証)、`check-light` (ビルド不要の検査のみ)、`fmt`、`lint`、`test`、`schema` (`spec/schema/validate.mjs` による JSON Schema 検証)、`sdk` (型保証検証)、`abi` (安定 ID 互換性検証、`--accept` で snapshot 更新)、`gen` (仕様・SDK 型の生成)。

### `src/uitree.rs` — 安定 ID からの生成と ABI 検査。正本は `core/uitree/src/ids.rs` のみで、`spec/03-uitree.md` と `sdk/src/ids.ts` を上書き生成し、削除・改名を破壊的として拒む。

# sdk (`sdk`)

> プラグイン作者が輸入する TypeScript SDK。ホスト注入の薄い façade。

## Files

### `src/index.ts` — 公開 façade。`ui.patch/exists/wrap/after/before/settings/stack/node/text/badge/button/icon`、`log`、`storage` が中核。`patch` は bottom-up・登録順で一度だけ走り、`wrap` は core ID を作れない。

### `src/uitree.ts` — UITree 型。`data` は ID から型付けされ読取専用。

### `src/runtime.ts` — プラグイン側実行時。順序と例外の扱いは ABI の一部であり、例外は当該枝に戻して失敗数を数える。

### `src/ids.ts` — 生成物。`cargo xtask gen` で生成され、手編集禁止。未知 ID は型検査で落とす。

### `src/data.ts` — `data` の領域型 (`UserData`/`MessageData`/`GuildData` 等)。生 Discord ペイロードは出さない。

### `package.json` — `@gumicord/sdk` の定義 (`build`/`typecheck` 等)。

### `test/run.mjs` — 型保証の実行器。肯定例は必通・否定例は必否を esbuild 束＋`tsc` 直実行で検証する。

### `test/positive.ts` — 必ず通る使用例集。

### `test/negative/` — 通ってはならない使用例集。`typo-node-id.ts` (未知 ID)、`wrong-data-field.ts` (型外 `data`)、`data-mutation.ts` (`data` 書換)、`core-id-via-node.ts` と `core-id-creation.ts` (core ID 生成)。

### `bin/gumicord-plugin.mjs` — `gumicord-plugin build/dev` の実装。IIFE 自己完結束で束ねる。

# tools/screenshot (`tools/screenshot`)

> ヘッドレス適合スクリーンショット (`NFR-015`)。固定場面を描画し、許容差で祝福画像と比較する。

## Files

### `Cargo.toml` — `gumicord-screenshot` (バイナリ `screenshot`) の定義。`gumicord-render/theme/uitree`・`png` に依存する。

### `src/main.rs` — 比較器本体。`login`/`chat`/`chat-hidpi` の 3 場面を ASCII・同梱フォントのみで描き、`--rebless` で祝福更新、不一致は実像と差分を残す。GPU 無しは成功扱いの skip。祝福画像は `render/tests/screenshots/<os>/`。

# packaging/ (`packaging/`)

> CI の `dist` が組み立てる配布用素材。

## Files

### `README.md` — 素材一覧と組立て方 (無署名)。

### `linux/gumicord.desktop` — AppImage 用 desktop entry。

### `macos/Info.plist` — `Gumicord.app` の束定義。

### `icons/app-icon.png` — 4096px のアイコン原版 (macOS 用 `sips` 変換元)。

### `icons/gumicord.png` — 512px の同柄 (desktop entry 用)。
