# ログイン画面 UI — 現状記録・問題点・制約 (手戻り引き継ぎ)

**状態: ドラフト — 後任 AI への手戻り引継ぎ用。**

この文書は、ログイン画面 UI を「直す」担当者へ渡す前提資料である。ここに現状の
実装・問題点・制約を書き留め、デザインの改善方針そのものは書かない（担当 AI が
[01-requirements.md](01-requirements.md) の `FR-001` と相談して決める）。

動作は確認済み（QR / パスワードフォーム / TOTP / WebView2 captcha モーダルすべて
動作する）。UI の見た目が未整備なため手戻りする。**機能は触らない。**

---

## 1. 実装位置

| 変更対象 | 場所 |
|---|---|
| ログイン画面のノード組み立て | `app/core/src/lib.rs` — `login_screen()` と `login_field` / `login_submit` / `login_secondary` / `login_button` / `login_field` / `submit_login` / `leave_login_form` |
| ログインの状態遷移・挙動 | `app/core/src/session.rs` |
| ログイン画面ノードID | `core/uitree/src/ids.rs` — `AppScreenLogin*` (セクション `app.screen.login.*`) |
| ノードのレイアウト (intrinsic) | `render/render/src/intrinsic.rs` — `AppScreenLogin` ほか |
| 見た目 (テーマ) | `examples/themes/midnight/theme.json` — `app.screen.login.*`, `primitive.*` |

ログイン状態は `Session` 列挙型（QR 待ち / `Password` / `PasswordTotp` / その他）で
`login_screen()` が 3 つに分岐してノードを組み立てる。

---

## 2. 現状のノード構成

ログイン画面 (`AppScreenLogin`) は `Intrinsic::column().grow(1.0).cross(Cross::Center)`
つまり「画面一杯・子は水平中央」の縦並び。最上部と最下部に `LayoutSpacer` を置いて
縦位置を調節している（→ [制約 4.1](#41-垂直中央に使える手段が限られる)）。

### 2.1 QR ログイン (既定)

```
LayoutSpacer
AppScreenLoginTitle  : "QR コードでログイン"
PrimitiveQr          : スキャン用 QR
AppScreenLoginHint   : 状態・エラー文言 (login.hint())
PrimitiveButton[login_password] : "パスワードでログイン"
LayoutSpacer
```

### 2.2 パスワードログイン (`Session::Password`)

```
LayoutSpacer
AppScreenLoginTitle   : "パスワードでログイン"
AppScreenLoginField[email]    : メールアドレス
AppScreenLoginField[password] : パスワード
PrimitiveButton[login_submit] : "ログイン"
PrimitiveButton[login_back]   : "戻る"
LayoutSpacer
```

captcha 要求中もこの画面のまま（モーダルが上に被さる）。表示上の「captcha を解いている」
という合図は一切ない。

### 2.3 TOTP ログイン (`Session::PasswordTotp`)

```
LayoutSpacer
AppScreenLoginTitle   : "認証コードを入力"
AppScreenLoginField[totp] : 認証コード
PrimitiveButton[login_submit] : "ログイン"
PrimitiveButton[login_back]   : "戻る"
LayoutSpacer
```

### 2.4 ボタンとスロット

- どの画面も「1 個の主ボタン + 0〜1 個の副ボタン」を持つ。
- 主従の区別は**スロット名だけ**（`login_submit` / `login_password` / `login_back`）。
  `PrimitiveButton` の見た目は全部同じ（→ [問題 3.3](#33-主ボタンと副ボタンの区別がない)）。
- ボタンは `is_hovered` で `State::Hover` を持つ（テーマがホバー着色する）。
- 入力欄は `State::Focus` をフォーカス時に持つ。

---

## 3. 問題点（なぜ「ひどい」と言われたか）

### 3.1 視覚的な階層がない
タイトル・入力欄・ボタンがすべて同じ `gap` で均等に縦に並ぶ。どの要素が「次にやるべき
操作」なのかが一目で分からない。見出しとフォームの間に十分な余白がない。

### 3.2 カード / コンテナでまとまっていない
入力欄とボタンが背景の上に直接浮いて見える。グループとしてまとめる枠（白/浮いた背景の
パネル等）がない。

### 3.3 主ボタンと副ボタンの区別がない
「ログイン」（主）と「戻る」（副）が同じ `primitive.button` の地味な箱。押してほしい
操作が強調されない。QR 画面の「パスワードでログイン」も同様に地味。

### 3.4 入力欄が野暮
フィールドは `app.screen.login.field` に `minHeight: 44` と背景・radius があるが、
ボタンと同じ幅で並ぶため野暮。プレースホルダと入力値の見分けや、エラー時の枠色などの
状態表現もほぼない（focus の `borderColor` のみ）。

### 3.5 縦位置が機械的
上下 `LayoutSpacer` による押し出しなので、真の「中央寄せ」ではなく、要素数によって
位置が動く。狭いウィンドウでは最上部に偏る。

### 3.6 QR 画面が寂しい
QR 画像 + 一行ヒント + リンク的ボタンだけ。ログイン手段の説明や視覚的な余白設計がない。

### 3.7 エラー / 状態の表現が薄い
`AppScreenLoginHint` 一つに状態・エラー・ヒントすべてを出す。エラーを赤くする等の
区別や、captcha 要求中の表示（モーダルが出るまでの間）がない。

---

## 4. 制約（直すときに踏まえること）

### 4.1 垂直中央に使える手段が限られる
レイアウトモデル (`Intrinsic`) に **紙軸 (main-axis) の整列 (`justify`) がない**。
縦 1 列 (`column`) の中で子を垂直中央にする手段は、
- 前後に `LayoutSpacer` を挟む（現行）
- `minHeight` / 高さ指定で近似する

のいずれか。真の中央寄せを足すにはレイアウタ側（`render/render`）に手を入れる必要が
ある。**ドラフト時点では対応していない**（要スパイク）。

水平の整列は `Cross::Center` で可能（現行の `AppScreenLogin` はそうなっている）。

### 4.2 ノードIDは安定 ABI（むやみに消せない）
`AppScreenLogin*` は Core ID で、プラグインが参照する可能性がある（`spec/03-uitree.md`,
`EXT-003`）。**既存 ID の削除・意味変更は不可。追加は可。** 新しい構成要素（例: ログイン
画面専用のカードノード、主ボタン用の区別）を足す場合は追加で良いが、既存 ID は残すこと。

### 4.3 見た目はテーマ、構造はコード
- **見た目**（配色・余白・radius・フォント・枠）は `examples/themes/midnight/theme.json`
  の `app.screen.login.*` と `primitive.*` で変えられる。`:hover` / `:focus` 等は
  `"when": { "state": ... }` で書ける（現状: `hover` / `focus` / `disabled` を利用）。
- **構造**（ノードの追加・入れ替え・順序）は `app/core/src/lib.rs` の `login_screen()`
  と、新ノードを足すなら `core/uitree/src/ids.rs` + `render/render/src/intrinsic.rs`。
- テーマで使えるプロパティ（要約）: `background`, `color`, `font`, `borderColor`,
  `borderWidth`, `radius`, `padding`, `gap`, `minWidth`, `maxWidth`, `minHeight`,
  `width`, `height`, `opacity`, `transition`。セレクタは `select` + `when`
  (`state` / `slot` / `platform` / `maxWidth` 等)。

### 4.4 ボタンの主従スタイルは今は無い
`primitive.button` は単一。主ボタン（`login_submit` / `login_password`）と副ボタン
（`login_back`）を視覚的に分けるには、
- `primitive.button` に `"when": { "slot": "login_submit" }` を追加する（テーマ側）
- または新ノードIDを足す（構造側）

のどちらか。テーマ側で済むならブロック量が少ない。※ 既存の `slot: "cancel_composing"`
のように、スロット別テーマは既に前例がある。

### 4.5 文字列は現状コード内に日本語で埋め込まれている (i18n)
画面に出る文言（`"QR コードでログイン"` 等）は `login_screen()` に直接書かれている。
後で外出しする前提があるため（`spec/README.md` ルール 6、i18n）、**組み立て式の
文字列にしない**こと。UI を直す際に文言を変えても、1 つのリテラルとして書く。

### 4.6 機能は壊さない（回帰注意）
フォーカス移動 (`login_field` + `slot`)、Enter 送信 (`submit_login`)、Escape で戻る
(`leave_login_form`)、QR→パスワード遷移、TOTP、captcha (`pending_captcha` /
`captcha_solved` / `captcha_cancelled`) は動作済み。構造を変えるときはこれらが
スロット ID とイベント配線を失わないこと。

---

## 5. 検証

| 内容 | 方法 |
|---|---|
| ユニットテスト | `cargo test -p gumicord-app`（ログイン関連は `login_tests` モジュール、全 194 件緑） |
| ビルド | `cargo build -p gumicord-desktop` |
| 実機 | `target\debug\gumicord.exe` — QR / パスワード / TOTP / captcha を確認。`GUMICORD_LOG=debug` |

変更時は上記を緑に保ち、`:focus` / `:hover` 両状態がテーマで正しく効くことを目視確認する。

---

## 6. 未決 / 保留

- 垂直中央寄せの実装（レイアウタ拡張）は「要スパイク」
- デザイン方針そのもの（配色・レイアウト・空状態の文言）は担当 AI と `FR-001` で決定
