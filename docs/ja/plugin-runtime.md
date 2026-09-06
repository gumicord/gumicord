# core: プラグイン実行

QuickJS によるプラグインの読込・隔離・権限強制・UITree パッチ適用を担う
(`core/plugin`)。関連仕様: [`spec/05-plugin-api.md`](../../spec/05-plugin-api.md)。
作者向けの使い方は [プラグイン API](https://github.com/gumicord/api-docs/blob/main/ja/plugins.md) を見ること。

## Files

### `src/lib.rs` — ホストの入口。各モジュールを公開し、開発者向けの英語失敗型を定義する。権限は拒否ではなく未注入で強制し、パッチには差分だけを渡す。Key items: `PluginError`、`PluginHost`、`PluginManager`、`Manifest`、`Storage`。

### `src/manager.rs` — 読込・連鎖・無効化を司る。順序付きホスト群が連鎖を同期実行し、ワーカー糸が最新木だけを主糸と受け渡して暴走を効果遅延に変える。承認・拒否・無効化の永続と設定画面行の算出も持つ。Key items: `PluginManager`、`ManagerEvent`、`PluginManager::start()`、`drain()`。能力なしは即時読込、能力ありは初見で承認要求し、空付与は拒否として残る。慢性失敗は unloading し、8ms 超の連鎖は分間 1 声で警告する。

### `src/host.rs` — 1 プラグインの QuickJS の家。`Runtime` と `Context` の対を糸に縛り、全実行を閉包内に閉じ込める。再読込は家ごと捨てて作り直す。Key items: `PluginHost`、`PluginSource`、`PluginHost::load()`、`apply_tree()`。付与能力だけを注入し、未宣言 API は `TypeError` にしかならない。設定頁は書込不可の捨て世界で読み、100ms で殺す。失敗は 60 秒窓で 100 件を数える。

### `src/manifest.rs` — `manifest.json` の身元と宣言能力の検証。手編集による権限拡大を荷下ろし時に弾く。Key items: `Manifest`、`Manifest::load()`、`KNOWN_CAPABILITIES`。ID は逆ドメイン、版はセマンティック版、能力の未知と重複を拒む。

### `src/convert.rs` — ホスト境界の双方向変換。構造と描画内容を渡し、ドメイン事実は `ctx` 経由で運ぶ。戻りは節ごとに検証し、未知 ID は当該出力を全棄却する。Key items: `PatchContext`、`node_to_js()`、`js_to_node()`、`data_key()`。`key`・状態・色・参照は入力木から継承し、JS 側を信用しない。

### `src/storage.rs` — ホスト側のプラグイン毎キーバリュー蓄積。再読込を越えて生き、他者の鍵は決して見えない。Key items: `Storage`、`Storage::load()`、`get()`、`set()`、`remove()`。不在は空出発。
