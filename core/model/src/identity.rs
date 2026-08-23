//! What this client claims to be.
//!
//! Discord receives the same claim twice: in the Gateway `identify`
//! `properties` and, base64-encoded, in the REST `X-Super-Properties` header.
//! They must not disagree — a disagreement is itself a signal — so both are
//! built from one source, including the HTTP `User-Agent`.
//!
//! The build number is measured at startup from the login page and pushed in
//! through [`set_measured_build_number`]; [`BUILD_NUMBER`] is only the
//! fallback when discord.com is unreachable. `CLIENT_VERSION` cannot be
//! measured: it lives in the desktop executable, not in anything served.
//!
//! `GUMICORD_CLIENT_BUILD` and `GUMICORD_CLIENT_VERSION` override both.
//!
//! Connecting with a user token from a non-official client violates
//! Discord's terms of service. Matching the claim makes detection harder, not
//! safe. See `spec/09-discord-protocol.md`.

use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;

/// Desktop client version claimed. Goes stale; cannot be measured.
const CLIENT_VERSION: &str = "1.0.9250";
/// Build number claimed when measurement fails.
///
/// Measured from `https://discord.com/login` on 2026-08-24. It goes stale in
/// weeks, but nothing reads it while measurement works.
const BUILD_NUMBER: u64 = 595_897;
/// Chromium version inside Electron.
const CHROME_VERSION: &str = "134.0.6998.205";
const ELECTRON_VERSION: &str = "35.7.5";

/// Build number measured at startup. Zero means "not measured yet".
static MEASURED_BUILD_NUMBER: AtomicU64 = AtomicU64::new(0);

/// Records a measured build number for later [`Identity::detect`] calls.
///
/// Call this before constructing any [`Identity`]. Setting it afterwards
/// leaves the earlier one claiming the stale number, which makes the two
/// transports disagree — worse than not measuring at all.
pub fn set_measured_build_number(build: u64) {
    MEASURED_BUILD_NUMBER.store(build, Ordering::Relaxed);
}

pub fn measured_build_number() -> Option<u64> {
    match MEASURED_BUILD_NUMBER.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
    }
}

/// The fallback build number. Public for logging and tests only.
pub const fn fallback_build_number() -> u64 {
    BUILD_NUMBER
}

/// Environment beats measurement beats the baked-in value.
///
/// Pure, taking both inputs as arguments, so the precedence itself is
/// testable without touching process-wide state.
const fn resolve_build_number(from_env: Option<u64>, measured: Option<u64>) -> u64 {
    match (from_env, measured) {
        (Some(n), _) => n,
        (None, Some(n)) => n,
        (None, None) => BUILD_NUMBER,
    }
}

/// What this client claims to be. Shared by the Gateway and REST.
#[derive(Debug, Clone)]
pub struct Identity {
    pub os: &'static str,
    pub browser: &'static str,
    pub device: &'static str,
    pub system_locale: String,
    pub browser_user_agent: String,
    pub browser_version: String,
    pub os_version: String,
    pub release_channel: &'static str,
    pub client_version: String,
    pub client_build_number: u64,
}

impl Default for Identity {
    fn default() -> Self {
        Identity::detect()
    }
}

impl Identity {
    pub fn detect() -> Identity {
        let client_version =
            env_or("GUMICORD_CLIENT_VERSION", Some).unwrap_or_else(|| CLIENT_VERSION.into());
        let client_build_number = resolve_build_number(
            env_or("GUMICORD_CLIENT_BUILD", |s| s.parse().ok()),
            measured_build_number(),
        );

        Identity {
            os: os_name(),
            browser: "Discord Client",
            // Desktop sends an empty device; anything else reads as mobile.
            device: "",
            system_locale: locale(),
            browser_user_agent: user_agent(&client_version),
            browser_version: ELECTRON_VERSION.to_owned(),
            os_version: os_version(),
            release_channel: "stable",
            client_version,
            client_build_number,
        }
    }

    /// The shape that goes into the Gateway `identify`.
    pub fn properties(&self) -> serde_json::Value {
        serde_json::json!({
            "os": self.os,
            "browser": self.browser,
            "device": self.device,
            "system_locale": self.system_locale,
            "browser_user_agent": self.browser_user_agent,
            "browser_version": self.browser_version,
            "os_version": self.os_version,
            "referrer": "",
            "referring_domain": "",
            "referrer_current": "",
            "referring_domain_current": "",
            "release_channel": self.release_channel,
            "client_build_number": self.client_build_number,
            "client_event_source": serde_json::Value::Null,
            "client_version": self.client_version,
            "native_build_number": serde_json::Value::Null,
        })
    }

    /// The `X-Super-Properties` header, built from [`Self::properties`] so the
    /// two transports cannot drift apart.
    pub fn super_properties(&self) -> String {
        let json = serde_json::to_vec(&self.properties()).unwrap_or_default();
        base64::engine::general_purpose::STANDARD.encode(json)
    }

    /// The HTTP `User-Agent`, identical to `browser_user_agent`.
    pub fn user_agent(&self) -> &str {
        &self.browser_user_agent
    }
}

fn user_agent(client_version: &str) -> String {
    format!(
        "Mozilla/5.0 ({ua_os}) AppleWebKit/537.36 (KHTML, like Gecko) \
         discord/{client_version} Chrome/{CHROME_VERSION} Electron/{ELECTRON_VERSION} Safari/537.36",
        ua_os = ua_os(),
    )
}

/// Capitalised: `std::env::consts::OS` says `windows`, Discord expects
/// `Windows`.
const fn os_name() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Mac OS X"
    } else {
        "Linux"
    }
}

const fn ua_os() -> &'static str {
    if cfg!(windows) {
        "Windows NT 10.0; Win64; x64"
    } else if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else {
        "X11; Linux x86_64"
    }
}

/// A version any machine could plausibly report. Sending the real one helps
/// nobody and getting it wrong stands out.
fn os_version() -> String {
    if cfg!(windows) {
        "10.0.26100".to_owned()
    } else if cfg!(target_os = "macos") {
        "26.0.0".to_owned()
    } else {
        String::new()
    }
}

/// The system language, falling back to English.
pub fn locale() -> String {
    sys_locale::get_locale()
        .map(|l| l.replace('_', "-"))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "en-US".to_owned())
}

/// Offset from UTC in minutes, for `X-Discord-Timezone`.
///
/// The region name is not claimed: it is not available here, and deriving it
/// from the offset gets it wrong.
pub fn timezone_offset_minutes() -> i32 {
    0
}

fn env_or<T>(key: &str, f: impl FnOnce(String) -> Option<T>) -> Option<T> {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_transports_claim_the_same_thing() {
        let id = Identity::detect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(id.super_properties())
            .expect("malformed base64");
        let parsed: serde_json::Value = serde_json::from_slice(&decoded).expect("malformed JSON");
        assert_eq!(parsed, id.properties());
    }

    #[test]
    fn the_user_agent_matches_the_claim() {
        let id = Identity::detect();
        assert_eq!(id.user_agent(), id.properties()["browser_user_agent"]);
    }

    /// A stale user agent beside a fresh version is itself a mismatch.
    #[test]
    fn changing_the_version_carries_the_whole_claim() {
        let id = Identity {
            client_version: "9.9.9999".to_owned(),
            browser_user_agent: user_agent("9.9.9999"),
            ..Identity::detect()
        };
        assert!(id.user_agent().contains("discord/9.9.9999"));
        assert_eq!(id.properties()["client_version"], "9.9.9999");
    }

    #[test]
    fn desktop_claims_no_device() {
        assert_eq!(Identity::detect().device, "");
    }

    #[test]
    fn the_os_name_is_capitalised() {
        let os = Identity::detect().os;
        assert!(
            os.chars().next().is_some_and(char::is_uppercase),
            "lowercase os name: {os}"
        );
    }

    #[test]
    fn without_measurement_or_override_the_baked_in_value_is_used() {
        assert_eq!(resolve_build_number(None, None), fallback_build_number());
    }

    #[test]
    fn a_measured_build_number_wins_over_the_baked_in_one() {
        // Differ from the fallback, or the test proves nothing.
        let measured = fallback_build_number() + 1;
        assert_eq!(resolve_build_number(None, Some(measured)), measured);
        assert_ne!(
            resolve_build_number(None, Some(measured)),
            fallback_build_number()
        );
    }

    /// Otherwise `GUMICORD_CLIENT_BUILD` would be meaningless.
    #[test]
    fn the_environment_wins_over_measurement() {
        assert_eq!(resolve_build_number(Some(451_672), Some(595_897)), 451_672);
    }

    #[test]
    fn a_measured_build_number_reads_back() {
        set_measured_build_number(123_456);
        assert_eq!(measured_build_number(), Some(123_456));
        // Process-wide state; restore it for the other tests.
        set_measured_build_number(0);
        assert_eq!(measured_build_number(), None);
    }
}
