//! OS のセキュアストレージ (`FR-003`, P4)。
//!
//! # 一番大事な規則
//!
//! **暗号化できないなら保存しない。**
//!
//! 平文で置いて「あとで直す」は、直るまでの間ずっと利用者のトークンが
//! ディスクに転がっているということである。対応していないプラットフォーム
//! では [`SecretError::Unsupported`] を返し、呼び出し側は**保存を諦めて
//! 起動のたびにログインし直す**。不便だが、こちらの不便で済む。
//!
//! | プラットフォーム | 実装 | 状態 |
//! |---|---|---|
//! | Windows | DPAPI (`CryptProtectData`) | ある |
//! | macOS | Keychain | まだない (M1.2) |
//! | Linux | Secret Service | まだない (M1.2) |
//! | Android | Keystore | まだない (M1.2) |
//! | iOS | Keychain | まだない (M1.2) |
//!
//! # DPAPI が守るもの・守らないもの
//!
//! 守る: **他の利用者アカウントからは開けない。** 鍵は Windows のログオン
//! 資格情報から導かれる。ディスクを抜き出しても、その利用者のパスワードが
//! なければ開かない。
//!
//! 守らない: **同じ利用者として走るプログラムからは開ける。** 追加の
//! エントロピーを渡してはいるが、それはこの原文がソースに書いてある以上、
//! 秘密ではない。区別を付けるためのものであって、防壁ではない。
//!
//! これは公式の Discord クライアントと同じ強さである。**OS の利用者
//! アカウントが乗っ取られた時点で守れるものはない**、という線を共有する。

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// このプラットフォームにはまだ実装がない。**平文へは退避しない**
    #[error("このプラットフォームにセキュアストレージの実装がない")]
    Unsupported,
    #[error("保存場所を決められない: {0}")]
    NoHome(&'static str),
    #[error("読み書きに失敗した: {0}")]
    Io(#[from] std::io::Error),
    /// 暗号化・復号そのものの失敗。
    ///
    /// **中身は出さない。** 失敗の詳細に秘密が混じる余地を作らない
    #[error("暗号化に失敗した (OS エラー {0})")]
    Crypto(u32),
}

/// OS のセキュアストレージに預けたもの。
///
/// 名前で出し入れする。いま入っているのはトークン 1 つだけだが、
/// あとから通知の登録鍵などが増える。
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    /// 保存場所を用意する。**作れなければここで失敗する。**
    pub fn new() -> Result<Self, SecretError> {
        let dir = base_dir()?.join("secrets");
        std::fs::create_dir_all(&dir)?;
        Ok(SecretStore { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        // ⚠️ 名前はこちらが決めた定数しか来ないが、経路を跨がせない
        let safe: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.bin"))
    }

    /// 預ける。**既にあれば上書きする。**
    pub fn store(&self, name: &str, secret: &[u8]) -> Result<(), SecretError> {
        let blob = protect(secret)?;
        // 途中で落ちても壊れた鍵束を残さない。書いてから差し替える
        let tmp = self.path(name).with_extension("tmp");
        std::fs::write(&tmp, &blob)?;
        std::fs::rename(&tmp, self.path(name))?;
        Ok(())
    }

    /// 取り出す。**無ければ `Ok(None)`。** 開けなければ誤りである。
    ///
    /// ⚠️ 開けないのは普通に起こる。別の Windows 利用者としてログオンした、
    /// プロファイルを作り直した、など。呼び出し側は**捨ててログインし直す**
    pub fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        let blob = match std::fs::read(self.path(name)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        unprotect(&blob).map(Some)
    }

    /// 捨てる。**無くても成功である。**
    ///
    /// トークンが弾かれたとき (`FR-004`) に呼ばれるので、
    /// 「無かった」で失敗されると後始末が書きにくい
    pub fn clear(&self, name: &str) -> Result<(), SecretError> {
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// 設定と鍵束を置く場所。
fn base_dir() -> Result<PathBuf, SecretError> {
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA").ok_or(SecretError::NoHome("APPDATA"))?;
        Ok(PathBuf::from(appdata).join("gumicord"))
    }
    #[cfg(not(windows))]
    {
        // XDG に従う。実装が入るのは M1.2 だが、置き場所は先に決めておく
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(x).join("gumicord"));
        }
        let home = std::env::var_os("HOME").ok_or(SecretError::NoHome("HOME"))?;
        Ok(PathBuf::from(home).join(".config").join("gumicord"))
    }
}

/// この製品を指す追加のエントロピー。
///
/// ⚠️ **秘密ではない。** ソースに書いてある。同じ利用者として走る別の
/// プログラムがこれを渡せば開けてしまう。他の DPAPI の塊と取り違えない
/// ための目印であって、防壁ではない
#[cfg(windows)]
const ENTROPY: &[u8] = b"dev.gumicord.secret.v1";

#[cfg(windows)]
fn protect(secret: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows_sys::Win32::Security::Cryptography::{CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData};

    // SAFETY: 入出力とも下の blob_in / take_blob が寿命を管理する。
    // 出力の確保は OS 側が行い、LocalFree で返す
    unsafe {
        let input = blob_in(secret);
        let entropy = blob_in(ENTROPY);
        let mut out = std::mem::zeroed();

        let ok = CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null_mut(),
            std::ptr::null(),
            // UI を出さない。**背景の仕事から呼ばれるので、
            // 誰も見ていない画面で入力待ちになると固まる**
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 {
            return Err(SecretError::Crypto(last_error()));
        }
        Ok(take_blob(out))
    }
}

#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows_sys::Win32::Security::Cryptography::{
        CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    // SAFETY: protect と同じ
    unsafe {
        let input = blob_in(blob);
        let entropy = blob_in(ENTROPY);
        let mut out = std::mem::zeroed();

        let ok = CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 {
            return Err(SecretError::Crypto(last_error()));
        }
        Ok(take_blob(out))
    }
}

/// 借りたバイト列を DPAPI の入力の形にする。**中身は複製しない**
#[cfg(windows)]
fn blob_in(data: &[u8]) -> windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
    windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

/// DPAPI が確保した出力を受け取り、**確実に解放する**。
///
/// ⚠️ 復号した平文がここを通る。読み終えたら 0 で潰してから返す。
/// プロセスのヒープに秘密の複製を残さない
///
/// # Safety
///
/// `out` は `CryptProtectData` / `CryptUnprotectData` が成功したときに
/// 埋めた blob でなければならない。
#[cfg(windows)]
unsafe fn take_blob(
    out: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> Vec<u8> {
    use windows_sys::Win32::Foundation::LocalFree;

    // SAFETY: 呼び出し側の契約により、out は OS が確保した有効な blob である
    unsafe {
        let data = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        LocalFree(out.pbData as *mut core::ffi::c_void);
        data
    }
}

#[cfg(windows)]
fn last_error() -> u32 {
    // SAFETY: 引数も戻り値もない
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(not(windows))]
fn protect(_secret: &[u8]) -> Result<Vec<u8>, SecretError> {
    Err(SecretError::Unsupported)
}

#[cfg(not(windows))]
fn unprotect(_blob: &[u8]) -> Result<Vec<u8>, SecretError> {
    Err(SecretError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 試験ごとに別の場所を使う。**利用者の本物の鍵束を触らない**
    fn scratch(tag: &str) -> SecretStore {
        let dir = std::env::temp_dir().join(format!("gumicord-secret-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        SecretStore { dir }
    }

    #[test]
    fn a_missing_secret_is_not_an_error() {
        let s = scratch("missing");
        assert!(s.load("token").unwrap().is_none());
        s.clear("token").expect("無いものを消しても成功する");
    }

    /// 保存したものがそのまま戻る
    #[cfg(windows)]
    #[test]
    fn what_goes_in_comes_back_out() {
        let s = scratch("roundtrip");
        s.store("token", b"mfa.\xe3\x81\x82\xe3\x81\x84").unwrap();
        assert_eq!(
            s.load("token").unwrap().as_deref(),
            Some(&b"mfa.\xe3\x81\x82\xe3\x81\x84"[..])
        );

        s.clear("token").unwrap();
        assert!(s.load("token").unwrap().is_none());
    }

    /// **ディスクに平文が残らない。** ここが破れたら P4 の意味がない
    #[cfg(windows)]
    #[test]
    fn the_plaintext_is_not_on_disk() {
        let s = scratch("ciphertext");
        s.store("token", b"SUPER_SECRET_TOKEN_VALUE").unwrap();

        let raw = std::fs::read(s.path("token")).unwrap();
        assert!(
            !raw.windows(24).any(|w| w == b"SUPER_SECRET_TOKEN_VALUE"),
            "平文がそのまま書かれている"
        );
    }

    /// 上書きできる。ログインし直したときに古いトークンが残らない
    #[cfg(windows)]
    #[test]
    fn storing_twice_replaces_the_first() {
        let s = scratch("overwrite");
        s.store("token", b"first").unwrap();
        s.store("token", b"second").unwrap();
        assert_eq!(s.load("token").unwrap().as_deref(), Some(&b"second"[..]));
    }

    /// 壊れた塊は**誤りとして返る**。黙って空を返すと、
    /// 「保存できていない」と「開けない」の区別が付かない
    #[cfg(windows)]
    #[test]
    fn a_corrupt_blob_is_an_error() {
        let s = scratch("corrupt");
        s.store("token", b"whatever").unwrap();

        let mut raw = std::fs::read(s.path("token")).unwrap();
        let n = raw.len();
        raw[n / 2] ^= 0xff;
        std::fs::write(s.path("token"), &raw).unwrap();

        assert!(s.load("token").is_err());
    }
}
