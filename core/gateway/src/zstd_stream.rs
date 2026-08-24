//! zstd-stream decompression for the Gateway.
//!
//! Frames are not independent, and this is the easiest thing to get wrong.
//! With `compress=zstd-stream` the payload arrives in binary WebSocket frames,
//! and reading each as one compressed JSON works for the first and breaks on
//! every one after: the whole connection is a single stream whose dictionary
//! keeps growing across frames, so the decoder must live as long as the
//! connection and be rebuilt on reconnect.
//!
//! ```text
//!   WS frames: [ ─── ][ ─ ][ ───── ][ ── ]
//!   zstd:      └────────── one stream ──────────┘
//!   JSON:      { … }  { …    …  }   { … }{ … }
//!                ↑ the boundaries do not line up ↑
//! ```
//!
//! So one frame may complete no JSON (an empty result) or several, and the
//! caller reads whatever comes back as a sequence of them.

use std::io::Write;

/// One connection's decoder. Rebuild it on reconnect.
pub struct ZstdStream {
    decoder: zstd::stream::write::Decoder<'static, Vec<u8>>,
}

impl core::fmt::Debug for ZstdStream {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ZstdStream")
    }
}

impl ZstdStream {
    pub fn new() -> std::io::Result<Self> {
        Ok(ZstdStream {
            decoder: zstd::stream::write::Decoder::new(Vec::new())?,
        })
    }

    /// Feeds one frame in and takes out whatever completed.
    ///
    /// Empty is not an error: that frame did not finish a message, and the
    /// next one will.
    pub fn push(&mut self, chunk: &[u8]) -> std::io::Result<Vec<u8>> {
        self.decoder.write_all(chunk)?;
        self.decoder.flush()?;
        Ok(std::mem::take(self.decoder.get_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream split anywhere still reassembles; this is the whole point.
    #[test]
    fn a_stream_split_across_frames_still_decodes() {
        let plain = br#"{"op":10,"d":{"heartbeat_interval":41250}}{"op":11}"#;

        let mut encoder =
            zstd::stream::write::Encoder::new(Vec::new(), 0).expect("符号化器を作れない");
        encoder.write_all(plain).unwrap();
        let compressed = encoder.finish().unwrap();

        // One byte at a time: the worst possible split must still work.
        let mut stream = ZstdStream::new().unwrap();
        let mut out = Vec::new();
        for byte in &compressed {
            out.extend(stream.push(&[*byte]).expect("解凍に失敗した"));
        }

        assert_eq!(out, plain);
    }

    /// Mid-frame gives an empty result, which is not an error.
    #[test]
    fn an_incomplete_frame_yields_nothing_rather_than_an_error() {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
        encoder.write_all(&[b'x'; 4096]).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut stream = ZstdStream::new().unwrap();
        // A zstd frame header alone yields nothing.
        let first = stream.push(&compressed[..4]).expect("誤りにはならない");
        assert!(first.is_empty());
    }

    /// Rebuild on reconnect; a new stream through an old decoder breaks.
    #[test]
    fn a_fresh_stream_is_needed_for_a_fresh_connection() {
        let make = |body: &[u8]| {
            let mut e = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
            e.write_all(body).unwrap();
            e.finish().unwrap()
        };

        let mut stream = ZstdStream::new().unwrap();
        assert_eq!(stream.push(&make(b"first")).unwrap(), b"first");

        // Rebuilt, it reads the next stream.
        let mut stream = ZstdStream::new().unwrap();
        assert_eq!(stream.push(&make(b"second")).unwrap(), b"second");
    }
}
