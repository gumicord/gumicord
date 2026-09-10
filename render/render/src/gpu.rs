//! wgpu setup and submission.
//!
//! Which backends are searched is narrowed per OS on purpose. An unsupported
//! backend does not merely make `request_adapter` return `None`: on the S1 test
//! machine an Intel Vulkan ICD segfaulted while the instance was being created
//! and took the process with it. Windows tries GL first on measurement — the
//! same scene resident in 18.1 MB against DX12's 285.7 MB.
//!
//! Each candidate backend is created in a probe child first ([`probe`]), so a
//! broken driver kills the child and the client starts without it instead of
//! dying with it.

use crate::draw::{DrawList, FLOATS_PER_GLYPH, FLOATS_PER_RECT, RunKind};

/// Why the GPU could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("サーフェスを作れない: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    #[error("対応する GPU アダプタが見つからない")]
    NoAdapter,
    #[error("GPU デバイスを取得できない: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("サーフェスがアダプタに対応していない")]
    Incompatible,
}

/// Whether a frame reached the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// It did.
    Yes,
    /// Nothing to draw, minimised or hidden. Not worth retrying.
    Skipped,
    /// It did not. The caller must ask for another redraw.
    Failed,
}

/// The initial instance capacity; doubled and rebuilt when it runs out.
const INITIAL_RECTS: usize = 4096;
const INITIAL_GLYPHS: usize = 16384;

pub struct Gpu {
    output: Output,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,

    rect_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,

    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    atlas_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    mip_sampler: wgpu::Sampler,

    rect_buf: wgpu::Buffer,
    rect_capacity: usize,
    glyph_buf: wgpu::Buffer,
    glyph_capacity: usize,
    /// What the buffers held last frame, for uploading only what changed.
    last_rects: Vec<f32>,
    last_glyphs: Vec<f32>,
    /// Microseconds of the last submit, split at present: uploading and
    /// encoding versus waiting for the screen.
    last_upload_us: u64,
    last_present_us: u64,

    pub backend: wgpu::Backend,
    pub adapter_name: String,
}

/// Where frames go. Screenshots render into a texture with a pinned format
/// instead of whatever the surface negotiated, so every machine compares
/// against the same bytes.
enum Output {
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Texture {
        texture: wgpu::Texture,
        size: (u32, u32),
    },
}

/// Fixed screenshot format. Surface formats may differ by platform.
const HEADLESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// What the frame draws into. A surface frame presents afterwards; a
/// texture view is used as-is.
enum Frame {
    Surface(wgpu::SurfaceTexture),
    Texture,
}

impl Gpu {
    pub fn new(
        target: wgpu::SurfaceTarget<'static>,
        width: u32,
        height: u32,
        probe_cache: Option<&std::path::Path>,
    ) -> Result<Self, GpuError> {
        let (instance, backends) = Self::open_instance(probe_cache)?;
        let surface = instance.create_surface(target)?;

        let adapter =
            pick_adapter(&instance, Some(&surface), backends).ok_or(GpuError::NoAdapter)?;
        let info = adapter.get_info();
        tracing::info!(backend = ?info.backend, adapter = info.name, "adapter picked");

        let (device, queue) = Self::open_device(&adapter)?;

        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(GpuError::Incompatible)?;

        // Pinned, so every platform draws the same result.
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(config.format);
        // Fifo = VSync
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        let format = config.format;

        Self::assemble(
            device,
            queue,
            Output::Surface { surface, config },
            format,
            info,
        )
    }

    /// No window: draws into a texture for screenshots instead. The format
    /// is pinned rather than negotiated, so shots compare across machines.
    pub fn headless(
        width: u32,
        height: u32,
        probe_cache: Option<&std::path::Path>,
    ) -> Result<Self, GpuError> {
        let (instance, backends) = Self::open_instance(probe_cache)?;
        let adapter = pick_adapter(&instance, None, backends).ok_or(GpuError::NoAdapter)?;
        let info = adapter.get_info();

        let (device, queue) = Self::open_device(&adapter)?;

        let size = (width.max(1), height.max(1));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gumicord-shot"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HEADLESS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        Self::assemble(
            device,
            queue,
            Output::Texture { texture, size },
            HEADLESS_FORMAT,
            info,
        )
    }

    /// Shared instance setup: backend narrowing before anything exists.
    /// A broken driver crashes while the instance is being created, not
    /// while adapters are enumerated, so this runs first in both modes.
    fn open_instance(
        probe_cache: Option<&std::path::Path>,
    ) -> Result<(wgpu::Instance, wgpu::Backends), GpuError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        // Narrowed before the instance exists: a broken driver crashes while
        // one is being created, not while adapters are enumerated. Each
        // candidate is created in a probe child first, so only survivors
        // reach this process.
        if std::env::var("WGPU_BACKEND").is_err() {
            desc.backends = crate::probe::surviving_backends(CANDIDATES, probe_cache);
            if desc.backends.is_empty() {
                return Err(GpuError::NoAdapter);
            }
        }
        let backends = desc.backends;
        Ok((wgpu::Instance::new(desc), backends))
    }

    fn open_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), GpuError> {
        Ok(pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("gumicord"),
                required_features: wgpu::Features::empty(),
                // Downlevel defaults still work on GLES 3.0 class hardware,
                // which mobile will need.
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            },
        ))?)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        device: wgpu::Device,
        queue: wgpu::Queue,
        output: Output,
        format: wgpu::TextureFormat,
        info: wgpu::AdapterInfo,
    ) -> Result<Self, GpuError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gumicord"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Glyphs are rasterised at physical pixel size and never scaled;
        // linear only so a fractional position blurs rather than snaps.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Backgrounds minify across whole screens, so they sail through
        // mipmaps; the atlas above stays single-level and sharp.
        let mip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("background"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let rect_pipeline = make_pipeline(
            &device,
            &shader,
            &[&globals_layout],
            format,
            "rect",
            "vs_rect",
            "fs_rect",
            &wgpu::VertexBufferLayout {
                array_stride: (FLOATS_PER_RECT * 4) as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 1,
                    },
                    // Corner radius and border width.
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 32,
                        shader_location: 2,
                    },
                ],
            },
        );

        let text_pipeline = make_pipeline(
            &device,
            &shader,
            &[&globals_layout, &atlas_layout],
            format,
            "text",
            "vs_text",
            "fs_text",
            &wgpu::VertexBufferLayout {
                array_stride: (FLOATS_PER_GLYPH * 4) as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 32,
                        shader_location: 2,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32,
                        offset: 48,
                        shader_location: 3,
                    },
                    // Corner radius in physical pixels; round avatars need it.
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32,
                        offset: 52,
                        shader_location: 4,
                    },
                ],
            },
        );

        let rect_buf = make_instance_buffer(&device, "rects", INITIAL_RECTS * FLOATS_PER_RECT);
        let glyph_buf = make_instance_buffer(&device, "glyphs", INITIAL_GLYPHS * FLOATS_PER_GLYPH);

        tracing::info!(
            backend = ?info.backend,
            adapter = %info.name,
            device_type = ?info.device_type,
            "GPU を初期化した"
        );

        Ok(Gpu {
            output,
            device,
            queue,
            rect_pipeline,
            text_pipeline,
            globals_buf,
            globals_bind,
            atlas_layout,
            sampler,
            mip_sampler,
            rect_buf,
            rect_capacity: INITIAL_RECTS,
            glyph_buf,
            glyph_capacity: INITIAL_GLYPHS,
            last_rects: Vec::new(),
            last_glyphs: Vec::new(),
            last_upload_us: 0,
            last_present_us: 0,
            backend: info.backend,
            adapter_name: info.name,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        match &self.output {
            Output::Surface { config, .. } => (config.width, config.height),
            Output::Texture { size, .. } => *size,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width, height) == self.size() {
            return;
        }
        tracing::debug!(
            from = ?self.size(),
            to = ?(width, height),
            "サーフェスを作り直す"
        );
        match &mut self.output {
            Output::Surface { surface, config } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            Output::Texture { texture, size } => {
                *texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("gumicord-shot"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: HEADLESS_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                *size = (width, height);
            }
        }
    }

    /// The bind group for the glyph atlas; recreate it when the atlas grows.
    pub fn atlas_bind_group(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.bind_texture(view, &self.sampler, "atlas")
    }

    /// The bind group for one background texture, with mip filtering.
    pub fn background_bind_group(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.bind_texture(view, &self.mip_sampler, "gumicord-background")
    }

    fn bind_texture(
        &self,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        label: &str,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// Acquires the texture to draw into.
    ///
    /// A resize almost always yields `Outdated` first. Giving up there leaves
    /// the window blank until the next input under `ControlFlow::Wait`, so it
    /// reconfigures and tries once more.
    fn acquire(&mut self) -> Result<Frame, Presented> {
        let Output::Surface { surface, config } = &mut self.output else {
            return Ok(Frame::Texture);
        };
        for attempt in 0..2 {
            match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f)
                | wgpu::CurrentSurfaceTexture::Suboptimal(f) => return Ok(Frame::Surface(f)),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    surface.configure(&self.device, config);
                    if attempt == 1 {
                        tracing::debug!("再構成してもサーフェスを取れなかった");
                    }
                }
                // Minimised or hidden. Asking for a redraw here would spin for
                // as long as it stays hidden.
                wgpu::CurrentSurfaceTexture::Occluded => return Err(Presented::Skipped),
                other => {
                    tracing::warn!(?other, "サーフェスを取得できなかった");
                    return Err(Presented::Skipped);
                }
            }
        }
        Err(Presented::Failed)
    }

    /// Submits and presents, reporting whether the frame reached the screen.
    ///
    /// `clear` is the fill colour, taken from the theme's `app.window`.
    ///
    /// A `Failed` needs another redraw request: under `ControlFlow::Wait`,
    /// giving up leaves the window blank until the next input.
    ///
    /// One bind group per atlas page, since a bind group names a single
    /// texture. Background textures ride alongside under their own runs.
    #[must_use]
    pub fn submit(
        &mut self,
        dl: &DrawList,
        atlas_binds: &[wgpu::BindGroup],
        bg_binds: &[wgpu::BindGroup],
        clear: [f32; 4],
    ) -> Presented {
        let submit_start = std::time::Instant::now();
        let frame = match self.acquire() {
            Ok(f) => f,
            Err(why) => return why,
        };
        let (width, height) = self.size();

        let grew = self.ensure_capacity(dl);

        self.queue.write_buffer(
            &self.globals_buf,
            0,
            bytemuck::cast_slice(&[width as f32, height as f32, 0.0, 0.0]),
        );
        upload_instances(
            &self.queue,
            &self.rect_buf,
            &dl.rects,
            &mut self.last_rects,
            grew,
        );
        upload_instances(
            &self.queue,
            &self.glyph_buf,
            &dl.glyphs,
            &mut self.last_glyphs,
            grew,
        );

        let view;
        let mut presentable = None;
        match frame {
            Frame::Surface(frame) => {
                view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                presentable = Some(frame);
            }
            Frame::Texture => {
                // Cloned, so the view below does not borrow the output
                // while the pass below needs `&mut self`.
                let texture = match &self.output {
                    Output::Texture { texture, .. } => texture.clone(),
                    Output::Surface { .. } => {
                        unreachable!("texture frame without texture output")
                    }
                };
                view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            }
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gumicord"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0] as f64,
                            g: clear[1] as f64,
                            b: clear[2] as f64,
                            a: clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            let (w, h) = (width, height);
            let mut current: Option<(RunKind, u32)> = None;
            let mut current_scissor: Option<Option<[u32; 4]>> = None;

            for run in &dl.runs {
                if run.count == 0 {
                    continue;
                }
                if let Some(s) = run.scissor
                    && (s[2] == 0 || s[3] == 0)
                {
                    // A collapsed clip has nothing inside it.
                    continue;
                }

                if current != Some((run.kind, run.page)) {
                    match run.kind {
                        RunKind::Rect => {
                            pass.set_pipeline(&self.rect_pipeline);
                            pass.set_bind_group(0, &self.globals_bind, &[]);
                            pass.set_vertex_buffer(0, self.rect_buf.slice(..));
                        }
                        RunKind::Glyph => {
                            pass.set_pipeline(&self.text_pipeline);
                            pass.set_bind_group(0, &self.globals_bind, &[]);
                            // A missing page is skipped; page zero would draw
                            // the wrong characters.
                            let Some(bind) = atlas_binds.get(run.page as usize) else {
                                continue;
                            };
                            pass.set_bind_group(1, bind, &[]);
                            pass.set_vertex_buffer(0, self.glyph_buf.slice(..));
                        }
                        RunKind::Image => {
                            pass.set_pipeline(&self.text_pipeline);
                            pass.set_bind_group(0, &self.globals_bind, &[]);
                            // A missing texture is skipped; the colour
                            // underneath already drew.
                            let Some(bind) = bg_binds.get(run.page as usize) else {
                                continue;
                            };
                            pass.set_bind_group(1, bind, &[]);
                            pass.set_vertex_buffer(0, self.glyph_buf.slice(..));
                        }
                    }
                    current = Some((run.kind, run.page));
                }

                if current_scissor != Some(run.scissor) {
                    match run.scissor {
                        Some(s) => pass.set_scissor_rect(s[0], s[1], s[2], s[3]),
                        None => pass.set_scissor_rect(0, 0, w, h),
                    }
                    current_scissor = Some(run.scissor);
                }

                pass.draw(0..4, run.first..(run.first + run.count));
            }
        }
        self.last_upload_us = submit_start.elapsed().as_micros() as u64;
        let presented_at = std::time::Instant::now();
        self.queue.submit(Some(encoder.finish()));
        if let Some(frame) = presentable {
            self.queue.present(frame);
        }
        self.last_present_us = presented_at.elapsed().as_micros() as u64;
        Presented::Yes
    }

    /// Copies the texture output into host memory as tightly packed RGBA8.
    /// `None` for window output, which presents instead of reading back.
    pub fn read_pixels(&self) -> Option<Vec<u8>> {
        let (texture, (width, height)) = match &self.output {
            Output::Texture { texture, size } => (texture, *size),
            Output::Surface { .. } => return None,
        };
        // Rows copy padded to 256 bytes; the padding is stripped below.
        let stride = width as usize * 4;
        let padded = stride.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gumicord-shot-read"),
            size: (padded * height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gumicord-shot-read"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        match rx.recv() {
            Ok(Ok(())) => {}
            _ => return None,
        }
        let mapped = slice.get_mapped_range().ok()?;
        let mut out = vec![0u8; stride * height as usize];
        for (dst, src) in out
            .chunks_exact_mut(stride)
            .zip(mapped.chunks_exact(padded))
        {
            dst.copy_from_slice(&src[..stride]);
        }
        drop(mapped);
        buffer.unmap();
        Some(out)
    }

    /// Microseconds of the last submit: uploading and encoding first, then
    /// waiting for the screen.
    pub fn last_submit_us(&self) -> (u64, u64) {
        (self.last_upload_us, self.last_present_us)
    }

    fn ensure_capacity(&mut self, dl: &DrawList) -> bool {
        let mut grew = false;
        let rects = dl.rect_count() as usize;
        if rects > self.rect_capacity {
            self.rect_capacity = rects.next_power_of_two();
            self.rect_buf =
                make_instance_buffer(&self.device, "rects", self.rect_capacity * FLOATS_PER_RECT);
            tracing::debug!(capacity = self.rect_capacity, "矩形バッファを広げた");
            grew = true;
        }
        let glyphs = dl.glyph_count() as usize;
        if glyphs > self.glyph_capacity {
            self.glyph_capacity = glyphs.next_power_of_two();
            self.glyph_buf = make_instance_buffer(
                &self.device,
                "glyphs",
                self.glyph_capacity * FLOATS_PER_GLYPH,
            );
            tracing::debug!(capacity = self.glyph_capacity, "グリフバッファを広げた");
            grew = true;
        }
        grew
    }
}

/// Uploads only what changed since the last frame. Static frames redraw on
/// wakeups that change nothing drawable — a blink phase, a hover that moved
/// away — and those upload nothing at all now.
fn upload_instances(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    current: &[f32],
    last: &mut Vec<f32>,
    grew: bool,
) {
    if grew || last.len() != current.len() {
        // A new buffer holds garbage, and a new length moves everything:
        // both upload whole.
        if !current.is_empty() {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(current));
        }
        last.clear();
        last.extend_from_slice(current);
        return;
    }
    let Some(runs) = diff_ranges(last, current) else {
        if !current.is_empty() {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(current));
        }
        last.clear();
        last.extend_from_slice(current);
        return;
    };
    for (start, end) in runs {
        queue.write_buffer(
            buffer,
            (start * 4) as u64,
            bytemuck::cast_slice(&current[start..end]),
        );
    }
    last.clear();
    last.extend_from_slice(current);
}

/// Changed float ranges between frames, merged across small gaps. `None`
/// means the whole buffer should go: too many runs cost more submission
/// calls than one upload saves.
fn diff_ranges(old: &[f32], new: &[f32]) -> Option<Vec<(usize, usize)>> {
    /// Gaps this wide or narrower ride along; splitting would cost more
    /// than re-uploading them.
    const MAX_GAP: usize = 64;
    /// More runs than this, and one upload wins.
    const MAX_RUNS: usize = 8;
    let mut runs = Vec::new();
    let mut i = 0;
    while i < new.len() {
        if old.get(i) == new.get(i) {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        loop {
            while i < new.len() && old.get(i) != new.get(i) {
                i += 1;
            }
            let mut gap = 0;
            while gap < MAX_GAP && i + gap < new.len() && old.get(i + gap) == new.get(i + gap) {
                gap += 1;
            }
            if i + gap >= new.len() || gap == MAX_GAP {
                break;
            }
            i += gap;
        }
        runs.push((start, i));
        if runs.len() > MAX_RUNS {
            return None;
        }
    }
    Some(runs)
}

/// The q-th percentile of samples, for frame-time logs. Sorts a copy; the
/// window is a few hundred long and the log fires rarely.
pub(crate) fn percentile(samples: &[u64], q: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank =
        ((q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[rank]
}

/// The backends to try, in order. Windows omits Vulkan after the S1 crash.
#[cfg(target_os = "windows")]
const CANDIDATES: &[wgpu::Backends] = &[wgpu::Backends::GL, wgpu::Backends::DX12];
#[cfg(any(target_os = "macos", target_os = "ios"))]
const CANDIDATES: &[wgpu::Backends] = &[wgpu::Backends::METAL];
#[cfg(target_os = "android")]
const CANDIDATES: &[wgpu::Backends] = &[wgpu::Backends::GL, wgpu::Backends::VULKAN];
#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
)))]
const CANDIDATES: &[wgpu::Backends] = &[wgpu::Backends::VULKAN, wgpu::Backends::GL];

/// Picks an adapter in candidate order.
///
/// Left to `request_adapter`, the choice is wgpu's; with GL sixteen times
/// lighter than DX12 on Windows, it is not a choice to delegate.
fn pick_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    backends: wgpu::Backends,
) -> Option<wgpu::Adapter> {
    // Headless screenshots prefer software rendering everywhere, so one
    // blessed image holds across machines. The instance backends above
    // still apply, so probed-out drivers stay out.
    if surface.is_none() {
        let fallback = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
            ..Default::default()
        }));
        if let Ok(a) = fallback {
            return Some(a);
        }
    }
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    for wanted in CANDIDATES {
        if !backends.contains(*wanted) {
            continue;
        }
        let found = adapters.iter().find(|a| {
            wanted.contains(wgpu::Backends::from(a.get_info().backend))
                && surface.is_none_or(|s| a.is_surface_supported(s))
        });
        if let Some(a) = found {
            return Some(a.clone());
        }
    }
    // Failing that, anything will do. An explicit `WGPU_BACKEND` lands here.
    // (The headless fallback above already tried software first.)
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: surface,
        ..Default::default()
    }))
    .ok()
}

fn make_instance_buffer(device: &wgpu::Device, label: &str, floats: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (floats * 4) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn make_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layouts: &[&wgpu::BindGroupLayout],
    format: wgpu::TextureFormat,
    label: &str,
    vs: &str,
    fs: &str,
    buffer: &wgpu::VertexBufferLayout<'_>,
) -> wgpu::RenderPipeline {
    let owned: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().copied().map(Some).collect();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &owned,
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers: &[Some(buffer.clone())],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Straight alpha, so translucency composites predictably.
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            // No vertex buffer; four vertices come from the vertex index.
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        // No depth: submission order is stacking order.
        depth_stencil: None,
        // No MSAA: analytic AA from `fwidth` suffices and follows the DPI.
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_frames_upload_nothing() {
        let frame = vec![1.0, 2.0, 3.0];
        assert_eq!(diff_ranges(&frame, &frame), Some(vec![]));
        assert_eq!(diff_ranges(&[], &[]), Some(vec![]));
    }

    #[test]
    fn one_change_makes_one_run() {
        let old = vec![0.0; 100];
        let mut new = old.clone();
        new[42] = 1.0;
        assert_eq!(diff_ranges(&old, &new), Some(vec![(42, 43)]));
    }

    #[test]
    fn small_gaps_ride_along_large_gaps_split() {
        let old = vec![0.0; 300];
        let mut new = old.clone();
        new[10] = 1.0;
        new[20] = 1.0;
        assert_eq!(diff_ranges(&old, &new), Some(vec![(10, 21)]));
        new[200] = 1.0;
        assert_eq!(diff_ranges(&old, &new), Some(vec![(10, 21), (200, 201)]));
    }

    #[test]
    fn a_changed_tail_runs_to_the_end() {
        let old = vec![1.0, 2.0];
        let new = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(diff_ranges(&old, &new), Some(vec![(2, 4)]));
    }

    #[test]
    fn too_many_runs_upload_whole() {
        let old = vec![0.0; 2000];
        let mut new = old.clone();
        for i in (0..2000).step_by(100) {
            new[i] = 1.0;
        }
        assert_eq!(diff_ranges(&old, &new), None);
    }

    #[test]
    fn percentiles_read_off_the_sorted_samples() {
        assert_eq!(percentile(&[], 0.99), 0);
        assert_eq!(percentile(&[7], 0.5), 7);
        let samples: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&samples, 0.5), 51);
        assert_eq!(percentile(&samples, 0.99), 99);
        assert_eq!(percentile(&samples, 0.0), 1);
    }
}
