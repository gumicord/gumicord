//! Connects to the real Discord and checks it gets as far as a showable QR.
//!
//! Ignored by default: a test that goes out to the network would turn CI and a
//! plain `cargo test` red whenever Discord is down.
//!
//! ```text
//! cargo test -p gumicord-gateway --test remote_auth_live -- --ignored --nocapture
//! ```
//!
//! Past this point only the human scanning and approving remains.

use gumicord_gateway::{RemoteAuth, RemoteAuthEvent};

#[tokio::test]
#[ignore = "本物の Discord に繋ぐ。--ignored で明示的に走らせる"]
async fn we_can_get_as_far_as_showing_a_qr() {
    let mut auth = RemoteAuth::connect().await.expect("接続できない");

    let event = tokio::time::timeout(std::time::Duration::from_secs(30), auth.next())
        .await
        .expect("30 秒以内に応答が無い")
        .expect("やりとりが失敗した");

    match event {
        RemoteAuthEvent::Ready { url, fingerprint } => {
            // The fingerprint is our own public key's SHA-256; the server's
            // word for it never reaches the QR.
            assert_eq!(
                fingerprint,
                auth.expected_fingerprint(),
                "指紋がこちらの鍵と一致しない"
            );
            assert!(
                url.contains(&fingerprint),
                "QR の URL に指紋が入っていない: {url}"
            );
            println!("QR: {url}");
        }
        other => panic!("Ready を待っていたが {other:?} が来た"),
    }
}
