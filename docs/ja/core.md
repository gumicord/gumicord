# core: 通信と状態

Discord との通信 (`model` / `rest` / `gateway`) と正規化された状態
(`store`)。関連仕様: [`spec/09-discord-protocol.md`](../../spec/09-discord-protocol.md)。

# gumicord-model (`core/model`)

> Discord のドメイン型とシリアライズだけを担い、振る舞いを持たない。使用するフィールドのみを宣言し、未知の種別は捨てずに吸収する。

## Files

### `src/lib.rs` — `User`・`Guild`・`Channel`・`Message` 等の中核ドメイン型。`Guild` はボット形・ユーザートークンの `properties` 形・unavailable 形の 3 形状を吸収し、壊れた 1 要素で全体を落とさない。Key items: `User`、`Guild`、`Channel`、`Message`、`ChannelKind`、`fn display_name()`。表示名は nick→global_name→username の順、アバターはギルド別→ユーザ→既定の順で解決する。

### `src/token.rs` — 認証トークンのラッパー。`Debug`/`Display` は redacted 固定で、値の取り出しは `expose` に限定する。Key items: `Token`、`TokenKind`、`fn new()`、`fn bot()`、`fn expose()`、`fn is_absent_from()`。

### `src/snowflake.rs` — Discord の 64bit ID を型分けする。JSON 文字列が正だが数値も受け入れ、型違いはコンパイル時に落とす。Key items: `Snowflake`、`GuildId`、`ChannelId`、`fn get()`、`fn created_at_ms()`。ID 順は生成順であり、上位ビットから Unix ミリ秒を復元できる。

### `src/identity.rs` — Gateway と REST が共有するクライアント偽装クレーム。`properties` から `super_properties` と User-Agent を一元生成し、二経路の不一致自体を信号にしない。Key items: `Identity`、`fn detect()`、`fn properties()`、`fn super_properties()`、`fn user_agent()`。環境変数→起動時測定→組込値の優先順でビルド番号を解決する。

### `src/de.rs` — 寛容なデシリアライズ補助。リストの 1 要素が読めなくても全体を落とさない。Key items: `fn lenient_vec()`。

### `src/asset.rs` — CDN 画像の位置と要求修飾（サイズ・形式）の分離。場所だけを持ち、URL 組み立てはここに集約する。Key items: `Asset`、`Format`、`fn user_avatar()`、`fn url()`、`fn with_size()`。デコーダが PNG のためアニメーション素材も PNG で要求し、サイズは 2 の冪に切り上げる。

# gumicord-rest (`core/rest`)

> Discord REST API への要求・レート制限回避・429 復帰を担うクライアント。取得物の保持はせず、状態は store 側の仕事とする。

## Files

### `src/lib.rs` — 各モジュールの再エクスポート集約。

### `src/client.rs` — 要求送信・事前待機・429 リトライの中核 (最大 `MAX_RETRIES` 回、API v9 固定)。Key items: `RestClient`、`RestError`、`CaptchaChallenge`、`SolvedCaptcha`、`fn send()`。ボットは `Bot <token>` と専用 UA、ユーザは素トークンと super-properties 系ヘッダで分岐し、401 のみを資格情報失効とみなす。API のエラー本文は `message` を抜き出して表示する (JSON の `\uXXXX` はその過程で復号される)。

### `src/route.rs` — パスとレート制限キー (メジャーパラメータのみ) の分離。メッセージ ID・limit・before/around をキーから外し、バケット共有を保つ。Key items: `Route`、`Method`、`fn create_message()`、`fn messages()`、`fn delete_message()`、`fn current_user()`。

### `src/ratelimit.rs` — バケット枯渇前の事前待機を担う非スリープ判定器。Key items: `RateLimiter`、`RateLimitHeaders`、`fn before()`、`fn after()`。429 の `retry_after` が優先し、不正値は破綻せず処理する。

### `src/auth.rs` — ログイン系 REST 呼び出し。パスワード/MFA/QR チケット交換とトークン検証を集める。Key items: `LoginOutcome`、`fn login()`、`fn mfa_totp()`、`fn remote_auth_login()`、`fn current_user()`。captcha 解決はヘッダで送る。

### `src/channel.rs` — チャンネル・メッセージ系 REST 呼び出し。履歴は最新順のまま返し、並べ替えは呼び出し側に残す。Key items: `fn create_message()`、`fn messages()`、`fn messages_before()`、`fn edit_message()`、`fn delete_message()`、`fn fetch_cdn()`。返信は `allowed_mentions.replied_user` で通知の有無を切り替え (`fail_if_not_exists=false`)、CDN 取得は無認証・4MB 上限で別扱いする。

### `src/build_number.rs` — 起動時にログインページ HTML から `BUILD_NUMBER` を測定し identity に記録する。失敗は起動を止めず組込値に退避する。Key items: `fn measure()`、`fn extract()`。

### `tests/build_number_live.rs` — tests-only。実 Discord への到達確認 (`--ignored` で明示実行)。

# gumicord-gateway (`core/gateway`)

> Discord Gateway への接続・identify・ハートビート・resume・zstd・dispatch を担い、呼び出し側は `next` を回すだけで再接続が内部進行する。

## Files

### `src/lib.rs` — 再エクスポート集約に rustls プロバイダの冪等な導入 (`fn install_crypto_provider()`) を添えたもの。

### `src/gateway.rs` — WebSocket 接続・Hello・identify/resume・ハートビート・再送の本体。`Fatal` 以外では指数バックオフで再試行し続け、resume は READY 由来の地域ホストに繋ぐ。Key items: `Gateway`、`Event`、`Ready`、`Fatal`、`Subscriptions`、`fn next()`。ACK 欠落を切断検出とし、認証系のみ fatal とする。ユーザートークンは op 14 購読と op 8 メンバ要求、ボットは intents 漸減フォールバックで分岐する。購読は接続に属するため READY/再開のたびに送り直す。

### `src/proto.rs` — 非公開 `user_settings_proto` 用の最小 protobuf ワイヤ読取。生成型なしで走査し、不正入力はパニックせず途中までを返す。Key items: `fn blocks()`、`fn varint()`、`fn wrapped_string()`。

### `src/status.rs` — 自分のステータス抽出。接続中＝オンラインとはみなさない。Key items: `Status`、`fn from_settings_proto()`、`fn from_wire()`、`fn as_wire()`。

### `src/remote_auth.rs` — QR ログイン (RSA-2048・nonce 証明・指紋再計算・チケット承認)。秘密鍵は構造体内に留める。Key items: `RemoteAuth`、`RemoteAuthEvent`、`ScannedUser`、`fn connect()`、`fn next()`、`fn decrypt_token()`。サーバ送付の指紋は自身の公開鍵ハッシュと照合し、不一致は QR 化しない。

### `src/member_list.rs` — 範囲購読のメンバ一覧差分適用。見出しも行として扱い、SYNC/INSERT/UPDATE/DELETE/INVALIDATE を位置基準で畳む。Key items: `MemberList`、`MemberRow`、`fn parse()`、`fn apply()`、`fn rows()`。範囲外差分は捨て、不明 op は推測せず無視し、presence 欠落はオフラインと読む。

### `src/guild_order.rs` — READY 内 base64 protobuf からのギルド順・フォルダ抽出。Key items: `Folder`、`fn from_settings_proto()`。半端な順序は破棄して到着順に譲る。

### `src/zstd_stream.rs` — zstd-stream 復号。接続全体が単一ストリームのためデコーダを接続寿命で保持し、再接続で作り直す。Key items: `ZstdStream`、`fn new()`、`fn push()`。1 フレームが 0 件または複数 JSON を生み得る。

### `tests/remote_auth_live.rs` — tests-only。実 Discord まで QR 表示可能か確認 (`--ignored` で明示実行)。

# gumicord-store (`core/store`)

> 正規化されたインメモリ状態と SQLite 永続化。起動時は Gateway 前にここから描画し、READY で置き換える。

## Files

### `src/lib.rs` — 正規化状態 `Store` の本体 (ギルド・チャンネル・メッセージ・順序・既読・通知・メンバ)。Key items: `Store`、`ReadMark`、`NotifLevel`、`fn guilds()`、`fn replace_guilds()`、`fn push_message()`。未読は件数でなく snowflake 比較、履歴編集は未知行を追加せず置換のみ行う。

### `src/db.rs` — ローカルキャッシュ (SQLite)。読みは起動時同期、書きは単一 writer スレッドへの fire-and-forget で、失敗はログに留めアプリを止めない。Key items: `Db`、`Snapshot`、`fn open()`、`fn save_guilds()`、`fn save_messages()`。ID は TEXT 保存、スキーマ不一致は移行せず再構築、チャンネル毎 200 件に修剪し、メッセージ本文は平文保存のためサインアウトで全消去する。
