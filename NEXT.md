# 次にやること

最終更新: 2026-10-02。**終わったものはこの一覧から消すこと。**

これは仕様ではなく引き継ぎのメモである。版計画は
[`spec/07-roadmap.md`](spec/07-roadmap.md) にある。
要件の M 列（M1／M2／M3）は適用範囲の bucket として残し、
版は出荷の乗り物に徹する。

> ⚠️ **利用規約に反する行為であり、名乗りを整えても安全にはならない。**
> 見分けが付きにくくなるだけで、アカウントを失う可能性は消えない。
> 2026-08-23 に実際にパスワードの再設定を求められている。

---

## v0.0.3 残作業 (機械以外待ち)

版上げは済み。コード側の到達は以下で、残りは人手・機械・参加登録待ち。
手順書は [`VERIFY-v0.0.3.md`](spec/verify/VERIFY-v0.0.3.md)。

- 済: 起動時 `duplicate child` 落ちの修正 (同キー姉妹の連番化＋回帰試験)。
  テスター機の再現形 (2 並びの箇条書き) で旧コードが落ち・新コードが通るのを確認
- 済: Windows のコンソール撤去 (`windows_subsystem`) と実行ログのファイル化
  (`%APPDATA%\gumicord\logs`、macOS／Linux は設定先の `logs/`)
- 済: CRT 静的リンク。再頒布 `VCRUNTIME140`／`MSVCP140` 参照なしを確認
- 済: デスクトップのファイル選択基盤 (`rfd`＋試験＋手動 smoke)。呼ぶ画面はまだ無い
- 済: 署名・公証の secrets 対応 (未設定なら未署名のまま)。`packaging/README.md` に手順
- 済: Windows blessed 画像の登録 (`render/tests/screenshots/windows/`)。CI 照合が実ゲート化
- 済: Linux の `desktop-file-validate` と AppImage 展開検査の CI 化
- 待ち: macOS／Linux 実機確認 (`VERIFY-v0.0.3.md` の 1〜4・6〜7)
- 待ち: Linux blessed 画像の登録 (CI の `screenshot-ubuntu-*` 実像を目視してから)。
  macOS は GPU 付きランナが要るため CI 対象外のまま
- 残: コンプライアンス画像が実際の app の木を描くようになったので、headless な
  `tools/screenshot` が `gumicord-app` を辿って GTK と WebKitGTK をリンクする。
  CI には apt を足して動かせているが、**取り除くなら** platform 層を
  「状態と木の組み立て」と「窓・GTK・鍵束」に分ける作業が要る
  (`Application` と `TextDocument` と `FrameCx` がまだ platform 側にあるため、
  crates を割るだけで済むとは限らない)。 screenshot の job は apt で 100 秒ほど伸びる
- 待ち: 署名・公証の実実行 (Apple Developer Program の参加と secrets 登録が要る)

---

## v0.0.4 — モバイルの殻 (入力以外)

- Android／iOS の殻と CI の初グリーン (Gradle・Xcode・pin 版の確定)
- 実機の殻挙動 (Xcode のライフサイクル順・Metal・GameActivity)
- Android の HTTPS 全面失敗 (2026-10-02): 0.7.0 では Android の
  `CertPathValidator` が OCSP responder の無い Discord 証明を `Revoked` と
  誤判定し、`https://discord.com` への要求が全て落ちていた
  (上流 rustls-platform-verifier#221)。0.7.1 (android 0.2.0) が CRL 取得を
  解禁して直す。**未実機確認**。QR が出て読み取り・承認まで進み、ログの
  `rustls_platform_verifier` の 2 行 (`... was revoked` / `invalid peer
  certificate: Revoked`) が出ないことを確認すること。Gradle の AAR 取得先が
  crate 内同梱から GitHub の `maven-archive` に移ったため、Gradle 解決を
  通すのは CI の nightly だけ
- Android のセッション途中切断 (2026-10-02 新規): QR を表示してスキャン待ちの
  idle の間に、remote-auth が `ECONNABORTED (os error 103)` で落ちる。
  ログでは毎回 `onStop`/`saveState` が先に来ており、Activity の停止 →
  再開 → GPU 再作成の往復が 2 回起きている。Android がバックグラウンドで
  凍結したせいか自前起因なのかは **まだ決まっていない**。这次的ビルドは
  `activity suspended` / `activity resumed` の情報ログと全行のタイムスタンプ
  で切り分けられる。切断は `RemoteAuthError::Dropped` として報告される
  (`Connect` ではない)。TLS は成功していた (`the QR is ready` は出ている)
- 触って切り替える操作とセーフエリア、Android のクリップボード
- 一員行の行き先 (プロフィール表示) は将来のまま。複数指は対象外
- iPad Stage Manager の窓化時の上部余白: `safe area changed` ログで inset 実測値を取ること
  (Y=0 で窓がメニューバーと重なる場合の PLT-041 適用か、winit の値の問題かの切分けが要る)
- 実機確認 (ADR-0014): Android／iOS でフリングの惰性感、つまみドラッグの掴みやすさ、
  起動スプラッシュ→メイン／ログインの遷移 (キャッシュあり／なし×トークン有効／無効の4通り)。
  `FLING_TAU` 等の数値微調整はこの確認の後に行うこと
- 実機確認 (2026-09-24 追加分): 上端フリックが履歴読み込みに乗り継ぐこと (惰性保持)、
  チャンネル切替後にメンバーリストが空にならないこと、狭幅シートの一覧が出ること。
  いずれも `eda1d74` 以降のビルドで確認すること (それ以前は修正が入っていない)
- 実機確認 (2026-09-24 追加分): iOS で `logs/` に EPERM 警告 (cache directory・gpu probe)
  が出ないこと。Caches へ DB が作られ、再起動後に前回の画面が出ること。
  `fling stopped at the bound` に `at/max` が付くようになったため、
  端死と溢れゼロの区別もログで確認できる
- 実機確認 (2026-09-24 夜追加分): 中央付近での長い惰性 (`fling stopped` が出ず
  `fling interrupted` で終わること)、`older page landed` と上端フリックの対応、
  ジャンプ報告時は直前の `older page landed` の有無と発生操作 (スクロール／フリック／切替)
- 実機確認 (2026-09-26 追加分, ADR-0015): captcha モーダル (iOS／Android／macOS／Linux)。
  パスワード・TOTP・QR の challenged 経路で頁が出て解けること、
  キャンセルで破綻しないこと。Linux は CI の compile も見ること
  (当地では host 試験と iOS／macOS／Android の target 検査までを通した)

---

## v0.0.5 — モバイル入力

- Android は実装済み (`6d88d87`、要実機確認)。iOS の `UITextInput` 橋渡しも実装済み (未 push なら push すること)。方針は [ADR-0011](spec/adr/0011-mobile-input-bridges.md)
- 実機確認項目 (両 OS 共通): 日本語変換の確定・候補、メール→パスワードの前進、送信、欄外タップで閉じること、キーボード表示中のレイアウト追従
- 残作業: Android のキーボード高さ追従 (`PLT-040`、iOS は通知で対応済み)、セーフエリア (`PLT-041`)
- 長押し編集 (未着手・新規): タップの caret 配置、長押し検出 (タイマー)、単語選択と自前ハンドル、メニュー (切取/複写/貼付/全選択)、Android クリップボード JNI (`D4` の残り)。机の上の右クリック菜单は済み
- iOS キーボード閉鎖の残件 (2026-09-28 実装済み分まで): メニュー表示で blur＋貼付で再フォーカス、確認ダイアログ表示で blur、スクロール開始で閉じる (携帯のみ)、自動入力ペアの両 rect 揃い待ち＋再装着 seed 競合の修正、wake 粘着再描画、TOTP 誤判定の文言分岐は実装済み。欄外タップ不発の本体は `press assessed focus` / `ios editor blur` / `proxy resigned` の実機ログで切り分けること (候補: Tap 不成立・意図維持経路・resign 失敗・孤児 owner)。直ればこの行を消すこと。
  2026-10-01: ログイン画面は入力・欄外タップ・自動入力とも実機で動くことを確認した。
  未検証なのはチャット画面だけであり、この行は **まだ閉じない**。同じ確認をチャット画面でやり直すこと
- ログは起動中も読めるように (2026-10-01): Android のログが `Android/data/...` に
  書かれるので、読めるのは root 端末だけだった。`Download/gumicord/logs/` へ
  実行中にミラーするようにした。3 秒ごとに「前回より伸びたか」だけ見て、伸びたときだけ
  コピーを上書きする。同じ名前の行を探して上書きするのは、毎回 insert すると
  同一名が積み重なって困るので。それとは別に
  `cargo clippy -p gumicord-platform --target aarch64-linux-android` が初めて緑になるので、
  **target つきの clippy も回すこと** (CI は host だけ)
- パスワード自動入力 (2026-10-01): ログイン画面で**ボタンが出ない**。
  入力中の `UITextField` に `userInteractionEnabled = false` を入れていたためと
  考えられる。自動入力は、触れない欄には候補を出さない。
  同じ view を余所へどける方針に変えた (`place` で 1px の細帯に置く)。
  **未実機確認**。確認するときは 3 つ必要:
  (1) CI の IPA は `CODE_SIGNING_ALLOWED=NO` の未署名なので、署名のついた
      ビルドで試す。entitlements は署名の中に入るため、署名なしでは付かない
  (2) 機器にパスワードが 1 件以上保存されていて、設定の自動入力が on
  (3) 埋められない部分: `discord.com` の AASA に `dev.gumicord.app` は無い。
      追加できないので、使えるのは鍵アイコンから手で選ぶ経路だけ
- 長押しメニューとキーボード (2026-10-01): 入力欄を長押ししてメニューを開くと
  キーボードが下がらなかった。app 側 (`context_menu` が `release_text_focus`) は正しく、
  落とすのは platform 層側だった。1 フレームの中で `sync_ime_proxy` が先に proxy を片づけ、
  `ios_hide_keyboard` が `is_active` で見ると誰も持っていないことになり、
  本命の `endEditing(true)` に届かなかった。`Host::keyboard_held` で
  「誰が持っていたか」を持つようにした。**未実機確認**。欄外タップとあわせて確認してほしい
- iOS のログイン後クラッシュ (2026-10-01 新規): ログインしてからチャット画面へ入ると落ちる。
  今回直した「高さ 0 のメッセージ一覧」が主犯の可能性があるが、**まだ確かめていない**。
  修正版で同じ手順をやり直すこと。まだ落ちたら `logs/` の実機ログが要る。
  `panic = "abort"` なので Rust の backtrace は出ない

---

## v0.1.0 — M1 完了

検証のみ。新規実装は含めない。vision の成功判定 5 項目を 5 環境で確認し、
記録を残す。初のマイナーに相応しいのはこの記録である。

---

## v0.2.0 — UI・設定・i18n

- 現 UI の棚卸し (どこがひどいかの列挙を版の最初に行い、完了条件を固定する)
- 全画面の見直し。機能追加は含めない
- 設定の完成: フレームレート上限・メモリ予算・モーション軽減・
  HW アクセラ切替・起動／閉じる動作・フォント設定・画像表示・
  開発者モード・更新確認・言語設定。保存は `settings.json` (原子化)
- i18n 基盤＋既存文言の全移行 (ADR-0010 どおり)。移行後は新規文言の直書き禁止
- 公式テーマ語彙の確定。フォント family は同梱のみ受理に変える
- 自動更新の適用とテーマエディターは後続版へ
- 送信キーの切替設定: Enter 送信／Shift+Enter 送信／Ctrl+Enter 送信の3択と、
  選ばれなかった側の改行対応。現在は Enter 送信＋Shift+Enter 改行で固定
  (`FR-024`)。保存は `settings.json` に相乗りし、設定画面の分類追加が要る
  (`spec/03-uitree.md` は分類を増やさない方針のため、仕様の更新と一緒に行う)

---

## v0.3.0 以降

[`spec/07-roadmap.md`](spec/07-roadmap.md) の v0.3.0〜v1.2.0 を見ること。
要点だけ抜き出す:

- v0.3.0 会話の深さ (スレッド／フォーラム／リアクション／メンバー一覧／添付送信)
- v0.4.0 操作 (コンポーネント／スラッシュ／検索)。検索⇔暗号化の順序決定つき
- v0.5.0 外部接続 (通知／Gateway イベント／音声 one-shot／fetch)
- v0.6.0 信頼＋拡張の深さ (複数アカウント／オフライン／復帰／暗号化＋
  レイアウト上書き等。Inspector 先行可)
- v1.0.0 M2 完了＝実用宣言。v1.1.0 M3 音声。v1.2.0〜 M4

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
  (`cd72d6f` の ✕ がこれで見つかった)。
  2026-10-01: `grow` の子を外側に足すだけで、親の余りを全部持っていかれることがある
  (メッセージ一覧が 0px になった)。外包りの行には **伸びない** 身分を足すこと
- Windows Vulkan は機種依存で死ぬことがある (HD 520 + igvk64.dll
  31.0.101.2115 で確認)。1機種の証拠で切らず、プローブ除外に任せる。
  別機種 (NVIDIA GTX 1660 Ti) では正常動作を確認済み。
  `WGPU_BACKEND=vulkan` で試せる
