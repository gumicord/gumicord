# ADR-0004: セマンティック UI ツリーを唯一の拡張 ABI とする

| | |
|---|---|
| ステータス | 承認 |
| 決定日 | 2026-08-14 |
| 関連要件 | `EXT-001`〜`EXT-006`, `EXT-012`, `EXT-032`, `EXT-043` |

## 背景

BetterDiscord と Vencord は、Discord の React コンポーネントツリーを実行時にパッチすることで、事実上あらゆる UI 改変を可能にしてきた。この**自由度の高さ**こそが Discord のカスタマイズ文化を成立させた。

同時に、この方式は**構造的に脆い**。

- パッチ対象は難読化された内部コンポーネントであり、Discord が意図的に公開したものではない
- webpack のモジュール ID もクラス名も、ビルドのたびに変わる
- プラグインは「特定のビルドの内部構造」に依存するため、Discord の更新のたびに壊れる
- プラグイン作者は機能追加ではなく**追従作業**に時間を費やし続ける

Gumicord は改造ではなく再実装なので、この問題を根本から選び直せる。

## 決定

**UI を「安定 ID を持つセマンティックノードのツリー (UITree)」として構築し、これを唯一かつ公開された拡張 ABI とする。**

テーマもプラグインも、UITree にのみ介入する。それ以外の内部構造は一切公開しない。

## UITree とは何か

UITree のノードは、**見た目ではなく意味**を表す。

```
app.root
└─ app.window
   ├─ chrome.titlebar
   │  ├─ chrome.titlebar.title
   │  └─ chrome.titlebar.controls
   ├─ nav.guild_list
   │  └─ nav.guild_list.item          [repeat: guild]
   ├─ nav.channel_list
   │  ├─ nav.channel_list.header
   │  └─ nav.channel_list.item        [repeat: channel]
   └─ chat.view
      ├─ chat.header
      ├─ chat.message_list
      │  └─ chat.message              [repeat: message]
      │     ├─ chat.message.avatar
      │     ├─ chat.message.header
      │     │  ├─ chat.message.header.author
      │     │  ├─ chat.message.header.badges
      │     │  └─ chat.message.header.timestamp
      │     ├─ chat.message.content
      │     ├─ chat.message.attachments
      │     ├─ chat.message.embeds
      │     ├─ chat.message.reactions
      │     └─ chat.message.actions
      └─ chat.input
         ├─ chat.input.toolbar
         ├─ chat.input.field
         └─ chat.input.actions
```

これらの ID は仕様書 [03-uitree.md](../03-uitree.md) に列挙され、**メジャーバージョン内で破壊的変更をしない** (`EXT-003`)。難読化されないし、ビルドごとに変わることもない。**これが「壊れない拡張」の正体である。**

各ノードは以下を持つ。

| 要素 | 説明 |
|---|---|
| **安定 ID** | `chat.message.header.author` — セレクタとして使う不変の識別子 |
| **状態** | `hover` / `active` / `focus` / `disabled` / `selected` — テーマの分岐条件 (`EXT-013`) |
| **データバインディング** | そのノードが表現しているドメインオブジェクト (メッセージ、ユーザー等) への参照 |
| **スタイル** | デザイントークンから解決された最終的な描画属性 |
| **子ノード** | |

## 拡張はどう介入するか

### テーマ = 宣言的なスタイル上書き

```jsonc
{
  "tokens": {
    "color.bg.primary":   "#1a1a2e",
    "color.text.primary": "#eaeaea",
    "radius.md":          8
  },
  "rules": [
    {
      "select": "chat.message.header.author",
      "style": { "color": "$color.accent", "fontWeight": 700 }
    },
    {
      "select": "chat.message",
      "when":   { "state": "hover" },
      "style":  { "background": "$color.bg.hover" }
    },
    {
      "select": "nav.guild_list.item",
      "when":   { "platform": "mobile" },
      "style":  { "size": 40 }
    }
  ]
}
```

CSS を知っていれば読める形にしつつ、JSON なのでパーサも検証も自前で完結する。

### プラグイン = 手続き的なツリー変換

```ts
import { ui, gateway } from "@gumicord/sdk";

// 送信者名の後ろにバッジを差し込む
ui.patch("chat.message.header.author", (node, ctx) => {
  if (!ctx.message.author.bot) return node;
  return ui.after(node, ui.badge({ text: "BOT", tone: "accent" }));
});

// メッセージ本文をラップして翻訳ボタンを付ける
ui.patch("chat.message.content", (node, ctx) =>
  ui.stack([node, ui.button({ label: "翻訳", onPress: () => translate(ctx.message) })]));
```

`ui.patch` の第一引数は安定 ID なので、`@gumicord/sdk` の型定義で補完も検査も効く。**存在しない ID を指定したらビルドが通らない。**

### 介入プリミティブ

| 操作 | 意味 |
|---|---|
| `insert` / `before` / `after` | 兄弟としてノードを追加する |
| `replace` | ノードを別のノードに差し替える |
| `wrap` | ノードを別のノードの子として包む |
| `remove` | ノードを取り除く |
| `style` | ノードのスタイルを上書きする (テーマより後に適用) |

BetterDiscord の React パッチでできることは、この 5 つの合成で表現できる。

## この決定が要件をどう満たすか

| 要件 | どう満たされるか |
|---|---|
| `EXT-043` 全 PF で同一挙動 | UITree の構築が全プラットフォームで単一実装 ([ADR-0001](0001-native-rust-renderer.md))。介入結果も必然的に同一 |
| `EXT-003` 壊れない ABI | 安定 ID は仕様書で管理され、CI で後方互換性が検査される |
| BetterDiscord 級の自由度 | 5 つのプリミティブで任意のツリー変形が可能。さらに描画層が自前なのでカスタムシェーダの注入まで開放できる |
| プラグインの安全性 | 介入点が UITree に限定されるため、権限モデルの適用範囲が明確になる (`SEC-010`) |

## 帰結

### 引き受けるコスト

1. **安定 ID の設計を間違えると恒久的に負債になる。** 破壊的変更ができないため、粒度の判断を最初に誤ると取り返しがつかない
   - → 対策: M1 の安定 ID セットは**意図的に小さく始める**。足りないノードは追加できるが、余計なノードは削除できない
2. **UITree の構築と介入がホットパスに乗る。** 毎フレーム再構築すると QuickJS との往復で破綻する
   - → 対策: UITree は差分更新とし、プラグインへは変更のあった部分木のみを渡す。介入結果はノードの入力が変わるまでキャッシュする
3. **仕様書のメンテナンスコストが継続的に発生する。** ID を増やすたびに `03-uitree.md`、SDK の型定義、適合テストを更新する必要がある
   - → 対策: 安定 ID の定義を Rust 側の単一のソースから生成し、仕様書と `.d.ts` を自動出力する。手書き同期をなくす

### 明示的に公開しないもの

以下は ABI に含めない。プラグインからアクセスできない。

- レンダラの内部状態 (描画コマンドバッファ、GPU リソース)
- Gateway / REST クライアントの内部実装
- 認証トークン (`SEC-002`)
- ローカルデータベースへの直接アクセス

必要な機能は、**UITree または明示的に設計された API 経由で**提供する。「内部が見えるから何でもできる」ではなく「必要なことができる API を設計する」を原則とする。この原則を守れなくなったとき、それは API 設計の失敗であって、内部を開放する理由にはならない。
