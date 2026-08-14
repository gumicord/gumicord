# 03. セマンティック UI ツリー仕様

| | |
|---|---|
| ステータス | **ドラフト** — M1 の安定 ID セットを提案する |
| 最終更新 | 2026-08-14 |
| 前提となる決定 | [ADR-0004](adr/0004-semantic-uitree-as-extension-abi.md), [ADR-0001](adr/0001-native-rust-renderer.md) |
| 関連要件 | `EXT-001`〜`EXT-006`, `EXT-012`, `EXT-032`, `EXT-043` |

> ⚠️ **本文書に載る安定 ID は、一度確定させると削除も改名もできない** (`EXT-003`)。
> したがって M1 のセットは**意図的に小さく始める**。足りないノードは後から追加できるが、余計なノードは取り除けない。
> **「あったほうが便利かもしれない」で足さない。「これがないと拡張が書けない」で足す。**

---

## 1. UITree とは

UI を「**安定 ID を持つセマンティックノードのツリー**」として表現したもの。ノードは**見た目ではなく意味**を表す。

これが Gumicord の**唯一の拡張 ABI** である。テーマもプラグインも UITree にのみ介入し、それ以外の内部構造は一切公開しない ([ADR-0004](adr/0004-semantic-uitree-as-extension-abi.md))。

```
UITree ──▶ テーマがスタイルを解決 ──▶ プラグインが変形 ──▶ レンダラが描画
   │                                                          │
   └──────────────────────────────────────────▶ アクセシビリティツリー
```

アクセシビリティツリーが UITree から直接生成できるのは、UITree がセマンティックだからである。これは自前レンダラの数少ない構造的な利点であり、`PLT-003` の実現手段でもある。

---

## 2. ノードの構造

```rust
struct UiNode {
    /// 安定 ID。本文書が定義する不変の識別子
    id: NodeId,
    /// 同じ親の下で同じ id を持つノードを区別する鍵 (リスト項目など)
    key: Option<Key>,
    /// 状態。テーマの条件分岐に使う
    states: StateSet,
    /// このノードが表現しているドメインオブジェクトへの参照
    data: Option<DataRef>,
    /// テーマとプラグインによって解決された最終的なスタイル
    style: Style,
    children: Vec<UiNode>,
}
```

### 2.1 安定 ID

小文字のドット区切り。名前空間 → 領域 → 部位 の順に絞り込む。

```
chat.message.header.author
└┬─┘ └──┬──┘ └─┬──┘ └─┬──┘
 │      │      │      └ 部位
 │      │      └ 部位
 │      └ 領域
 └ 名前空間
```

**規則**:

| # | 規則 |
|---|---|
| N1 | 使える文字は `[a-z0-9_.]` のみ。単語の区切りは `_` |
| N2 | 階層は最大 4 段。5 段目が必要になったら、そもそも設計を疑う |
| N3 | **見た目を表す語を使わない。** `chat.message.blue_text` ではなく `chat.message.header.author` |
| N4 | 親の ID は子の ID の接頭辞である。ツリー構造と ID 構造を一致させる |
| N5 | 複数形は使わない。コンテナは `_list` を付ける (`nav.guild_list`) |

### 2.2 `key` — 同じ ID を持つノードの区別

リスト項目のように同じ安定 ID のノードが並ぶ場合、`key` で区別する。差分更新の同一性判定にも使う。

```
nav.guild_list
├─ nav.guild_list.item  key=Guild(123456)
├─ nav.guild_list.item  key=Guild(789012)
└─ nav.guild_list.item  key=Guild(345678)
```

**プラグインは `key` を読めるが、セレクタには使えない。** 特定のギルドだけを狙い撃つのは `data` を見て判断する。

### 2.3 状態 (`states`)

テーマの条件分岐 (`EXT-013`) に使う。M1 で定義するのは以下のみ。

| 状態 | 意味 |
|---|---|
| `hover` | ポインタが乗っている |
| `active` | 押下中 |
| `focus` | キーボードフォーカスがある |
| `selected` | 選択されている (現在開いているチャンネルなど) |
| `disabled` | 操作できない |
| `unread` | 未読がある |
| `mentioned` | メンションされている |
| `loading` | 読み込み中 |

複数が同時に立ちうる。テーマ側での優先順位は [04-theme.md](04-theme.md) が定義する。

### 2.4 `data` — ドメインオブジェクトへの参照

そのノードが何を表現しているか。プラグインが判断材料に使う。

```ts
ui.patch("chat.message.header.author", (node, ctx) => {
  if (!ctx.data?.author?.bot) return node;   // ← data を見て分岐
  return ui.after(node, ui.badge({ text: "BOT" }));
});
```

**公開するのは読み取り専用のスナップショットであり、内部の状態そのものではない。** `data` 経由でクライアントの状態を書き換えることはできない。

---

## 3. 安定 ID セット (M1)

`03` の本体。**この一覧が拡張 ABI である。**

### 3.1 `app.*` — アプリのルートと画面

| ID | 意味 |
|---|---|
| `app.root` | ツリーの根 |
| `app.window` | ウィンドウ 1 枚 |
| `app.screen.loading` | 起動中 |
| `app.screen.login` | ログイン画面 (`FR-001`) |
| `app.screen.main` | メイン画面 |

### 3.2 `chrome.*` — ウィンドウクローム

`PLT-020` の独自タイトルバー。デスクトップのみ。

| ID | 意味 |
|---|---|
| `chrome.titlebar` | 独自タイトルバー |
| `chrome.titlebar.title` | タイトル表示 |
| `chrome.titlebar.controls` | ウィンドウ操作ボタン群 |
| `chrome.titlebar.control` | 個々のボタン (`key` で minimize/maximize/close を区別) |

> モバイルには `chrome.*` が存在しない。テーマは `when: { platform: ... }` で分岐する (`EXT-014`)。
> **プラグインは「あるかもしれないし、ないかもしれない」前提で書く必要がある。** これは [5.3](#53-存在しないノードへのパッチ) で扱う。

### 3.3 `nav.*` — ナビゲーション

| ID | 意味 | 要件 |
|---|---|---|
| `nav.guild_list` | ギルド一覧 | `FR-010` |
| `nav.guild_list.home` | DM への入口 | `FR-013` |
| `nav.guild_list.item` | ギルド 1 個 | `FR-010` |
| `nav.guild_list.item.icon` | ギルドアイコン | |
| `nav.guild_list.item.badge` | 未読・メンション数 | `FR-042` |
| `nav.channel_list` | チャンネル一覧 | `FR-011` |
| `nav.channel_list.header` | ギルド名などの見出し | |
| `nav.channel_list.category` | カテゴリ | `FR-011` |
| `nav.channel_list.item` | チャンネル 1 個 | `FR-011` |
| `nav.channel_list.item.icon` | 種別アイコン | |
| `nav.channel_list.item.name` | チャンネル名 | |
| `nav.channel_list.item.badge` | 未読・メンション数 | `FR-042` |
| `nav.dm_list` | DM 一覧 | `FR-013` |
| `nav.dm_list.item` | DM 1 件 | `FR-013` |

### 3.4 `chat.*` — チャット

**拡張が最も集中する領域。** BetterDiscord のプラグインの大半はここを触る。

| ID | 意味 | 要件 |
|---|---|---|
| `chat.view` | チャット領域全体 | |
| `chat.header` | チャンネルヘッダ | |
| `chat.header.title` | チャンネル名 | |
| `chat.header.topic` | トピック | |
| `chat.message_list` | メッセージ一覧 | `FR-020` |
| `chat.message` | メッセージ 1 件 | `FR-020` |
| `chat.message.avatar` | 送信者アイコン | |
| `chat.message.header` | 送信者行 | |
| `chat.message.header.author` | 送信者名 | `FR-022` |
| `chat.message.header.badges` | BOT タグなど | |
| `chat.message.header.timestamp` | 時刻 | |
| `chat.message.reply_ref` | 返信元の参照表示 | `FR-028` |
| `chat.message.content` | 本文 | `FR-021` |
| `chat.message.attachments` | 添付一覧 | `FR-025` |
| `chat.message.attachment` | 添付 1 件 | `FR-025` |
| `chat.message.embeds` | 埋め込み一覧 | `FR-026` |
| `chat.message.embed` | 埋め込み 1 件 | `FR-026` |
| `chat.message.actions` | ホバー時の操作群 | `FR-024` |
| `chat.typing_indicator` | 入力中表示 | `FR-027` |
| `chat.input` | 入力欄全体 | `FR-024` |
| `chat.input.field` | テキスト入力そのもの | `PLT-001` |
| `chat.input.toolbar` | 入力欄の上部 | |
| `chat.input.actions` | 送信・添付などのボタン群 | |

### 3.5 `primitive.*` — 描画プリミティブ

**プラグインが新しいノードを作るときに使う語彙。** これがないとプラグインは何も生成できない。

| ID | 意味 |
|---|---|
| `primitive.text` | 文字列 |
| `primitive.image` | 画像 |
| `primitive.icon` | アイコン |
| `primitive.avatar` | 円形の人物画像 |
| `primitive.badge` | 小さなラベル |
| `primitive.button` | 押せるもの |
| `primitive.divider` | 区切り線 |
| `primitive.spinner` | 読み込み表示 |
| `primitive.mention` | メンション (`FR-022`) |
| `primitive.emoji` | 絵文字 (`FR-023`) |
| `primitive.code_block` | コードブロック (`FR-021`) |
| `primitive.spoiler` | スポイラー (`FR-021`) |
| `primitive.link` | リンク (`FR-021`) |

### 3.6 `layout.*` — レイアウトノード

| ID | 意味 |
|---|---|
| `layout.row` | 横並び |
| `layout.column` | 縦並び |
| `layout.stack` | 重ね |
| `layout.scroll` | スクロール領域 |
| `layout.spacer` | 空き |

### 3.7 M1 の合計

**約 60 個。** 意図的にここで止めている。

**M1 で意図的に含めなかったもの** (M2 以降で追加する):

| 領域 | 理由 |
|---|---|
| `overlay.*` (モーダル・コンテキストメニュー・トースト) | 設計が固まっていない。急いで決めると負債になる |
| `settings.*` (設定画面) | プラグインの設定画面 (`EXT-035`) と併せて設計する必要がある |
| `chat.message.reactions` | `FR-029` が M2 のため |
| `member_list.*` | `FR-043` が M2 のため |
| `thread.*` / `forum.*` | `FR-015`, `FR-016` が M2 のため |
| `voice.*` | M3 |

---

## 4. 後方互換性の規則 (`EXT-003`, `EXT-004`)

> **これは技術的な制約ではなく、プロジェクトの約束である。**
> BetterDiscord のプラグインが壊れ続ける問題を解くことが Gumicord の存在理由であり、この約束を破ると存在理由が消える。

| # | 規則 |
|---|---|
| C1 | メジャーバージョン内で安定 ID を**削除しない** |
| C2 | メジャーバージョン内で安定 ID を**改名しない** |
| C3 | **追加は自由。** 新しい ID を足すことは破壊的変更ではない |
| C4 | ノードの**親子関係の変更は破壊的変更**である。`ui.wrap` の結果が変わるため |
| C5 | ノードが**出現しなくなる**のも破壊的変更である (ID は残っているが誰も生成しない状態) |
| C6 | やむを得ず C1〜C5 に反する必要が生じた場合、**1 メジャーバージョン以上の非推奨期間**を設ける (`EXT-004`) |

### 4.1 CI による強制

`spec/03-uitree.md` の変更を含む PR では、CI が以下を検査する。

```
1. 前のリリースの ID 一覧と比較する
2. 削除・改名があれば PR を落とす
3. 追加のみなら通す
4. 親子関係の変更を検出したら警告する
```

### 4.2 非推奨の手順

```
v1.4  ノード X を非推奨とマークする。動作は変わらない。
      SDK の型定義に @deprecated が付き、プラグインのビルド時に警告が出る
v1.5  ランタイムでも警告を出す
v2.0  削除できる
```

---

## 5. プラグインから見た規約

### 5.1 パッチ適用の意味論

[05-plugin-api.md 2 章](05-plugin-api.md#2-パッチ適用の意味論) が規定する P1〜P4 に従う。要点:

- 走査は**ボトムアップ**
- セレクタの照合は**パッチ適用前の**安定 ID に対して行う
- **パッチの出力に再帰しない**
- 同一ノードへの複数パッチは**登録順**に連鎖する

### 5.2 差分の単位

プラグインへ渡すのは**変更のあった部分木のみ**である。UITree 全体は渡さない ([05-plugin-api.md 3 章](05-plugin-api.md#3-uitree-の受け渡し))。

S3 の実測では、1601 ノードを丸ごと渡すと 5.242 ms、差分のみなら 0.033 ms だった。

**したがってプラグインは「ツリー全体を走査する」ことができない。** 自分がパッチした部分木の外側は見えない。これは制約であると同時に、**プラグインが全体を壊せないという保証**でもある。

### 5.3 存在しないノードへのパッチ

`chrome.titlebar` はモバイルに存在しない。存在しない ID にパッチを登録しても**エラーにはならず、単に呼ばれない**。

```ts
// デスクトップでのみ呼ばれる。モバイルでは何も起きない
ui.patch("chrome.titlebar.title", (node) => ui.wrap(node, /* ... */));
```

**これは `EXT-043` (全プラットフォームで同一の挙動) と矛盾しない。** 同一なのは「同じ入力に対して同じ結果を返す」ことであって、「すべてのプラットフォームに同じノードが存在する」ことではない。

> プラグイン作者への含意: **プラットフォーム固有のノードに依存する機能は、なくても成立するように書く。**
> SDK は `ui.exists(id)` を提供し、事前に分岐できるようにする。

---

## 6. 定義の生成 (`EXT-002` の実装)

**安定 ID の唯一の定義元は Rust のコードである。** 本文書と SDK の型定義はそこから生成する。

```
core/uitree/src/ids.rs        ← 唯一の真実の源
        │
        ├──[生成]──▶ spec/03-uitree.md の 3 章 (この一覧)
        ├──[生成]──▶ sdk/src/ids.d.ts     (文字列リテラル型)
        └──[生成]──▶ 適合テストの期待値
```

手書きで同期しない。同期漏れが起きた瞬間、ABI の保証が崩れるためである ([ADR-0004](adr/0004-semantic-uitree-as-extension-abi.md#引き受けるコスト) の帰結 3)。

SDK 側では文字列リテラル型になるため、**存在しない ID を指定するとビルドが通らない**。

```ts
type NodeId =
  | "app.root"
  | "chat.message.header.author"
  | /* ... */;

ui.patch("chat.message.header.autor", fn);
//        ^^^^^^^^^^^^^^^^^^^^^^^^^^ 型エラー: typo をビルド時に落とす
```

---

## 7. 未確定の論点

| 論点 | 備考 |
|---|---|
| `chat.message.actions` の中身をどこまで ID 化するか | 個々のボタンまで ID を振ると数が膨らむ。`key` で区別する案が有力 |
| Markdown の描画結果をどこまでノードにするか | `FR-021` の全要素に ID を振ると数十個増える。M1 では `primitive.*` の合成として扱い、細分化は M2 で判断する |
| 仮想化 (`NFR-007`) と UITree の関係 | 画面外のメッセージはノードを生成しない。プラグインから見て「存在しないノード」になる。この扱いを明文化する必要がある |
| プラグインが挿入したノードの ID 空間 | `plugin.<プラグインID>.*` を強制するか、自由にするか。衝突を避けるなら前者 |
| `data` で公開するフィールドの範囲 | 公開しすぎるとそれ自体が ABI になる。最小から始める |

### 7.1 とくに 3 番目が重要

`NFR-007` の仮想化と `EXT-032` のパッチは根本的に噛み合わない。**画面外のメッセージにパッチを当てることはできない。**

プラグインが「全メッセージを数える」ような処理を書けないということであり、これは制約として明示する必要がある。代わりに `gateway` イベントのミドルウェア (`EXT-033`, `EXT-034`) を使ってもらう。

**M1 の実装前にこの論点を決める。** 後から変えるとプラグインの前提が崩れるため。
