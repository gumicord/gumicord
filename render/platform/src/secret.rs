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
//! | macOS | Keychain | to come |
//! | Linux | Secret Service | to come |
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
}

/// What the OS secure store holds, by name. Only the token for now.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    /// Prepares the directory, failing here if it cannot be made.
    pub fn new() -> Result<Self, SecretError> {
        let dir = base_dir()?.join("secrets");
        std::fs::create_dir_all(&dir)?;
        Ok(SecretStore { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        // Only our own constants reach this, but never let one traverse.
        let safe: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.bin"))
    }

    /// Stores, replacing anything already there.
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
    pub fn clear(&self, name: &str) -> Result<(), SecretError> {
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Where settings and secrets live.
fn base_dir() -> Result<PathBuf, SecretError> {
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA").ok_or(SecretError::NoHome("APPDATA"))?;
        Ok(PathBuf::from(appdata).join("gumicord"))
    }
    #[cfg(not(windows))]
    {
        // XDG. The backend comes later; the location is settled now.
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(x).join("gumicord"));
        }
        let home = std::env::var_os("HOME").ok_or(SecretError::NoHome("HOME"))?;
        Ok(PathBuf::from(home).join(".config").join("gumicord"))
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
    #[cfg(windows)]
    #[test]
    fn storing_twice_replaces_the_first() {
        let s = scratch("overwrite");
        s.store("token", b"first").unwrap();
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
