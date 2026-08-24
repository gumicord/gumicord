//! wgpu setup and submission.
//!
//! Which backends are searched is narrowed per OS on purpose. An unsupported
//! backend does not merely make `request_adapter` return `None`: on the S1 test
//! machine an Intel Vulkan ICD segfaulted while the instance was being created
//! and took the process with it. Windows tries GL first on measurement — the
//! same scene resident in 18.1 MB against DX12's 285.7 MB.
//!
//! Probing in a separate process at startup is still to come, so a broken
//! driver still takes the client down with it.

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
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    rect_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,

    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    atlas_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,

    rect_buf: wgpu::Buffer,
    rect_capacity: usize,
    glyph_buf: wgpu::Buffer,
    glyph_capacity: usize,

    pub backend: wgpu::Backend,
    pub adapter_name: String,
}

impl Gpu {
    pub fn new(
        target: wgpu::SurfaceTarget<'static>,
        width: u32,
        height: u32,
    ) -> Result<Self, GpuError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        // Narrowed before the instance exists: a broken driver crashes while
        // one is being created, not while adapters are enumerated.
        if std::env::var("WGPU_BACKEND").is_err() {
            desc.backends = CANDIDATES
                .iter()
                .fold(wgpu::Backends::empty(), |a, b| a | *b);
        }
        let backends = desc.backends;

        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(target)?;

        let adapter = pick_adapter(&instance, &surface, backends).ok_or(GpuError::NoAdapter)?;
        let info = adapter.get_info();

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("gumicord"),
                required_features: wgpu::Features::empty(),
                // Downlevel defaults still work on GLES 3.0 class hardware,
                // which mobile will need.
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;

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

        let rect_pipeline = make_pipeline(
            &device,
            &shader,
            &[&globals_layout],
            config.format,
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
            config.format,
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
            surface,
            device,
            queue,
            config,
            rect_pipeline,
            text_pipeline,
            globals_buf,
            globals_bind,
            atlas_layout,
            sampler,
            rect_buf,
            rect_capacity: INITIAL_RECTS,
            glyph_buf,
            glyph_capacity: INITIAL_GLYPHS,
            backend: info.backend,
            adapter_name: info.name,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
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
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// The bind group for the glyph atlas; recreate it when the atlas grows.
    pub fn atlas_bind_group(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &self.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Acquires the texture to draw into.
    ///
    /// A resize almost always yields `Outdated` first. Giving up there leaves
    /// the window blank until the next input under `ControlFlow::Wait`, so it
    /// reconfigures and tries once more.
    fn acquire(&mut self) -> Result<wgpu::SurfaceTexture, Presented> {
        for attempt in 0..2 {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f)
                | wgpu::CurrentSurfaceTexture::Suboptimal(f) => return Ok(f),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.surface.configure(&self.device, &self.config);
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
    /// texture.
    #[must_use]
    pub fn submit(
        &mut self,
        dl: &DrawList,
        atlas_binds: &[wgpu::BindGroup],
        clear: [f32; 4],
    ) -> Presented {
        let frame = match self.acquire() {
            Ok(f) => f,
            Err(why) => return why,
        };

        self.ensure_capacity(dl);

        self.queue.write_buffer(
            &self.globals_buf,
            0,
            bytemuck::cast_slice(&[
                self.config.width as f32,
                self.config.height as f32,
                0.0,
                0.0,
            ]),
        );
        if !dl.rects.is_empty() {
            self.queue
                .write_buffer(&self.rect_buf, 0, bytemuck::cast_slice(&dl.rects));
        }
        if !dl.glyphs.is_empty() {
            self.queue
                .write_buffer(&self.glyph_buf, 0, bytemuck::cast_slice(&dl.glyphs));
        }

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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

            let (w, h) = (self.config.width, self.config.height);
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
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Presented::Yes
    }

    fn ensure_capacity(&mut self, dl: &DrawList) {
        let rects = dl.rect_count() as usize;
        if rects > self.rect_capacity {
            self.rect_capacity = rects.next_power_of_two();
            self.rect_buf =
                make_instance_buffer(&self.device, "rects", self.rect_capacity * FLOATS_PER_RECT);
            tracing::debug!(capacity = self.rect_capacity, "矩形バッファを広げた");
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
        }
    }
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
    surface: &wgpu::Surface<'_>,
    backends: wgpu::Backends,
) -> Option<wgpu::Adapter> {
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    for wanted in CANDIDATES {
        if !backends.contains(*wanted) {
            continue;
        }
        let found = adapters.iter().find(|a| {
            wanted.contains(wgpu::Backends::from(a.get_info().backend))
                && a.is_surface_supported(surface)
        });
        if let Some(a) = found {
            return Some(a.clone());
        }
    }
    // Failing that, anything will do. An explicit `WGPU_BACKEND` lands here.
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(surface),
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
