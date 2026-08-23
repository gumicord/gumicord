//! 本物の `https://discord.com/login` から、いまのビルド番号を実測できるか。
//!
//! **既定では走らない。** 網に出る試験を CI と開発機の `cargo test` に
//! 混ぜると、Discord が落ちているだけで赤くなる。
//!
//! ```text
//! cargo test -p gumicord-rest --test build_number_live -- --ignored --nocapture
//! ```
//!
//! ⚠️ **単体試験が緑でも、これが赤いことはある。** 単体試験が確かめるのは
//! 「保存した形を読めるか」であって、**Discord がいまその形で返してくるか**
//! ではない。形が変わったことは、ここでしか分からない。

use gumicord_model::identity::{self, Identity};

#[tokio::test]
#[ignore = "本物の Discord に繋ぐ。--ignored で明示的に走らせる"]
async fn いまのビルド番号を実測できる() {
    let build = gumicord_rest::build_number::measure()
        .await
        .expect("ログイン画面からビルド番号を取り出せない (形が変わった可能性がある)");

    println!("実測: {build}");
    println!("埋め込み: {}", identity::fallback_build_number());

    assert_eq!(
        identity::measured_build_number(),
        Some(build),
        "測ったのに据えられていない"
    );

    // ⚠️ **名乗りに載っているか**まで見る。据えただけでは意味がない
    let id = Identity::detect();
    if std::env::var_os("GUMICORD_CLIENT_BUILD").is_none() {
        assert_eq!(
            id.client_build_number, build,
            "実測したのに名乗りが古いままである"
        );
    }
    assert_eq!(
        id.properties()["client_build_number"],
        id.client_build_number,
        "名乗りの中で食い違っている"
    );

    // 埋め込みが実測から離れすぎていたら、落ちる先として役に立たなくなって
    // いる。**止めはしないが、書き換える合図である**
    let 差 = build.abs_diff(identity::fallback_build_number());
    if 差 > 20_000 {
        println!("⚠️ 埋め込みが {差} ぶん古い。identity.rs の BUILD_NUMBER を書き換えること");
    }
}
