//! Theme background textures: one texture per image, with mipmaps.
//!
//! Avatars share the glyph atlas; backgrounds do not. A wallpaper is bigger
//! than an atlas page, and minifying it every frame without mipmaps
//! shimmers. Each background gets its own texture, uploaded once with a
//! prefiltered chain, and drawn through the same glyph instances.

use std::collections::HashMap;

use crate::text::ImageData;

/// One uploaded background. The view keeps its texture alive.
struct Entry {
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

/// Background images by lookup key, in upload order. The order is the draw
/// index: runs name a texture by position, and positions must not move under
/// a frame that is already built.
#[derive(Default)]
pub struct Backgrounds {
    entries: HashMap<String, Entry>,
    order: Vec<String>,
}

impl Backgrounds {
    /// Uploads an image, building its mipmap chain on the CPU. Returns the
    /// draw index, stable for the key until [`Backgrounds::clear`].
    pub fn put(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: String,
        image: &ImageData,
    ) -> Option<u32> {
        if image.width == 0 || image.height == 0 {
            return None;
        }
        if image.rgba.len() != image.width as usize * image.height as usize * 4 {
            tracing::warn!(key, "pixel count does not match the dimensions");
            return None;
        }
        if let Some(index) = self.index_of(&key) {
            return Some(index);
        }
        let levels = mip_levels(image.width, image.height);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gumicord-background"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut level = (image.rgba.clone(), image.width, image.height);
        for mip in 0..levels {
            let (rgba, w, h) = &level;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(*h),
                },
                wgpu::Extent3d {
                    width: *w,
                    height: *h,
                    depth_or_array_layers: 1,
                },
            );
            if mip + 1 < levels {
                level = downsample_half(&level.0, level.1, level.2);
            }
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let (w, h) = (image.width, image.height);
        self.entries.insert(key.clone(), Entry { view, w, h });
        self.order.push(key);
        Some(self.order.len() as u32 - 1)
    }

    /// The draw index and pixel size, if uploaded.
    pub fn get(&self, key: &str) -> Option<(u32, u32, u32)> {
        let index = self.index_of(key)?;
        let entry = &self.entries[key];
        Some((index, entry.w, entry.h))
    }

    /// One view per uploaded background, in draw-index order.
    pub fn views(&self) -> Vec<&wgpu::TextureView> {
        self.order
            .iter()
            .filter_map(|key| self.entries.get(key).map(|e| &e.view))
            .collect()
    }

    /// Forgets everything, as on a theme switch. Stale keys would otherwise
    /// draw the previous theme's pictures under the new theme's colours.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn index_of(&self, key: &str) -> Option<u32> {
        self.order.iter().position(|k| k == key).map(|i| i as u32)
    }
}

/// Mipmap levels for a size, down to 1x1. Halving rounds up, so this counts
/// how many rounds reach a single texel rather than trusting logarithms.
pub fn mip_levels(w: u32, h: u32) -> u32 {
    let mut n = w.max(h);
    if n == 0 {
        return 1;
    }
    let mut levels = 1u32;
    while n > 1 {
        n = n.div_ceil(2);
        levels += 1;
    }
    levels
}

/// Halves an image by averaging 2x2 blocks. Odd edges clamp, like the blur:
/// dropping the last row would shift the picture by half a texel per level.
pub fn downsample_half(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = (w.max(1).div_ceil(2), h.max(1).div_ceil(2));
    let at = |x: u32, y: u32, c: usize| -> u32 {
        let x = x.min(w - 1);
        let y = y.min(h - 1);
        rgba[(y as usize * w as usize + x as usize) * 4 + c] as u32
    };
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let sum = at(x * 2, y * 2, c)
                    + at(x * 2 + 1, y * 2, c)
                    + at(x * 2, y * 2 + 1, c)
                    + at(x * 2 + 1, y * 2 + 1, c);
                out[(y as usize * nw as usize + x as usize) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    (out, nw, nh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_ends_in_one_texel() {
        assert_eq!(mip_levels(1920, 1080), 12);
        assert_eq!(mip_levels(1024, 1024), 11);
        assert_eq!(mip_levels(1, 1), 1);
        assert_eq!(mip_levels(0, 0), 1);
        assert_eq!(mip_levels(4, 4), 3);
        // Each level halves until nothing is left to halve.
        let (mut w, mut h) = (1920u32, 1080u32);
        for _ in 1..mip_levels(1920, 1080) {
            (w, h) = (w.max(1).div_ceil(2), h.max(1).div_ceil(2));
        }
        assert_eq!((w, h), (1, 1));
    }

    #[test]
    fn halving_averages_blocks() {
        // 2x2 distinct corners average to their mean.
        let rgba = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let (out, w, h) = downsample_half(&rgba, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![128, 128, 128, 255]);
    }

    #[test]
    fn odd_edges_clamp_instead_of_dropping() {
        let rgba = vec![200u8; 12];
        let (out, w, h) = downsample_half(&rgba, 3, 1);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![200u8; 8]);
    }
}
