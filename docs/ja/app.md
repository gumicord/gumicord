# アプリ

画面と状態を持つ共有ロジック (`app/core`) と、各プラットフォームの薄い殻。
関連仕様: [`spec/10-login-screen-ui.md`](../../spec/10-login-screen-ui.md)、
[`spec/11-multi-account.md`](../../spec/11-multi-account.md)。

# gumicord-app (`app/core`)

> 画面・状態・フレームパイプラインの順序を所有し、各プラットフォームから呼ばれる。

## Files

### `src/lib.rs` — アプリ状態 `Gumicord` と画面分岐・入力・テーマ選択を束ねる中心。Key items: `Gumicord::new()`、`Panes::for_width()`、`Composing`、`LoginField`、`SettingsView`、`Application` 実装 (`start`/`wake`/`pressed`/`scrolled`)。フレーム毎に時計は先頭で一度だけ読む。モバイルではタイトルバーを出さず、初回フレームの診断を `diag.log` に書く。

### `src/live.rs` — 実データの配線。`Store`・ゲートウェイ・REST・キャッシュを束ねる `Live` が中核。Key items: `LiveEvent`、`Live::without_cache()`、`open_channel()`、`extend_members()`、`send_message()`、`mark_read()`。キャッシュ先行・REST 置換・ゲートウェイ追従の順序で、遅延キャッシュが REST を上書きしない。ボットは `MESSAGE_ACK` を送らず、名簿は OP 8 で取る。購読チャンクの経路はアカウント種別で決める (index の有無ではない)。

### `src/session.rs` — ログイン状態とバックグラウンド進行。`Session::Connecting/WaitingForScan/Password/PasswordTotp/Token/Exchanging/LoggedIn/Failed` が画面の根拠。Key items: `Login::start()`、`submit_password()`、`submit_totp()`、`cancel_password()`、`poll()`。QR が既定で保存トークンを先に試し、失敗時は即破棄する。

### `src/account.rs` — 複数アカウントの保存と選択。トークンは `account_user_<id>`／`account_bot_<id>` の個別キーで OS 安全ストアに保存し、索引はトークンを含まない。Key items: `AccountsIndex::load()`、`remember()`、`load_token()`、`remove()`。`remember` が旧単一キーを掃除する。

### `src/a11y.rs` — UITree からスクリーンリーダー木への翻訳。QR ペイロードと隠しスポイラーは読み上げない。Key items: `tree_update()`。親 ID 込みの安定 ID でフレーム間の対応を保つ。

### `src/assets.rs` — テーマ背景アセットの解決・取得・復号。未承認リモートは宣言＋承認まで触らず、失敗はフォールバック色と警告で返す。Key items: `ThemeAssets::request()`、`poll_ask()`、`approve_hosts()`。

### `src/demo.rs` — レンダラー・テーマ確認用の固定ダミーデータ。日本語主体で折り返し・メンション・スポイラー・リンクの目視を含む足場。

### `src/images.rs` — アバター等の取得・復号・受け渡し。同時 6 件・最長辺 128px に抑え、画素は木に載せず `take_images()` 経由でのみ渡す。

### `src/markdown.rs` — 解析済み本文から UITree ノードへの着色。行内装飾は `Span` に畳み、見た目はテーマが決める。Key items: `Ink`、`Reveals`。

### `src/menu.rs` — メニュー・確認ダイアログ・トースト等の浮動層。不可逆操作は本文プレビュー付きダイアログを挟み、項目は索引で指名する。Key items: `Floating`、`Menu`、`Confirm`、`Action`。

### `src/time.rs` — ISO 8601 時刻の表示用変換。日付計算は自前の civil-date routines のみで行う。Key items: `parse_unix()`、`continues()`。

# app/desktop (`app/desktop`)

> デスクトップの入口。ライフサイクルとログだけを持ち、実処理は `gumicord-app` に委ねる。

## Files

### `src/main.rs` — 起動処理。プローブ子は即終了し、`gumicord_platform::run(Gumicord::new())` で共有ループに入る。`GUMICORD_LOG` は自クレートのみ既定 `info`、依存側は `GUMICORD_LOG_DEPS` で既定 `warn`。Windows ではコンソールを出さず、全行を `logs/` の実行ログと stderr の両方へ書く。CRT は静的リンクで再頒布不要。

### `Cargo.toml` — バイナリ `gumicord` の定義。

# app/android (`app/android`)

> Android の入口。GameActivity ライフサイクルとデータディレクトリ決定だけを持つ。

## Files

### `src/lib.rs` — `cdylib` の入口。`android_main()`、`data_dir()`、`init_tls_verifier()` が中核。外部ストレージ優先・内部フォールバックで決めたパスを `GUMICORD_DATA_DIR` に設定し、TLS 検証器に JVM を渡してから共有ループに入る。終了時はプロセスを落として次回起動に備える。

### `Cargo.toml` — `gumicord-android` (ライブラリ名 `main`、`cdylib`) の定義。Android 時のみ `android_logger` 等を足す。

### `settings.gradle` / `build.gradle` — リポジトリと AGP 版の定義。版管理をここに集約する。

### `app/build.gradle` — アプリの定義。`namespace/applicationId dev.gumicord.app`、`ndkVersion`、`compileSdk/targetSdk 34/minSdk 26`、`abiFilters arm64-v8a/x86_64`、`games-activity:4.4.0` 依存。`games-activity` は `android-activity` 0.6 と対になる 4.x 系で固定する。

### `gradle.properties` — `android.useAndroidX=true` のみ。

### `app/src/main/AndroidManifest.xml` — `GameActivity` 単一アクティビティの宣言。`INTERNET`/`ACCESS_NETWORK_STATE` のみ求め、カメラ・マイク・ストレージ権限は持たない。

### `app/src/main/res/values/themes.xml` — `Theme.AppCompat.NoActionBar` 継承の `GumicordTheme` のみ。描画は Rust 側。

### `README.md` — 配置・ABI・外部ストレージ優先・ファイルログ・ビルド手順の説明。

# app/ios (`app/ios`)

> iOS の入口。Xcode が所有するバンドルから一度だけ呼ばれる薄い静的ライブラリ。

## Files

### `src/lib.rs` — 入口 `gumicord_ios_main(documents_dir)`。C 文字列を複写して `GUMICORD_DATA_DIR` に設定し、共有ループに入る。winit が `UIApplicationMain` を呼ぶため Swift 側で先に呼ばない。

### `Cargo.toml` — `gumicord-ios` (静的ライブラリ) の定義。

### `Gumicord/main.swift` — Swift 入口。Documents を取得し Rust に引き渡すのみ。

### `Gumicord/Gumicord-Bridging-Header.h` — `gumicord_ios_main` の宣言のみ。

### `Gumicord/Info.plist` — バンドル定義。`UIFileSharingEnabled` で Files 可視化。

### `Gumicord.xcodeproj/project.pbxproj` — 手書き最小 Xcode プロジェクト。`main.swift` のみビルドし、`libgumicord_ios.a` をリンクする。

### `README.md` — 配置・winit 所有・無署名・Files 可視・ビルド手順の説明。
