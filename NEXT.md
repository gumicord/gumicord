# 次にやること

最終更新: 2026-08-25。**終わったものはこの一覧から消すこと。**

これは仕様ではなく引き継ぎのメモである。腰を据えた計画は
[`spec/07-roadmap.md`](spec/07-roadmap.md) にある。

---

## 1. 検知への対応 — まだ途中である

`cf5ce15` で名乗り (`X-Super-Properties` / `User-Agent` / identify の
`properties`) を公式デスクトップクライアントと同じにした。ただし
**これで終わりではない。**

> ⚠️ **利用規約に反する行為であり、名乗りを整えても安全にはならない。**
> 見分けが付きにくくなるだけで、アカウントを失う可能性は消えない。
> 2026-08-23 に実際にパスワードの再設定を求められている。

### 1.1 ~~ビルド番号を実測する~~ ✅ 済んだ

起動時に `https://discord.com/login` の HTML から取るようにした。
JS の束を追う必要は無く、`GLOBAL_ENV` の `"BUILD_NUMBER"` を読むだけで
足りた。取れなければ埋め込みに落ちる。実測 595897 (書いてあった推測は
451672 で、**14 万ぶん古かった**)。

⚠️ **`CLIENT_VERSION` のほうは実測できていない。** 配信物のどこにも無く、
デスクトップの実行ファイル自身が持っている値である。埋め込みのままである。

⚠️ **形が変わったことは単体試験では分からない。** たまに
`cargo test -p gumicord-rest --test build_number_live -- --ignored` を
走らせること。

### 1.2 `/science` をどうするか — 決めていない

公式クライアントは操作の記録を定期的に `/science` へ送っており、
**沈黙していること自体が目印になりうる**。

⚠️ ただしこれは「偽の操作記録を作って流す」ことである。効き目と引き換えに
何を送るのかは、実装する前に決めること。

### 1.3 要求範囲を絞る — 手を付けていない

**調べたら `op 8` と `op 14` は別の話だった。**

| | いまどうなっているか |
|---|---|
| `op 8` (`REQUEST_GUILD_MEMBERS`) | [`live.rs`](app/core/src/live.rs) の `fill_members`。**既に絞れている** — 開いているチャンネルの発言者のうち、姿を知らない人だけ |
| `op 14` (`GUILD_SUBSCRIBE`) | [`gateway.rs`](core/gateway/src/gateway.rs) の `MEMBER_RANGES`。**こちらが問題である** |

`MEMBER_RANGES` は `[[0,99],[100,199],[200,299]]` で固定してあり、
**メンバー一覧を出していない幅の窓でも 300 人ぶん頼んでいる。**
公式クライアントは見えている範囲を送る。

⚠️ **今夜は触らなかった。** 購読の形を変えると、新着も入力中も来なく
なりうる ([`spec/07-roadmap.md`](spec/07-roadmap.md) の「実機でしか
見つからなかったこと」)。**本物の Discord に繋がないと確かめられない**
ので、起きている人が居るときにやること。

- 出すか出さないかは `Panes::members()` (幅 >= 4 ペイン) が既に知っている
- 巻いた先を頼み直す仕組みが要る (4. の「300 人で止まる」と同じところ)

---

## 2. ~~削除に確認の窓が無い~~ ✅ 入れた

`overlay.modal.*` の安定 ID 7 個を足し、削除の前に窓を挟むようにした。
窓には消える本文の 1 行が出る。「やめる」が先、「削除する」が後ろ。
Esc とボタンで閉じるが、**外を押しても閉じない** (決めたのか消えたのかが
分からなくなるため)。

⚠️ **見た目は確かめていない。** 矩形は
`gumicord_render::layout_for_test` で見てある (真ん中に出る・はみ出さない・
400px 幅でも収まる) が、**色と間合いは見てもらう必要がある。**

まだ決めていない `overlay.*` は `overlay.tooltip` / `overlay.toast` の 2 つ。

---

## 3. C7 (Markdown) の残り

動いているが、まだ手を付けていないもの:

| | |
|---|---|
| リンクを押して開く | いま色が付くだけ |
| カスタム絵文字を絵で出す | いま `:名前:` と文字で出る |
| ~~`<t:…:R>` の相対表示~~ | ✅ 済んだ。書式 `t T d D f F R` を全部出す。描き直しは `Application::next_frame_in` で頼む — **次に文字が変わるまで寝る** |
| スポイラーを走りごとに開く | いまメッセージ単位。走りごとにするには走りごとの当たり判定が要る |

---

## 3.5 ~~ログアウトできない~~ ✅ 入れた

自分の欄を副ボタンで押すと「ログアウト」が出る。削除と同じ確認の窓を通り、
**携帯が要ることを窓に書いてある** (C4b がまだ無いので、書かないと自分の
クライアントから締め出される)。

⚠️ **不具合も一緒に直した。** `Live::started` が `forget_everything` の
後も真のままで、**トークンが弾かれた後に入り直しても Gateway が繋がって
いなかった**。QR を読み直してもキャッシュの画面が出るだけで、何も届かない
状態である。意図的なログアウトも自動のものも、いまは同じ `sign_out` を通る。

---

## 4. コードを英語にする — 途中である

2026-08-24 に決めた規則
([`spec/README.md`](spec/README.md#6-コードは英語仕様は日本語))。
**識別子は全部終わった。コメントは半分残っている。**

| | |
|---|---|
| 識別子 (試験関数名 159 個ほか) | ✅ **全部終わった。** 日本語の識別子はもう 1 つも無い |
| `core/model` `core/rest` `core/markdown` `core/uitree` `core/plugin` | ✅ |
| `core/store/lib.rs` `core/gateway/gateway.rs` | ✅ |
| `app/core` 全部 (`lib.rs` `live.rs` `session.rs` `menu.rs` `markdown.rs`) | ✅ |
| `render/render/{text,layout,draw,intrinsic}.rs` | ✅ |
| `render/platform/{window,clock,clipboard,lib}.rs` | ✅ |
| `.md` (`spec/` と `NEXT.md` 以外) と `.github/workflows/ci.yml` | ✅ 英語にした |
| コミットメッセージ | ✅ これから英語 |
| **残り** | 下の表。**約 1,600 行** |

```text
  125  core/gateway/src/guild_order.rs
  116  core/store/src/db.rs
  107  render/render/src/lib.rs
  100  render/render/src/motion.rs
   80  core/gateway/src/member_list.rs
   78  core/theme/src/lib.rs
   71  app/core/src/images.rs
   70  render/render/src/icon.rs
   70  core/gateway/src/status.rs
   68  core/theme/src/parse.rs
   63  render/platform/src/secret.rs
   61  core/gateway/src/remote_auth.rs
   55  render/render/src/gpu.rs
  ...  core/theme/{resolve,token,cond,diag}.rs,
       render/platform/text_input/*, xtask/*,
       core/gateway/{zstd_stream,proto}.rs, app/desktop/main.rs
```

数え直す:

```text
  grep -rn '^\s*//' --include=*.rs core app render xtask | grep -c '[ぁ-んァ-ヶ一-龠]'
```

⚠️ **`grep` の文字クラスは `—` (em dash) も拾う。** 英語に直した後の
ファイルにも数行残るので、数えるときは目で見ること。

⚠️ **残すもの**:

- 画面に出る文字 (「やめる」「@不明なユーザー」「3 分前」の単位)
- `RestError` の `Display` — あれはログイン画面に「失敗しました: …」と出る
- `AssetRefError` の説明 — テーマを書いた人に見せる診断である
- `core/uitree/src/ids.rs` の説明文 — `cargo xtask gen` が
  `spec/03-uitree.md` の表に書き込む中身である

⚠️ **モジュールの説明を丸ごと差し替えるときは `use` を巻き込まないこと。**
2 回やった (`core/store/src/lib.rs` は `pub mod db;` ごと消えて CI が落ちた)。
**1 ファイル直すたびに `cargo check` を通すこと。**

---

## 5. 前から積んである宿題

| | |
|---|---|
| **C4b** パスワード + TOTP ログイン | いま QR だけ。**携帯が無いと戻れない**。ログアウトの窓もそう断っている。ADR-0007 の hCaptcha のホスト名がまだ未確認 |
| **R4** フォントの列挙を起動の道から外す | 初回 360ms。`NFR-001` の 500ms に対して重い |
| **E4〜E8** プラグイン実行環境 | **何も無い。** これがプロジェクトの存在理由である |
| **R6** 仮想化 (`NFR-007`) | 一覧を全部組んでいる |
| ミュート設定を読んでいない | 黙らせたチャンネルも光る (`FR-041`, M2) |
| `PRESENCE_UPDATE` を見ていない | 他の端末でステータスを変えても追わない |
| メンバー一覧が 300 人で止まる | 1.3 と同じところ |
| ステータスが出ない | 利用者のアカウントで `設定の中にステータスが無い raw=(無し)` |

---

## 覚えておくこと

- **コードは英語、仕様は日本語** (`spec/README.md` 6)。コメントは
  「なぜ」だけを 1〜2 行。**要件番号 (`FR-024` など) をコメントに書かない**
- **git commit で author を指定しない。** この機械の `~/.gitconfig` の
  身元を使う。セッションが渡してくるメールアドレスは GitHub 上で別人に
  紐づいており、過去に 24 件が誤って別人の名前で記録された
- 見た目は自分で確認できない。**変えたら利用者に見てもらうこと**
- テーマの数値を読んでも、置いた結果は分からない。
  `gumicord_render::layout_for_test` で矩形を出して確かめる
  (`cd72d6f` の ✕ がこれで見つかった)
