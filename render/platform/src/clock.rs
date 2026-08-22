//! OS から時刻に関する情報を取る。
//!
//! いま要るのは**現地時刻とのずれ**だけである。Discord が返す時刻は
//! すべて UTC なので、これが無いと**日本の利用者に 9 時間ずれた時刻**を
//! 見せることになる。
//!
//! # なぜ crate を足さないのか
//!
//! `chrono` も `time` も、この用途では OS の API を 1 本呼ぶために
//! 依存を増やすことになる。ここは既に「OS に触るコードを閉じ込める層」
//! なので、**本来ここに置くべきものである**。
//!
//! 暦の計算そのもの (閏年・月の日数) は増やさない。ずれを分で返すだけで、
//! それをどう使うかは上の層が決める。

/// 現地時刻と UTC のずれ (分)。日本なら `+540`。
///
/// ⚠️ **夏時間を考慮した「いまの」ずれ**である。年に 2 回変わる地域が
/// あるので、起動時に 1 回取って使い回すのではなく、必要なときに聞く。
///
/// 取れなければ `0` — つまり UTC のまま表示する。**推測はしない。**
pub fn local_utc_offset_minutes() -> i32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Time::GetTimeZoneInformation;

        /// `GetTimeZoneInformation` の戻り値。
        /// ⚠️ 定数は `SystemServices` にあり、機能フラグを 1 つ増やすことに
        /// なるので、値だけをここに写している
        const STANDARD: u32 = 1;
        const DAYLIGHT: u32 = 2;

        // SAFETY: OS が埋める構造体を渡すだけ。ポインタは有効である
        unsafe {
            let mut info = std::mem::zeroed();
            let kind = GetTimeZoneInformation(&mut info);

            // ⚠️ Windows の Bias は「現地 → UTC」の向きである。
            // UTC = 現地 + Bias なので、**符号を反転させる**
            let bias = match kind {
                STANDARD => info.Bias + info.StandardBias,
                DAYLIGHT => info.Bias + info.DaylightBias,
                // TIME_ZONE_ID_INVALID を含む。分からないので動かさない
                _ => return 0,
            };
            -bias
        }
    }
    #[cfg(not(windows))]
    {
        // M1.2 で各プラットフォームの実装が入る。
        // **それまでは UTC のまま出す。ずらして誤魔化さない**
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 現実にありうる範囲に収まる。UTC-12 から UTC+14 まで
    #[test]
    fn the_offset_is_a_real_one() {
        let m = local_utc_offset_minutes();
        assert!((-12 * 60..=14 * 60).contains(&m), "ありえないずれ: {m} 分");
        // 15 分刻みでない時間帯は存在しない
        assert_eq!(m % 15, 0, "15 分で割り切れないずれ: {m} 分");
    }
}
