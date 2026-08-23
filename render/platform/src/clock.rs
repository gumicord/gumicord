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

/// いま何時か。1970-01-01 00:00 UTC からの秒。
///
/// ⚠️ **フレームの頭で 1 回だけ読むこと。** 組んでいる最中に何度も読むと、
/// 同じ画面に並んだ「3 分前」と「4 分前」が同じ時刻を指しうる。
///
/// ⚠️ **利用者が時計を戻すと過去へ飛ぶ。** 時間の間隔を測るのに使っては
/// ならない ([`std::time::Instant`] がそのためにある)。ここで要るのは
/// 「Discord が寄越した時刻と比べる」ことだけである。
///
/// 1970 年より前 (時計が壊れている等) なら負の値を返す。**握り潰さない。**
pub fn now_unix() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        // 1970 年より前。逆向きの差が返る
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod now_tests {
    use super::*;

    /// ⚠️ **止まっていないこと。** 0 を返す実装でも「動いている」と
    /// 見えてしまうので、現実にありうる範囲かを見る
    #[test]
    fn the_clock_is_in_this_century() {
        let now = now_unix();
        // 2020-01-01 〜 2100-01-01
        assert!(
            (1_577_836_800..4_102_444_800).contains(&now),
            "ありえない時刻: {now}"
        );
    }
}

/// キャレットが点滅する間隔。**OS の設定に従う。**
///
/// ⚠️ **自前の値を決め打ちしない。** 点滅の速さは「コントロールパネル →
/// キーボード」で変えられる設定であり、**点滅させない設定もある**
/// (てんかんの光過敏や、単に目障りだという理由で切る人がいる)。
///
/// `None` は「点滅させない」である。**0 ではない。**
pub fn caret_blink_interval() -> Option<std::time::Duration> {
    /// OS の設定が壊れているときに使う値。Windows の既定と同じ
    const FALLBACK_MS: u64 = 530;

    #[cfg(windows)]
    {
        // SAFETY: 引数がなく、戻り値も数値だけである
        let ms = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
        match ms {
            // 点滅させない設定
            u32::MAX => None,
            // 取れなかった
            0 => Some(std::time::Duration::from_millis(FALLBACK_MS)),
            ms => Some(std::time::Duration::from_millis(ms as u64)),
        }
    }
    #[cfg(not(windows))]
    {
        Some(std::time::Duration::from_millis(FALLBACK_MS))
    }
}

#[cfg(test)]
mod blink_tests {
    use super::*;

    /// 取れた値が現実的な範囲にある。**0 は返さない** —
    /// 0 だと 1 フレームごとに点滅して、目に見えないほど速くなる
    #[test]
    fn the_blink_interval_is_usable_or_absent() {
        if let Some(d) = caret_blink_interval() {
            assert!(d.as_millis() >= 100, "速すぎる: {d:?}");
            assert!(d.as_millis() <= 5_000, "遅すぎる: {d:?}");
        }
    }
}
