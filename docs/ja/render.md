# レンダラ

プラットフォーム非依存の描画本体 (`render/render`) と OS 統合層
(`render/platform`)。関連仕様: [`spec/06-renderer.md`](../../spec/06-renderer.md)。

# gumicord-render (`render/render`)

> UITree を受け取り GPU 描画コマンドを発行する。OS 固有処理は含まない。

## Files

### `src/lib.rs` — UITree を 1 フレーム描画する `Renderer` の本体。スクロール・ヒットテスト・リンク／スポイラー press 解決・画像の欠落収集まで担う。Key items: `Renderer`、`Renderer::new()`、`Renderer::headless()`、`Renderer::render()`、`FrameStats`、`Hit`。前フレームの配置に対するヒットテストであり、スクリーンショット用のヘッドレス描画と読戻し (`Gpu::read_pixels`) も持つ。

### `src/gpu.rs` — wgpu の初期化とサブミット。矩形・テキストの 2 パイプライン、表面／ヘッドレス出力、差分アップロード、バックエンド候補の順序選択を持つ。Key items: `Gpu`、`GpuError`、`Gpu::new()`、`Gpu::headless()`、`Gpu::submit()`。Windows は GL を先に試し、候補は `probe` の子プロセスで事前検証する。ヘッドレスはフォールバックアダプタ優先で決定的な描画にする。

### `src/shader.wgsl` — 矩形・テキスト共用の手書きシェーダ。SDF 角丸矩形とテクスチャ付きクアッド。コンピュートシェーダ不使用 (GLES 対応のため)。

### `src/text.rs` — テキスト整形とグリフアトラス。cosmic-text による整形と、グリフを上から・画像を下から詰める shelf-packed な RGBA8 アトラスを同居させる。Key items: `TextEngine`、`Shaper`、`Shaper::shape_rich()`、`TextEngine::put_image()`。CJK 統合漢字の扱いのため日本語フォールバック順序を独自実装し、システムフォントは別スレッドで列挙して後から折り込む。

### `src/draw.rs` — レイアウト結果から描画コマンド列を構築する唯一の論理→物理ピクセル変換点。背景・テキスト・アイコン・画像・QR を順に積み、パイプラインとシザー毎に run 結合する。Key items: `DrawList`、`Run`、`RunKind`、`build()`。デプスバッファはなく描画順が重なり順であり、欠落した画像は描画せず報告する。

### `src/layout.rs` — 制約を下ろしサイズを返すレイアウト。Row／Column／Stack の 3 軸のみ。Key items: `layout()`、`LayoutResult`、`Placed`、`ScrollState`。末尾固定リストは位置ではなく意図で記憶し、スクロール子はクリップされヒットしなくなる。

### `src/geom.rs` — 論理ピクセル専用の幾何型。物理変換は描画直前に一度だけ行う。Key items: `Rect`、`Size`、`Rect::contains()`、`Rect::intersect()`。`contains` は終端辺を含まない。

### `src/backgrounds.rs` — テーマ背景画像用の 1 画像 1 テクスチャ管理。アトラスに入らず、CPU で事前生成したミップチェーン付きでアップロードする。Key items: `Backgrounds`、`Backgrounds::put()`、`mip_levels()`。テーマ切替時は `clear()` で旧テーマの絵を忘れる。

### `src/font_cache.rs` — システムフォント列挙のディスクキャッシュ。ファイル同一性で検証し、未証明分だけ再パースする。Key items: `Stats`、`populate()`。診断用に CJK 系ファミリ数も数える。

### `src/icon.rs` — フォントではなくテクスチャとして描くアイコン定義集。単位正方形上のポリラインを要求サイズでラスタライズしグリフアトラスに載せる。Key items: `IconDef`、`ICONS`、`lookup()`。未知名はエラーにせず描画しない。

### `src/intrinsic.rs` — 安定 ID 毎の既定レイアウト表。テーマが書かない幅・軸・スクロール可否などはここが決め、テーマの明示値が勝つ。拡張 ABI の一部ではなく、変更は見た目のみ変える。

### `src/motion.rs` — 解決済みスタイル値を目標へ時間駆動で近づけるアニメーション。初見ノードは動かさず、未使用トラックは破棄する。Key items: `Motion`、`Motion::new()`、`Motion::apply()`。

### `src/probe.rs` — GPU バックエンドの子プロセス検証。壊れたドライバがインスタンス生成時にプロセスごと落とすため、同一バイナリを `--probe-gpu=<backend>` で起動し応答したものだけ残す。Key items: `surviving_backends()`、`run_probe()`。

# gumicord-platform (`render/platform`)

> OS に触れる処理を集めた統合層。窓・入力・IME・クリップボード・秘密保管・URL 起動・captcha 等を持ち、描画本体は `gumicord-render` に委ねる。

## Files

### `src/lib.rs` — 入口と再エクスポート、パニックフックとファイルロガー。Key items: `install_panic_hook()`、`init_file_logging()`、`write_diag_file()`、`Application`、`Waker`。IME 候補位置は入力欄全体を渡す。

### `src/window.rs` — 装飾なしウィンドウとオンデマンド再描画のイベントループ。タイトルバー領域のドラッグ移動、縁のリサイズ、制御ボタンの press/release 分離、スクロールバー掴み、リンク／スポイラー優先の press 解決、IME・キー入力配送、点滅・次フレーム期限による待機制御を持つ。Key items: `Application`、`Waker`、`FrameCx`、`PlatformError`、`run()`、`RevealRequest`、`ImeProxy`。最大化状態は保持せず都度問い合わせ、描画直前に実サイズへリサイズし直す。モバイルでは窓寸法を OS 任せにし、タイトルバーは出さない。

### `src/text_input/mod.rs` — テキスト入力の OS 非依存インターフェース。編集キー・隠しキー・クリップボード操作を OS 型なしで定義する。Key items: `TextInputHost`、`EditKey`、`HiddenKey`、`ClipboardOp`、`TextDocument`。`Enter`／`Escape` は文書でなく呼び出し側が扱う。

### `src/text_input/document.rs` — 編集中テキストの実体。全位置は UTF-8 バイトオフセット、キャレット移動は書記素単位。Key items: `TextDocument`、`insert()`、`set_composition()`、`selection()`、`take()`。未確定の変換中範囲を保持し、確定・取消を区別する。

### `src/touch.rs` — 生タッチ点列からの純粋なジェスチャ認識。タップ・スクロール差分・スワイプを判定し、2 本指目は無視する。Key items: `Tracker`、`TouchAction`、`Swipe`、`Tracker::press()`、`release()`。

### `src/clipboard.rs` — テキストと画像のクリップボード。Windows は Win32、Linux/macOS は `arboard`、iOS は `UIPasteboard`、Android は未実装。Key items: `ClipboardImage`、`ClipboardError`、`set_text()`、`text()`、`set_image()`、`image()`。占有時は `Busy` で失敗を隠さず、開閉は必ず対にする。

### `src/secret.rs` — OS の安全な保管庫。暗号化できない場所には平文で書かず、未対応環境は `Unsupported` で毎回ログインに戻す。Key items: `SecretStore`、`SecretError`、`store()`、`load()`、`clear()`。Windows は DPAPI、Linux/macOS は keyring、Android/iOS は未実装。

### `src/proxy.rs` — iOS パスワード自動入力用の不可視ログインフィールド (iOS のみ)。winit 側は `UIKeyInput` しか話さないため、username＋password の隠し `UITextField` 双子に fill を受け、ポーリングで文書へ戻す。Key items: `Proxy`、`ProxyEvent`、`set_active()`、`poll()`。表示編集は自前描画欄に残す。

### `src/clock.rs` — OS からの時刻情報。Key items: `local_utc_offset_minutes()`、`now_unix()`、`caret_blink_interval()`。

### `src/dirs.rs` — アプリのデータディレクトリ解決。Key items: `app_data_dir()`。`GUMICORD_DATA_DIR` (空値は無視) が最優先で、モバイルシェルが起動時に設定する。

### `src/url.rs` — URL の OS 引き渡し。Key items: `open_url()`。`http`／`https` のみ許可する。

### `src/captcha/mod.rs` — captcha 提示の抽象層。アプリは素データだけ扱い、表示は本モジュールが担う。Key items: `CaptchaChallenge`、`CaptchaHost`、`WebView2Captcha`。非 Windows の `solve()` は `Unsupported` のスタブ。

### `src/captcha/webview2.rs` — Windows 専用の WebView2 captcha ホスト。子ウィンドウで hCaptcha ページを開き IPC でトークンを受ける。
