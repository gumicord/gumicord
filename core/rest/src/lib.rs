//! Discord REST client: requests, rate limit avoidance, and 429 backoff.
//!
//! Rate limits apply per *bucket*, not per route, so routes map to buckets and
//! buckets map to state. Sequential user-driven requests never exhaust a
//! bucket — round-trip latency alone spaces them out — so the limiter only
//! matters for concurrent bursts.
//!
//! See `spec/09-discord-protocol.md`.

pub mod auth;
pub mod build_number;
pub mod channel;
pub mod client;
pub mod ratelimit;
pub mod route;

pub use client::{CaptchaChallenge, RestClient, RestError};
pub use ratelimit::{RateLimitHeaders, RateLimiter};
pub use route::{Method, Route};
