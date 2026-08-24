//! The glyph atlas and text shaping.
//!
//! cosmic-text shapes and lays out; the rasterised glyphs go into an atlas
//! texture on the GPU and are drawn as textured quads.
//!
//! The atlas is RGBA8 so colour emoji fit. Mask glyphs are stored as
//! (255,255,255,alpha) and the shader applies the colour.

use std::collections::HashMap;

use cosmic_text::{CacheKey, FontSystem, SwashCache, SwashContent};

pub const ATLAS_SIZE: u32 = 2048;

#[derive(Clone, Copy, Debug)]
pub struct GlyphEntry {
    /// UV within the atlas.
    pub uv: [f32; 4],
    /// Offset from the pen position, in physical pixels.
    pub left: i32,
    pub top: i32,
    pub w: u32,
    pub h: u32,
    /// True for a colour emoji, which the shader leaves alone.
    pub is_color: bool,
}

pub struct GlyphAtlas {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    entries: HashMap<CacheKey, Option<GlyphEntry>>,
    // Shelf packing.
    cursor_x: u32,
    cursor_y: u32,
    shelf_h: u32,
    full: bool,
    pub uploaded: usize,
}

impl GlyphAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph-atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            entries: HashMap::new(),
            cursor_x: 0,
            cursor_y: 0,
            shelf_h: 0,
            full: false,
            uploaded: 0,
        }
    }

    pub fn get(
        &mut self,
        queue: &wgpu::Queue,
        font_system: &mut FontSystem,
        cache: &mut SwashCache,
        key: CacheKey,
    ) -> Option<GlyphEntry> {
        if let Some(e) = self.entries.get(&key) {
            return *e;
        }

        let entry = self.rasterize_and_pack(queue, font_system, cache, key);
        self.entries.insert(key, entry);
        entry
    }

    fn rasterize_and_pack(
        &mut self,
        queue: &wgpu::Queue,
        font_system: &mut FontSystem,
        cache: &mut SwashCache,
        key: CacheKey,
    ) -> Option<GlyphEntry> {
        let image = cache.get_image(font_system, key).as_ref()?;
        let p = image.placement;
        if p.width == 0 || p.height == 0 {
            // Whitespace and the like: nothing to draw, but the metrics still count.
            return Some(GlyphEntry {
                uv: [0.0; 4],
                left: p.left,
                top: p.top,
                w: 0,
                h: 0,
                is_color: false,
            });
        }
        if self.full {
            return None;
        }

        let (w, h) = (p.width, p.height);
        let is_color = matches!(image.content, SwashContent::Color);

        // Normalised to RGBA8.
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        match image.content {
            SwashContent::Mask => {
                for (i, a) in image.data.iter().enumerate() {
                    let o = i * 4;
                    if o + 3 >= rgba.len() {
                        break;
                    }
                    rgba[o] = 255;
                    rgba[o + 1] = 255;
                    rgba[o + 2] = 255;
                    rgba[o + 3] = *a;
                }
            }
            SwashContent::Color | SwashContent::SubpixelMask => {
                let n = rgba.len().min(image.data.len());
                rgba[..n].copy_from_slice(&image.data[..n]);
            }
        }

        // Shelf packing, with 1px of padding so neighbours do not bleed.
        const PAD: u32 = 1;
        if self.cursor_x + w + PAD > ATLAS_SIZE {
            self.cursor_x = 0;
            self.cursor_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.cursor_y + h + PAD > ATLAS_SIZE {
            // The spike ignores a full atlas; the real one needs pages or an LRU.
            eprintln!("[atlas] 溢れました。実装では複数ページ化が必要です");
            self.full = true;
            return None;
        }

        let (x, y) = (self.cursor_x, self.cursor_y);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        self.cursor_x += w + PAD;
        self.shelf_h = self.shelf_h.max(h);
        self.uploaded += 1;

        let s = ATLAS_SIZE as f32;
        Some(GlyphEntry {
            uv: [
                x as f32 / s,
                y as f32 / s,
                (x + w) as f32 / s,
                (y + h) as f32 / s,
            ],
            left: p.left,
            top: p.top,
            w,
            h,
            is_color,
        })
    }
}
