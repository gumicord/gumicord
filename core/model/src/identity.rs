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
//! 本物は Discord の配信物の中にあり、数週間で変わる。古いまま名乗ると
//! 「更新していないクライアント」に見える。
//!
//! ビルド番号は**起動時に実測する**。`https://discord.com/login` が返す
//! HTML の `GLOBAL_ENV` に `"BUILD_NUMBER":"595897"` の形で入っており、
//! 取ってきた値を [`set_measured_build_number`] へ渡すと以降の名乗りに載る。
//! 取りに行くのは `gumicord_rest::build_number` の仕事である。
//!
//! ⚠️ **取れなかったら [`BUILD_NUMBER`] に落ちる。** 起動は止めない。
//!
//! [`CLIENT_VERSION`] のほうは実測できていない。**配信物のどこにも無く、
//! デスクトップの実行ファイル自身が持っている値**だからである。
//!
//! 環境変数で差し替えられるようにしてある。**実測より環境変数が強い**:
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

use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;

/// 名乗るデスクトップクライアントの版。**古くなる。** モジュールの説明を見よ
const CLIENT_VERSION: &str = "1.0.9250";
/// 実測できなかったときに名乗るビルド番号。**古くなる。**
///
/// ⚠️ **これは落ちる先であって、当てにする値ではない。** 普段は起動時に
/// [`set_measured_build_number`] で本物が入る。ここが使われるのは
/// **discord.com に届かなかったとき**だけである。
///
/// 2026-08-24 に `https://discord.com/login` から実測した値を置いてある。
/// 数週間で古くなるが、実測が効いていれば誰も見ない
const BUILD_NUMBER: u64 = 595_897;
/// 中で動いている Chromium の版
const CHROME_VERSION: &str = "134.0.6998.205";
/// 中で動いている Electron の版
const ELECTRON_VERSION: &str = "35.7.5";

/// 起動時に実測したビルド番号。`0` は「まだ測っていない」を意味する
static MEASURED_BUILD_NUMBER: AtomicU64 = AtomicU64::new(0);

/// 実測したビルド番号を据える。以降の [`Identity::detect`] がこれを名乗る。
///
/// ⚠️ **[`Identity`] を作るより前に呼ぶこと。** 後から呼ぶと、先に作った
/// [`Identity`] だけが古い番号を名乗り、**経路の間で食い違う**。食い違い
/// そのものが目印になるので、遅れて据えるくらいなら据えないほうがましである。
///
/// ⚠️ **`GUMICORD_CLIENT_BUILD` のほうが強い。** 手で指定した値を実測が
/// 黙って上書きしたら、環境変数の意味がない
pub fn set_measured_build_number(build: u64) {
    MEASURED_BUILD_NUMBER.store(build, Ordering::Relaxed);
}

/// 実測できているならその値。まだなら `None`
pub fn measured_build_number() -> Option<u64> {
    match MEASURED_BUILD_NUMBER.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
    }
}

/// 実測が無いときに落ちる先。**記録と試験のためだけに公開している**
pub const fn fallback_build_number() -> u64 {
    BUILD_NUMBER
}

/// どのビルド番号を名乗るかを決める。**強い順に 環境変数 → 実測 → 埋め込み。**
///
/// ⚠️ **プロセス全体の状態を読まない純粋な関数にしてある。** 環境変数も
/// [`static`] も引数で受けるので、優先順位そのものを直接試験できる
const fn resolve_build_number(from_env: Option<u64>, measured: Option<u64>) -> u64 {
    match (from_env, measured) {
        (Some(n), _) => n,
        (None, Some(n)) => n,
        (None, None) => BUILD_NUMBER,
    }
}

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
        let client_build_number = resolve_build_number(
            env_or("GUMICORD_CLIENT_BUILD", |s| s.parse().ok()),
            measured_build_number(),
        );

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

    /// 何も無ければ埋め込みに落ちる。**起動を止めない**
    #[test]
    fn 実測も指定も無ければ埋め込みに落ちる() {
        assert_eq!(resolve_build_number(None, None), fallback_build_number());
    }

    /// 実測できたらそれを名乗る。埋め込みは見ない
    #[test]
    fn 実測できたら実測を名乗る() {
        // ⚠️ 埋め込みと違う値を使う。同じ値では効いているか分からない
        let 実測 = fallback_build_number() + 1;
        assert_eq!(resolve_build_number(None, Some(実測)), 実測);
        assert_ne!(
            resolve_build_number(None, Some(実測)),
            fallback_build_number()
        );
    }

    /// ⚠️ **手で指定した値を実測が黙って上書きしない。**
    ///
    /// 上書きすると `GUMICORD_CLIENT_BUILD` に意味がなくなり、古い番号を
    /// 名乗って試すことができなくなる
    #[test]
    fn 環境変数は実測より強い() {
        assert_eq!(resolve_build_number(Some(451_672), Some(595_897)), 451_672);
    }

    /// 据えれば読める。`0` は「まだ測っていない」であって値ではない
    #[test]
    fn 実測を据えると読み出せる() {
        set_measured_build_number(123_456);
        assert_eq!(measured_build_number(), Some(123_456));
        // ⚠️ プロセス全体の状態なので、他の試験のために戻しておく
        set_measured_build_number(0);
        assert_eq!(measured_build_number(), None);
    }
}
