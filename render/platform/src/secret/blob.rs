//! How a sealed secret is laid out on disk.
//!
//! `version | iv length | iv | ciphertext`, so a file from another scheme
//! is refused instead of guessed at, and a short one is refused instead of
//! half-decrypted.

// Only Android reads these back; the desktop compiles them so the layout
// keeps its tests where they can actually run.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

/// The only version written so far.
const VERSION: u8 = 1;
/// Longest IV the framing takes. GCM generates 12 bytes.
const MAX_IV: usize = 32;
/// The tag AES-GCM always appends, in bytes. A tail shorter than this was
/// cut short, and half a tag authenticates nothing.
const TAG: usize = 16;

/// Lays a sealed secret out for the file.
pub(super) fn pack(iv: &[u8], cipher_text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + iv.len() + cipher_text.len());
    out.push(VERSION);
    out.push(iv.len() as u8);
    out.extend_from_slice(iv);
    out.extend_from_slice(cipher_text);
    out
}

/// Splits one back into the IV and the ciphertext, or `None` when the file
/// is not one of ours.
pub(super) fn split(sealed: &[u8]) -> Option<(&[u8], &[u8])> {
    if sealed.first() != Some(&VERSION) {
        return None;
    }
    let iv_len = *sealed.get(1)? as usize;
    if iv_len == 0 || iv_len > MAX_IV {
        return None;
    }
    let iv = sealed.get(2..2 + iv_len)?;
    // GCM always appends a tag, so a tail shorter than one means the file
    // was cut and half a tag authenticates nothing.
    let cipher_text = sealed.get(2 + iv_len..)?;
    if cipher_text.len() < TAG {
        return None;
    }
    Some((iv, cipher_text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IV: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    /// What was packed comes back apart.
    #[test]
    fn what_was_packed_comes_back_apart() {
        let sealed = pack(&IV, b"ciphertext and tag");
        let (iv, cipher_text) = split(&sealed).expect("整った封筒");
        assert_eq!(iv, IV);
        assert_eq!(cipher_text, b"ciphertext and tag");
    }

    /// A file from another scheme is refused rather than guessed at.
    #[test]
    fn an_unknown_version_is_refused() {
        let mut sealed = pack(&IV, b"bytes");
        sealed[0] = VERSION + 1;
        assert!(split(&sealed).is_none());
    }

    /// Every way of cutting into the header, the IV, or the tag is refused.
    /// A cut that leaves a whole tag looks like a shorter message and is
    /// caught by the tag itself when the cipher checks it.
    #[test]
    fn a_short_file_is_refused() {
        let sealed = pack(&IV, &[7u8; 40]);
        let whole_tag = 2 + IV.len() + TAG;
        for cut in 0..whole_tag {
            assert!(split(&sealed[..cut]).is_none(), "{cut} バイト目で切った");
        }
        assert!(split(&sealed).is_some());
        assert!(split(&[]).is_none());
    }

    /// An IV length that cannot be right is refused before slicing.
    #[test]
    fn an_absurd_iv_length_is_refused() {
        let mut sealed = pack(&IV, b"bytes");
        sealed[1] = 200;
        assert!(split(&sealed).is_none());
    }
}
