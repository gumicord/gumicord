//! レンダラ。UITree を受け取り GPU の描画コマンドを出す。
//!
//! **プラットフォーム固有のコードを一切含まない。** OS に触る必要が生じたら
//! それは [`gumicord_platform`] の仕事である。
//!
//! 描画プリミティブは 3 種類しかない:
//! - 角丸矩形 (SDF)
//! - テクスチャ付きクアッド (画像・グリフ)
//! - クリップ矩形
//!
//! ⚠️ **描画にコンピュートシェーダを使わない。** 使うと GL / GLES バックエンドが
//! 選べなくなる。S1 の実測では Windows で DX12 と GL の間に常駐メモリで
//! 16 倍 (285.7 MB vs 18.1 MB) の差があった。
//!
//! S1 の実測: Intel HD 520 で 20,000 インスタンスまで 60fps を維持。
//!
//! 要件: `NFR-001`〜`NFR-007`, `NFR-015`, `EXT-020`〜`EXT-027`
//! 仕様: [`spec/06-renderer.md`]

pub mod draw;
pub mod geom;
pub mod gpu;
pub mod intrinsic;
pub mod layout;
pub mod text;

pub use geom::{Rect, Size};
pub use gpu::GpuError;
pub use intrinsic::{Axis, Cross, Intrinsic, intrinsic};
pub use layout::{SCROLL_TO_END, ScrollState};

use gumicord_uitree::{Key, NodeId, UiNode};

use crate::gpu::Gpu;
use crate::text::TextEngine;

/// 1 フレームで何を描いたか。性能の見張りに使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub nodes: usize,
    pub rects: u32,
    pub glyphs: u32,
    /// draw の発行回数。パイプラインか切り取りが変わるたびに増える
    pub draw_calls: usize,
}

/// 当たり判定の結果 1 件。
///
/// [`crate::layout::Placed`] は木を借用するので、フレームをまたいで持てない。
/// 当たり判定に要るぶんだけ写して持つ。
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: NodeId,
    pub key: Option<Key>,
    pub rect: Rect,
    /// 切り取り矩形。**この外側に出た部分は当たらない。**
    /// スクロールで隠れた項目に反応してはいけない
    pub clip: Option<Rect>,
}

/// UITree を描くもの。
pub struct Renderer {
    gpu: Gpu,
    text: TextEngine,
    atlas_bind: wgpu::BindGroup,
    scale: f32,
    scroll: ScrollState,
    /// 直前のフレームの配置。入力の当たり判定に使う
    hits: Vec<Hit>,
    /// スクロール領域ごとの、はみ出した量
    overflow: std::collections::HashMap<NodeId, f32>,
}

impl Renderer {
    pub fn new(
        target: wgpu::SurfaceTarget<'static>,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<Self, GpuError> {
        let gpu = Gpu::new(target, width, height)?;
        let text = TextEngine::new(&gpu.device, scale);
        let atlas_bind = gpu.atlas_bind_group(text.atlas_view());
        Ok(Renderer {
            gpu,
            text,
            atlas_bind,
            scale,
            scroll: ScrollState::new(),
            hits: Vec::new(),
            overflow: std::collections::HashMap::new(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
    }

    /// DPI が変わった。グリフは物理ピクセルでラスタライズされているので、
    /// アトラスごと作り直す ([`spec/06-renderer.md`] 3 章)。
    pub fn set_scale(&mut self, scale: f32) {
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        self.text.set_scale(&self.gpu.device, scale);
        self.atlas_bind = self.gpu.atlas_bind_group(self.text.atlas_view());
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// 表示領域 (論理 px)。テーマの `when.maxWidth` の照合にも使う
    pub fn viewport(&self) -> Size {
        let (w, h) = self.gpu.size();
        Size::new(w as f32 / self.scale, h as f32 / self.scale)
    }

    pub fn backend(&self) -> wgpu::Backend {
        self.gpu.backend
    }

    pub fn adapter_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    /// スクロールを動かす。`delta` は論理 px。
    ///
    /// 上限は直前のフレームで分かったはみ出し量で抑える。1 フレーム遅れるが、
    /// 動かしてから測り直すと 1 フレームぶん余計にレイアウトすることになる。
    pub fn scroll_by(&mut self, id: NodeId, delta: f32) -> bool {
        let max = self.overflow.get(&id).copied().unwrap_or(0.0);
        if max <= 0.0 {
            return false;
        }
        // まだ動かされていない領域の現在位置は、既定の貼り付き先である
        let default = if intrinsic(id).anchor_end { max } else { 0.0 };
        let cur = self
            .scroll
            .get(&id)
            .copied()
            .unwrap_or(default)
            .clamp(0.0, max);
        let next = (cur + delta).clamp(0.0, max);
        if (next - cur).abs() < 0.5 {
            return false;
        }
        self.scroll.insert(id, next);
        true
    }

    /// スクロール位置を直接置く。[`SCROLL_TO_END`] で一番下に貼り付く。
    pub fn set_scroll(&mut self, id: NodeId, at: f32) {
        self.scroll.insert(id, at);
    }

    /// 1 フレーム描く。木は**スタイル解決済み**でなければならない。
    pub fn render(&mut self, root: &UiNode) -> FrameStats {
        let viewport = self.viewport();
        let layout = layout::layout(root, viewport, self.text.shaper(), &self.scroll);

        self.hits.clear();
        self.hits.extend(layout.placed.iter().map(|p| Hit {
            id: p.node.id,
            key: p.node.key.clone(),
            rect: p.rect,
            clip: p.clip,
        }));
        self.overflow.clone_from(&layout.overflow);

        let dl = draw::build(
            &layout,
            &mut self.text,
            &self.gpu.queue,
            self.scale,
            self.gpu.size(),
        );

        let stats = FrameStats {
            nodes: layout.placed.len(),
            rects: dl.rect_count(),
            glyphs: dl.glyph_count(),
            draw_calls: dl.runs.len(),
        };

        self.gpu.submit(&dl, &self.atlas_bind, CLEAR);
        stats
    }

    /// 点の上にあるノードを手前から順に。座標は**論理 px**。
    ///
    /// 直前に描いたフレームの配置に対して答える。
    pub fn hit_test(&self, x: f32, y: f32) -> impl Iterator<Item = &Hit> {
        self.hits
            .iter()
            .rev()
            .filter(move |h| h.rect.contains(x, y) && h.clip.is_none_or(|c| c.contains(x, y)))
    }
}

/// 最初のフレームが出るまでの色。テーマの背景がすぐ上に載るので、
/// これが見えるのは起動直後の一瞬だけである
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
