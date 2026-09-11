//! Offset conversion at the IME boundary.
//!
//! Windows TSF, Android's GameTextInput and iOS's `UITextInput` all count in
//! UTF-16 code units; [`TextDocument`](super::TextDocument) counts UTF-8
//! bytes. Convert here, at the boundary, and never store converted positions.
//!
//! Out-of-range positions clamp to the text; mid-character positions snap to
//! the character start on the way in (UTF-16 to bytes) and count the whole
//! character on the way out. A real IME only sends valid edges, so the snap
//! direction never matters in practice.

/// UTF-16 code units to a UTF-8 byte offset.
pub fn utf16_to_bytes(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// UTF-8 byte offset to UTF-16 code units.
pub fn bytes_to_utf16(text: &str, bytes: usize) -> usize {
    let bytes = bytes.min(text.len());
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if byte >= bytes {
            break;
        }
        units += ch.len_utf16();
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ASCII maps one to one.
    #[test]
    fn ascii_is_identity() {
        assert_eq!(utf16_to_bytes("hello", 2), 2);
        assert_eq!(bytes_to_utf16("hello", 2), 2);
    }

    /// Hiragana is three bytes and one unit per character.
    #[test]
    fn japanese_differs_by_encoding() {
        let text = "あいう";
        assert_eq!(utf16_to_bytes(text, 2), 6);
        assert_eq!(bytes_to_utf16(text, 6), 2);
    }

    /// Astral emoji is four bytes and two units.
    #[test]
    fn astral_emoji_counts_two_units() {
        let text = "a🍣b";
        assert_eq!(utf16_to_bytes(text, 1), 1);
        assert_eq!(utf16_to_bytes(text, 2), 1 + 4);
        assert_eq!(bytes_to_utf16(text, 1 + 4), 3);
    }

    /// Past the end clamps to the end.
    #[test]
    fn overflow_clamps() {
        assert_eq!(utf16_to_bytes("あ", 99), 3);
        assert_eq!(bytes_to_utf16("あ", 99), 1);
    }

    /// A byte offset inside a character counts the whole character.
    #[test]
    fn mid_character_rounds_up() {
        assert_eq!(bytes_to_utf16("あい", 1), 1);
        assert_eq!(bytes_to_utf16("あい", 4), 2);
    }

    /// Converting there and back lands on a boundary.
    #[test]
    fn round_trip_is_stable() {
        let text = "aあ🍣b";
        for units in 0..=5 {
            let bytes = utf16_to_bytes(text, units);
            assert!(text.is_char_boundary(bytes));
            assert_eq!(utf16_to_bytes(text, bytes_to_utf16(text, bytes)), bytes);
        }
    }
}
