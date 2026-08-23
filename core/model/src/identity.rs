//! こちらが何者だと名乗るか (`NFR-020`)。
//!
//! # なぜ 1 か所にまとめるのか
//!
//! Discord は同じ内容を **2 つの経路**で受け取る。
//!
//! | 経路 | 形 |
//! |---|---|
//! | Gateway の `identify` | `d.properties` にそのまま |
//! | REST の全リクエスト | `X-Super-Properties` に base64 で |
//!
//! ⚠️ **食い違ってはいけない。** 「Gateway では Windows のデスクトップ、
//! REST では別の何か」は、どちらか片方が嘘だということである。**食い違い
//! そのものが目印になる。** だから出所を 1 つにする。
//!
//! `User-Agent` も同じ物から作る。`browser_user_agent` に書いた文字列と
//! HTTP の `User-Agent` が違えば、それもまた食い違いである。
//!
//! # ⚠️ ここの値は古くなる
//!
//! [`BUILD_NUMBER`] と [`CLIENT_VERSION`] は**書いた時点の推測**である。
//! 本物は Discord の配信物の中にあり、数週間で変わる。古いまま名乗ると
//! 「更新していないクライアント」に見える。
//!
//! 環境変数で差し替えられるようにしてある:
//!
//! ```text
//! GUMICORD_CLIENT_BUILD=451000
//! GUMICORD_CLIENT_VERSION=1.0.9250
//! ```
//!
//! # ⚠️ 利用規約について
//!
//! **公式クライアント以外から利用者トークンで接続することは、Discord の
//! 利用規約に反する。** ここを整えると見分けが付きにくくはなるが、
//! **安全になるわけではない。** アカウントを失う可能性は消えない。
//!
//! この判断は利用者が引き受けるものであり、ここはその判断を実装している
//! だけである。

use base64::Engine as _;

/// 名乗るデスクトップクライアントの版。**古くなる。** モジュールの説明を見よ
const CLIENT_VERSION: &str = "1.0.9250";
/// 名乗るビルド番号。**古くなる。**
///
/// ⚠️ **これは書いた時点の推測であり、確かめた値ではない。**
/// 本物は `https://discord.com/app` が読み込む JS の中の `build_number`
/// にある。環境変数 `GUMICORD_CLIENT_BUILD` で差し替えられる
const BUILD_NUMBER: u64 = 451_672;
/// 中で動いている Chromium の版
const CHROME_VERSION: &str = "134.0.6998.205";
/// 中で動いている Electron の版
const ELECTRON_VERSION: &str = "35.7.5";

/// こちらが何者か。**Gateway と REST で同じものを使う。**
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
    /// この機械の値で組み立てる。
    pub fn detect() -> Identity {
        let client_version =
            env_or("GUMICORD_CLIENT_VERSION", Some).unwrap_or_else(|| CLIENT_VERSION.into());
        let client_build_number =
            env_or("GUMICORD_CLIENT_BUILD", |s| s.parse().ok()).unwrap_or(BUILD_NUMBER);

        Identity {
            // ⚠️ **頭を大文字にする。** `std::env::consts::OS` は
            // `"windows"` を返すが、Discord が受け取っているのは `"Windows"`
            // である
            os: os_name(),
            browser: "Discord Client",
            // ⚠️ **デスクトップは空文字列である。** 何か入れると携帯に見える
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

    /// Gateway の `identify` に入れる形。
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

    /// REST の `X-Super-Properties` に入れる形。
    ///
    /// ⚠️ **`identify` と同じ物から作る。** 別々に組み立てると、片方だけ
    /// 直したときに食い違う
    pub fn super_properties(&self) -> String {
        let json = serde_json::to_vec(&self.properties()).unwrap_or_default();
        base64::engine::general_purpose::STANDARD.encode(json)
    }

    /// HTTP の `User-Agent`。**`browser_user_agent` と同じ文字列である。**
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

/// ⚠️ **細かく当てにいかない。** 本当のビルド番号を送っても誰も得をせず、
/// 外すと目立つ。どの機械でもあり得る値を送る
fn os_version() -> String {
    if cfg!(windows) {
        "10.0.26100".to_owned()
    } else if cfg!(target_os = "macos") {
        "26.0.0".to_owned()
    } else {
        String::new()
    }
}

/// システムの言語。分からなければ英語を名乗る。
pub fn locale() -> String {
    sys_locale::get_locale()
        .map(|l| l.replace('_', "-"))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "en-US".to_owned())
}

/// UTC からのずれ (分)。`X-Discord-Timezone` に使う。
///
/// ⚠️ **地域名までは名乗らない。** `Asia/Tokyo` のような名前は手元に無く、
/// ずれから逆に決めると外れる
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

    /// ⚠️ **2 つの経路で同じものを名乗ること。**
    ///
    /// 食い違いそのものが目印になる
    #[test]
    fn 二つの経路で同じことを名乗る() {
        let id = Identity::detect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(id.super_properties())
            .expect("base64 が壊れている");
        let parsed: serde_json::Value =
            serde_json::from_slice(&decoded).expect("JSON が壊れている");
        assert_eq!(parsed, id.properties());
    }

    /// ⚠️ **`User-Agent` と `browser_user_agent` が違えば、それも食い違いである**
    #[test]
    fn ユーザーエージェントは名乗りと同じ() {
        let id = Identity::detect();
        assert_eq!(id.user_agent(), id.properties()["browser_user_agent"]);
    }

    /// 版を差し替えたら、名乗り全体が付いてくること。
    ///
    /// ⚠️ 片方だけ古いままになると、**版と UA が食い違う**
    #[test]
    fn 版を差し替えると名乗り全体が変わる() {
        let id = Identity {
            client_version: "9.9.9999".to_owned(),
            browser_user_agent: user_agent("9.9.9999"),
            ..Identity::detect()
        };
        assert!(id.user_agent().contains("discord/9.9.9999"));
        assert_eq!(id.properties()["client_version"], "9.9.9999");
    }

    /// デスクトップの `device` は空である。**何か入れると携帯に見える**
    #[test]
    fn 机の上では_device_を名乗らない() {
        assert_eq!(Identity::detect().device, "");
    }

    /// ⚠️ `std::env::consts::OS` は `"windows"` を返す。Discord が
    /// 受け取っているのは `"Windows"` である
    #[test]
    fn os_の名前は頭が大文字() {
        let os = Identity::detect().os;
        assert!(
            os.chars().next().is_some_and(char::is_uppercase),
            "頭が小文字である: {os}"
        );
    }
}
