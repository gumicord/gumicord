//! スパイク S4: Discord Gateway 接続と REST の検証
//!
//! 検証する仮説 (spec/08-spike-plan.md):
//!   Gateway に接続してメッセージを受信し、REST でメッセージを送信できる。
//!   レート制限を事前に抑制できる。
//!
//! 検証項目:
//!   4-1 identify → ready                    (NFR-020)
//!   4-2 zstd-stream 圧縮でイベント受信        (NFR-023)
//!   4-3 ハートビートと再接続 (resume)         (NFR-010)
//!   4-4 MESSAGE_CREATE の受信                (FR-020)
//!   4-5 REST でメッセージ送信                 (FR-024)
//!   4-6 レート制限ヘッダの解釈と事前抑制        (NFR-021)
//!   4-7 429 からの指数バックオフ              (NFR-022)
//!
//! ■ トークンの扱い (SEC-001)
//!   トークンは環境変数または .env からのみ読む。
//!   ログ・エラー・パニックメッセージに出力しない。.env は .gitignore 済み。
//!
//! ■ Bot トークンを推奨する理由
//!   Gateway のハンドシェイク・ハートビート・再接続・圧縮・レート制限の機構は
//!   ユーザーアカウントと完全に同一である。したがって 4-1〜4-7 は Bot で検証でき、
//!   その場合アカウント停止のリスクがない。
//!   ユーザーアカウント固有なのは identify ペイロードの形だけであり、
//!   それは実測しなくても仕様化できる。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const GATEWAY_VERSION: u8 = 10;
const API_BASE: &str = "https://discord.com/api/v10";

// ============================================================ トークン

#[derive(Clone, Copy, PartialEq, Debug)]
enum TokenKind {
    Bot,
    User,
}

struct Config {
    token: String,
    kind: TokenKind,
    /// 4-5 のメッセージ送信先。未設定なら送信を飛ばす
    channel_id: Option<String>,
    zstd: bool,
}

impl Config {
    fn load() -> Result<Self, String> {
        // .env を読む (依存を増やさないため自前で最小実装)
        if let Ok(text) = std::fs::read_to_string(".env") {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim().trim_matches('"').trim_matches('\'');
                    if std::env::var(k.trim()).is_err() {
                        unsafe { std::env::set_var(k.trim(), v) };
                    }
                }
            }
        }

        let token = std::env::var("GUMICORD_TOKEN")
            .map_err(|_| "GUMICORD_TOKEN が設定されていません".to_string())?;
        if token.trim().is_empty() {
            return Err("GUMICORD_TOKEN が空です".into());
        }

        let kind = match std::env::var("GUMICORD_TOKEN_KIND").as_deref() {
            Ok("user") => TokenKind::User,
            _ => TokenKind::Bot,
        };

        Ok(Self {
            token: token.trim().to_string(),
            kind,
            channel_id: std::env::var("GUMICORD_CHANNEL_ID").ok().filter(|s| !s.is_empty()),
            zstd: std::env::var("GUMICORD_NO_ZSTD").is_err(),
        })
    }

    /// SEC-001: Authorization ヘッダの値。ログに出さない
    fn auth_header(&self) -> String {
        match self.kind {
            TokenKind::Bot => format!("Bot {}", self.token),
            TokenKind::User => self.token.clone(),
        }
    }
}

/// SEC-001 の検証: トークンが誤ってログに出ていないか自己点検する
fn assert_no_token(haystack: &str, token: &str) {
    if token.len() >= 8 && haystack.contains(token) {
        panic!("★SEC-001 違反★ 出力にトークンが含まれている");
    }
}

// ============================================================ レート制限

/// NFR-021: レート制限ヘッダを解釈し、事前にリクエストを抑制する。
/// Discord はバケット単位で制限をかけるため、バケットごとに残量と回復時刻を持つ。
#[derive(Debug, Clone)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
    limit: u32,
}

#[derive(Default)]
struct RateLimiter {
    /// ルート → バケット ID
    routes: HashMap<String, String>,
    /// バケット ID → 状態
    buckets: HashMap<String, Bucket>,
}

impl RateLimiter {
    /// リクエスト前に呼ぶ。必要なら待つ (事前抑制)
    async fn acquire(&mut self, route: &str) {
        let Some(bucket_id) = self.routes.get(route) else {
            return; // 未知のルートは 1 回目なので通す
        };
        let Some(b) = self.buckets.get(bucket_id) else {
            return;
        };
        if b.remaining == 0 {
            let now = Instant::now();
            if b.reset_at > now {
                let wait = b.reset_at - now;
                println!(
                    "[rate]    事前抑制: route={route} 残量0 → {:.2}秒待機",
                    wait.as_secs_f32()
                );
                tokio::time::sleep(wait).await;
            }
        }
    }

    /// レスポンスヘッダから状態を更新する
    fn update(&mut self, route: &str, headers: &reqwest::header::HeaderMap) {
        let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);

        let Some(bucket_id) = get("x-ratelimit-bucket") else {
            return;
        };
        let remaining = get("x-ratelimit-remaining")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);
        let limit = get("x-ratelimit-limit")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);
        let reset_after = get("x-ratelimit-reset-after")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);

        println!(
            "[rate]    route={route} bucket={bucket_id} 残={remaining}/{limit} 回復まで={reset_after:.2}秒"
        );

        self.routes.insert(route.to_string(), bucket_id.clone());
        self.buckets.insert(
            bucket_id,
            Bucket {
                remaining,
                limit,
                reset_at: Instant::now() + Duration::from_secs_f64(reset_after),
            },
        );
    }
}

// ============================================================ Gateway

/// 4-2: zstd-stream は WebSocket フレームを跨いで 1 本のストリームとして届く。
/// フレームごとに独立した解凍はできず、状態を保持したデコーダが必要になる。
struct ZstdStream {
    decoder: zstd::stream::write::Decoder<'static, Vec<u8>>,
}

impl ZstdStream {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            decoder: zstd::stream::write::Decoder::new(Vec::new())?,
        })
    }

    fn push(&mut self, chunk: &[u8]) -> std::io::Result<Vec<u8>> {
        use std::io::Write;
        self.decoder.write_all(chunk)?;
        self.decoder.flush()?;
        let out = std::mem::take(self.decoder.get_mut());
        Ok(out)
    }
}

fn identify_payload(cfg: &Config) -> Value {
    match cfg.kind {
        // NFR-020: 公式クライアントと同等のプロパティで接続する。
        // 隠すためではなく、サーバーに嘘の情報を渡さないため。
        TokenKind::Bot => json!({
            "op": 2,
            "d": {
                "token": cfg.token,
                // GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT
                "intents": (1 << 0) | (1 << 9) | (1 << 15),
                "properties": {
                    "os": std::env::consts::OS,
                    "browser": "Gumicord",
                    "device": "Gumicord"
                },
                "compress": false
            }
        }),
        TokenKind::User => json!({
            "op": 2,
            "d": {
                "token": cfg.token,
                "capabilities": 161789,
                "properties": {
                    "os": "Windows",
                    "browser": "Discord Client",
                    "device": "",
                    "system_locale": "ja-JP",
                    "client_version": "1.0.9200",
                    "os_version": "10.0.19045",
                    "release_channel": "stable",
                    "client_build_number": 400000
                },
                "presence": { "status": "unknown", "since": 0, "activities": [], "afk": false },
                "compress": false,
                "client_state": { "guild_versions": {} }
            }
        }),
    }
}

// ============================================================ main

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("======== S4: Discord Gateway ========");

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!();
            eprintln!("{e}");
            eprintln!();
            eprintln!("  spike/s4-gateway/.env を作って以下を書いてください:");
            eprintln!();
            eprintln!("    GUMICORD_TOKEN=<Bot トークン>");
            eprintln!("    GUMICORD_TOKEN_KIND=bot          # または user");
            eprintln!("    GUMICORD_CHANNEL_ID=<送信テスト先のチャンネルID>   # 任意");
            eprintln!();
            eprintln!("  ■ Bot トークンを推奨します。");
            eprintln!("    Gateway の機構はユーザーアカウントと同一なので 4-1〜4-7 は Bot で検証でき、");
            eprintln!("    その場合アカウント停止のリスクがありません。");
            eprintln!("    https://discord.com/developers/applications で作成し、");
            eprintln!("    Bot 設定で MESSAGE CONTENT INTENT を有効にしてください。");
            eprintln!();
            eprintln!("  .env は .gitignore 済みです。トークンはログにも出力しません (SEC-001)。");
            return Ok(());
        }
    };

    println!("[config]  トークン種別 = {:?}", cfg.kind);
    println!("[config]  zstd-stream = {}", cfg.zstd);
    if cfg.kind == TokenKind::User {
        println!();
        println!("  ⚠️  ユーザートークンでの検証はアカウント停止のリスクがあります。");
        println!("      捨てて良いテスト用アカウントであることを確認してください。");
        println!();
    }

    // ---------------------------------------------------------------- REST
    let http = reqwest::Client::builder()
        .user_agent("Gumicord/0.0.1-spike (https://github.com/gumicord)")
        .build()?;
    let mut limiter = RateLimiter::default();

    // 4-6: レート制限ヘッダの解釈
    println!();
    println!("──────── 4-6 レート制限ヘッダの解釈 (NFR-021) ────────");
    let route = "GET /users/@me";
    limiter.acquire(route).await;
    let t = Instant::now();
    let resp = http
        .get(format!("{API_BASE}/users/@me"))
        .header("Authorization", cfg.auth_header())
        .send()
        .await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    println!("[MEASURE] GET /users/@me = {status} ({:.0}ms)", t.elapsed().as_secs_f32() * 1000.0);
    limiter.update(route, &headers);

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        assert_no_token(&body, &cfg.token);
        eprintln!("[error]   認証に失敗しました: {body}");
        eprintln!("          トークンが正しいか、種別 (bot/user) が合っているか確認してください。");
        return Ok(());
    }

    let me: Value = resp.json().await?;
    println!(
        "[MEASURE] 認証成功: {}#{} (id={})",
        me["username"].as_str().unwrap_or("?"),
        me["discriminator"].as_str().unwrap_or("0"),
        me["id"].as_str().unwrap_or("?")
    );

    // 4-6 続き: 連続リクエストで残量が減るのを観測する。
    // NFR-024 に反しないよう少数回に留める。
    for i in 1..=3 {
        limiter.acquire(route).await;
        let r = http
            .get(format!("{API_BASE}/users/@me"))
            .header("Authorization", cfg.auth_header())
            .send()
            .await?;
        println!("  {i} 回目: {}", r.status());
        limiter.update(route, r.headers());
    }

    // ---------------------------------------------------------------- Gateway
    println!();
    println!("──────── 4-1〜4-4 Gateway 接続 ────────");

    let compress = if cfg.zstd { "&compress=zstd-stream" } else { "" };
    let url = format!("wss://gateway.discord.gg/?v={GATEWAY_VERSION}&encoding=json{compress}");
    println!("[gateway] 接続先 = {url}");

    let t_connect = Instant::now();
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await?;
    println!("[MEASURE] WebSocket 確立 = {:.0}ms", t_connect.elapsed().as_secs_f32() * 1000.0);

    let mut zstd_stream = if cfg.zstd { Some(ZstdStream::new()?) } else { None };
    let mut heartbeat_interval = Duration::from_secs(41);
    let mut last_seq: Option<u64> = None;
    let mut identified = false;
    let mut ready_at: Option<Duration> = None;
    let mut heartbeat_acks = 0u32;
    let mut heartbeats_sent = 0u32;
    let mut messages_seen = 0u32;
    let mut session_id: Option<String> = None;
    let mut resume_url: Option<String> = None;

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut hb_timer = tokio::time::interval(Duration::from_secs(1));
    let mut next_heartbeat = Instant::now() + Duration::from_secs(60);

    println!("[gateway] 60 秒間観測します。Ctrl+C で中断できます。");

    loop {
        tokio::select! {
            _ = hb_timer.tick() => {
                if Instant::now() > deadline { break; }
                // 4-3: ハートビート
                if identified && Instant::now() >= next_heartbeat {
                    let hb = json!({ "op": 1, "d": last_seq });
                    ws.send(Message::Text(hb.to_string().into())).await?;
                    heartbeats_sent += 1;
                    next_heartbeat = Instant::now() + heartbeat_interval;
                    println!("[gateway] ハートビート送信 #{heartbeats_sent} (seq={last_seq:?})");
                }
            }
            msg = ws.next() => {
                let Some(msg) = msg else { println!("[gateway] 接続が閉じられました"); break };
                let msg = msg?;

                let text: String = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => {
                        // 4-2: zstd-stream
                        match &mut zstd_stream {
                            Some(z) => {
                                let out = z.push(&b)?;
                                if out.is_empty() { continue; }
                                String::from_utf8_lossy(&out).into_owned()
                            }
                            None => { println!("[gateway] 予期しないバイナリフレーム"); continue }
                        }
                    }
                    Message::Close(c) => { println!("[gateway] Close: {c:?}"); break }
                    _ => continue,
                };

                let payload: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => { println!("[gateway] JSON 解析失敗: {e}"); continue }
                };

                if let Some(s) = payload["s"].as_u64() { last_seq = Some(s); }
                let op = payload["op"].as_u64().unwrap_or(u64::MAX);

                match op {
                    // Hello
                    10 => {
                        let ms = payload["d"]["heartbeat_interval"].as_u64().unwrap_or(41250);
                        heartbeat_interval = Duration::from_millis(ms);
                        println!("[MEASURE] op=10 Hello  heartbeat_interval = {ms}ms");
                        if cfg.zstd {
                            println!("[MEASURE] 4-2 zstd-stream の解凍に成功 (Hello を復号できた)");
                        }

                        // 4-1: identify
                        let id = identify_payload(&cfg);
                        let s = id.to_string();
                        ws.send(Message::Text(s.into())).await?;
                        identified = true;
                        // 最初のハートビートは jitter を入れる (公式クライアントと同じ挙動)
                        next_heartbeat = Instant::now() + heartbeat_interval.mul_f64(0.5);
                        println!("[gateway] op=2 Identify 送信");
                    }
                    // Heartbeat ACK
                    11 => {
                        heartbeat_acks += 1;
                        println!("[gateway] op=11 Heartbeat ACK #{heartbeat_acks}");
                    }
                    // サーバーからのハートビート要求
                    1 => {
                        ws.send(Message::Text(json!({"op":1,"d":last_seq}).to_string().into())).await?;
                        heartbeats_sent += 1;
                    }
                    // Invalid session
                    9 => {
                        println!("[gateway] op=9 Invalid Session (resumable={})", payload["d"].as_bool().unwrap_or(false));
                        break;
                    }
                    // Reconnect
                    7 => { println!("[gateway] op=7 Reconnect 要求"); break }
                    // Dispatch
                    0 => {
                        let ev = payload["t"].as_str().unwrap_or("");
                        match ev {
                            "READY" => {
                                ready_at = Some(t_connect.elapsed());
                                session_id = payload["d"]["session_id"].as_str().map(str::to_string);
                                resume_url = payload["d"]["resume_gateway_url"].as_str().map(str::to_string);
                                let guilds = payload["d"]["guilds"].as_array().map(|a| a.len()).unwrap_or(0);
                                println!("[MEASURE] 4-1 READY 到達 = {:.0}ms", ready_at.unwrap().as_secs_f32() * 1000.0);
                                println!("[MEASURE] ギルド数 = {guilds}");
                                println!("[MEASURE] resume_gateway_url = {}", resume_url.as_deref().unwrap_or("なし"));
                                let raw = payload.to_string();
                                assert_no_token(&raw, &cfg.token);
                                println!("[MEASURE] SEC-001 自己点検: READY ペイロードにトークンなし");

                                // 4-5: メッセージ送信
                                if let Some(ch) = &cfg.channel_id {
                                    let route = "POST /channels/:id/messages";
                                    limiter.acquire(route).await;
                                    let t = Instant::now();
                                    let r = http
                                        .post(format!("{API_BASE}/channels/{ch}/messages"))
                                        .header("Authorization", cfg.auth_header())
                                        .json(&json!({ "content": "Gumicord スパイク S4 からの送信テストです" }))
                                        .send()
                                        .await?;
                                    println!("[MEASURE] 4-5 メッセージ送信 = {} ({:.0}ms)", r.status(), t.elapsed().as_secs_f32()*1000.0);
                                    limiter.update(route, r.headers());
                                } else {
                                    println!("[skip]    4-5 送信テストは GUMICORD_CHANNEL_ID 未設定のため飛ばします");
                                }
                            }
                            "MESSAGE_CREATE" => {
                                messages_seen += 1;
                                let author = payload["d"]["author"]["username"].as_str().unwrap_or("?");
                                let content = payload["d"]["content"].as_str().unwrap_or("");
                                let preview: String = content.chars().take(40).collect();
                                println!("[MEASURE] 4-4 MESSAGE_CREATE #{messages_seen}: {author}: {preview}");
                            }
                            other if !other.is_empty() => {
                                println!("[gateway] dispatch: {other}");
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // ---------------------------------------------------------------- 4-3 resume
    println!();
    println!("──────── 4-3 再接続 (resume) ────────");
    if let (Some(sid), Some(url), Some(seq)) = (&session_id, &resume_url, last_seq) {
        let resume_url = format!("{url}/?v={GATEWAY_VERSION}&encoding=json");
        println!("[gateway] resume を試行: {resume_url}");
        let t = Instant::now();
        match tokio_tungstenite::connect_async(&resume_url).await {
            Ok((mut ws2, _)) => {
                // Hello を待つ
                let mut resumed = false;
                let dl = Instant::now() + Duration::from_secs(15);
                while Instant::now() < dl {
                    let Some(Ok(m)) = ws2.next().await else { break };
                    let Message::Text(frame) = m else { continue };
                    let v: Value = serde_json::from_str(&frame)?;
                    match v["op"].as_u64() {
                        Some(10) => {
                            let payload = json!({
                                "op": 6,
                                "d": { "token": cfg.token, "session_id": sid, "seq": seq }
                            });
                            ws2.send(Message::Text(payload.to_string().into())).await?;
                            println!("[gateway] op=6 Resume 送信 (seq={seq})");
                        }
                        Some(0) if v["t"] == "RESUMED" => {
                            println!("[MEASURE] 4-3 RESUMED 成功 = {:.0}ms", t.elapsed().as_secs_f32()*1000.0);
                            resumed = true;
                            break;
                        }
                        Some(9) => { println!("[gateway] op=9 resume 不可 (セッション期限切れ)"); break }
                        _ => {}
                    }
                }
                if !resumed {
                    println!("[MEASURE] 4-3 RESUMED を確認できませんでした");
                }
                let _ = ws2.close(None).await;
            }
            Err(e) => println!("[gateway] resume 接続に失敗: {e}"),
        }
    } else {
        println!("[skip]    session_id / resume_gateway_url が得られなかったため飛ばします");
    }

    // ---------------------------------------------------------------- まとめ
    println!();
    println!("======== S4 測定結果 ========");
    println!("[MEASURE] 4-1 identify → READY = {}", ready_at.map(|d| format!("{:.0}ms", d.as_secs_f32()*1000.0)).unwrap_or("★未達★".into()));
    println!("[MEASURE] 4-2 zstd-stream      = {}", if cfg.zstd { "有効で復号成功" } else { "無効" });
    println!("[MEASURE] 4-3 ハートビート      = 送信 {heartbeats_sent} / ACK {heartbeat_acks}");
    println!("[MEASURE] 4-4 MESSAGE_CREATE   = {messages_seen} 件");
    println!("[MEASURE] 4-6 追跡バケット数     = {}", limiter.buckets.len());
    println!("=============================");
    Ok(())
}
