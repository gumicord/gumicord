//! アイコン。**フォントのグリフではなくテクスチャとして持つ。**
//!
//! # なぜフォントを使わないのか
//!
//! タイトルバーの最小化・最大化・閉じるを `−` `□` `✕` の文字で描くと、
//! 太さも大きさも書体任せになり、ピクセルグリッドにも乗らない。3 つ並べた
//! ときに揃わないのは、字としては正しくてもアイコンとしては誤りである。
//!
//! Windows には Segoe Fluent Icons があるが、それを使うと Windows でしか
//! 同じ見た目にならず `EXT-020` (全プラットフォームで同一の描画結果) と
//! 正面から衝突する。
//!
//! [`spec/06-renderer.md`] 2.1 の「汎用のパスやベジェ曲線は持たない。必要に
//! なったらアイコンをテクスチャとして持つ」がそのままここの方針である。
//!
//! # 折れ線を CPU でラスタライズする
//!
//! アイコンは単位正方形 (0.0〜1.0) 上の**折れ線**として定義する。描画に要る
//! 物理ピクセルの大きさが決まった時点でラスタライズし、グリフと同じアトラスへ
//! 載せる。
//!
//! SVG を読まないのは、パーサとパス塗りつぶしを持ち込む必要があるからである。
//! いま要るのは線分の集まりだけで、それは点と線分の距離で書ける。
//!
//! **結果は環境に依存しない。** 同じ大きさなら全プラットフォームで 1 ビットも
//! 違わない (`EXT-020`)。

/// 単位正方形の上に置かれた折れ線の集まり。
#[derive(Debug, Clone, Copy)]
pub struct IconDef {
    /// 折れ線。各要素が 1 本の連続した線
    strokes: &'static [&'static [(f32, f32)]],
    /// 線の太さ (単位正方形に対する比)
    width: f32,
}

/// 名前とアイコンの対応。**これが公開しているアイコンの一覧である。**
pub static ICONS: &[(&str, IconDef)] = &[
    ("window.minimize", WINDOW_MINIMIZE),
    ("window.maximize", WINDOW_MAXIMIZE),
    ("window.restore", WINDOW_RESTORE),
    ("window.close", WINDOW_CLOSE),
    ("channel.text", CHANNEL_TEXT),
    ("channel.voice", CHANNEL_VOICE),
    ("folder", FOLDER),
];

/// 名前からアイコンを引く。返すのは**正規化された名前**とその定義。
///
/// 名前を返すのは、アトラスの鍵に `&'static str` を使いたいからである。
/// 呼び出し側が持っているのは毎フレーム作られる `String` なので、そのまま
/// 鍵にすると割り当てが増える。
///
/// **知らない名前は誤りではない。** 新しいクライアント向けに書かれた
/// プラグインを古いクライアントで動かすと起こりうる。描かずに進む。
pub fn lookup(name: &str) -> Option<(&'static str, &'static IconDef)> {
    ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, def)| (*n, def))
}

// ─────────────────────────────────────────────────────────── 定義
//
// ⚠️ 「あったほうが便利かもしれない」で足さない。使う場所ができてから足す。

/// 線の標準の太さ。12px のアイコンでちょうど 1px になる
const W: f32 = 1.0 / 12.0;

const WINDOW_MINIMIZE: IconDef = IconDef {
    strokes: &[&[(0.15, 0.5), (0.85, 0.5)]],
    width: W,
};

const WINDOW_MAXIMIZE: IconDef = IconDef {
    strokes: &[&[
        (0.18, 0.18),
        (0.82, 0.18),
        (0.82, 0.82),
        (0.18, 0.82),
        (0.18, 0.18),
    ]],
    width: W,
};

/// 最大化されているときの「元に戻す」。四角が 2 枚重なった形
const WINDOW_RESTORE: IconDef = IconDef {
    strokes: &[
        &[
            (0.12, 0.32),
            (0.66, 0.32),
            (0.66, 0.88),
            (0.12, 0.88),
            (0.12, 0.32),
        ],
        &[
            (0.32, 0.32),
            (0.32, 0.12),
            (0.88, 0.12),
            (0.88, 0.68),
            (0.66, 0.68),
        ],
    ],
    width: W,
};

const WINDOW_CLOSE: IconDef = IconDef {
    strokes: &[&[(0.18, 0.18), (0.82, 0.82)], &[(0.82, 0.18), (0.18, 0.82)]],
    width: W,
};

/// `#`。縦棒はわずかに傾ける。まっすぐだと記号ではなく格子に見える
const CHANNEL_TEXT: IconDef = IconDef {
    strokes: &[
        &[(0.42, 0.08), (0.30, 0.92)],
        &[(0.74, 0.08), (0.62, 0.92)],
        &[(0.12, 0.36), (0.86, 0.36)],
        &[(0.08, 0.64), (0.82, 0.64)],
    ],
    width: 1.2 / 12.0,
};

/// スピーカー。四角と、右へ広がる 2 本の弧の代わりの斜線
const CHANNEL_VOICE: IconDef = IconDef {
    strokes: &[
        &[
            (0.10, 0.38),
            (0.28, 0.38),
            (0.50, 0.16),
            (0.50, 0.84),
            (0.28, 0.62),
            (0.10, 0.62),
            (0.10, 0.38),
        ],
        &[(0.66, 0.34), (0.74, 0.50), (0.66, 0.66)],
        &[(0.82, 0.22), (0.94, 0.50), (0.82, 0.78)],
    ],
    width: 1.2 / 12.0,
};

/// 書類挟み。**開いたフォルダの見出しに使う**。
///
/// 中身を 2×2 で敷き詰めた閉じた姿と並ぶので、**閉じた姿と間違えようが
/// ない形**でなければならない。つまみを左上に出して輪郭だけで描く
const FOLDER: IconDef = IconDef {
    strokes: &[&[
        (0.14, 0.76),
        (0.14, 0.26),
        (0.40, 0.26),
        (0.48, 0.36),
        (0.86, 0.36),
        (0.86, 0.76),
        (0.14, 0.76),
    ]],
    width: 1.2 / 12.0,
};

// ─────────────────────────────────────────────────────── ラスタライズ

impl IconDef {
    /// 一辺 `size` ピクセルの RGBA8 マスクを作る。
    ///
    /// グリフと同じ扱いにするため `(255, 255, 255, alpha)` で返す。色は
    /// シェーダが掛ける ([`spec/06-renderer.md`] 6.1)。
    pub fn rasterize(&self, size: u32) -> Vec<u8> {
        let n = size.max(1);
        let mut out = vec![0u8; (n * n * 4) as usize];
        // 単位正方形での距離をピクセルへ直すための倍率
        let s = n as f32;
        let half = self.width * s * 0.5;

        for y in 0..n {
            for x in 0..n {
                // ピクセルの中心で測る
                let px = (x as f32 + 0.5) / s;
                let py = (y as f32 + 0.5) / s;

                let mut d = f32::MAX;
                for stroke in self.strokes {
                    for seg in stroke.windows(2) {
                        d = d.min(distance_to_segment(px, py, seg[0], seg[1]));
                    }
                }

                // 距離をピクセルに直し、1px の傾斜でアンチエイリアスする。
                // fwidth が使えない CPU 側では、これが素直で環境にも依存しない
                let alpha = (half - d * s + 0.5).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let o = ((y * n + x) * 4) as usize;
                out[o] = 255;
                out[o + 1] = 255;
                out[o + 2] = 255;
                out[o + 3] = (alpha * 255.0).round() as u8;
            }
        }
        out
    }
}

/// 点と線分の距離。折れ線の継ぎ目と端は丸くなる。
fn distance_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (ax, ay) = a;
    let (bx, by) = b;
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);

    let len2 = vx * vx + vy * vy;
    // 長さ 0 の線分は点として扱う
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0)
    };

    let dx = wx - vx * t;
    let dy = wy - vy * t;
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_names_are_not_an_error() {
        assert_eq!(lookup("window.close").map(|(n, _)| n), Some("window.close"));
        assert!(lookup("まだ存在しないアイコン").is_none());
    }

    /// すべての制御点が単位正方形に収まっていること。
    /// はみ出すと切れたアイコンになる
    #[test]
    fn control_points_stay_inside_the_unit_square() {
        for (name, def) in ICONS {
            for stroke in def.strokes {
                for &(x, y) in *stroke {
                    assert!((0.0..=1.0).contains(&x), "{name}: x={x} が範囲外");
                    assert!((0.0..=1.0).contains(&y), "{name}: y={y} が範囲外");
                }
                assert!(stroke.len() >= 2, "{name}: 線分になっていない");
            }
            assert!(def.width > 0.0, "{name}: 太さが 0");
        }
    }

    /// 一覧に重複がないこと。あると先に書いたほうしか引けない
    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in ICONS {
            assert!(seen.insert(*name), "重複したアイコン名: {name}");
        }
    }

    #[test]
    fn distance_to_a_segment_is_measured_from_the_nearest_point() {
        // 線分の真横
        let d = distance_to_segment(0.5, 0.0, (0.0, 0.0), (1.0, 0.0));
        assert!(d.abs() < 1e-6);
        // 線分の外側は端点からの距離になる (端が丸い)
        let d = distance_to_segment(2.0, 0.0, (0.0, 0.0), (1.0, 0.0));
        assert!((d - 1.0).abs() < 1e-6);
    }

    /// 描いたものが実際に不透明な画素を持つこと。
    /// 太さや座標を間違えると真っ白なまま気づけない
    #[test]
    fn rasterising_produces_visible_pixels() {
        for (name, def) in ICONS {
            let px = def.rasterize(16);
            let opaque = px.chunks(4).filter(|p| p[3] > 128).count();
            assert!(opaque > 4, "{name}: 濃い画素が {opaque} 個しかない");
            // 全部塗りつぶしてしまっていないこと
            assert!(opaque < 16 * 16 / 2, "{name}: 塗りすぎ ({opaque})");
        }
    }

    /// 同じ大きさなら毎回同じ結果になること (`EXT-020` の前提)
    #[test]
    fn rasterising_is_deterministic() {
        let (_, def) = lookup("window.close").unwrap();
        assert_eq!(def.rasterize(12), def.rasterize(12));
        assert_eq!(def.rasterize(12).len(), 12 * 12 * 4);
    }
}
