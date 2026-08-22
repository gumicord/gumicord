//! 幾何の基本型。**すべて論理ピクセル**である。
//!
//! 物理ピクセルへの変換はレイアウトの最後に一度だけ行う
//! ([`spec/06-renderer.md`] 3 章)。この型を物理ピクセルで使わない。

/// 大きさ (論理 px)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };

    pub const fn new(w: f32, h: f32) -> Self {
        Size { w, h }
    }

    /// 負にならないように丸める。制約から余白を引くと簡単に負になる。
    pub fn non_negative(self) -> Self {
        Size {
            w: self.w.max(0.0),
            h: self.h.max(0.0),
        }
    }
}

/// 矩形 (論理 px)。左上原点。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };

    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub const fn from_size(size: Size) -> Self {
        Rect {
            x: 0.0,
            y: 0.0,
            w: size.w,
            h: size.h,
        }
    }

    pub const fn size(self) -> Size {
        Size {
            w: self.w,
            h: self.h,
        }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// 四辺を内側へ縮める。
    pub fn deflate(self, e: Edges) -> Rect {
        Rect {
            x: self.x + e.left,
            y: self.y + e.top,
            w: (self.w - e.left - e.right).max(0.0),
            h: (self.h - e.top - e.bottom).max(0.0),
        }
    }

    /// 全周を同じ量だけ内側へ縮める。枠線の内側を求めるのに使う。
    pub fn inset(self, v: f32) -> Rect {
        self.deflate(Edges::all(v))
    }

    /// 交差。重なりがなければ幅か高さが 0 になる。
    pub fn intersect(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        Rect {
            x,
            y,
            w: (r - x).max(0.0),
            h: (b - y).max(0.0),
        }
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

pub use gumicord_uitree::value::Edges;

/// 余白の合計。主軸・交差軸それぞれで引く量を求めるのに使う。
pub trait EdgesExt {
    fn horizontal(&self) -> f32;
    fn vertical(&self) -> f32;
}

impl EdgesExt for Edges {
    fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_never_goes_negative() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0).deflate(Edges::all(20.0));
        assert_eq!(r.w, 0.0);
        assert_eq!(r.h, 0.0);
    }

    #[test]
    fn intersect_of_disjoint_is_empty() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 20.0, 10.0, 10.0);
        assert!(a.intersect(b).is_empty());
    }

    #[test]
    fn contains_excludes_the_far_edge() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(0.0, 0.0));
        assert!(r.contains(9.9, 9.9));
        assert!(!r.contains(10.0, 5.0), "右辺は含まない");
    }
}
