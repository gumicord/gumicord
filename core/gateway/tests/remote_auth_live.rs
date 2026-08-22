//! 本物の Discord に繋いで、QR を出せるところまで進むか確かめる。
//!
//! **既定では走らない。** 網に出る試験を CI と開発機の `cargo test` に
//! 混ぜると、Discord が落ちているだけで赤くなる。
//!
//! ```text
//! cargo test -p gumicord-gateway --test remote_auth_live -- --ignored --nocapture
//! ```
//!
//! ここまで通れば、残るのは「読み取って承認する」人間の側だけである。

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
            // ⚠️ 指紋は我々の公開鍵の SHA-256 である。サーバの言い値を
            // そのまま QR にしない
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
