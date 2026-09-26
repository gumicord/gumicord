# ADR-0015: 残りの captcha ホスト (macOS / Linux / iOS / Android)

| | |
|---|---|
| ステータス | **承認** |
| 起票日 | 2026-09-26 |
| 関連要件 | `FR-001`, `FR-002`, `SEC-002`, `PLT-008` |
| 前提となる決定 | [ADR-0007](0007-login-paths-and-captcha.md) (captcha は OS の webview に出す)、`app/android/README.md` (自前の Java/Kotlin を持たない) |
| 関連仕様 | [12-mobile-hotfix-plan.md](../12-mobile-hotfix-plan.md) 束 A (A2) |

---

## 背景

`CaptchaHost` の実装は Windows の WebView2 のみである。
macOS / Linux / iOS / Android では `Unsupported` を返し、
captcha を要求されたログインはその場で捨てられる。
`spec/12` の A2 は出し方だけ決めており、手段の決定が残っている。

## 決定

`CaptchaHost::solve` の同期署名は変えない。
各 OS のモーダルは呼び出し側スレッド (winit のイベントスレッド) で
内側の待ち受けを回し、終わったら返す。Windows の実装と同型である。

| OS | 手段 |
|---|---|
| Windows | 現状維持 (WebView2＋子窓＋内側メッセージポンプ) |
| macOS | `wry` (WKWebView) を子として出し、`CFRunLoop` の内側待ち受けで回す |
| Linux | `wry` (WebKitGTK)。X11 では子として出し、Wayland では自前のトップレベル窓に `build_gtk` で出す（真の子モーダルにはならないがアプリ内には留まる。配置はコンポジタ任せ）。待ち受けは `gtk` の反復で回す |
| iOS | `wry` (WKWebView) を全画面で出し、`CFRunLoop` の内側待ち受けで回す |
| Android | 自前の `WebView` (JNI 生成)＋小さな Kotlin 受け口 (`@JavascriptInterface`)。待ち受けは `MessageQueue.next()` と `dispatchMessage` の手回しで回し、activity の継承は変えない |

チャレンジ頁と `type:payload` の解釈は全 OS で共通化し、
取り出し口だけ変える (`window.ipc` と受け口呼び出し)。

## なぜ Android に wry を使わないか

wry の Android 対応は生成 Kotlin と `WryActivity` 継承を要求する。
主 activity は `GameActivity` の直接指定であり、wry 用の継承に替えると
winit の activity 経路と衝突する。自前の受け口 1 枚 (Kotlin 数十行) と
既存 API だけの JNI 生成のほうが、触る面が小さい。

## 「自前の Java/Kotlin を持たない」の扱い

`app/android/README.md` の決定を、captcha の受け口 1 枚に限って覆す。
Kotlin 側は受け口の宣言だけを持ち、判断はすべて Rust 側に残す。
Gradle には Kotlin 導入版を pin して足す。

## Swift を足さない理由

iOS / macOS は `wry` が WKWebView を賄うため、自前の Swift は要らない。
利用者の了承 (自前 Kotlin 可・Kotlin 優先) は Android の受け口にのみ使う。

## 引き受けるリスク

| リスク | 対処 |
|---|---|
| 内側待ち受けの再入 (特に入力・描画イベント) | モーダル中は captcha 頁だけが応答すればよく、後ろの滞留は復帰後に捌く |
| Linux で WebKitGTK のパッケージが要る | デスクトップで唯一の実行時依存になる。CI に `libwebkit2gtk-4.1-dev` を足す |
| Wayland で真の子モーダルにできない | 自前のトップレベル窓で出す。配置はコンポジタ任せだがアプリの外へは飛ばさない |
| 端末の WebView 不在・無効 (Android) | 生成失敗は `Open` として諦める |
| Windows 機では Linux の compile 検査ができない | iOS／macOS／Android 検査は当地で回し、Linux は CI と実機確認に委ねる (`NEXT.md` に積む) |

## 却下した選択肢

| | なぜ却下したか |
|---|---|
| 既定ブラウザで開く | [ADR-0007](0007-login-paths-and-captcha.md) で体験劣化として却下済み |
| `objc2` / 素 JNI の全面自作 (Apple / Android) | `wry` (Apple)・受け口 1 枚 (Android) で足りる重複である |
| `solve` の非同期化 | desktop 実装と呼び出し側 (`pump_captcha`) の作り替えになる。内側待ち受けで足りる |
