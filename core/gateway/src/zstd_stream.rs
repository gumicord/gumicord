//! Gateway の zstd-stream 解凍 ([`spec/09-discord-protocol.md`] 4 章)。
//!
//! # フレームごとに独立していない
//!
//! ⚠️ **これが zstd-stream で一番間違えやすいところである。**
//!
//! `compress=zstd-stream` を指定すると、ペイロードは WebSocket の**バイナリ
//! フレーム**で届く。ここで「フレーム 1 枚 = 圧縮された JSON 1 個」だと
//! 思うと、最初の 1 枚は解凍できて、2 枚目から必ず壊れる。
//!
//! 実際には**接続の頭から終わりまでが 1 本の連続したストリーム**である。
//! 辞書はフレームを跨いで育ち続けるので、状態を持ったデコーダを接続の
//! 生存期間中ずっと保持しなければならない。**繋ぎ直したら作り直す。**
//!
//! ```text
//!   WS フレーム: [ ─── ][ ─ ][ ───── ][ ── ]
//!   zstd:        └────────── 1 本のストリーム ──────────┘
//!   JSON:        { … }  { …    …  }   { … }{ … }
//!                  ↑ 境界は一致しない ↑
//! ```
//!
//! したがって:
//!
//! - 1 枚のフレームで 1 個の JSON が完結するとは限らない (空が返る)
//! - 1 枚のフレームに複数の JSON が入っていることもある
//!
//! 呼び出し側は**返ってきたバイト列をそのまま JSON の並びとして読む**。

use std::io::Write;

/// 接続 1 本ぶんの解凍器。**繋ぎ直したら捨てて作り直すこと。**
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

    /// フレームを 1 枚流し込み、**そこまでに完結した平文**を取り出す。
    ///
    /// 空が返るのは誤りではない。**そのフレームだけでは 1 個ぶんに
    /// 届かなかった**というだけで、次のフレームで出てくる。
    pub fn push(&mut self, chunk: &[u8]) -> std::io::Result<Vec<u8>> {
        self.decoder.write_all(chunk)?;
        self.decoder.flush()?;
        Ok(std::mem::take(self.decoder.get_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 本のストリームを**途中で切って**流し込んでも、
    /// 繋げれば元に戻る。ここが zstd-stream の肝である
    #[test]
    fn a_stream_split_across_frames_still_decodes() {
        let plain = br#"{"op":10,"d":{"heartbeat_interval":41250}}{"op":11}"#;

        let mut encoder =
            zstd::stream::write::Encoder::new(Vec::new(), 0).expect("符号化器を作れない");
        encoder.write_all(plain).unwrap();
        let compressed = encoder.finish().unwrap();

        // **1 バイトずつ**流す。最悪の切れ方でも壊れないこと
        let mut stream = ZstdStream::new().unwrap();
        let mut out = Vec::new();
        for byte in &compressed {
            out.extend(stream.push(&[*byte]).expect("解凍に失敗した"));
        }

        assert_eq!(out, plain);
    }

    /// フレームの途中では空が返る。**誤りではない**
    #[test]
    fn an_incomplete_frame_yields_nothing_rather_than_an_error() {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
        encoder.write_all(&[b'x'; 4096]).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut stream = ZstdStream::new().unwrap();
        // zstd の枠の頭だけでは中身が出てこない
        let first = stream.push(&compressed[..4]).expect("誤りにはならない");
        assert!(first.is_empty());
    }

    /// 繋ぎ直したら作り直す。**古い解凍器に新しいストリームを流すと壊れる**
    #[test]
    fn a_fresh_stream_is_needed_for_a_fresh_connection() {
        let make = |body: &[u8]| {
            let mut e = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
            e.write_all(body).unwrap();
            e.finish().unwrap()
        };

        let mut stream = ZstdStream::new().unwrap();
        assert_eq!(stream.push(&make(b"first")).unwrap(), b"first");

        // 作り直せば次のストリームも読める
        let mut stream = ZstdStream::new().unwrap();
        assert_eq!(stream.push(&make(b"second")).unwrap(), b"second");
    }
}
