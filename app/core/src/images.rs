//! 画像を取ってきて、復号して、レンダラへ渡す (R5)。
//!
//! # なぜアプリの仕事なのか
//!
//! **レンダラは網にもディスクにも触らない** ([`spec/02-architecture.md`])。
//! プラットフォームごとに違うものを、全プラットフォーム共通のレンダラに
//! 持ち込まないためである。
//!
//! ```text
//!   UITree            Content::Image(URL)      ← 画素は載せない
//!     │
//!   ここ              取ってくる / 復号 / 保存
//!     │
//!   Application::take_images()                 ← 画素はここだけを通る
//!     │
//!   レンダラ          アトラスへ詰めて描く
//! ```
//!
//! ⚠️ **UITree に画素を載せない。** 木は毎フレーム組み直されるので、
//! 載せると 1 フレームごとに数 MB を複製することになる。
//!
//! # PNG だけを読む
//!
//! CDN には `.png` で頼むので、画像の万能ライブラリは要らない。
//! 添付ファイル (利用者が上げた任意の形式) を出すときに考え直す。
//!
//! ⚠️ **動くアバターも静止画として頼む。** `a_` で始まる印は GIF だが、
//! `.png` を頼めば 1 コマ目が PNG で返る。動かす仕組みは別の話である。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use gumicord_platform::Waker;
use gumicord_render::ImageData;
use gumicord_rest::RestClient;

/// 同時に取りに行く数。
///
/// ⚠️ **一覧を開いた瞬間に何十枚も要求される。** 上限が無いと、
/// CDN にも自分の回線にも一気に負荷をかける
const IN_FLIGHT: usize = 6;

/// 1 枚の picture が使ってよい辺の長さ (物理 px)。
///
/// アバターは 40、サーバアイコンは 48 なので、2 倍の画面でも 96 で足りる。
/// **アトラスは 2048² しかない**ので、大きく取ると数十枚で埋まる
const MAX_SIDE: u32 = 128;

/// 取ってきた画像を運ぶもの。
pub struct Images {
    tx: Sender<ImageData>,
    rx: Receiver<ImageData>,
    rt: Option<tokio::runtime::Handle>,
    rest: Option<RestClient>,
    waker: Option<Waker>,
    /// 取りに行った URL。**二重に叩かないため**
    requested: HashSet<String>,
    /// いま取りに行っている数
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// 復号済みを置く場所。**次の起動で網に出ない**
    dir: Option<PathBuf>,
    /// 届いていて、まだレンダラへ渡していないもの。
    ///
    /// ⚠️ **溜める場所が要る。** 起こされた時点で受け取っておかないと、
    /// 「何か届いた」を呼び出し側へ伝えられない。伝えられないと**再描画が
    /// 起きず、届いた顔がいつまでも出ない**
    ready: Vec<ImageData>,
}

impl Images {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Images {
            tx,
            rx,
            rt: None,
            rest: None,
            waker: None,
            requested: HashSet::new(),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            dir: cache_dir(),
            ready: Vec::new(),
        }
    }

    pub fn start(&mut self, rt: &tokio::runtime::Handle, rest: RestClient, waker: Waker) {
        self.rt = Some(rt.clone());
        self.rest = Some(rest);
        self.waker = Some(waker);
    }

    /// その URL の絵を要求する。**何度呼んでもよい。**
    ///
    /// ⚠️ 既にレンダラが持っているかどうかはここでは分からない。
    /// 呼び出し側が `has_image` で確かめてから呼ぶ
    pub fn request(&mut self, url: &str) {
        if url.is_empty() || !self.requested.insert(url.to_owned()) {
            return;
        }
        let (Some(rt), Some(rest), Some(waker)) = (&self.rt, &self.rest, &self.waker) else {
            return;
        };

        let (rest, tx, waker) = (rest.clone(), self.tx.clone(), waker.clone());
        let (url, dir) = (url.to_owned(), self.dir.clone());
        let counter = std::sync::Arc::clone(&self.in_flight);

        rt.spawn(async move {
            // ⚠️ **一気に取りに行かない。** 一覧を開くと何十枚も要求される
            while counter.load(std::sync::atomic::Ordering::Relaxed) >= IN_FLIGHT {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let bytes = match read_cached(dir.as_deref(), &url) {
                Some(b) => Some(b),
                None => match rest.fetch_cdn(&url).await {
                    Ok(b) => {
                        write_cached(dir.as_deref(), &url, &b);
                        Some(b)
                    }
                    Err(e) => {
                        tracing::debug!(%e, url, "画像を取れなかった");
                        None
                    }
                },
            };
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            // ⚠️ 復号は**別スレッドへ逃がす**。数十枚あると主スレッドの
            // 隣で回している非同期の仕事まで止まる
            if let Some(bytes) = bytes
                && let Ok(Some(image)) =
                    tokio::task::spawn_blocking(move || decode_png(&url, &bytes)).await
            {
                let _ = tx.send(image);
                waker.wake();
            }
        });
    }

    /// 届いたぶんを受け取る。**何か届いていたら真**。
    ///
    /// ⚠️ **ここで受け取っておかないと再描画が起きない。** イベント
    /// ループは寝ており ([`NFR-005`])、起こされたときに「変わった」と
    /// 言えなければそのまま二度寝する
    pub fn poll(&mut self) -> bool {
        let before = self.ready.len();
        while let Ok(image) = self.rx.try_recv() {
            self.ready.push(image);
        }
        self.ready.len() != before
    }

    /// 届いた絵を引き取る。**呼んだ側がレンダラへ渡す**
    pub fn take(&mut self) -> Vec<ImageData> {
        self.poll();
        std::mem::take(&mut self.ready)
    }

    /// 「もう頼んだ」印を落とす。**取り直せるようにする。**
    ///
    /// アトラスが絵を忘れたときに呼ぶ。**円盤には残っている**ので、
    /// 網へは出ずに読み直されるだけである。
    ///
    /// ⚠️ **消すのは印だけで、円盤のものは消さない。** 消すと本当に
    /// 取り直しになり、忘れるたびに何十枚も網へ出ることになる
    pub fn forget_requested(&mut self) {
        self.requested.clear();
    }

    /// `SEC-021`: ログアウトしたら**取ってきた絵も残さない**
    pub fn forget_everything(&mut self) {
        self.requested.clear();
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Default for Images {
    fn default() -> Self {
        Self::new()
    }
}

fn cache_dir() -> Option<PathBuf> {
    let path = gumicord_store::default_path().ok()?;
    let dir = path.parent()?.join("images");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// URL からファイル名を作る。
///
/// ⚠️ **URL をそのままファイル名にしない。** `/` も `?` も入っているし、
/// 長さの上限にも当たる。指紋にする
fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}.png", h.finish())
}

fn read_cached(dir: Option<&std::path::Path>, url: &str) -> Option<Vec<u8>> {
    std::fs::read(dir?.join(cache_name(url))).ok()
}

/// ⚠️ **書けなくても構わない。** 次から網に出るだけである
fn write_cached(dir: Option<&std::path::Path>, url: &str, bytes: &[u8]) {
    let Some(dir) = dir else { return };
    let _ = std::fs::write(dir.join(cache_name(url)), bytes);
}

/// PNG を RGBA8 にする。**読めなければ `None`。**
///
/// ⚠️ 他人が作ったファイルである。**壊れていても落ちない**こと。
fn decode_png(url: &str, bytes: &[u8]) -> Option<ImageData> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // 8 ビット RGBA へ揃える。パレットも 16 ビットもここで潰れる
    decoder.set_transformations(png::Transformations::normalize_to_color8());

    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);

    // ⚠️ 大きすぎるものは読まない。**アトラスは 2048² しかない**
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        tracing::debug!(url, w, h, "画像の大きさが扱える範囲を超えている");
        return None;
    }

    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let raw = &buf[..frame.buffer_size()];

    let rgba = match frame.color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => raw
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        png::ColorType::GrayscaleAlpha => raw
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => raw.iter().flat_map(|v| [*v, *v, *v, 0xff]).collect(),
        // `normalize_to_color8` が潰しているはずだが、信じずに諦める
        other => {
            tracing::debug!(url, ?other, "読めない色の形");
            return None;
        }
    };

    Some(shrink(
        ImageData {
            url: url.to_owned(),
            width: w,
            height: h,
            rgba,
        },
        MAX_SIDE,
    ))
}

/// 大きすぎる絵を**整数倍で**縮める。
///
/// ⚠️ 半端な倍率で縮めると、いまの実装 (近傍から 1 点取る) では模様が出る。
/// 整数倍に限れば、元の画素をきれいに間引くだけで済む。
///
/// **これは正しい縮小ではない。** 面で平均する縮小は R5 の残件として
/// ミップマップと一緒に入れる
fn shrink(image: ImageData, max_side: u32) -> ImageData {
    let side = image.width.max(image.height);
    if side <= max_side {
        return image;
    }
    let step = side.div_ceil(max_side);
    let (w, h) = (image.width / step, image.height / step);
    if w == 0 || h == 0 {
        return image;
    }

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let src = (((y * step) * image.width + x * step) * 4) as usize;
            rgba.extend_from_slice(&image.rgba[src..src + 4]);
        }
    }
    ImageData {
        url: image.url,
        width: w,
        height: h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PNG を書く。**試験のために本物の PNG を作る**
    fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
        }
        out
    }

    #[test]
    fn a_png_becomes_rgba() {
        let bytes = png_bytes(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]);
        let image = decode_png("https://example/a.png", &bytes).expect("読めない");

        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba, vec![255, 0, 0, 255, 0, 255, 0, 128]);
    }

    /// ⚠️ **他人が作ったファイルである。壊れていても落ちない**
    #[test]
    fn rubbish_does_not_panic() {
        assert!(decode_png("x", &[]).is_none());
        assert!(decode_png("x", "これは PNG ではない".as_bytes()).is_none());
        assert!(decode_png("x", &[0x89, b'P', b'N', b'G', 0, 0, 0, 0]).is_none());
    }

    /// 大きすぎる絵は縮む。**アトラスは 2048² しかない**
    #[test]
    fn an_oversized_image_is_shrunk() {
        let big = ImageData {
            url: "x".to_owned(),
            width: 512,
            height: 256,
            rgba: vec![7; 512 * 256 * 4],
        };
        let small = shrink(big, 128);

        assert!(small.width <= 128 && small.height <= 128);
        assert_eq!(
            small.rgba.len(),
            (small.width * small.height * 4) as usize,
            "画素の数が大きさと合わない"
        );
        // 縦横の比が保たれている
        assert_eq!(small.width, small.height * 2);
    }

    /// 収まっているものは触らない
    #[test]
    fn a_small_image_is_left_alone() {
        let image = ImageData {
            url: "x".to_owned(),
            width: 64,
            height: 64,
            rgba: vec![0; 64 * 64 * 4],
        };
        assert_eq!(shrink(image, 128).width, 64);
    }

    /// ⚠️ **URL をそのままファイル名にしない。** `/` も `?` も入っている
    #[test]
    fn a_cache_name_is_a_safe_filename() {
        let name = cache_name("https://cdn.discordapp.com/avatars/1/ab.png?size=128");
        assert!(name.ends_with(".png"));
        assert!(!name.contains('/') && !name.contains('?') && !name.contains(':'));
    }

    /// 同じ URL は同じ名前、違う URL は違う名前
    #[test]
    fn cache_names_follow_the_url() {
        assert_eq!(cache_name("https://a/1.png"), cache_name("https://a/1.png"));
        assert_ne!(cache_name("https://a/1.png"), cache_name("https://a/2.png"));
    }
}
