//! Checks that the current build number can still be measured from
//! `https://discord.com/login`.
//!
//! Ignored by default: a network test in CI turns red whenever Discord is
//! down.
//!
//! ```text
//! cargo test -p gumicord-rest --test build_number_live -- --ignored --nocapture
//! ```
//!
//! The unit tests can be green while this is red. They check that a saved
//! shape parses, not that Discord still sends that shape.

use gumicord_model::identity::{self, Identity};

#[tokio::test]
#[ignore = "hits the real Discord; run explicitly with --ignored"]
async fn the_current_build_number_can_be_measured() {
    let build = gumicord_rest::build_number::measure()
        .await
        .expect("no build number on the login page; the shape may have changed");

    println!("measured: {build}");
    println!("fallback: {}", identity::fallback_build_number());

    assert_eq!(
        identity::measured_build_number(),
        Some(build),
        "measured but not recorded"
    );

    // Recording is not enough; it has to reach the claim.
    let id = Identity::detect();
    if std::env::var_os("GUMICORD_CLIENT_BUILD").is_none() {
        assert_eq!(
            id.client_build_number, build,
            "measured, but the claim is still stale"
        );
    }
    assert_eq!(
        id.properties()["client_build_number"],
        id.client_build_number,
        "the claim disagrees with itself"
    );

    // Far enough behind and the fallback is no longer a useful landing spot.
    let drift = build.abs_diff(identity::fallback_build_number());
    if drift > 20_000 {
        println!("fallback is {drift} behind; update BUILD_NUMBER in identity.rs");
    }
}
