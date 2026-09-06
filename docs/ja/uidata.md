# core: UI データ

UITree (`uitree`)、テーマ (`theme`)、Markdown (`markdown`)。関連仕様:
[`spec/03-uitree.md`](../../spec/03-uitree.md)、[`spec/04-theme.md`](../../spec/04-theme.md)。

# gumicord-uitree (`core/uitree`)

> セマンティック UI ツリーと拡張 ABI である安定 ID を定義する。安定 ID の
> 追加のみを許し、削除・改名は破壊的変更として拒む。

## Files

### `src/lib.rs` — 入口。`ids`・`node`・`style`・`value` を公開し、テーマが使うノード状態 (`State`、`StateSet`) を定義する。`StateSet` は u16 ビットセットで、`when.state` 配列は全件一致を要求する。

### `src/ids.rs` — 安定 ID の唯一の定義サイト。`define_node_ids!` から `NodeId` と付随情報を生成する。Key items: `NodeId`、`DataKind`、`Origin`、`as_str()`、`parent()`。ドット区切り名から名前空間と親 ID を導出する。最大 4 階層・`[a-z0-9_.]` の命名規則を持つ。

### `src/node.rs` — ノード本体。安定 ID に表示内容・キー・状態・参照・子を結びつける。Key items: `UiNode`、`Content`、`Span`、`DataRef`、`UiNode::new()`、`walk()`。`with_data()` は ID が宣言する `DataKind` のある所にだけ付着する。走査は深さ優先の前順で描画順と一致する。

### `src/style.rs` — 解決済みスタイル。カスケードは後勝ち・プロパティ単位・詳細度なし。未指定は `None` のまま残す。Key items: `Style`、`Decoration`、`Style::overlay()`、`inherit_from()`。継承するのは `color` と `font` のみ。

### `src/value.rs` — テーマ値型。長さは常に論理 px で、DPI 換算はレンダラの仕事。Key items: `Color`、`AssetRef`、`Background`、`Color::parse()`、`AssetRef::parse()`。`AssetRef::parse` はバンドル外参照や未宣言ホストを拒否する。

# gumicord-theme (`core/theme`)

> テーマ JSON の検証・トークン解決・セレクタ照合・スタイル確定を担う。

## Files

### `src/lib.rs` — JSON を受けて使える `Theme` と診断の組を返す。失敗しても offender だけを捨て、使える部分は適用し続ける。Key items: `Theme`、`ParseResult`、`Theme::parse()`、`style_for()`、`CLIENT_ABI`。全体を捨てるのは JSON 構文破損とマニフェスト欠落・不正のみ。

### `src/resolve.rs` — フレームパイプラインのテーマ適用層。木全体にスタイルを行き渡らせ継承を流す。Key items: `resolve()`、`clear()`。`Slot` の鍵だけが照合に使われ、スノーフレークは決して一致しない。

### `src/parse.rs` — JSON からの読取り。未知プロパティは警告でその箇所だけ落とし、未知の `when` キーや値はルールごと落とす。`$data.tint` は印として保持し、ノード毎に実色を流し込む。Key items: `Manifest`、`Rule`、`Tinted`。`blur` は読込時適用のため上限 256 を持つ。

### `src/token.rs` — デザイントークン表。色と数値は即時確定し、オブジェクトは使用箇所で型付けするため未型付けのまま保持する。参照連鎖は反復走査で解決する。Key items: `Tokens`、`TokenValue`、`Tokens::build()`、`get()`。循環と未定義は表から落とす。深い連鎖でも再帰しない。

### `src/diag.rs` — 診断収集。1 つの誤りで画面を白紙にしないための苦情リスト。Key items: `Diagnostic`、`Diagnostics`、`Severity`、`Diagnostics::error()`、`warn()`。未知は前方互換のため警告、作者の誤りはエラーとし、JSON パスで位置を示す。

### `src/cond.rs` — ルール条件。全キーの AND だが、状態配列は AND・プラットフォーム配列は OR という非対称を持つ。Key items: `When`、`MatchContext`、`Platform`、`When::matches()`、`MatchContext::new()`。幅境界は両端を含み、`slot` は同 ID 兄弟の位置区別にのみ使う。

# gumicord-markdown (`core/markdown`)

> Discord 方言の Markdown を構文解析し、描画を知らない `Block` 列だけを返す。色・寸法・名前解決は持たず、読めない構文は文字通りに出す。

## Files

### `src/lib.rs` — 公開入口。全体用 `parse()` とインライン限定 `parse_inline()`、模型型の再エクスポート。

### `src/model.rs` — 解析結果の模型。装飾はビット集合で重ね合わせ可能。Key items: `Block`、`Inline`、`InlineKind`、`Deco`、`Mention`。スポイラーは独立要素ではなく装飾側に置き、行の折返しを保つ。

### `src/block.rs` — ブロック段解析。フェンスを先に切り出してから行分割するため、コード内の `# ` や `> ` を誤認しない。Key items: `parse()`。閉じないフェンスは文字通り扱い、引用は `>>> ` と連続 `> ` を束ねる。

### `src/inline.rs` — インライン段解析。開き記号は閉じ手が見つかって初めて入り、見つからなければ文字通りに残す。Key items: `parse()`。closer 探索はコードスパンを飛び越え、`_` は英数直後には開かない。文末約物は裸 URL に含めない。

### `src/tests.rs` — tests-only。一行形式に潰して期待値と比較する。
