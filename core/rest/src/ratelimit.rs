//! レート制限の事前抑制 (`NFR-021`, `NFR-022`)。
//!
//! # ルートごとではなくバケットごとにかかる
//!
//! 複数のルートが同じバケットを共有することがある。したがって
//! **ルート → バケット ID → 状態**の 2 段のマッピングが要る
//! ([`spec/09-discord-protocol.md`] 7 章)。
//!
//! # 429 を受けてから待つのでは遅い
//!
//! `NFR-021` は**送る前に**抑制することを求める。残量が 0 なら送らない。
//!
//! # 時刻を引数で受ける
//!
//! **ここは眠らない。** 「どれだけ待つべきか」を返すだけで、実際に待つのは
//! 呼び出し側である。時刻も引数で受ける。
//!
//! そうしてある理由は試験である。仕様には「429 からの復帰は未検証。M1 の
//! 実装時にモックサーバーで検証する」と書いてあるが、**眠らない設計にすれば
//! モックサーバーすら要らない**。
//!
//! # 残量は助言である
//!
//! S4 の実測で `x-ratelimit-remaining` が 2 → 2 と減らないことがあり、
//! `reset-after` が 1.00 → 4.08 秒と伸び続けた。
//! **厳密な値として扱わず、429 を受けても壊れないようにする。**

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 1 バケットの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
}

/// レート制限の状態。**眠らない。待つべき時間を返すだけである。**
#[derive(Debug, Default)]
pub struct RateLimiter {
    /// ルートの鍵 → バケット ID
    routes: HashMap<String, String>,
    /// バケット ID → 状態
    buckets: HashMap<String, Bucket>,
    /// グローバル制限が解けるまで。**バケット単位では防げない**
    global_until: Option<Instant>,
}

/// レスポンスから読み取ったレート制限の情報。
///
/// HTTP の層を持ち込まないため、ヘッダそのものではなく解釈済みの値を受ける。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimitHeaders {
    /// `x-ratelimit-bucket`
    pub bucket: Option<String>,
    /// `x-ratelimit-remaining`
    pub remaining: Option<u32>,
    /// `x-ratelimit-reset-after` (秒)
    pub reset_after: Option<f64>,
    /// `x-ratelimit-global` — グローバル制限に当たったか
    pub global: bool,
    /// 429 のときの `retry_after` (秒)
    pub retry_after: Option<f64>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 送る前に、待つべき時間を返す。`None` なら待たなくてよい。
    ///
    /// **知らないルートは通す。** 1 回目は通し、返ってきたヘッダで学ぶ。
    pub fn before(&self, route: &str, now: Instant) -> Option<Duration> {
        // グローバル制限が先。バケットを見るまでもない
        if let Some(until) = self.global_until
            && until > now
        {
            return Some(until - now);
        }

        let bucket = self.buckets.get(self.routes.get(route)?)?;
        if bucket.remaining > 0 || bucket.reset_at <= now {
            return None;
        }
        Some(bucket.reset_at - now)
    }

    /// 応答から学ぶ。
    ///
    /// バケット ID が無ければ何もしない。Discord がレート制限を課していない
    /// ルートでは付いてこない。
    pub fn after(&mut self, route: &str, h: &RateLimitHeaders, now: Instant) {
        if h.global
            && let Some(retry) = h.retry_after
        {
            // ⚠️ グローバル制限は**全リクエストを止める**。
            // バケット単位の抑制では防げない
            self.global_until = Some(now + secs(retry));
        }

        let Some(id) = &h.bucket else { return };
        self.routes.insert(route.to_owned(), id.clone());

        // 429 のときは retry_after を優先する。残量が嘘でも従える
        let reset_at = match (h.retry_after, h.reset_after) {
            (Some(r), _) => now + secs(r),
            (None, Some(r)) => now + secs(r),
            (None, None) => now,
        };

        self.buckets.insert(
            id.clone(),
            Bucket {
                remaining: if h.retry_after.is_some() {
                    0
                } else {
                    h.remaining.unwrap_or(1)
                },
                reset_at,
            },
        );
    }

    /// グローバル制限に当たっているか。診断のために使う
    pub fn is_globally_limited(&self, now: Instant) -> bool {
        self.global_until.is_some_and(|u| u > now)
    }

    /// 覚えているバケットの数。診断のために使う
    pub fn known_buckets(&self) -> usize {
        self.buckets.len()
    }
}

/// 秒を [`Duration`] へ。**負や NaN でも壊れない**
fn secs(v: f64) -> Duration {
    Duration::try_from_secs_f64(v.max(0.0)).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(bucket: &str, remaining: u32, reset_after: f64) -> RateLimitHeaders {
        RateLimitHeaders {
            bucket: Some(bucket.to_owned()),
            remaining: Some(remaining),
            reset_after: Some(reset_after),
            ..Default::default()
        }
    }

    /// 知らないルートは通す。1 回目は送ってヘッダで学ぶ
    #[test]
    fn an_unknown_route_is_never_delayed() {
        let rl = RateLimiter::new();
        assert_eq!(rl.before("POST /channels/1/messages", Instant::now()), None);
    }

    /// **NFR-021 の本体。** 残量 0 なら送る前に待つ
    ///
    /// 仕様 7 章の表をそのまま試験にしてある
    #[test]
    fn it_waits_before_sending_when_the_bucket_is_empty() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();
        let route = "POST /channels/1/messages";

        // 残量 0 / 回復 0.5 秒後 → 待つ
        rl.after(route, &headers("62df3a8b", 0, 0.5), now);
        let wait = rl.before(route, now).expect("待つはず");
        assert!(wait > Duration::from_millis(400) && wait <= Duration::from_millis(500));

        // 残量 3 / 回復 5 秒後 → 待たない
        rl.after(route, &headers("62df3a8b", 3, 5.0), now);
        assert_eq!(rl.before(route, now), None);

        // 残量 0 / 回復時刻を経過済み → 待たない
        rl.after(route, &headers("62df3a8b", 0, 0.5), now);
        assert_eq!(rl.before(route, now + Duration::from_secs(1)), None);
    }

    /// **複数のルートが同じバケットを共有する。** だから 2 段になっている
    #[test]
    fn routes_sharing_a_bucket_limit_each_other() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();

        rl.after("GET /a", &headers("共有", 0, 1.0), now);
        rl.after("GET /b", &headers("共有", 0, 1.0), now);

        assert_eq!(rl.known_buckets(), 1, "バケットは 1 つ");
        assert!(rl.before("GET /a", now).is_some());
        assert!(rl.before("GET /b", now).is_some(), "片方の消費が両方に効く");
    }

    /// **グローバル制限は全リクエストを止める。** バケットを見るまでもない
    #[test]
    fn a_global_limit_stops_everything() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();

        rl.after(
            "POST /channels/1/messages",
            &RateLimitHeaders {
                global: true,
                retry_after: Some(2.0),
                ..Default::default()
            },
            now,
        );

        assert!(rl.is_globally_limited(now));
        // 一度も触っていないルートまで止まる
        assert!(rl.before("GET /users/@me", now).is_some());
        // 明けたら通る
        assert_eq!(
            rl.before("GET /users/@me", now + Duration::from_secs(3)),
            None
        );
    }

    /// 429 を受けたら retry_after に従う。**残量が嘘でも従える**
    #[test]
    fn a_429_overrides_whatever_remaining_says() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();
        let route = "POST /channels/1/messages";

        rl.after(
            route,
            &RateLimitHeaders {
                bucket: Some("62df3a8b".into()),
                // 残っていると言っているが 429 である
                remaining: Some(5),
                retry_after: Some(1.5),
                ..Default::default()
            },
            now,
        );

        let wait = rl.before(route, now).expect("429 のあとは待つ");
        assert!(wait > Duration::from_millis(1400));
    }

    /// 変な値で壊れない。**残量は助言であって厳密な値ではない**
    #[test]
    fn nonsense_values_do_not_break_it() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();
        let route = "GET /x";

        for bad in [f64::NAN, f64::INFINITY, -5.0] {
            rl.after(route, &headers("b", 0, bad), now);
            // 待たないか、有限の時間だけ待つ。どちらでもよいが落ちてはいけない
            let _ = rl.before(route, now);
        }

        // バケット ID が無いヘッダは黙って無視する
        rl.after(route, &RateLimitHeaders::default(), now);
    }
}
