//! 名乗るビルド番号を**起動時に実測する** (`NFR-020`)。
//!
//! # なぜ埋め込みでは駄目なのか
//!
//! ソースに書いた番号は数週間で古くなる。古い番号を名乗るクライアントは
//! **「何か月も更新していない Discord」**に見える。実際にはそんな端末は
//! ほとんど無いので、埋め込みの値はそれ自体が目印になる。
//!
//! # どこに本物があるか
//!
//! `https://discord.com/login` が返す HTML の中に、こういう塊がある。
//!
//! ```text
//! window.GLOBAL_ENV = {"NODE_ENV":"production","BUILT_AT":"...",
//!   "BUILD_NUMBER":"595897","RELEASE_CHANNEL":"stable", ... }
//! ```
//!
//! **JS の束を落としてくる必要はない。** HTML の 60 KB を読むだけで足りる。
//!
//! # ⚠️ 取れなくても起動を止めない
//!
//! 網が落ちている、Discord が形を変えた、串の向こうにいる — どれも普通に
//! 起こる。取れなければ [`gumicord_model::identity`] の埋め込みに落ちる。
//! **ここで止めると、番号が古いだけの理由でアプリが起動しなくなる。**
//!
//! # ⚠️ トークンを載せない
//!
//! ここはログイン前に呼ばれる。**専用の [`reqwest::Client`] を作り**、
//! [`crate::RestClient`] は通さない。認証ヘッダも `X-Super-Properties` も
//! 送らない — ただの匿名の GET である。

use std::time::Duration;

use gumicord_model::identity;

/// 番号の載っているページ。**JS の束ではなく HTML である**
const LOGIN_PAGE: &str = "https://discord.com/login";

/// `GLOBAL_ENV` の中の目印
const MARKER: &str = "\"BUILD_NUMBER\"";

/// ここまでに返らなければ諦める。
///
/// ⚠️ **長くしない。** ログインの手前に挟まる待ち時間である。届かない網の
/// 前で 30 秒固まるより、埋め込みで進むほうがましである
const TIMEOUT: Duration = Duration::from_secs(5);

/// あり得るビルド番号の範囲。
///
/// 桁が違うものを掴んだら、それは番号ではなく形が変わった合図である。
/// **黙って変な値を名乗るより、落ちる先に落ちるほうがよい**
const PLAUSIBLE: std::ops::RangeInclusive<u64> = 100_000..=99_999_999;

/// 取りに行って据える。**取れたら `Some`、駄目なら `None`。**
///
/// ⚠️ **[`crate::RestClient`] を作るより前に呼ぶこと。** 後から呼ぶと、
/// 先に作ったものだけが古い番号を名乗り、Gateway と REST で食い違う。
pub async fn measure() -> Option<u64> {
    let build = fetch().await?;
    identity::set_measured_build_number(build);
    tracing::info!(build, "ビルド番号を実測した");
    Some(build)
}

/// 取りに行くだけ。据えない。
async fn fetch() -> Option<u64> {
    // ⚠️ ログイン前なので、トークンを持つ経路を通さない
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(identity::Identity::detect().user_agent())
        .build()
        .inspect_err(|e| tracing::warn!(%e, "実測用の HTTP を組めない。埋め込みで進む"))
        .ok()?;

    let html = async {
        let res = http.get(LOGIN_PAGE).send().await?;
        let status = res.status();
        if !status.is_success() {
            tracing::warn!(%status, "ログイン画面が {status} を返した。埋め込みで進む");
            return Ok(None);
        }
        res.text().await.map(Some)
    }
    .await
    .inspect_err(|e: &reqwest::Error| {
        // ⚠️ ここは異常ではない。網が無いだけかもしれない
        tracing::warn!(%e, "ビルド番号を取りに行けない。埋め込みで進む");
    })
    .ok()
    .flatten()?;

    let found = extract(&html);
    if found.is_none() {
        // 形が変わった可能性がある。**中身は出さない** (`SEC-001`)
        tracing::warn!(
            bytes = html.len(),
            "ログイン画面に {MARKER} が見当たらない。埋め込みで進む"
        );
    }
    found
}

/// HTML からビルド番号を取り出す。**網に触らない純粋な関数である。**
///
/// `"BUILD_NUMBER":"595897"` も `"BUILD_NUMBER": 595897` も読める。
/// Discord がどちらで書いてくるかは向こうの都合であって、こちらが
/// 決められることではない。
pub fn extract(html: &str) -> Option<u64> {
    let after = &html[html.find(MARKER)? + MARKER.len()..];
    // `:` と、その後ろの空白や引用符を読み飛ばす
    let after = after.strip_prefix(':')?;
    let digits: String = after
        .chars()
        .skip_while(|c| c.is_whitespace() || *c == '"')
        .take_while(char::is_ascii_digit)
        .collect();

    let build: u64 = digits.parse().ok()?;
    if !PLAUSIBLE.contains(&build) {
        tracing::warn!(build, "ビルド番号にしては桁が合わない。埋め込みで進む");
        return None;
    }
    Some(build)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-24 に実際に返ってきた形。**これが読めなければ意味がない**
    const 実物: &str = r#"<script>window.GLOBAL_ENV = {"NODE_ENV":"production","BUILT_AT":"1787095329146","HTML_TIMESTAMP":Date.now(),"BUILD_NUMBER":"595897","PROJECT_ENV":"production","RELEASE_CHANNEL":"stable"};</script>"#;

    #[test]
    fn 実物から取り出せる() {
        assert_eq!(extract(実物), Some(595_897));
    }

    /// 引用符が無い形で来ても読める。**向こうの都合で変わりうる**
    #[test]
    fn 引用符が無くても読める() {
        assert_eq!(extract(r#"{"BUILD_NUMBER":595897}"#), Some(595_897));
        assert_eq!(extract(r#"{"BUILD_NUMBER": "595897"}"#), Some(595_897));
    }

    /// 目印が無い。**形が変わったということなので、埋め込みに落ちる**
    #[test]
    fn 見当たらなければ何も返さない() {
        assert_eq!(extract("<html><body>ログイン</body></html>"), None);
        assert_eq!(extract(""), None);
    }

    /// ⚠️ **桁が違うものを掴んだら番号ではない。**
    ///
    /// 変な値を黙って名乗るより、落ちる先に落ちるほうがよい
    #[test]
    fn 桁が合わなければ捨てる() {
        assert_eq!(extract(r#""BUILD_NUMBER":"0""#), None);
        assert_eq!(extract(r#""BUILD_NUMBER":"12""#), None);
        assert_eq!(extract(r#""BUILD_NUMBER":"123456789012345""#), None);
    }

    /// 数字で始まらない。**推測で拾わない**
    #[test]
    fn 数字でなければ捨てる() {
        assert_eq!(extract(r#""BUILD_NUMBER":null"#), None);
        assert_eq!(extract(r#""BUILD_NUMBER":"stable""#), None);
        assert_eq!(extract(r#""BUILD_NUMBER" = "595897""#), None);
    }

    /// 途中で切れた HTML でも panic しない。**中途半端な応答は普通に来る**
    #[test]
    fn 途中で切れていても落ちない() {
        for n in 0..実物.len() {
            let _ = extract(&実物[..n]);
        }
    }

    /// ⚠️ **UTF-8 の境目で切らない。** `find` の戻りは文字境界である
    #[test]
    fn 日本語が混ざっていても落ちない() {
        assert_eq!(extract(r#"あ"BUILD_NUMBER":"595897"い"#), Some(595_897));
    }
}
