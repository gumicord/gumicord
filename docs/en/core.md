# core: protocol and state

Discord protocol (`model` / `rest` / `gateway`) and normalized state
(`store`). Governing spec: [`spec/09-discord-protocol.md`](../../spec/09-discord-protocol.md).

# gumicord-model (`core/model`)

> Discord domain types and serialization only; no behavior. Declares only
> the fields in use and absorbs unknown shapes instead of dropping them.

## Files

### `src/lib.rs` — Core domain types `User`, `Guild`, `Channel`, `Message`.
A `Guild` absorbs three shapes (bot, user-token `properties`, unavailable)
without dropping the whole list on one broken element. Key items: `User`,
`Guild`, `Channel`, `Message`, `ChannelKind`, `fn display_name()`.
Display names resolve nick → global_name → username; avatars resolve
per-guild → user → default.

### `src/token.rs` — Auth token wrapper. `Debug`/`Display` are fixed to
redacted; the value only comes out through `expose`. Key items: `Token`,
`TokenKind`, `fn new()`, `fn bot()`, `fn expose()`, `fn is_absent_from()`.

### `src/snowflake.rs` — Typed 64-bit Discord IDs. JSON strings are
canonical but numbers are accepted; mixing types fails at compile time.
Key items: `Snowflake`, `GuildId`, `ChannelId`, `fn get()`,
`fn created_at_ms()`. ID order is creation order; Unix millis are
recoverable from the high bits.

### `src/identity.rs` — Client-claim credentials shared by gateway and
REST. Generates `super_properties` and User-Agent from one `properties`
source so the two paths never disagree. Key items: `Identity`,
`fn detect()`, `fn properties()`, `fn super_properties()`,
`fn user_agent()`. Build numbers resolve env var → measured at startup →
embedded fallback, in that order.

### `src/de.rs` — Lenient deserialization helpers. One unreadable list
element does not drop the whole list. Key items: `fn lenient_vec()`.

### `src/asset.rs` — CDN image locations separated from request modifiers
(size, format). Key items: `Asset`, `Format`, `fn user_avatar()`,
`fn url()`, `fn with_size()`. Animated assets are still requested as PNG
(the decoder only reads PNG); sizes round up to powers of two.

# gumicord-rest (`core/rest`)

> Client for the Discord REST API: requests, rate-limit avoidance, 429
> recovery. Holds nothing it fetches; state belongs to the store.

## Files

### `src/lib.rs` — Re-export aggregation over the modules.

### `src/client.rs` — Request sending, pre-waiting, 429 retries (up to
`MAX_RETRIES`, API v9 pinned). Key items: `RestClient`, `RestError`,
`CaptchaChallenge`, `SolvedCaptcha`, `fn send()`. Bots send `Bot <token>`
with a dedicated UA; users send a bare token with super-properties
headers. Only 401 counts as dead credentials. API error bodies surface
the `message` field (which also decodes the `\uXXXX` escapes JSON carries).

### `src/route.rs` — Paths separated from rate-limit keys (major parameters
only). Message IDs, limits, before/around stay out of keys so buckets stay
shared. Key items: `Route`, `Method`, `fn create_message()`,
`fn messages()`, `fn delete_message()`, `fn current_user()`.

### `src/ratelimit.rs` — Non-sleeping pre-wait judge. Two maps
(route → bucket, bucket → state) cover shared buckets and the global
limit. Key items: `RateLimiter`, `RateLimitHeaders`, `fn before()`,
`fn after()`. A 429 `retry_after` wins; malformed values degrade without
breaking.

### `src/auth.rs` — Login REST calls. Password/MFA/QR ticket exchange and
token checks live here. Key items: `LoginOutcome`, `fn login()`,
`fn mfa_totp()`, `fn remote_auth_login()`, `fn current_user()`. Captcha
solutions travel in headers.

### `src/channel.rs` — Channel and message REST calls. History comes back
newest-first; ordering stays with the caller. Key items:
`fn create_message()`, `fn messages()`, `fn messages_before()`,
`fn edit_message()`, `fn delete_message()`, `fn fetch_cdn()`. Replies set
no notification suppression with `fail_if_not_exists=false`; CDN fetches
are unauthenticated, capped at 4MB, and handled separately.

### `src/build_number.rs` — Measures `BUILD_NUMBER` from the login page
HTML at startup and records it into the identity. Failure falls back to
the embedded value without stopping startup. Key items: `fn measure()`,
`fn extract()`.

### `tests/build_number_live.rs` — Tests-only. Reachability check against
real Discord (explicit `--ignored` runs).

# gumicord-gateway (`core/gateway`)

> Discord gateway connection: connect, identify, heartbeats, resume, zstd,
> dispatch. Callers pump `next` and reconnection proceeds inside.

## Files

### `src/lib.rs` — Re-export aggregation plus idempotent rustls provider
setup (`fn install_crypto_provider()`).

### `src/gateway.rs` — WebSocket connection, Hello, identify/resume,
heartbeats, resends. Retries with exponential backoff on anything but
`Fatal`; resumes against the region host from READY. Key items: `Gateway`,
`Event`, `Ready`, `Fatal`, `Subscriptions`, `fn next()`. Missing ACKs
mean a dead connection; only auth failures are fatal. User tokens take
op-14 subscriptions and op-8 member requests; bots step down intents on
rejection. Subscriptions belong to the connection and are resent on every
READY/resume.

### `src/proto.rs` — Minimal protobuf wire reader for the undocumented
`user_settings_proto`. No generated types; walks varints, fixed and
length-delimited fields, returning partial results on malformed input
instead of panicking. Key items: `fn blocks()`, `fn varint()`,
`fn wrapped_string()`.

### `src/status.rs` — Own-status extraction. Connected does not mean
online; unknown values are not rounded to online. Key items: `Status`,
`fn from_settings_proto()`, `fn from_wire()`, `fn as_wire()`.

### `src/remote_auth.rs` — QR login (RSA-2048, nonce proof, fingerprint
recomputation, ticket approval). The private key never leaves the struct.
Key items: `RemoteAuth`, `RemoteAuthEvent`, `ScannedUser`, `fn connect()`,
`fn next()`, `fn decrypt_token()`. The server-sent fingerprint is checked
against our own public-key hash; mismatch never becomes a QR.

### `src/member_list.rs` — Range-subscription member-list diffs. Headings
count as rows; SYNC/INSERT/UPDATE/DELETE/INVALIDATE fold by position. Key
items: `MemberList`, `MemberRow`, `fn parse()`, `fn apply()`, `fn rows()`.
Out-of-range diffs are dropped, unknown ops are ignored without guessing,
missing presence reads as offline.

### `src/guild_order.rs` — Guild order and folder extraction from the
base64 protobuf in READY. Key items: `Folder`,
`fn from_settings_proto()`. Ragged orders are dropped in favor of arrival
order.

### `src/zstd_stream.rs` — zstd-stream decoding. The whole connection is
one stream, so the decoder lives as long as the connection and is rebuilt
on reconnect. Key items: `ZstdStream`, `fn new()`, `fn push()`. One frame
can yield zero or several JSON payloads.

### `tests/remote_auth_live.rs` — Tests-only. QR display check against real
Discord (explicit `--ignored` runs).

# gumicord-store (`core/store`)

> Normalized in-memory state plus SQLite persistence. Draws from here
> before the gateway is up at startup; READY replaces it.

## Files

### `src/lib.rs` — Normalized `Store` (guilds, channels, messages, order,
read marks, notifications, members). Key items: `Store`, `ReadMark`,
`NotifLevel`, `fn guilds()`, `fn replace_guilds()`, `fn push_message()`.
Unread compares snowflakes, not counts; history edits replace, never add
unknown rows.

### `src/db.rs` — Local SQLite cache. Synchronous reads at startup,
fire-and-forget writes to a single writer thread; failures are logged,
never fatal. Key items: `Db`, `Snapshot`, `fn open()`,
`fn save_guilds()`, `fn save_messages()`. IDs are stored as TEXT, schema
mismatches rebuild instead of migrating, channels are pruned to 200
messages, and message bodies are plaintext — so sign-out wipes everything.
