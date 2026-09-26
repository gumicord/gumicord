# モバイル不具合の修正計画

| | |
|---|---|
| ステータス | **ドラフト** |
| 起票日 | 2026-09-26 |
| 関連要件 | `FR-001`, `FR-002`, `FR-020`, `NFR-003`, `NFR-007`, `PLT-008`, `PLT-040`〜`PLT-043`, `PLT-046`, `SEC-011`, `SEC-022`, `SEC-023`, `EXT-044`〜`EXT-046` |
| 関連 | [ADR-0007](adr/0007-login-paths-and-captcha.md)、[ADR-0011](adr/0011-mobile-input-bridges.md)、[ADR-0013](adr/0013-login-autofill-proxies.md)、[ADR-0014](adr/0014-mobile-gestures-and-splash.md) |

iOS / iPad 実機で報告された 12 件の不具合・改善要望を束ねた修正計画である。
版の乗り物は `07-roadmap.md` に従い、内容は v0.0.4（モバイルの殻）・v0.0.5（モバイル入力）・
v0.2.0（UI 棚卸し）へ振り分ける。v0.0.3 は版上げ済みのため本計画の置き場にはしない。

進め方は `spec/README.md` の「仕様が先」に従う。各束の実装前に本書の該当節を確定させ、
コミットメッセージで要件 ID を参照する。要件 ID をコードのコメントには書かない。

## 束 A — ログイン致命（iOS / iPad）

### A1. メール欄の自動入力がパスワード欄まで埋まらない

現状は [ADR-0013](adr/0013-login-autofill-proxies.md) どおり不可視 `UITextField` 双子を
貼っているが、`proxy.poll()` が 1 回 1 イベントしか返さず、呼び出し側
（`render/platform/src/window.rs` の `sync_ime_proxy`）も 1 件だけ消費して
`Submitted` なら即 `submit_login()` する。ペア補完の 2 件目が届く前に送信が走り、
空または古いパスワードでログインしたように見える。

決め：両欄が安定するまで送信しない。1 ティックで両欄の差分を drains し、
`Submitted` があっても両方の文書へ反映してから一度だけ送信する。
配置矩形が未確定の欄は poll 対象から外す（`place` の `None` 維持）。

実装済み：`render/platform/src/window.rs` の `sync_ime_proxy` が差分を
drains してから一度だけ送信する。配置未確定欄は対象外のまま。

### A2. 二段階認証で固まる・モバイルの captcha 未実装

二段階認証のアカウントでログイン画面が固まるのは、`mfa_totp` 経路に
captcha の再試行が無いためである。`POST /auth/mfa/totp` が
`captcha-required` を返しても汎用エラーとして TOTP 再要求に戻り、
`CaptchaNeeded` を出さないのでモーダルが開かず無限に認証コードを聞き続ける。
`await_totp` も `Captcha` 指令を捨てる。

決め：

- `mfa_totp` に `captcha` 引数を足し、他経路と同じく `X-Captcha-Key` /
  `X-Captcha-Rqtoken` / `X-Captcha-Session-Id` ヘッダで再試行する。
- TOTP 待ちでも `CaptchaRequired` を受けたら `CaptchaNeeded` を出し、
  解けたら同じチケット・同じコードで再試行する（`await_captcha` で受ける。
  コード待ちの `await_totp` に未解決の challenge は無いため、そこで来た
  `Captcha` は古いものとして捨てる）。
- モバイルの `CaptchaHost` は `WKWebView`（iOS / macOS）・
  `android.webkit.WebView` で実装する。外部ブラウザへの fallback は採らない
  （[ADR-0007](adr/0007-login-paths-and-captcha.md) の「アプリの外へ飛ばさない」を維持）。

### 状態

- 実装済み：`mfa_totp` の captcha 再試行、TOTP 待ちの `CaptchaNeeded` と
  同じチケット・同じコードでの再送。各 OS の受け口は
  `render/platform/src/captcha` に置く（[ADR-0015](adr/0015-mobile-captcha-hosts.md)。
  Windows は既存の WebView2、macOS／iOS は `wry` の WKWebView、
  Linux は `wry` の WebKitGTK（X11 は子、Wayland は自前のトップレベル窓。
  真の子モーダルにはならないがアプリ内には留まる）、
  Android は JNI の `WebView`＋`CaptchaBridge.kt`）。頁と応答の解釈は共通化し、
  取り出し口だけ OS ごとに変える
- 要実機確認：当地では Windows の host 試験と iOS／macOS／Android の
  target 検査までを通した。Linux の compile と各 OS の振る舞い
  （頁が出て解けること、キャンセルで破綻しないこと）は実機・CI で確かめる。
  `NEXT.md` に積む

**Discord に向けるすべての HTTP リクエストには captcha が来うる。**
`POST /auth/login` と QR のチケット交換だけでなく、`POST /auth/mfa/totp` を含む
あらゆるログイン関連リクエスト、さらにはログイン後の通常 API 呼び出しも
`captcha-required` で応答しうるものとして扱う。したがって captcha の解決と
再試行の仕組みはログイン専用ではなく REST 層の共通路に置き、各呼び出しは
「challenged されたら解いて同じ内容を 1 度だけ送り直す」ことを前提とする。
単発の使い捨て内容（QR チケット等）は再送せず出し直す。

## 束 B — メッセージ・スクロール・性能

### B0. 前提：公式 Discord desktop のスクロール挙動調査

desktop の wheel 慣性を廃止するか軽い平滑化に留めるかは、公式 Discord desktop の
JS を読んでから決める。結果は [ADR-0014](adr/0014-mobile-gestures-and-splash.md) の
追補か新規 ADR に残す。`touch::Fling` は現状 desktop / mobile 共通路のため、
調査後は `is_mobile()` で物理を分離する（desktop は `scroll_by` のみ、
mobile は `Fling`＋`hold_fling`＋`VelocityTracker` を維持）。

調査済み・実装済み：公式 desktop は Chromium ネイティブの wheel 物理であり
（`--disable-smooth-scrolling` が効く）、自前慣性の寄せ先は無い。
`render/platform/src/window.rs` の `is_mobile()` で分離し、desktop は
wheel 噴き・touch 解放の惰性を作らない。mobile は従来どおり。
詳細は [ADR-0014](adr/0014-mobile-gestures-and-splash.md) の B0 追補に残す。

### B1. 古いメッセージ読み込みで位置が飛び暴走する

`at <= 400` で発火し、補正は `overflow_after - overflow_before` の加算のみ。
前フレーム基準の overflow・`take_prepended` 合体時の 2 回目無補正・
`loading_row` の高さ変動・`hold_fling` 3 秒維持が重なり `at ≈ 0` に残留して
即 `load_older` が再発火する。

決め：可視先頭の `message.id + offset` を基準とする内容アンカーへ移行し
（Discord と同じく読み込み前後で見た目位置を保つ）、合体した prepend は
合体分だけ補正する。`FR-020` の「上方向への無限スクロール」の位置保持の
定義を内容アンカーと読み替える（`07-roadmap.md` C9 の 400px / 50 件は維持）。

実装済み：可視先頭行の `message.id` と見た目上の差分を基準に
（`render/render` の `anchor_row`／`anchor_scroll`）、行の高さを使わず
位置を保つ。行が消えていた場合のみ従来の溢れ差分に倒す。
合体した prepend も行基準のため補正は要らない。
`FR-020` の定義は内容アンカーに読み替えた。

### B2. 1 万ノードで 60fps を割る

仮想化なし（`NFR-007` は M2）に毎フレーム全行構築＋ Markdown 再 parse が
乗っている。まずは B1 の暴走停止（ノード数の頭打ち）＋ parse 結果の
id 単位キャッシュ（内容付きで覚え、編集は外して解き直す）で止血する。
prepend 時の二重 layout は稀なフレームだけのため残す。10 万件対応の本格的な
仮想化は M2 の作業とし、`03-uitree.md` §7 の V1〜V3 制約をそのときに確定させる。

止血済み：B1 の暴走停止でノード数は頭打ち、本文 parse は内容付きの
id 単位キャッシュ（編集で解き直す。試験で固定）。本格仮想化は M2 のまま。

### B3. 改行メッセージがアイコン下に潜る

原因はテーマの規則の重なりである。`chat.message` の狭幅規則
（`maxWidth: 600`、文末）が `state: grouped` の字下げ規則より後に
書かれているため、狭い画面では後に勝つ方式で grouped の字下げが
消え、追従文がアバター欄の下に潜る。レイアウト自体の二段階配分は
狭幅回帰試験で正常を確認した。

決め：狭幅 grouped 規則（`state: grouped`＋`maxWidth: 600`、
`padding [0, 8, 0, 56]`＝側 8＋像 40＋間 8）を両サンプルテーマに足し、
解決値の回帰試験と狭幅行配置の試験で固定する。字下げ自体は
テーマの仕事のまま（`message()` の方針を維持し、木の形は変えない）。

実装済み：両サンプルテーマに狭幅 grouped 規則を足し、解決値試験
（`grouped_messages_keep_their_indent_on_narrow_screens`）で固定。

## 束 C — モバイルシェル UI

### C1. iOS 設定の高負荷・勝手に閉じる

行間タップが即 `close_settings()` になる判定（空隙＝外側扱い）と、
Touch `Moved` 毎の全木再構築が原因。実装済み：行外タップの即閉じをやめ、
閉じる行と Esc だけが閉じる（`03-uitree.md` の settings.screen 表を更新）。
残り：設定 build の差分化など全木再構築の軽減は v0.2.0 の UI 棚卸しと一緒に
行う（毎フレーム全木再構築は全体の設計であり設定だけ切り離せない）。

### C2. 狭幅 drawer の dim 裏抜け・透明域応答

drawer / sheet に `OverlayLayer` / `OverlayScrim` がなく hit 吸収が無い上に、
`overlay_press` が矩形でなく ID で判定するため裏のボタン類が内容扱いされる。
実装済み：drawer / sheet の開いている間に `OverlayScrim`（`slot: dim`）を
兄弟として敷き、内外判定を矩形優先に変えた（面内のみ `press_loop` へ、
面外は後ろへ抜かず閉じるだけ、面内の隙間は何もしない）。
`03-uitree.md` の drawer / sheet 表に scrim 要件を追記済み。

### C3. スマホ設定の配置

`Panes::One` では分類一覧と中身を別画面にする（Discord mobile 式）。
実装済み：`SettingsView.narrow_page` で分類／中身を切り替え、分類選択で
掘り下げ、先頭の「← 設定」で戻る（`Action::SettingsNarrowBack` 追加）。
`03-uitree.md:370-372` の 2 欄固定に狭幅例外を付け済み。
本格的な見直しは v0.2.0 の UI 棚卸しと一緒に行う。

### C4. sheet / メニューの swipe 対応

`swiped()` が floating / settings で早期 return し、handle ドラッグが未配線の
ため tap 閉じのみになっている。実装済み：棚・面の内側からの横・下払いで閉じる、
上払いは何もしない、外側からの払いは後ろへ抜かず閉じるだけ、メニューの面の
下払いで閉じる、確かめの窓は払いで閉じない（`03-uitree.md` X1 に追記済み）。
掴みしろのドラッグ追従も実装済み：面の把手の掴みしろを掴んだ指に面が付いてきて、
離すと速さと半分で閉じるか戻る。払い確定は tap を出さない（`touch::Tracker::release`
は一つの verdict のみを返す。試験で固定）。

### C5. swipe 開閉のアニメーション

実装済み（棚と面）。方針は `transition` の流用ではなく、面の進み具合を
持つ別仕組みである：

- 配置は進み具合で面をずらす（`render/render` の `SlideState` と
  `layout_slid`。形と寸法は不変のため計測と描画は一致する。
  棚は横、面は縦にずらす）
- 開く・閉じるは 220ms の滑走（端を開く・選ぶ・外側・Esc・払いのいずれも）。
  閉じる旗は着地で倒れるため、閉じ途中に開き直すと引き返す
- 端からの右払いは指に付いてくる。開いた棚の中を左へ払うと閉じ方向に
  付いてくる。面の把手の掴みしろを掴むと下へ付いてくる。
  離すと速さと半分で開閉を決める
- 面（member sheet・メニュー面）は同じ進み具合を共有する。
  メニューを開くと一員面は即座に閉じるため、滑走路に面は一つだけ載る。
  選ぶ操作は滑走せず即座に消える。確かめの窓は滑走しない
- 閉じ途中の面は木に残るため、着地までスクリーンリーダーには一瞬見える

## 束 D — テーマ・プラグイン導入

導入形式は `zip`＋`tarball`（`.tar.gz` / `.tgz` / `.tar`）で確定する。
`7z` は見送る。読むためだけに復号器を積む量が携帯に見合わない。

### 形式（`app/core/src/install.rs` が実装）

- 中身は入っている場所の写し。説明書（テーマは `theme.json`、
  プラグインは `manifest.json`）が直下か、ただ一つの上の箱の中にあること
- 危険な道（絶対・`..`・Windows の持分）は止める。tar の繋ぎ類も入れない
- 上限は 5000 件・合計 256MB・1 件 64MB。超えたら止める
- 説明書は読んで確かめる。壊れていたら置き場に届く前に止める
- 同じ ID がすでに入っていたら上書きせず止める

### 状態

- 実装済み：安全な展開と検証、desktop の `rfd` 呼び出し、設定画面の
  導入行（テーマ欄・プラグイン欄の末尾）、導入後の再読み込み。
  プラグインは再走査ののち、権限は従来の許可の対話を通る
  （黙って許さない）。携帯では選び口が無い旨を toast で言う
- 未着手：Android の SAF・iOS の文書選び。どちらも OS との往復が要る
  （Android は activity の返し、iOS は選びの委任）ため、Rust のみでの
  穴埋めはしない。Android は「自前の Java/Kotlin を持たない」決定を
  覆すことになるため、やるなら ADR を起票して決める。
  穴が埋まれば、bytes を渡す口はすでに共通のため UI 側の手直しは要らない
- 形式の置き場所は将来 `04-theme.md` §5.4 と `05-plugin-api.md` §4.2 に
  移す。registry（`EXT-044`）と一緒に決める
