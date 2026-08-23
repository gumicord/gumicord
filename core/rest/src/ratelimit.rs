//! Rate limit avoidance.
//!
//! Limits apply per bucket, and several routes can share one, so this keeps
//! two maps: route to bucket id, bucket id to state. Waiting only after a 429
//! is too late — an empty bucket must not be sent to at all.
//!
//! Nothing here sleeps. It returns how long to wait and takes the current
//! time as an argument, which makes 429 recovery, global limits, bucket
//! sharing and malformed headers all testable without a mock server.
//!
//! Treat `remaining` as advice: it has been observed not to decrease while
//! `reset-after` kept growing, so receiving a 429 anyway must not break
//! anything.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
}

/// Rate limit state. Never sleeps; only reports how long to wait.
#[derive(Debug, Default)]
pub struct RateLimiter {
    routes: HashMap<String, String>,
    buckets: HashMap<String, Bucket>,
    /// A global limit cannot be avoided per bucket.
    global_until: Option<Instant>,
}

/// Rate limit information read off a response.
///
/// Interpreted values rather than raw headers, to keep the HTTP layer out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimitHeaders {
    /// `x-ratelimit-bucket`
    pub bucket: Option<String>,
    /// `x-ratelimit-remaining`
    pub remaining: Option<u32>,
    /// `x-ratelimit-reset-after`, in seconds.
    pub reset_after: Option<f64>,
    /// `x-ratelimit-global`
    pub global: bool,
    /// `retry_after` from a 429, in seconds.
    pub retry_after: Option<f64>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long to wait before sending, if at all.
    ///
    /// Unknown routes pass: the first request goes out and the response
    /// teaches us the bucket.
    pub fn before(&self, route: &str, now: Instant) -> Option<Duration> {
        // A global limit outranks any bucket.
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

    /// Learns from a response. Routes Discord does not limit carry no bucket
    /// id, and are ignored.
    pub fn after(&mut self, route: &str, h: &RateLimitHeaders, now: Instant) {
        if h.global
            && let Some(retry) = h.retry_after
        {
            self.global_until = Some(now + secs(retry));
        }

        let Some(id) = &h.bucket else { return };
        self.routes.insert(route.to_owned(), id.clone());

        // On a 429, retry_after wins, so a lying `remaining` cannot override
        // it.
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

    /// For diagnostics.
    pub fn is_globally_limited(&self, now: Instant) -> bool {
        self.global_until.is_some_and(|u| u > now)
    }

    /// For diagnostics.
    pub fn known_buckets(&self) -> usize {
        self.buckets.len()
    }
}

/// Seconds to a [`Duration`], surviving negatives and NaN.
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

    #[test]
    fn an_unknown_route_is_never_delayed() {
        let rl = RateLimiter::new();
        assert_eq!(rl.before("POST /channels/1/messages", Instant::now()), None);
    }

    #[test]
    fn it_waits_before_sending_when_the_bucket_is_empty() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();
        let route = "POST /channels/1/messages";

        rl.after(route, &headers("62df3a8b", 0, 0.5), now);
        let wait = rl.before(route, now).expect("should wait");
        assert!(wait > Duration::from_millis(400) && wait <= Duration::from_millis(500));

        rl.after(route, &headers("62df3a8b", 3, 5.0), now);
        assert_eq!(rl.before(route, now), None);

        // Past the reset time.
        rl.after(route, &headers("62df3a8b", 0, 0.5), now);
        assert_eq!(rl.before(route, now + Duration::from_secs(1)), None);
    }

    /// The reason for the two-level mapping.
    #[test]
    fn routes_sharing_a_bucket_limit_each_other() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();

        rl.after("GET /a", &headers("shared", 0, 1.0), now);
        rl.after("GET /b", &headers("shared", 0, 1.0), now);

        assert_eq!(rl.known_buckets(), 1);
        assert!(rl.before("GET /a", now).is_some());
        assert!(rl.before("GET /b", now).is_some());
    }

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
        // Even a route never touched before.
        assert!(rl.before("GET /users/@me", now).is_some());
        assert_eq!(
            rl.before("GET /users/@me", now + Duration::from_secs(3)),
            None
        );
    }

    #[test]
    fn a_429_overrides_whatever_remaining_says() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();
        let route = "POST /channels/1/messages";

        rl.after(
            route,
            &RateLimitHeaders {
                bucket: Some("62df3a8b".into()),
                // Claims headroom, yet returned a 429.
                remaining: Some(5),
                retry_after: Some(1.5),
                ..Default::default()
            },
            now,
        );

        let wait = rl.before(route, now).expect("should wait after a 429");
        assert!(wait > Duration::from_millis(1400));
    }

    #[test]
    fn nonsense_values_do_not_break_it() {
        let now = Instant::now();
        let mut rl = RateLimiter::new();
        let route = "GET /x";

        for bad in [f64::NAN, f64::INFINITY, -5.0] {
            rl.after(route, &headers("b", 0, bad), now);
            // Either answer is acceptable; panicking is not.
            let _ = rl.before(route, now);
        }

        // Headers without a bucket id are ignored.
        rl.after(route, &RateLimitHeaders::default(), now);
    }
}
