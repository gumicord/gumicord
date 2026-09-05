//! Icons, drawn as textures rather than font glyphs.
//!
//! Spelling the title bar buttons as `−` `□` `✕` leaves their weight and size
//! to the typeface and off the pixel grid; three of them failing to line up is
//! correct as text and wrong as icons. Segoe Fluent Icons would look like this
//! only on Windows, where every platform is meant to draw the same result.
//!
//! Each icon is a set of polylines on the unit square, rasterised once its
//! pixel size is known and packed into the glyph atlas. SVG would bring a
//! parser and a path filler; line segments need only a point-to-segment
//! distance, and give the same bits everywhere.

/// Polylines on the unit square.
#[derive(Debug, Clone, Copy)]
pub struct IconDef {
    /// Each element is one continuous line.
    strokes: &'static [&'static [(f32, f32)]],
    /// Stroke width, relative to the unit square.
    width: f32,
}

/// The icons this exposes, by name.
pub static ICONS: &[(&str, IconDef)] = &[
    ("window.minimize", WINDOW_MINIMIZE),
    ("window.maximize", WINDOW_MAXIMIZE),
    ("window.restore", WINDOW_RESTORE),
    ("window.close", WINDOW_CLOSE),
    // The same drawing, but `window.close` outside the title bar would read as
    // closing the window.
    ("close", WINDOW_CLOSE),
    ("channel.text", CHANNEL_TEXT),
    ("channel.voice", CHANNEL_VOICE),
    ("folder", FOLDER),
    ("copy", COPY),
    ("check", CHECK),
    ("id", ID),
    ("reply", REPLY),
    ("edit", EDIT),
    ("trash", TRASH),
    ("cut", CUT),
    ("paste", PASTE),
    ("select_all", SELECT_ALL),
    ("logout", LOGOUT),
    ("gear", GEAR),
    ("members", MEMBERS),
    ("back", BACK),
];

/// Looks an icon up, returning the interned name alongside it so the atlas can
/// key on a `&'static str` rather than the caller's per-frame `String`.
///
/// An unknown name is not an error: a plugin written for a newer client can ask
/// for one. Nothing is drawn.
pub fn lookup(name: &str) -> Option<(&'static str, &'static IconDef)> {
    ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, def)| (*n, def))
}

// ─────────────────────────────────────────────────────────── Definitions
//
// Added when something uses them, not when they might be handy.

/// The usual width: exactly 1px on a 12px icon.
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

/// Restore from maximised: two overlapping squares.
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

/// A `#`. The uprights lean slightly; upright they read as a grid.
const CHANNEL_TEXT: IconDef = IconDef {
    strokes: &[
        &[(0.42, 0.08), (0.30, 0.92)],
        &[(0.74, 0.08), (0.62, 0.92)],
        &[(0.12, 0.36), (0.86, 0.36)],
        &[(0.08, 0.64), (0.82, 0.64)],
    ],
    width: 1.2 / 12.0,
};

/// A speaker: a box, and two slashes standing in for sound waves.
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

/// Two sheets of paper. Only a corner of the back one shows; drawn in full it
/// collapses into a single square at small sizes.
const COPY: IconDef = IconDef {
    strokes: &[
        &[
            (0.34, 0.30),
            (0.80, 0.30),
            (0.80, 0.84),
            (0.34, 0.84),
            (0.34, 0.30),
        ],
        &[(0.22, 0.70), (0.22, 0.16), (0.66, 0.16)],
    ],
    width: 1.2 / 12.0,
};

/// A tick.
const CHECK: IconDef = IconDef {
    strokes: &[&[(0.20, 0.52), (0.42, 0.74), (0.80, 0.28)]],
    width: 1.4 / 12.0,
};

/// A rounded frame with two lines. No digits: a shape that changes with the
/// number of them will not line up beside its neighbours.
const ID: IconDef = IconDef {
    strokes: &[
        &[
            (0.14, 0.28),
            (0.86, 0.28),
            (0.86, 0.72),
            (0.14, 0.72),
            (0.14, 0.28),
        ],
        &[(0.32, 0.42), (0.32, 0.58)],
        &[(0.48, 0.42), (0.68, 0.42), (0.68, 0.58), (0.48, 0.58)],
    ],
    width: 1.2 / 12.0,
};

/// An arrow turning left; a reply goes back.
const REPLY: IconDef = IconDef {
    strokes: &[
        &[(0.38, 0.26), (0.16, 0.46), (0.38, 0.66)],
        &[(0.16, 0.46), (0.62, 0.46), (0.62, 0.46), (0.84, 0.66)],
    ],
    width: 1.2 / 12.0,
};

/// A slanted pen, with a blunt tip: a point just thickens the line when small.
const EDIT: IconDef = IconDef {
    strokes: &[
        &[
            (0.22, 0.78),
            (0.26, 0.62),
            (0.68, 0.20),
            (0.82, 0.34),
            (0.40, 0.76),
            (0.22, 0.78),
        ],
        &[(0.60, 0.28), (0.74, 0.42)],
    ],
    width: 1.1 / 12.0,
};

/// A lid and a body. No lines inside; they merge into a blob when small.
const TRASH: IconDef = IconDef {
    strokes: &[
        &[(0.16, 0.30), (0.84, 0.30)],
        &[(0.40, 0.30), (0.40, 0.20), (0.60, 0.20), (0.60, 0.30)],
        &[(0.26, 0.30), (0.30, 0.82), (0.70, 0.82), (0.74, 0.30)],
    ],
    width: 1.1 / 12.0,
};

/// Scissors.
const CUT: IconDef = IconDef {
    strokes: &[
        &[(0.24, 0.16), (0.66, 0.66)],
        &[(0.76, 0.16), (0.34, 0.66)],
        &[
            (0.34, 0.72),
            (0.24, 0.82),
            (0.34, 0.88),
            (0.42, 0.80),
            (0.34, 0.72),
        ],
        &[
            (0.66, 0.72),
            (0.76, 0.82),
            (0.66, 0.88),
            (0.58, 0.80),
            (0.66, 0.72),
        ],
    ],
    width: 1.0 / 12.0,
};

/// A clipboard and a sheet.
const PASTE: IconDef = IconDef {
    strokes: &[
        &[(0.24, 0.24), (0.24, 0.84), (0.76, 0.84), (0.76, 0.24)],
        &[
            (0.38, 0.24),
            (0.38, 0.14),
            (0.62, 0.14),
            (0.62, 0.24),
            (0.38, 0.24),
        ],
        &[(0.36, 0.46), (0.64, 0.46)],
        &[(0.36, 0.62), (0.64, 0.62)],
    ],
    width: 1.1 / 12.0,
};

/// A frame around every line.
/// A door with an arrow leaving through it.
const LOGOUT: IconDef = IconDef {
    strokes: &[
        &[(0.52, 0.18), (0.20, 0.18), (0.20, 0.82), (0.52, 0.82)],
        &[(0.44, 0.50), (0.82, 0.50)],
        &[(0.66, 0.34), (0.82, 0.50), (0.66, 0.66)],
    ],
    width: 1.1 / 12.0,
};

const SELECT_ALL: IconDef = IconDef {
    strokes: &[
        &[
            (0.14, 0.22),
            (0.86, 0.22),
            (0.86, 0.78),
            (0.14, 0.78),
            (0.14, 0.22),
        ],
        &[(0.28, 0.38), (0.72, 0.38)],
        &[(0.28, 0.50), (0.72, 0.50)],
        &[(0.28, 0.62), (0.56, 0.62)],
    ],
    width: 1.1 / 12.0,
};

/// A folder, used as an open folder's heading. It sits beside the closed form,
/// which is a 2x2 of its contents, so it must not be mistakable for it: outline
/// only, with the tab at the top left.
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

/// A gear: two octagon rings with eight teeth. Octagons, not circles: at
/// 16px a true circle's segments would alias unevenly, while eight straight
/// runs read as a ring.
const GEAR: IconDef = IconDef {
    strokes: &[
        &[
            (0.80, 0.50),
            (0.71, 0.71),
            (0.50, 0.80),
            (0.29, 0.71),
            (0.20, 0.50),
            (0.29, 0.29),
            (0.50, 0.20),
            (0.71, 0.29),
            (0.80, 0.50),
        ],
        &[
            (0.64, 0.50),
            (0.60, 0.60),
            (0.50, 0.64),
            (0.40, 0.60),
            (0.36, 0.50),
            (0.40, 0.40),
            (0.50, 0.36),
            (0.60, 0.40),
            (0.64, 0.50),
        ],
        &[(0.80, 0.50), (0.93, 0.50)],
        &[(0.71, 0.71), (0.80, 0.80)],
        &[(0.50, 0.80), (0.50, 0.93)],
        &[(0.29, 0.71), (0.20, 0.80)],
        &[(0.20, 0.50), (0.07, 0.50)],
        &[(0.29, 0.29), (0.20, 0.20)],
        &[(0.50, 0.20), (0.50, 0.07)],
        &[(0.71, 0.29), (0.80, 0.20)],
    ],
    width: 1.1 / 12.0,
};

/// Two people, front and back: the member list's mark.
const MEMBERS: IconDef = IconDef {
    strokes: &[
        &[
            (0.47, 0.30),
            (0.44, 0.39),
            (0.36, 0.43),
            (0.28, 0.39),
            (0.25, 0.30),
            (0.28, 0.21),
            (0.36, 0.17),
            (0.44, 0.21),
            (0.47, 0.30),
        ],
        &[
            (0.16, 0.84),
            (0.16, 0.72),
            (0.36, 0.58),
            (0.56, 0.72),
            (0.56, 0.84),
        ],
        &[
            (0.76, 0.34),
            (0.74, 0.41),
            (0.68, 0.44),
            (0.62, 0.41),
            (0.60, 0.34),
            (0.62, 0.27),
            (0.68, 0.24),
            (0.74, 0.27),
            (0.76, 0.34),
        ],
        &[
            (0.58, 0.84),
            (0.58, 0.74),
            (0.68, 0.66),
            (0.78, 0.74),
            (0.78, 0.84),
        ],
    ],
    width: 1.1 / 12.0,
};

/// A left chevron: back to the lists.
const BACK: IconDef = IconDef {
    strokes: &[&[(0.62, 0.24), (0.36, 0.50), (0.62, 0.76)]],
    width: 1.4 / 12.0,
};

// ─────────────────────────────────────────────────────── Rasterising

impl IconDef {
    /// Rasterises a `size` square RGBA8 mask.
    ///
    /// White with an alpha, like a glyph, so the shader applies the colour.
    pub fn rasterize(&self, size: u32) -> Vec<u8> {
        let n = size.max(1);
        let mut out = vec![0u8; (n * n * 4) as usize];
        // Scales a unit-square distance into pixels.
        let s = n as f32;
        let half = self.width * s * 0.5;

        for y in 0..n {
            for x in 0..n {
                // Measured at the pixel centre.
                let px = (x as f32 + 0.5) / s;
                let py = (y as f32 + 0.5) / s;

                let mut d = f32::MAX;
                for stroke in self.strokes {
                    for seg in stroke.windows(2) {
                        d = d.min(distance_to_segment(px, py, seg[0], seg[1]));
                    }
                }

                // A 1px ramp antialiases the edge. Without `fwidth` on the CPU
                // side this is both the plain way and a portable one.
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

/// Point-to-segment distance, which rounds joins and ends.
fn distance_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (ax, ay) = a;
    let (bx, by) = b;
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);

    let len2 = vx * vx + vy * vy;
    // A zero-length segment is a point.
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

    /// Every point stays inside the unit square; outside it the icon is clipped.
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

    /// No duplicate names; only the first would ever be found.
    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in ICONS {
            assert!(seen.insert(*name), "重複したアイコン名: {name}");
        }
    }

    #[test]
    fn distance_to_a_segment_is_measured_from_the_nearest_point() {
        // Beside the segment.
        let d = distance_to_segment(0.5, 0.0, (0.0, 0.0), (1.0, 0.0));
        assert!(d.abs() < 1e-6);
        // Past the end, the distance is to the endpoint, so ends are round.
        let d = distance_to_segment(2.0, 0.0, (0.0, 0.0), (1.0, 0.0));
        assert!((d - 1.0).abs() < 1e-6);
    }

    /// Every icon actually marks pixels; a bad width or coordinate would leave
    /// a blank that nothing else catches.
    #[test]
    fn rasterising_produces_visible_pixels() {
        for (name, def) in ICONS {
            let px = def.rasterize(16);
            let opaque = px.chunks(4).filter(|p| p[3] > 128).count();
            assert!(opaque > 4, "{name}: 濃い画素が {opaque} 個しかない");
            // And does not fill the whole square.
            assert!(opaque < 16 * 16 / 2, "{name}: 塗りすぎ ({opaque})");
        }
    }

    /// The same size gives the same bits every time.
    #[test]
    fn rasterising_is_deterministic() {
        let (_, def) = lookup("window.close").unwrap();
        assert_eq!(def.rasterize(12), def.rasterize(12));
        assert_eq!(def.rasterize(12).len(), 12 * 12 * 4);
    }
}
