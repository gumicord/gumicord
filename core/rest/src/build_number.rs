//! Measures the build number to claim, at startup.
//!
//! A number baked into the source goes stale in weeks, and a client claiming
//! a months-old build is itself distinctive — almost no real installation is
//! that far behind.
//!
//! `https://discord.com/login` carries it in the HTML:
//!
//! ```text
//! window.GLOBAL_ENV = {"NODE_ENV":"production", … ,"BUILD_NUMBER":"595897", … }
//! ```
//!
//! Reading 60 KB of HTML is enough; the JS bundles need not be fetched.
//!
//! Failure falls back to the baked-in value rather than blocking startup: no
//! network, a changed page shape, or a proxy in the way are all ordinary.
//!
//! This runs before login, so it builds its own [`reqwest::Client`] and sends
//! neither a token nor `X-Super-Properties`.

use std::time::Duration;

use gumicord_model::identity;

/// The page that carries the number — HTML, not a JS bundle.
const LOGIN_PAGE: &str = "https://discord.com/login";

const MARKER: &str = "\"BUILD_NUMBER\"";

/// Kept short: this sits in front of login, and freezing for thirty seconds
/// on a dead network is worse than using the fallback.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Plausible range. A number outside it means the page shape changed, and
/// falling back beats claiming something odd.
const PLAUSIBLE: std::ops::RangeInclusive<u64> = 100_000..=99_999_999;

/// Fetches the build number and records it.
///
/// Call before constructing a [`crate::RestClient`] or connecting the
/// Gateway; recording it afterwards leaves one of them claiming the stale
/// number.
pub async fn measure() -> Option<u64> {
    let build = fetch().await?;
    identity::set_measured_build_number(build);
    tracing::info!(build, "measured the client build number");
    Some(build)
}

async fn fetch() -> Option<u64> {
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(identity::Identity::detect().user_agent())
        .build()
        .inspect_err(|e| tracing::warn!(%e, "cannot build the probe client; using the fallback"))
        .ok()?;

    let html = async {
        let res = http.get(LOGIN_PAGE).send().await?;
        let status = res.status();
        if !status.is_success() {
            tracing::warn!(%status, "login page returned an error; using the fallback");
            return Ok(None);
        }
        res.text().await.map(Some)
    }
    .await
    .inspect_err(|e: &reqwest::Error| {
        // Not exceptional; there may simply be no network.
        tracing::warn!(%e, "cannot reach the login page; using the fallback");
    })
    .ok()
    .flatten()?;

    let found = extract(&html);
    if found.is_none() {
        // The page shape may have changed. Never log the body itself.
        tracing::warn!(
            bytes = html.len(),
            "no {MARKER} on the login page; using the fallback"
        );
    }
    found
}

/// Extracts the build number from the page. Pure; touches no network.
///
/// Accepts both `"BUILD_NUMBER":"595897"` and `"BUILD_NUMBER": 595897`, since
/// which one arrives is Discord's choice.
pub fn extract(html: &str) -> Option<u64> {
    let after = &html[html.find(MARKER)? + MARKER.len()..];
    let after = after.strip_prefix(':')?;
    let digits: String = after
        .chars()
        .skip_while(|c| c.is_whitespace() || *c == '"')
        .take_while(char::is_ascii_digit)
        .collect();

    let build: u64 = digits.parse().ok()?;
    if !PLAUSIBLE.contains(&build) {
        tracing::warn!(build, "implausible build number; using the fallback");
        return None;
    }
    Some(build)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape actually returned on 2026-08-24.
    const REAL: &str = r#"<script>window.GLOBAL_ENV = {"NODE_ENV":"production","BUILT_AT":"1787095329146","HTML_TIMESTAMP":Date.now(),"BUILD_NUMBER":"595897","PROJECT_ENV":"production","RELEASE_CHANNEL":"stable"};</script>"#;

    #[test]
    fn it_reads_the_real_shape() {
        assert_eq!(extract(REAL), Some(595_897));
    }

    #[test]
    fn it_reads_an_unquoted_number_too() {
        assert_eq!(extract(r#"{"BUILD_NUMBER":595897}"#), Some(595_897));
        assert_eq!(extract(r#"{"BUILD_NUMBER": "595897"}"#), Some(595_897));
    }

    #[test]
    fn a_missing_marker_yields_nothing() {
        assert_eq!(extract("<html><body>login</body></html>"), None);
        assert_eq!(extract(""), None);
    }

    #[test]
    fn an_implausible_number_is_rejected() {
        assert_eq!(extract(r#""BUILD_NUMBER":"0""#), None);
        assert_eq!(extract(r#""BUILD_NUMBER":"12""#), None);
        assert_eq!(extract(r#""BUILD_NUMBER":"123456789012345""#), None);
    }

    #[test]
    fn a_non_numeric_value_is_rejected() {
        assert_eq!(extract(r#""BUILD_NUMBER":null"#), None);
        assert_eq!(extract(r#""BUILD_NUMBER":"stable""#), None);
        assert_eq!(extract(r#""BUILD_NUMBER" = "595897""#), None);
    }

    /// Truncated responses are ordinary.
    #[test]
    fn a_truncated_page_does_not_panic() {
        for n in 0..REAL.len() {
            let _ = extract(&REAL[..n]);
        }
    }

    /// `find` returns a char boundary, so slicing stays valid.
    #[test]
    fn multibyte_text_does_not_panic() {
        assert_eq!(extract(r#"あ"BUILD_NUMBER":"595897"い"#), Some(595_897));
    }
}
