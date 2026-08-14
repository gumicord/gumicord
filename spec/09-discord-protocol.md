# 09. Discord プロトコル仕様

| | |
|---|---|
| ステータス | **ドラフト** — スパイク S4 で実測した範囲のみ |
| 最終更新 | 2026-08-14 |
| 関連要件 | `NFR-010`, `NFR-020`〜`NFR-024`, `FR-020`, `FR-024`, `SEC-001` |
| 実測環境 | Windows 10 / Rust 1.97.1 / `tokio-tungstenite` 0.30 / `reqwest` 0.13 / Bot トークン |

> ✅ = スパイク S4 で実測して確認した / 🔬 = 未検証

## 1. エンドポイント

| 用途 | URL |
|---|---|
| REST | `https://discord.com/api/v10` |
| Gateway (初回) | `wss://gateway.discord.gg/?v=10&encoding=json&compress=zstd-stream` ✅ |
| Gateway (resume) | READY で受け取る `resume_gateway_url` ✅ |

`resume_gateway_url` は**リージョン別のホスト**が返る。実測で観測した例:

```
wss://gateway-us-east1-b.discord.gg
wss://gateway-us-east1-d.discord.gg
```

**resume では必ずこの URL を使う。** 初回と同じ `gateway.discord.gg` に繋ぐと別のサーバーに割り当てられ、resume に失敗しうる。

## 2. 接続シーケンス ✅

```
Client                                Discord
  │                                      │
  ├─ WebSocket 確立 ────────────────────▶│   実測 338〜390 ms
  │◀───────────────────── op=10 Hello ───┤   heartbeat_interval = 41250 ms
  │                                      │
  ├─ op=2 Identify ────────────────────▶│
  │◀──────────────── op=0 t=READY ───────┤   実測 672〜1120 ms (接続開始から)
  │                                      │   session_id, resume_gateway_url, guilds
  │◀──────────── op=0 t=GUILD_CREATE ────┤
  │                                      │
  ├─ op=1 Heartbeat ───────────────────▶│   最初は interval × jitter 後
  │◀──────────────── op=11 Heartbeat ACK ┤
  │                                      │
```

### 実測値

| 指標 | 実測 |
|---|---|
| WebSocket 確立 | 338〜390 ms |
| 接続開始から READY 到達 | **672〜1120 ms** |
| `heartbeat_interval` | 41,250 ms |
| resume 完了 (接続から RESUMED まで) | **553〜619 ms** |

> `NFR-001` (コールドスタート 500ms) との関係: **Gateway の READY を待つと 500ms を超える。**
> したがって「操作可能になるまで」の定義に READY を含めてはならない。
> 起動時はローカルキャッシュ (`NFR-011`) を先に描画し、READY は非同期に反映する設計が必須である。
> これは S4 が仕様に与えた最も重要な制約。

## 3. Identify ペイロード (`NFR-020`)

### Bot トークンの場合 ✅

```json
{
  "op": 2,
  "d": {
    "token": "<token>",
    "intents": 33281,
    "properties": { "os": "windows", "browser": "Gumicord", "device": "Gumicord" },
    "compress": false
  }
}
```

`intents` はビットフラグ:

| ビット | 値 | 名称 | 特権 |
|---|---|---|---|
| 0 | `1 << 0` | `GUILDS` | — |
| 9 | `1 << 9` | `GUILD_MESSAGES` | — |
| 15 | `1 << 15` | `MESSAGE_CONTENT` | **特権** |

**特権インテントは Developer Portal での許可が必要。** 未許可のまま要求すると接続直後に切断される。

```
Close: code=4014 reason="Disallowed intent(s)."
```

`MESSAGE_CONTENT` がなくても `MESSAGE_CREATE` 自体は届く。**`content` が空文字列になるだけ**である ✅。

### ユーザートークンの場合 🔬

未検証。実測せずに仕様化する ([00-vision.md のリスク](00-vision.md#リスクと前提) により、テスト用アカウントでの検証も推奨しない)。

```json
{
  "op": 2,
  "d": {
    "token": "<token>",
    "capabilities": 161789,
    "properties": {
      "os": "Windows", "browser": "Discord Client", "device": "",
      "system_locale": "ja-JP", "client_version": "...", "os_version": "...",
      "release_channel": "stable", "client_build_number": 400000
    },
    "presence": { "status": "unknown", "since": 0, "activities": [], "afk": false },
    "compress": false,
    "client_state": { "guild_versions": {} }
  }
}
```

> `NFR-020` の「公式クライアントと同等の identify プロパティ」は、**検出を回避するためではなく、
> サーバーに嘘の情報を渡さないため**の要件である。`client_build_number` などは実際の値に追随させる。

## 4. 圧縮: zstd-stream ✅

`compress=zstd-stream` を指定すると、ペイロードは WebSocket の**バイナリフレーム**で届く。

> **重要**: zstd-stream は**フレームを跨ぐ 1 本の連続ストリーム**である。
> フレームごとに独立して解凍することはできない。**状態を保持したデコーダを接続の生存期間中ずっと持つ必要がある。**

Rust での実装 (S4 で動作確認済み):

```rust
struct ZstdStream {
    decoder: zstd::stream::write::Decoder<'static, Vec<u8>>,
}

impl ZstdStream {
    fn push(&mut self, chunk: &[u8]) -> std::io::Result<Vec<u8>> {
        use std::io::Write;
        self.decoder.write_all(chunk)?;
        self.decoder.flush()?;
        Ok(std::mem::take(self.decoder.get_mut()))
    }
}
```

`flush()` 後に取り出したバッファが空なら、そのフレームだけでは 1 メッセージが完結していないので次のフレームを待つ。

## 5. ハートビート (`NFR-010`) ✅

| 規則 | 内容 |
|---|---|
| 間隔 | `op=10 Hello` の `d.heartbeat_interval` (実測 41,250 ms) |
| ペイロード | `{"op": 1, "d": <最後に受信した s の値、なければ null>}` |
| **初回** | `interval × jitter` 後に送る。全クライアントが同時に叩かないようにするため |
| サーバー要求 | `op=1` を受信したら即座に返す |
| 応答 | `op=11` (Heartbeat ACK) |
| ACK が来ない場合 | 接続を破棄して resume する 🔬 |

## 6. 再接続と resume (`NFR-010`) ✅

```
1. READY から session_id と resume_gateway_url を保存する
2. 受信した最後の s (シーケンス番号) を保持し続ける
3. 切断されたら resume_gateway_url へ接続する
4. op=10 Hello を受けたら op=6 Resume を送る
     {"op": 6, "d": {"token": "...", "session_id": "...", "seq": <最後の s>}}
5. op=0 t=RESUMED が返れば成功。取りこぼしたイベントが順に再送される
6. op=9 (Invalid Session) が返ればセッション切れ。identify からやり直す
```

実測: **接続から RESUMED まで 553〜619 ms** ✅

### 切断時の分岐 🔬

| Close コード | 意味 | 対応 |
|---|---|---|
| 4014 | Disallowed intent(s) | 設定の誤り。再試行しない ✅ |
| 4004 | 認証失敗 | トークン破棄してログイン画面へ (`FR-004`) |
| 4008 | レート制限 | バックオフして再接続 |
| 4009 | セッションタイムアウト | resume |
| その他 / 異常切断 | — | resume を試み、失敗したら identify |

`FR-004`, `NFR-022` に対応。詳細な分岐表は実装時に埋める。

## 7. レート制限 (`NFR-021`, `NFR-022`) ✅

### バケット方式

Discord のレート制限は**ルートごとではなくバケットごと**にかかる。複数のルートが同じバケットを共有することがあるため、**ルート → バケット ID → 状態**の 2 段のマッピングが必要である。

レスポンスヘッダ:

| ヘッダ | 意味 |
|---|---|
| `x-ratelimit-bucket` | バケットの識別子 |
| `x-ratelimit-limit` | そのバケットの上限 |
| `x-ratelimit-remaining` | 残り回数 |
| `x-ratelimit-reset-after` | 回復までの秒数 |

実測した値:

| ルート | バケット | 上限 | 回復 |
|---|---|---|---|
| `GET /users/@me` | `78bb8553...` | 1000 | 0.00 秒 |
| `POST /channels/:id/messages` | `62df3a8b...` | **5** | **1.00 秒** |

### 事前抑制 ✅

`NFR-021` は「事前にリクエストを抑制する」ことを要求する。429 を受けてから待つのではなく、**残量が 0 なら送る前に待つ**。

```rust
async fn acquire(&mut self, route: &str) {
    let Some(bucket_id) = self.routes.get(route) else { return };  // 未知のルートは通す
    let Some(b) = self.buckets.get(bucket_id) else { return };
    if b.remaining == 0 && b.reset_at > Instant::now() {
        tokio::time::sleep(b.reset_at - Instant::now()).await;
    }
}
```

事前抑制ロジックは合成したバケット状態に対して検証済み ✅:

| 条件 | 期待 | 実測 |
|---|---|---|
| 残量 0 / 回復 0.5 秒後 | 待つ | 502 ms 待機 |
| 残量 3 / 回復 5 秒後 | 待たない | 0 ms |
| 残量 0 / 回復時刻を経過済み | 待たない | 0 ms |

### 発見: 逐次リクエストではレート制限にほぼ当たらない ✅

テストチャンネルへ 7 通連続送信した実測 (バケット上限は 5):

```
1/7: 200 OK (372ms)   残=4/5  回復まで=1.00秒
2/7: 200 OK (334ms)   残=3/5  回復まで=1.64秒
3/7: 200 OK (445ms)   残=2/5  回復まで=2.30秒
4/7: 200 OK (833ms)   残=2/5  回復まで=2.81秒
5/7: 200 OK (615ms)   残=1/5  回復まで=3.01秒
6/7: 200 OK (321ms)   残=1/5  回復まで=3.42秒
7/7: 200 OK (620ms)   残=0/5  回復まで=4.08秒

送信成功 = 7/7 (3.5秒)   事前抑制の発動 = 0 回   429 到達 = 0 回
```

**往復遅延 (321〜833 ms) 自体が自然な間隔になるため、逐次リクエストではバケットを使い切れない。**

> **設計への含意**: レート制限が実際に問題になるのは**並行リクエストのバースト**であって、
> ユーザー操作に起因する逐次リクエストではない。
> 起動時に複数チャンネルの履歴を一斉に取得するような場面が本当の危険地帯である。

もう 1 つの観測: `x-ratelimit-remaining` は 2 → 2 のように**減らないことがある**。また `reset-after` が 1.00 → 4.08 秒と伸び続けた。**残量は厳密な値ではなく助言として扱い、実装は 429 を受けても壊れないようにする。**

### 429 からの復帰 (`NFR-022`) 🔬

**未検証。** 実装は用意したが、上記のとおり 429 に到達しなかったため発動していない。

意図的に 429 を起こすには並行リクエストのバーストが必要で、`NFR-024` (自動化された連続リクエストを行わない) と衝突するため実行しない。**M1 の実装時にモックサーバーで検証する。**

429 応答には以下が含まれる (仕様上):

- `retry_after` (秒)
- `global` — グローバル制限かどうか
- `x-ratelimit-scope` — `user` / `global` / `shared`

グローバル制限に当たった場合は**全リクエストを止める**必要がある。バケット単位の抑制では防げない。

## 8. トークンの取り扱い (`SEC-001`) ✅

- トークンは OS のセキュアストレージから読み、メモリ上でも必要最小限の寿命に留める (`FR-003`)
- **ログ・エラーメッセージ・クラッシュレポートに出力しない**
- S4 では READY ペイロード全体に対してトークン文字列が含まれないことを実行時に自己点検した。同種の点検を本実装のテストにも入れる

```rust
fn assert_no_token(haystack: &str, token: &str) {
    if token.len() >= 8 && haystack.contains(token) {
        panic!("SEC-001 違反: 出力にトークンが含まれている");
    }
}
```

## 9. 未執筆・未検証

| 項目 | 状態 |
|---|---|
| REST の全エンドポイント定義 | 未執筆 |
| Gateway イベントの全一覧と型 | 未執筆 |
| ETF エンコーディング | 未検討 (JSON で十分か要判断) |
| シャーディング | 未検討 (ユーザークライアントには不要の見込み) |
| ユーザートークンでの identify | 🔬 未検証 (意図的に検証しない) |
| 429 からのバックオフ | 🔬 未検証 (M1 でモックサーバーにより検証) |
| Close コードごとの分岐 | 🔬 部分的 (4014 のみ実測) |
| 音声 (Voice Gateway) | 未検討 (M3) |
