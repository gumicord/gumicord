# ADR-0008: Windows スナップレイアウトは親 HWND のサブクラス化で出す

| | |
|---|---|
| ステータス | **調査完了** |
| 起票日 | 2026-08-27 |
| 関連要件 | `PLT-022` (M2) |
| 置き換える決定 | なし (新規) |

---

## 背景

`PLT-022` は「Windows のスナップレイアウト (最大化ボタンのホバー) に対応しなければ
ならない」。Gumicord は `with_decorations(false)` で OS のタイトルバーを使わず、自前の
タイトルバーをクライアント領域に描いている。OS のスナップレイアウトは、**OS 標準の
最大化ボタンにホバーしたときだけ表示される**。自前ボタンには出ない。

この調査で、実装可否と方式を固める (ロードマップ P6)。

## 何が起きたか / 何を調べたか

まず境界を整理する。「スナップ」には二つの意味があり、扱いが違う。

| 挙動 | Windows 11 | Windows 10 | Gumicord での対応 |
|---|---|---|---|
| 端へのドラッグ / Win+矢印で貼り付け | ✅ | ✅ | **既に動く** (`winit` が `WS_THICKFRAME` を残すため) |
| 最大化ボタンのホバーで出るスナップレイアウト (フライアウト) | ✅ | 機能自体が無い | **出ない**。`HTMAXBUTTON` が要る |

フライアウトは、ウィンドウが `WM_NCHITTEST` に `HTMAXBUTTON` を返したときだけ
表示される (Microsoft 公式ドキュメント)。`decorations(false)` の自前タイトルバーでは
`winit` が `HTMAXBUTTON` を返せない (winit issue #3884, 未解決)。

ただし winit でできないのは「`winit` の API 経由で返すこと」だけであり、**ウィンドウの
HWND を直接サブクラス化して `WM_NCHITTEST` を傍受し、最大化ボタン矩形で `HTMAXBUTTON`
を返す**のは可能である。Windows Terminal や VS Code と同じ方式で、エコシステムが
収束した解法でもある。

### 重要な違い: Tauri / WebView 版との差異

Tauri では WebView2 の**子 HWND** がクライアント全体のヒットテストを奪うため、
親 HWND のサブクラス化だけでは効かず、透明な子ウィンドウを最大化ボタンの上に重ねる
必要がある。**Gumicord は自前レンダラなので子ウィンドウは無い。** クライアント領域の
ヒット判定は自前 (`Zone::Control("maximize")`) で行っている。したがって**親 HWND を
サブクラス化して `WM_NCHITTEST` で `HTMAXBUTTON` を返すだけで済む**。

### 同時に見つかったバグ: 最大化時の閉じるボタン

最大化時に閉じるボタンを押すと、**後ろにあるウィンドウの閉じるボタンも押されて
しまう**ことが観測された。押下 (`MouseInput::Pressed`) の時点で `event_loop.exit()`
すると、Windows がリリースを押下時点の座標でウィンドウの後ろのウィンドウへ配り直し、
そこも押したことになる。最大化中は座標が画面端に揃うため、症状が顕在化しやすい。

→ **閉じに限らず、タイトルバーの制御ボタン (最小化・最大化・閉じる) すべてを
リリース時に遅延**させる修正を適用済み (押下で `control_pending` にスロットを立て、
リリースで実行)。フォーカスが移る最小化や、座標の変わる最大化でも、押下時即実行だと
リリースが後ろのウィンドウや別の座標に落ちるため、同じ理由でリリースに揃えた。
これは実装であり、M2 を待たずに済んだ。

## 決定

M2 (`PLT-022`) で実装するときは、次の方式を採る。

1. winit の `Window` から生の `HWND` を取り出す
   (`raw-window-handle` 経由。`WindowExtWindows::hwnd()` は winit の非安定なので
   `raw_window_handle()` を使う)。
2. `SetWindowLongPtrW(GWLP_WNDPROC)` で自前の WndProc を差し込み、元の WndProc
   へのポインタを保持する (サブクラス化)。
3. `WM_NCHITTEST` で、まず `DefWindowProcW` を呼んで既定の結果を得る。それが
   `HTCLIENT` のときだけ、座標が最大化ボタン矩形 (`ChromeTitlebarControl` /
   slot `"maximize"`) の上かどうかを自前で判定し、`HTMAXBUTTON` に上書きする。
4. クリック・ホバーはこれまでの自前判定 (`zone_at`) のまま `set_maximized` で
   処理する。スナップレイアウトは OS 側が担当する。
5. サブクラス化はウィンドウ生成後 1 回だけ。HWND が変わったら (再生成) 付け直す。

### 使う crates

- 既に `windows-sys` (同じバージョン) を使っている。
- 必要機能: `Win32_UI_WindowsAndMessaging` (`WM_NCHITTEST`, `HTCLIENT`,
  `HTMAXBUTTON`, `SetWindowLongPtrW`, `GWLP_WNDPROC`), `Win32_UI_Input`
  (`GetMessagePos` で画面座標を取る場合)。

### 実装時の注意

- `x = 0x84` (`WM_NCHITTEST`) の `lParam` は**画面座標**。クライアント座標へは
  `ScreenToClient` で変換してから自前の矩形判定をすること。既存の `zone_at` は
  クライアント座標を使うので、座標系を揃える。
- 最大化ボタン矩形はテーマが動かせるため (`ChromeTitlebarControl` のスロット)、
  ピクセル定数でなく**当該ノードの描かれた矩形**から取る。
- ホバー時に `HTMAXBUTTON` を返しても、Gumicord は `WM_MOUSEMOVE` をクライアント
  から受け取れなくなる可能性がある (OS が非クライアント扱いするため)。ホバー描画と
  クリックは、`WM_NCHITTEST` を `HTMAXBUTTON` と返した分だけ、フック側で
  補完してアプリへ通知する必要が出るかもしれない。**実機で確認すること。**

## 代替案 (却下)

| 案 | 却下理由 |
|---|---|
| 拡張フレーム (`WM_NCCALCSIZE` + `DwmExtendFrameIntoClientArea`) | OS の最大化ボタンが残り、テーマで塗り替えられない。自前制御ボタン (要件 2) と両立しない |
| 透明子ウィンドウを重ねる | WebView 系の話。Gumicord には子ウィンドウが無いので不要 |
| シミュレート (自前ピッカー / `Win+Z` 送出) | OS のゾーンピッカーを再現できず、将来の Windows の挙動に追従しない |

## 影響

- `PLT-022` は M2 要件なので、M1.1 のブロッカーにはならない。
- 実装対象は `render/platform` の Windows 側だけ。他のプラットフォームに影響しない。
- winit が将来 `HTMAXBUTTON` を返せるようになったら、自前サブクラス化は撤去できる
  (winit #3884 を追う)。
