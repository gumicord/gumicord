//! Spike S4: the Discord Gateway and REST.
//!
//! The hypothesis: the Gateway can be connected to and its messages read,
//! REST can send one, and rate limits can be held off before they bite.
//!
//! Covers zstd-stream events, heartbeats and resume, MESSAGE_CREATE,
//! sending over REST, reading the rate limit headers and throttling ahead
//! of them, and backing off from a 429.
//!
//! The token comes only from the environment or .env, never reaches a log,
//! an error or a panic message, and .env is gitignored.
//!
//! A bot token is preferred because the handshake, heartbeat, resume,
//! compression and rate limiting are identical to a user account, so all
//! of this can be verified without risking one being locked. Only the
//! shape of the identify payload is user-specific, and that can be
//! specified without measuring it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const GATEWAY_VERSION: u8 = 10;
const API_BASE: &str = "https://discord.com/api/v10";

// ============================================================ The token

#[derive(Clone, Copy, PartialEq, Debug)]
enum TokenKind {
    Bot,
    User,
}

struct Config {
    token: String,
    kind: TokenKind,
    /// Where 4-5 sends. Sending is skipped when unset.
    channel_id: Option<String>,
    zstd: bool,
}

impl Config {
    fn load() -> Result<Self, String> {
        // Reads .env, minimally, rather than adding a dependency.
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

    /// The Authorization header value. Never logged.
    fn auth_header(&self) -> String {
        match self.kind {
            TokenKind::Bot => format!("Bot {}", self.token),
            TokenKind::User => self.token.clone(),
        }
    }
}

/// Checks the token has not leaked into a log.
fn assert_no_token(haystack: &str, token: &str) {
    if token.len() >= 8 && haystack.contains(token) {
        panic!("★SEC-001 違反★ 出力にトークンが含まれている");
    }
}

// ============================================================ Rate limits

/// One bucket: what Discord limits, individually.
///
/// The headers are read and requests held back before they are refused.
#[derive(Debug, Clone)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
    limit: u32,
}

#[derive(Default)]
struct RateLimiter {
    /// Route to bucket ID.
    routes: HashMap<String, String>,
    /// Bucket ID to state.
    buckets: HashMap<String, Bucket>,
}

impl RateLimiter {
    /// Called before a request, waiting if it must. True when it waited.
    async fn acquire(&mut self, route: &str) -> bool {
        let Some(bucket_id) = self.routes.get(route) else {
            return false; // 未知のルートは 1 回目なので通す
        };
        let Some(b) = self.buckets.get(bucket_id) else {
            return false;
        };
        if b.remaining == 0 {
            let now = Instant::now();
            if b.reset_at > now {
                let wait = b.reset_at - now;
                println!(
                    "[rate]    ★事前抑制: route={route} 残量0 → {:.2}秒待機",
                    wait.as_secs_f32()
                );
                tokio::time::sleep(wait).await;
                return true;
            }
        }
        false
    }

    /// Updates the state from the response headers.
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

/// zstd-stream arrives as one stream spanning WebSocket frames, so a frame
/// cannot be decompressed alone and the decoder must keep its state.
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

fn identify_payload(cfg: &Config, intents: u64) -> Value {
    match cfg.kind {
        // Connects with the same properties as the official client: not to hide,
        // but so the server is not told something false.
        TokenKind::Bot => json!({
            "op": 2,
            "d": {
                "token": cfg.token,
                "intents": intents,
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


// ============================================================ Watching

#[derive(Default)]
struct GatewayResult {
    ready_ms: Option<f32>,
    guilds: usize,
    heartbeats_sent: u32,
    heartbeat_acks: u32,
    messages_seen: u32,
    session_id: Option<String>,
    resume_url: Option<String>,
    last_seq: Option<u64>,
    close_code: Option<u16>,
    zstd_ok: bool,
    sent_ok: Option<String>,
}

/// GUILDS | GUILD_MESSAGES, neither privileged.
const INTENTS_BASIC: u64 = (1 << 0) | (1 << 9);
/// The above plus MESSAGE_CONTENT, which the Developer Portal must allow.
const INTENTS_PRIVILEGED: u64 = INTENTS_BASIC | (1 << 15);

async fn observe_gateway(
    cfg: &Config,
    intents: u64,
    http: &reqwest::Client,
    limiter: &mut RateLimiter,
    observe_secs: u64,
) -> Result<GatewayResult, Box<dyn std::error::Error>> {
    let mut r = GatewayResult::default();

    let compress = if cfg.zstd { "&compress=zstd-stream" } else { "" };
    let url = format!("wss://gateway.discord.gg/?v={GATEWAY_VERSION}&encoding=json{compress}");
    println!("[gateway] 接続先 = {url}");

    let t_connect = Instant::now();
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await?;
    println!("[MEASURE] WebSocket 確立 = {:.0}ms", t_connect.elapsed().as_secs_f32() * 1000.0);

    let mut zstd_stream = if cfg.zstd { Some(ZstdStream::new()?) } else { None };
    let mut heartbeat_interval = Duration::from_secs(41);
    let mut identified = false;

    let deadline = Instant::now() + Duration::from_secs(observe_secs);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut next_heartbeat = Instant::now() + Duration::from_secs(3600);

    println!("[gateway] {observe_secs} 秒間観測します");

    loop {
        tokio::select! {
            _ = tick.tick() => {
                if Instant::now() > deadline { break; }
                // Heartbeats.
                if identified && Instant::now() >= next_heartbeat {
                    ws.send(Message::Text(json!({"op":1,"d":r.last_seq}).to_string().into())).await?;
                    r.heartbeats_sent += 1;
                    next_heartbeat = Instant::now() + heartbeat_interval;
                    println!("[gateway] ハートビート送信 #{} (seq={:?})", r.heartbeats_sent, r.last_seq);
                }
            }
            msg = ws.next() => {
                let Some(msg) = msg else { println!("[gateway] ストリーム終端"); break };
                let msg = msg?;

                let text: String = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => {
                        // zstd-stream is one stream across frames.
                        match &mut zstd_stream {
                            Some(z) => {
                                let out = z.push(&b)?;
                                if out.is_empty() { continue; }
                                r.zstd_ok = true;
                                String::from_utf8_lossy(&out).into_owned()
                            }
                            None => { println!("[gateway] 予期しないバイナリフレーム"); continue }
                        }
                    }
                    Message::Close(c) => {
                        if let Some(f) = &c {
                            r.close_code = Some(u16::from(f.code));
                            println!("[gateway] Close: code={} reason={}", u16::from(f.code), f.reason);
                        }
                        break;
                    }
                    _ => continue,
                };

                let payload: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => { println!("[gateway] JSON 解析失敗: {e}"); continue }
                };
                if let Some(s) = payload["s"].as_u64() { r.last_seq = Some(s); }

                match payload["op"].as_u64().unwrap_or(u64::MAX) {
                    10 => {
                        let ms = payload["d"]["heartbeat_interval"].as_u64().unwrap_or(41250);
                        heartbeat_interval = Duration::from_millis(ms);
                        println!("[MEASURE] op=10 Hello  heartbeat_interval = {ms}ms");
                        if cfg.zstd && r.zstd_ok {
                            println!("[MEASURE] 4-2 zstd-stream の解凍に成功");
                        }
                        ws.send(Message::Text(identify_payload(cfg, intents).to_string().into())).await?;
                        identified = true;
                        // The first heartbeat is jittered, as the official client does.
                        next_heartbeat = Instant::now() + heartbeat_interval.mul_f64(0.5);
                        println!("[gateway] op=2 Identify 送信 (intents={intents:#x})");
                    }
                    11 => {
                        r.heartbeat_acks += 1;
                        println!("[gateway] op=11 Heartbeat ACK #{}", r.heartbeat_acks);
                    }
                    1 => {
                        ws.send(Message::Text(json!({"op":1,"d":r.last_seq}).to_string().into())).await?;
                        r.heartbeats_sent += 1;
                    }
                    9 => { println!("[gateway] op=9 Invalid Session"); break }
                    7 => { println!("[gateway] op=7 Reconnect 要求"); break }
                    0 => {
                        match payload["t"].as_str().unwrap_or("") {
                            "READY" => {
                                r.ready_ms = Some(t_connect.elapsed().as_secs_f32() * 1000.0);
                                r.session_id = payload["d"]["session_id"].as_str().map(str::to_string);
                                r.resume_url = payload["d"]["resume_gateway_url"].as_str().map(str::to_string);
                                r.guilds = payload["d"]["guilds"].as_array().map(|a| a.len()).unwrap_or(0);
                                println!("[MEASURE] 4-1 READY 到達 = {:.0}ms", r.ready_ms.unwrap());
                                println!("[MEASURE] ギルド数 = {}", r.guilds);
                                assert_no_token(&payload.to_string(), &cfg.token);
                                println!("[MEASURE] SEC-001 自己点検: READY ペイロードにトークンなし");

                                // Sending over REST.
                                if let Some(ch) = &cfg.channel_id {
                                    let route = "POST /channels/:id/messages";
                                    limiter.acquire(route).await;
                                    let t = Instant::now();
                                    let resp = http
                                        .post(format!("{API_BASE}/channels/{ch}/messages"))
                                        .header("Authorization", cfg.auth_header())
                                        .json(&json!({ "content": "Gumicord スパイク S4 からの送信テストです" }))
                                        .send().await?;
                                    let st = resp.status();
                                    println!("[MEASURE] 4-5 メッセージ送信 = {st} ({:.0}ms)", t.elapsed().as_secs_f32()*1000.0);
                                    limiter.update(route, resp.headers());
                                    r.sent_ok = Some(st.to_string());
                                    if !st.is_success() {
                                        let body = resp.text().await.unwrap_or_default();
                                        assert_no_token(&body, &cfg.token);
                                        println!("           応答: {body}");
                                    }
                                } else {
                                    println!("[skip]    4-5 は GUMICORD_CHANNEL_ID 未設定のため飛ばします");
                                }
                            }
                            "MESSAGE_CREATE" => {
                                r.messages_seen += 1;
                                let author = payload["d"]["author"]["username"].as_str().unwrap_or("?");
                                let content = payload["d"]["content"].as_str().unwrap_or("");
                                let preview: String = content.chars().take(40).collect();
                                let note = if content.is_empty() { "  (本文が空 = MESSAGE_CONTENT 未許可)" } else { "" };
                                println!("[MEASURE] 4-4 MESSAGE_CREATE #{}: {author}: {preview}{note}", r.messages_seen);
                            }
                            other if !other.is_empty() => println!("[gateway] dispatch: {other}"),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let _ = ws.close(None).await;
    Ok(r)
}

/// Trying a resume.
async fn try_resume(cfg: &Config, r: &GatewayResult) -> Result<bool, Box<dyn std::error::Error>> {
    let (Some(sid), Some(base), Some(seq)) = (&r.session_id, &r.resume_url, r.last_seq) else {
        println!("[skip]    session_id / resume_gateway_url が得られなかったため飛ばします");
        return Ok(false);
    };
    let url = format!("{base}/?v={GATEWAY_VERSION}&encoding=json");
    println!("[gateway] resume を試行: {url}");
    let t = Instant::now();
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await?;

    let dl = Instant::now() + Duration::from_secs(20);
    while Instant::now() < dl {
        let Some(Ok(m)) = ws.next().await else { break };
        let Message::Text(frame) = m else { continue };
        let v: Value = serde_json::from_str(&frame)?;
        match v["op"].as_u64() {
            Some(10) => {
                let p = json!({ "op": 6, "d": { "token": cfg.token, "session_id": sid, "seq": seq }});
                ws.send(Message::Text(p.to_string().into())).await?;
                println!("[gateway] op=6 Resume 送信 (seq={seq})");
            }
            Some(0) if v["t"] == "RESUMED" => {
                println!("[MEASURE] 4-3 RESUMED 成功 = {:.0}ms", t.elapsed().as_secs_f32() * 1000.0);
                let _ = ws.close(None).await;
                return Ok(true);
            }
            Some(9) => { println!("[gateway] op=9 resume 不可 (セッション期限切れ)"); break }
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    println!("[MEASURE] 4-3 RESUMED を確認できませんでした");
    Ok(false)
}

// ============================================================ Rate limits

/// Reaches the rate limit, to check both the throttling and the recovery
/// from an exponential backoff.
///
/// POST /channels/:id/messages measures at five per second, so sending a
/// little over that should throttle without ever producing a 429.
/// Never seeing a 429 is the pass condition; if one does arrive, the
/// backoff must recover from it.
async fn test_rate_limit(
    cfg: &Config,
    http: &reqwest::Client,
    limiter: &mut RateLimiter,
    count: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(ch) = &cfg.channel_id else {
        println!("[skip]    GUMICORD_CHANNEL_ID 未設定のため飛ばします");
        return Ok(());
    };
    let route = "POST /channels/:id/messages";

    let mut sent = 0u32;
    let mut preempted = 0u32;
    let mut hit_429 = 0u32;
    let mut max_backoff = Duration::ZERO;
    let t_all = Instant::now();

    for i in 1..=count {
        if limiter.acquire(route).await {
            preempted += 1;
        }

        // A 429 is retried with an exponential backoff.
        let mut attempt = 0u32;
        loop {
            let t = Instant::now();
            let resp = http
                .post(format!("{API_BASE}/channels/{ch}/messages"))
                .header("Authorization", cfg.auth_header())
                .json(&json!({ "content": format!("S4 レート制限テスト {i}/{count}") }))
                .send()
                .await?;
            let st = resp.status();
            let headers = resp.headers().clone();

            if st.as_u16() == 429 {
                hit_429 += 1;
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                let retry_after = body["retry_after"].as_f64().unwrap_or(1.0);
                let global = body["global"].as_bool().unwrap_or(false);
                let scope = headers
                    .get("x-ratelimit-scope")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("?");
                // The server's retry_after is the floor, grown exponentially per attempt.
                let backoff = Duration::from_secs_f64(retry_after * 2f64.powi(attempt as i32));
                max_backoff = max_backoff.max(backoff);
                println!(
                    "[rate]    ★429 到達 (試行{}) global={global} scope={scope} retry_after={retry_after:.2}秒 → {:.2}秒待機",
                    attempt + 1,
                    backoff.as_secs_f32()
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
                if attempt >= 5 {
                    println!("[rate]    ★5 回試しても復帰できませんでした");
                    break;
                }
                continue;
            }

            limiter.update(route, &headers);
            if st.is_success() {
                sent += 1;
                println!("  {i}/{count}: {st} ({:.0}ms)", t.elapsed().as_secs_f32() * 1000.0);
            } else {
                let body = resp.text().await.unwrap_or_default();
                assert_no_token(&body, &cfg.token);
                println!("  {i}/{count}: {st} — {body}");
            }
            break;
        }
    }

    println!();
    println!("[MEASURE] 4-7 送信成功        = {sent}/{count} ({:.1}秒)", t_all.elapsed().as_secs_f32());
    println!("[MEASURE] 4-7 事前抑制の発動   = {preempted} 回");
    println!("[MEASURE] 4-7 429 到達        = {hit_429} 回");
    if hit_429 == 0 {
        println!("[MEASURE] 4-7 判定 = ✅ NFR-021 合格 (429 に到達せず事前抑制で回避)");
        println!("           ※ NFR-022 (バックオフ) は発動しなかったため未検証のまま");
    } else {
        println!("[MEASURE] 4-7 最大バックオフ  = {:.2}秒", max_backoff.as_secs_f32());
        println!("[MEASURE] 4-7 判定 = ⚠️ 429 に到達した。NFR-021 の事前抑制に漏れがある");
        if sent == count {
            println!("           ただし NFR-022 のバックオフでは全件復帰できた");
        }
    }
    Ok(())
}

/// Verifies the throttling itself, locally.
///
/// Against the real server the round trip (321-833ms) spaces requests out
/// enough that sequential ones rarely reach the limit: seven messages left
/// the bucket empty only after the last, and it never throttled.
///
/// Not throttling proves nothing about throttling, so acquire() is checked
/// directly against a synthesised bucket. No network, and nothing posted
/// to a channel.
async fn verify_limiter_locally() {
    println!("──────── 4-7 補完: 事前抑制ロジックのローカル検証 ────────");

    let mut limiter = RateLimiter::default();
    let route = "POST /test";
    let bucket = "synthetic".to_string();

    // A bucket with nothing left, recovering in half a second.
    limiter.routes.insert(route.to_string(), bucket.clone());
    limiter.buckets.insert(
        bucket.clone(),
        Bucket {
            remaining: 0,
            limit: 5,
            reset_at: Instant::now() + Duration::from_millis(500),
        },
    );

    let t = Instant::now();
    let waited = limiter.acquire(route).await;
    let elapsed = t.elapsed();
    println!(
        "[MEASURE] 残量0 のバケット: 待機={waited} 実測={:.0}ms (期待 約500ms)",
        elapsed.as_secs_f32() * 1000.0
    );
    let ok_blocked = waited && elapsed >= Duration::from_millis(450);

    // A bucket with room does not wait.
    limiter.buckets.insert(
        bucket.clone(),
        Bucket {
            remaining: 3,
            limit: 5,
            reset_at: Instant::now() + Duration::from_secs(5),
        },
    );
    let t = Instant::now();
    let waited2 = limiter.acquire(route).await;
    let elapsed2 = t.elapsed();
    println!(
        "[MEASURE] 残量3 のバケット: 待機={waited2} 実測={:.0}ms (期待 約0ms)",
        elapsed2.as_secs_f32() * 1000.0
    );
    let ok_passthrough = !waited2 && elapsed2 < Duration::from_millis(50);

    // Nor does one past its reset.
    limiter.buckets.insert(
        bucket,
        Bucket {
            remaining: 0,
            limit: 5,
            reset_at: Instant::now() - Duration::from_secs(1),
        },
    );
    let t = Instant::now();
    let waited3 = limiter.acquire(route).await;
    let elapsed3 = t.elapsed();
    println!(
        "[MEASURE] 残量0 だが回復済み: 待機={waited3} 実測={:.0}ms (期待 約0ms)",
        elapsed3.as_secs_f32() * 1000.0
    );
    let ok_expired = !waited3 && elapsed3 < Duration::from_millis(50);

    println!(
        "[MEASURE] 4-7 事前抑制ロジック = {}",
        if ok_blocked && ok_passthrough && ok_expired { "✅ 3 条件すべて期待どおり" } else { "★不合格★" }
    );
}

// ============================================================ main

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("======== S4: Discord Gateway ========");

    // The checks that need no network come first.
    verify_limiter_locally().await;
    println!();

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!();
            eprintln!("{e}");
            eprintln!();
            eprintln!("  spike/s4-gateway/.env.example をコピーして .env を作ってください。");
            return Ok(());
        }
    };

    println!("[config]  トークン種別 = {:?}", cfg.kind);
    println!("[config]  zstd-stream = {}", cfg.zstd);
    println!("[config]  送信テスト先 = {}", if cfg.channel_id.is_some() { "設定済み" } else { "未設定" });
    if cfg.kind == TokenKind::User {
        println!();
        println!("  ⚠️  ユーザートークンでの検証はアカウント停止のリスクがあります。");
        println!("      捨てて良いテスト用アカウントであることを確認してください。");
        println!();
    }

    let http = reqwest::Client::builder()
        .user_agent("Gumicord/0.0.1-spike (https://github.com/gumicord)")
        .build()?;
    let mut limiter = RateLimiter::default();

    // ---------------------------------------------------------------- 4-6
    println!();
    println!("──────── 4-6 レート制限ヘッダの解釈 (NFR-021) ────────");
    let route = "GET /users/@me";
    limiter.acquire(route).await;
    let t = Instant::now();
    let resp = http.get(format!("{API_BASE}/users/@me"))
        .header("Authorization", cfg.auth_header()).send().await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    println!("[MEASURE] GET /users/@me = {status} ({:.0}ms)", t.elapsed().as_secs_f32() * 1000.0);
    limiter.update(route, &headers);

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        assert_no_token(&body, &cfg.token);
        eprintln!("[error]   認証に失敗しました: {body}");
        return Ok(());
    }
    let me: Value = resp.json().await?;
    println!("[MEASURE] 認証成功: {} (id={})",
        me["username"].as_str().unwrap_or("?"), me["id"].as_str().unwrap_or("?"));

    for i in 1..=3 {
        limiter.acquire(route).await;
        let rr = http.get(format!("{API_BASE}/users/@me"))
            .header("Authorization", cfg.auth_header()).send().await?;
        println!("  {i} 回目: {}", rr.status());
        limiter.update(route, rr.headers());
    }

    // ---------------------------------------------------------------- Diagnosis
    // Why READY carried no guilds, asked rather than guessed.
    println!();
    println!("──────── 診断: 参加ギルドとチャンネルの到達性 ────────");
    let gr = http.get(format!("{API_BASE}/users/@me/guilds"))
        .header("Authorization", cfg.auth_header()).send().await?;
    let gst = gr.status();
    if gst.is_success() {
        let guilds: Value = gr.json().await?;
        let list = guilds.as_array().cloned().unwrap_or_default();
        println!("[diag]    参加ギルド数 = {}", list.len());
        for g in &list {
            println!("            - {} (id={})",
                g["name"].as_str().unwrap_or("?"), g["id"].as_str().unwrap_or("?"));
        }
        if list.is_empty() {
            println!("[diag]    ★ Bot がどのサーバーにも参加していません。");
            println!("            招待 URL (client_id は上の id と同じ):");
            println!("            https://discord.com/oauth2/authorize?client_id={}&scope=bot&permissions=68608",
                me["id"].as_str().unwrap_or("APPLICATION_ID"));
        }
    } else {
        let body = gr.text().await.unwrap_or_default();
        assert_no_token(&body, &cfg.token);
        println!("[diag]    GET /users/@me/guilds = {gst}: {body}");
    }

    if let Some(ch) = &cfg.channel_id {
        let cr = http.get(format!("{API_BASE}/channels/{ch}"))
            .header("Authorization", cfg.auth_header()).send().await?;
        let cst = cr.status();
        let body = cr.text().await.unwrap_or_default();
        assert_no_token(&body, &cfg.token);
        if cst.is_success() {
            let c: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            println!("[diag]    チャンネル到達可: #{} (type={}, guild_id={})",
                c["name"].as_str().unwrap_or("?"),
                c["type"].as_u64().unwrap_or(999),
                c["guild_id"].as_str().unwrap_or("DM?"));
        } else {
            println!("[diag]    ★ チャンネルに到達できません: {cst} {body}");
            println!("            GUMICORD_CHANNEL_ID が Bot の参加サーバー内のものか確認してください。");
        }
    }

    // ---------------------------------------------------------------- 4-1〜4-5
    println!();
    println!("──────── 4-1〜4-5 Gateway 接続 (特権インテントあり) ────────");
    let mut result = observe_gateway(&cfg, INTENTS_PRIVILEGED, &http, &mut limiter, 45).await?;
    let mut used_intents = INTENTS_PRIVILEGED;

    // 4014 = disallowed intents: the Developer Portal has not granted the
    // privileged ones. Dropping MESSAGE_CONTENT still delivers MESSAGE_CREATE
    // with an empty body, so this carries on rather than stopping.
    if result.close_code == Some(4014) {
        println!();
        println!("[info]    4014 Disallowed intent(s) — 特権インテントが未許可です。");
        println!("          MESSAGE_CONTENT を外して再試行します。");
        println!("          (本文まで取得したい場合は Developer Portal → Bot →");
        println!("           Privileged Gateway Intents で MESSAGE CONTENT INTENT を有効化)");
        println!();
        println!("──────── 4-1〜4-5 Gateway 接続 (非特権インテントのみ) ────────");
        result = observe_gateway(&cfg, INTENTS_BASIC, &http, &mut limiter, 45).await?;
        used_intents = INTENTS_BASIC;
    }

    // ---------------------------------------------------------------- 4-3 resume
    println!();
    println!("──────── 4-3 再接続 (resume) ────────");
    let resumed = try_resume(&cfg, &result).await.unwrap_or(false);

    // ---------------------------------------------------------------- 4-7
    if let Ok(n) = std::env::var("GUMICORD_TEST_RATELIMIT") {
        let count: u32 = n.parse().unwrap_or(7);
        println!();
        println!("──────── 4-7 レート制限の事前抑制とバックオフ (NFR-021 / NFR-022) ────────");
        println!("[info]    テストチャンネルへ {count} 通送信します");
        test_rate_limit(&cfg, &http, &mut limiter, count).await?;
    }

    // ---------------------------------------------------------------- Summary
    println!();
    println!("======== S4 測定結果 ========");
    println!("[MEASURE] intents            = {used_intents:#x}{}",
        if used_intents == INTENTS_BASIC { " (MESSAGE_CONTENT なし)" } else { "" });
    println!("[MEASURE] 4-1 identify→READY = {}",
        result.ready_ms.map(|m| format!("{m:.0}ms  ギルド {} 個", result.guilds)).unwrap_or("★未達★".into()));
    println!("[MEASURE] 4-2 zstd-stream    = {}",
        if !cfg.zstd { "無効".to_string() } else if result.zstd_ok { "復号成功".into() } else { "★復号なし★".into() });
    println!("[MEASURE] 4-3 ハートビート    = 送信 {} / ACK {}", result.heartbeats_sent, result.heartbeat_acks);
    println!("[MEASURE] 4-3 resume         = {}", if resumed { "RESUMED 成功" } else { "未確認" });
    println!("[MEASURE] 4-4 MESSAGE_CREATE = {} 件", result.messages_seen);
    println!("[MEASURE] 4-5 メッセージ送信   = {}", result.sent_ok.as_deref().unwrap_or("未実施"));
    println!("[MEASURE] 4-6 追跡バケット数   = {}", limiter.buckets.len());
    println!("=============================");
    Ok(())
}
