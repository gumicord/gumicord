# ADR-0017: Android のログイン自動入力は双子 `EditText` で出す

| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-10-17 |
| 決定日 | 2026-10-17 |
| 関連要件 | `FR-001`, `PLT-040` |
| 関連 | [ADR-0011](0011-mobile-input-bridges.md) (ログイン 2 欄だけ差し替え)、[ADR-0013](0013-login-autofill-proxies.md) (iOS の双子。Android 版はこれの `EditText` 版)、[ADR-0016](0016-kotlin-receiving-points.md) (受け口)、`spec/12-mobile-hotfix-plan.md` 束 A (A1)・束 B (B5) |

---

## 背景

Android の入力接続は GameActivity の `InputEnabledSurfaceView` が握っている。これは
`SurfaceView` であり、autofill の型は `AUTOFILL_TYPE_NONE` を返す。**fillable な
view が窓に無ければ、パスワード管理側は候補も保存も出さない**。`autofillHints` を
足しても、編集できない view では話にならない。

[ADR-0013](0013-login-autofill-proxies.md) の双子は `UITextField` 前提で、Android の
版は無い。利用者に案を問うた結果、次の 2 点が了承された: **双子にフォーカスを移す**
こと (案 B)、**可視のログイン欄の矩形にそのまま重ねて置く**こと。

AOSP と games-activity のソースで次を確かめた:

- 自動入力の成立条件は「editable な `EditText` が窓内に `username` と `password`
  の両ヒントで存在する」ことだけ。ペア補完にフォーカスは要らない。
- `AutofillManager.requestAutofill()` は非フォーカスでも成立する
  (`FLAG_VIEW_NOT_FOCUSED` を用意している) が、候補 UI が確実に出るのは
  フォーカスを取る側。
- 非フォーカス・非クリックの `EditText` はタッチを消費しない
  (`TextView.onTouchEvent` の収束条件より)。可視欄に重ねても打鍵は SurfaceView へ素通りする。
- `android_main` は Java main thread ではない。ビューの生成・配置・フォーカスは
  [ADR-0016](0016-kotlin-receiving-points.md) の受け口経由にするのが正。
- キーボードの付け替えは戻せる。`InputConnection.setSoftKeyboardActive(true)` は
  `setFocusableInTouchMode(true)` から始まっているので、SurfaceView は再び
  フォーカスを取れる。

## 決定

| | |
|---|---|
| 対象 | ログインのメール欄・パスワード欄だけ (iOS と同じ線引き)。2FA・作曲欄は GameTextInput のまま |
| 仕掛け | content view に `EditText` 双子。メール欄は `AUTOFILL_HINT_USERNAME`、パスワード欄は `TYPE_TEXT_VARIATION_PASSWORD` + `AUTOFILL_HINT_PASSWORD`。**両方を同時に窓に置く**のでペア補完が成立する |
| 駆動 | 欄がアクティブな間だけ双子にフォーカスと IME を移す。テキストは 16ms タイマーのポーリングで文書へ渡す ([ADR-0011](0011-mobile-input-bridges.md) のペース)。独自フレームは回さない |
| 送信 | 双子の `OnEditorActionListener` (Kotlin) が Next / Done を積み、Rust がポーリングで拾って `ime_newline` に渡す。末尾改行のハックはログイン 2 欄では使わない |
| 破棄 | 欄を離れたそのイベントで blur と IME を隠す。双子の文字列も落とす。古い fill が文書へ再生されるのを防ぐため |
| 配置 | 双子は可視のログイン欄の矩形にそのまま重ねる。dp への変換は受け口側、位置決めは UI thread |
| タッチ | 非アクティブな双子は `focusable=false` / `clickable=false` にしてタッチを素通りさせる。触れるのはアクティブな間だけ (触れない欄には候補が出ない、iOS と同じ理由) |
| winit IME | Android では winit の on/off 切替を双子の代わりに動かさない。1 フレームの中で双子の show が先に届き、後から届く hide がキーボードを殺すため。キーボードは受け口が握る |
| 受け口 | [ADR-0016](0016-kotlin-receiving-points.md) の形。`LoginEditor.kt` が生成・配置・フォーカス・IME・リスナ・文字列の読み出しだけを持ち、判断はすべて Rust 側 |

## 却下した選択肢

| | なぜ却下したか |
|---|---|
| 非フォーカス + `requestAutofill` | framework は用意しているが、候補 UI が確実に出る保証が無い。`FLAG_MANUAL_REQUEST` の経路は保険であって正路ではない。出なければ作り直しになる |
| 双子を 1px の細帯に停める (iOS の当初の形) | ドロップダウンの出る位置が欄から離れる。Android の候補は入力している view の矩形に寄る |
| GameTextInput のまま `InputConnection` を自作 | [ADR-0011](0011-mobile-input-bridges.md) で触る面が大きいと却下済み。フォーカスのある `EditText` は OS 標準の変換をそのまま受け取れる |
| 双子に常時フォーカスを置く | ログイン画面の外で打鍵が双子へ流れ、作曲欄などが書けなくなる。アクティブな間だけが正しい |

## 引き受けるリスク

| リスク | 対処 |
|---|---|
| 双子 → GameTextInput の付け替えでキーボードが戻らない | `setSoftKeyboardActive(true)` が `setFocusableInTouchMode` から始めるので戻る見込み。実機で 2FA・作曲欄まで確認する |
| 双子の文字列と文書の二重管理 | `last` ベースの差分化は iOS と同じ形。blur で双子側を空にし、再生を防ぐ |
| Kotlin の受け口が肥大化する | 行数を `app/android/README.md` に載せる。委譲以外を書き始めたら止める |
| Proguard が JNI 参照を剥がす | `minifyEnabled` が入るときだけ、既存の `-keep` 方針に合わせる |

## 見直し条件

- ログイン 2 欄以外で自動入力が要ると分かったとき (2FA・作曲欄) → 双子に足すか
  [ADR-0011](0011-mobile-input-bridges.md) の GameTextInput に残すかを ADR で決める
- winit の IME 切替と双子の競合が別解で解けると分かったとき → この ADR の
  「winit IME」の行を見直す

## 参考

- `app/android/app/src/main/kotlin/dev/gumicord/app/LoginEditor.kt`
- `render/platform/src/android_proxy.rs`
- AOSP `InputMethodManager` (`showSoftInput` は served view でなければ通らない)、
  `TextView` / `View` (autofill の成立条件とタッチの収束条件)
