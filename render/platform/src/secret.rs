//! The OS secure store.
//!
//! The rule that matters: if it cannot be encrypted, it is not stored. Writing
//! plaintext and fixing it later means the user's token sits on disk until
//! then. An unsupported platform returns [`SecretError::Unsupported`] and the
//! caller simply logs in again each start — inconvenient, but only that.
//!
//! | Platform | Backend | State |
//! |---|---|---|
//! | Windows | DPAPI (`CryptProtectData`) | done |
//! | Linux | Secret Service (`keyring`) | done |
//! | macOS | Keychain (`keyring`) | done |
//! | Android | Keystore | to come |
//! | iOS | Keychain | to come |
//!
//! DPAPI keeps another user account out: the key derives from the Windows
//! logon credentials, so pulling the disk out is not enough. It does not keep
//! out a program running as the same user — the extra entropy is in this
//! source and is a label, not a wall.
//!
//! That is the strength the official client has too, and the same line: once
//! the OS account is taken, nothing here can help.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// No backend here yet; never falls back to plaintext.
    #[error("このプラットフォームにセキュアストレージの実装がない")]
    Unsupported,
    #[error("保存場所を決められない: {0}")]
    NoHome(&'static str),
    #[error("読み書きに失敗した: {0}")]
    Io(#[from] std::io::Error),
    /// Encryption or decryption itself failed. Carries no content, so no
    /// secret can reach the message.
    #[error("暗号化に失敗した (OS エラー {0})")]
    Crypto(u32),
    /// The value is not text; the keyring holds strings.
    #[error("保存できない値だった")]
    NotText,
}

/// What the OS secure store holds, by name. Only the token for now.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    /// Uses an explicit directory. For tests and isolated instances.
    pub fn in_dir(dir: PathBuf) -> Result<Self, SecretError> {
        std::fs::create_dir_all(&dir)?;
        Ok(SecretStore { dir })
    }

    /// Prepares the directory, failing here if it cannot be made.
    pub fn new() -> Result<Self, SecretError> {
        Self::in_dir(base_dir()?.join("secrets"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn path(&self, name: &str) -> PathBuf {
        // Only our own constants reach this, but never let one traverse.
        let safe: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.bin"))
    }

    /// Stores, replacing anything already there.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn store(&self, name: &str, secret: &[u8]) -> Result<(), SecretError> {
        let blob = protect(secret)?;
        // Written then renamed, so a crash leaves no half-written secret.
        let tmp = self.path(name).with_extension("tmp");
        std::fs::write(&tmp, &blob)?;
        std::fs::rename(&tmp, self.path(name))?;
        Ok(())
    }

    /// Reads one back; absent is `Ok(None)`, unreadable is an error.
    ///
    /// Unreadable happens normally — a different Windows user, a rebuilt
    /// profile — and the caller should discard it and log in again.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        let blob = match std::fs::read(self.path(name)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        unprotect(&blob).map(Some)
    }

    /// Discards one. Absent still succeeds: this runs when a token is
    /// rejected, and failing on "not there" makes that path awkward.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn clear(&self, name: &str) -> Result<(), SecretError> {
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Keyring service. Production uses it plainly; anything else is a test
/// or an isolated instance and is namespaced away from real credentials.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SERVICE: &str = "dev.gumicord";

/// The namespace for this store. Only the production directory maps to
/// the real service; test and scratch directories get their own, so a
/// test run never touches the user's credentials.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn service_for(dir: &std::path::Path) -> String {
    match dir.file_name().and_then(|s| s.to_str()) {
        Some("secrets") => SERVICE.to_owned(),
        Some(other) => format!("{SERVICE}.test.{other}"),
        None => format!("{SERVICE}.test"),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl SecretStore {
    fn entry(&self, name: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(&service_for(&self.dir), name).ok()
    }

    /// Stores, replacing anything already there. Without a keyring to
    /// talk to there is nowhere encrypted to put it.
    pub fn store(&self, name: &str, secret: &[u8]) -> Result<(), SecretError> {
        let text = std::str::from_utf8(secret).map_err(|_| SecretError::NotText)?;
        let Some(entry) = self.entry(name) else {
            return Err(SecretError::Unsupported);
        };
        entry
            .set_password(text)
            .map_err(|_| SecretError::Unsupported)
    }

    /// Reads one back. Unreachable storage holds nothing of ours, which
    /// reads the same as absent.
    pub fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        let Some(entry) = self.entry(name) else {
            return Ok(None);
        };
        match entry.get_password() {
            Ok(text) => Ok(Some(text.into_bytes())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(SecretError::Unsupported),
        }
    }

    /// Discards one. Absent — or unreachable — still succeeds.
    pub fn clear(&self, name: &str) -> Result<(), SecretError> {
        let Some(entry) = self.entry(name) else {
            return Ok(());
        };
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(SecretError::Unsupported),
        }
    }
}

/// Where settings and secrets live.
fn base_dir() -> Result<PathBuf, SecretError> {
    crate::dirs::app_data_dir().ok_or(SecretError::NoHome(home_var()))
}

/// Names the missing variable for the error.
fn home_var() -> &'static str {
    #[cfg(windows)]
    {
        "APPDATA"
    }
    #[cfg(not(windows))]
    {
        "HOME"
    }
}

/// Extra entropy naming this product.
///
/// Not a secret — it is right here, and another program running as the same
/// user can pass it. It tells our blobs from other DPAPI blobs; it is not a
/// wall.
#[cfg(windows)]
const ENTROPY: &[u8] = b"dev.gumicord.secret.v1";

#[cfg(windows)]
fn protect(secret: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows_sys::Win32::Security::Cryptography::{CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData};

    // SAFETY: `blob_in` and `take_blob` own both lifetimes. The OS allocates
    // the output and `LocalFree` returns it.
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
            // No UI: this runs from background work, where a prompt nobody
            // sees would hang.
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

    // SAFETY: as in `protect`.
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

/// Shapes borrowed bytes as a DPAPI input, without copying them.
#[cfg(windows)]
fn blob_in(data: &[u8]) -> windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
    windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

/// Takes DPAPI's output and frees it.
///
/// Decrypted plaintext passes through here, so it is zeroed before the memory
/// goes back and no copy is left on the heap.
///
/// # Safety
///
/// `out` must be a blob filled by a successful `CryptProtectData` or
/// `CryptUnprotectData`.
#[cfg(windows)]
unsafe fn take_blob(
    out: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> Vec<u8> {
    use windows_sys::Win32::Foundation::LocalFree;

    // SAFETY: by the contract above, `out` is a valid OS-allocated blob.
    unsafe {
        let data = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        LocalFree(out.pbData as *mut core::ffi::c_void);
        data
    }
}

#[cfg(windows)]
fn last_error() -> u32 {
    // SAFETY: no arguments, no return value.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn protect(_secret: &[u8]) -> Result<Vec<u8>, SecretError> {
    Err(SecretError::Unsupported)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn unprotect(_blob: &[u8]) -> Result<Vec<u8>, SecretError> {
    Err(SecretError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh location per test, never the user's real store.
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

    /// What went in comes back.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn what_goes_in_comes_back_out() {
        let s = scratch("roundtrip");
        if s.store("token", b"mfa.\xe3\x81\x82\xe3\x81\x84").is_err() {
            eprintln!("no secret store here; skipping");
            return;
        }
        assert_eq!(
            s.load("token").unwrap().as_deref(),
            Some(&b"mfa.\xe3\x81\x82\xe3\x81\x84"[..])
        );

        s.clear("token").unwrap();
        assert!(s.load("token").unwrap().is_none());
    }

    /// No plaintext reaches the disk; the whole point fails otherwise.
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

    /// Overwriting works, so logging in again leaves no old token.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn storing_twice_replaces_the_first() {
        let s = scratch("overwrite");
        if s.store("token", b"first").is_err() {
            eprintln!("no secret store here; skipping");
            return;
        }
        s.store("token", b"second").unwrap();
        assert_eq!(s.load("token").unwrap().as_deref(), Some(&b"second"[..]));
    }

    /// A corrupt blob errors; returning empty would blur "never stored" and
    /// "cannot be opened".
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
