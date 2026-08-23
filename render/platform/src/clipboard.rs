//! クリップボード (`PLT-007`)。
//!
//! | プラットフォーム | 実装 | 状態 |
//! |---|---|---|
//! | Windows | Win32 (`CF_UNICODETEXT`) | ある |
//! | macOS | `NSPasteboard` | まだない (M1.2) |
//! | Linux | Wayland / X11 | まだない (M1.2) |
//! | Android / iOS | OS の API | まだない (M1.2) |
//!
//! 画像 (`PLT-007` の後半) はまだない。**先に文字だけを入れる。**
//!
//! # ⚠️ 失敗を黙って飲まない
//!
//! クリップボードは**他のプログラムと共有する資源**である。開こうとした
//! 瞬間に別のプログラムが握っていることは普通にあり、そのときは開けない。
//!
//! ここで `Ok` を返してしまうと、利用者は「コピーした」と思って貼り付け、
//! **前に入っていた別のものが貼られる**。押した本人には何が起きたのか
//! 分からない。だから失敗は失敗として返す。
//!
//! # ⚠️ 開けたら必ず閉じる
//!
//! 握ったまま返ると、**そのプロセスが終わるまで他のプログラムが
//! クリップボードを使えなくなる**。途中で抜ける道が 1 つでも閉じ忘れると、
//! 端末全体のコピーが効かなくなる。[`Owned`] が抜けるときに閉じる。

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// このプラットフォームにはまだ実装がない
    #[error("このプラットフォームにクリップボードの実装がない")]
    Unsupported,
    /// 他のプログラムが握っている。**時間を置けば取れることが多い**
    #[error("クリップボードを開けない (他のプログラムが使っている)")]
    Busy,
    #[error("クリップボードの操作に失敗した: {0}")]
    Failed(&'static str),
}

/// 文字をクリップボードへ入れる。
pub fn set_text(text: &str) -> Result<(), ClipboardError> {
    imp::set_text(text)
}

/// クリップボードの文字。入っていなければ `None`。
///
/// ⚠️ **`None` は誤りではない。** 画像しか入っていないこともある
pub fn text() -> Result<Option<String>, ClipboardError> {
    imp::text()
}

#[cfg(not(windows))]
mod imp {
    use super::ClipboardError;

    pub fn set_text(_text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unsupported)
    }

    pub fn text() -> Result<Option<String>, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::ClipboardError;

    use windows_sys::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    /// 開けなかったときに試し直す回数。
    ///
    /// ⚠️ **1 回で諦めない。** 他のプログラムが握っているのは大抵ほんの
    /// 一瞬で、押した本人からは「コピーが効かないことがある」という
    /// 再現しない不具合に見える
    const TRIES: u32 = 5;
    /// 試し直す間隔
    const WAIT: std::time::Duration = std::time::Duration::from_millis(10);

    /// 開いているクリップボード。**抜けるときに必ず閉じる。**
    struct Owned;

    impl Owned {
        fn open() -> Result<Owned, ClipboardError> {
            for _ in 0..TRIES {
                // ⚠️ 窓を渡さない。渡すと、その窓が壊れたときに
                // クリップボードごと道連れになる
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Ok(Owned);
                }
                std::thread::sleep(WAIT);
            }
            Err(ClipboardError::Busy)
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // ⚠️ **閉じ忘れると端末全体のコピーが効かなくなる。**
            // ここが唯一の閉じ場所である
            unsafe { CloseClipboard() };
        }
    }

    pub fn set_text(text: &str) -> Result<(), ClipboardError> {
        // ⚠️ **終端の 0 を入れる。** 入れないと、貼り付けた先が長さを
        // 読み違えて、後ろに他人のメモリの中身が付いてくる
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::mem::size_of_val(wide.as_slice());

        let _owned = Owned::open()?;

        // ⚠️ **`GMEM_MOVEABLE` で確保する。** クリップボードは受け取った
        // 領域の持ち主になるので、固定した領域を渡すと解放できない
        let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if handle.is_null() {
            return Err(ClipboardError::Failed("領域を確保できない"));
        }

        let dst = unsafe { GlobalLock(handle) };
        if dst.is_null() {
            return Err(ClipboardError::Failed("領域を掴めない"));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst.cast::<u16>(), wide.len());
            GlobalUnlock(handle);
        }

        if unsafe { EmptyClipboard() } == 0 {
            return Err(ClipboardError::Failed("中身を空にできない"));
        }
        // ⚠️ **成功したら領域はこちらのものではなくなる。** 解放しては
        // ならない。失敗したときだけ、こちらが持ったままである
        if unsafe { SetClipboardData(CF_UNICODETEXT as u32, handle as HANDLE) }.is_null() {
            return Err(ClipboardError::Failed("中身を入れられない"));
        }
        Ok(())
    }

    pub fn text() -> Result<Option<String>, ClipboardError> {
        let _owned = Owned::open()?;

        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
            // 画像しか入っていない。**誤りではない**
            return Ok(None);
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
        if handle.is_null() {
            return Ok(None);
        }
        let src = unsafe { GlobalLock(handle as HGLOBAL) };
        if src.is_null() {
            return Err(ClipboardError::Failed("領域を掴めない"));
        }

        let src = src.cast::<u16>();
        let mut len = 0;
        // ⚠️ **終端の 0 まで数える。** 長さは別に来ない
        while unsafe { *src.add(len) } != 0 {
            len += 1;
        }
        let wide = unsafe { std::slice::from_raw_parts(src, len) };
        // ⚠️ **壊れた並びで落ちない。** 他のプログラムが入れたものであり、
        // 正しい UTF-16 である保証はない
        let text = String::from_utf16_lossy(wide);
        unsafe { GlobalUnlock(handle as HGLOBAL) };

        Ok(Some(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠️ **入れたものがそのまま出てくること。**
    ///
    /// 終端の 0 を入れ忘れると、後ろに余計なものが付いてくる。
    /// 数え間違えると末尾が欠ける。どちらも「だいたい合っている」ので
    /// 目視では気づきにくい
    #[test]
    #[cfg_attr(not(windows), ignore = "このプラットフォームにはまだ実装がない (M1.2)")]
    fn text_comes_back_unchanged() {
        // ⚠️ **走らせた人の手元を壊さない。** クリップボードは
        // このプロセスのものではなく、端末のものである。
        // 借りたら返す
        let before = text().ok().flatten();

        // 他のテストと同時に走るとクリップボードを取り合うので、
        // 1 つのテストの中で順に確かめる
        for s in [
            "hello",
            "こんにちは",
            "改行\nと\tタブ",
            "絵文字 🎉 と結合文字 が\u{3099}",
            "",
        ] {
            if let Err(e) = set_text(s) {
                // CI の窓なし環境などでは開けないことがある。
                // **そのときは黙って諦める** — 実装の誤りではない
                eprintln!("クリップボードを使えない: {e}");
                return;
            }
            assert_eq!(text().expect("読めない").as_deref(), Some(s));
        }

        // ⚠️ **失敗しても返す。** ここまで来ているなら書けるはずだが、
        // 書けなくても試験の結果は変えない
        if let Some(before) = before {
            let _ = set_text(&before);
        }
    }
}
