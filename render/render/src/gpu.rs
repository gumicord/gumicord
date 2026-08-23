//! wgpu の初期化と描画の送出。
//!
//! # バックエンドの探索対象は OS ごとに明示的に絞る
//!
//! 「対応していないバックエンドは `request_adapter` が `None` を返す」という
//! 前提は**成り立たない**。S1 の検証機では Intel の Vulkan ICD がインスタンス
//! 生成中にセグメンテーション違反を起こし、プロセスごと落ちた。
//!
//! Windows で GL を先に試すのは S1 の実測による。同一シーンで常駐メモリが
//! DX12 の 285.7 MB に対し GL は 18.1 MB — **16 倍**の差がある
//! ([`spec/06-renderer.md`] 10 章)。
//!
//! ⚠️ 起動時プローブ (別プロセスでの初期化試行) はロードマップ R7 で、
//! まだない。壊れたドライバはいまはクライアント本体を道連れにする。

use crate::draw::{DrawList, FLOATS_PER_GLYPH, FLOATS_PER_RECT, RunKind};

/// GPU の準備に失敗した理由。
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

/// 1 フレームを表示できたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// 表示した
    Yes,
    /// 描く必要がなかった (最小化・隠れている)。**再試行しない**
    Skipped,
    /// 表示できなかった。**もう一度描き直しを要求する必要がある**
    Failed,
}

/// 最初に確保するインスタンス数。足りなくなったら倍にして作り直す
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
        // ⚠️ 探索対象を絞るのは**インスタンスを作る前**でなければならない。
        // 壊れたドライバはアダプタの列挙ではなくインスタンス生成で落ちる
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
                // downlevel_defaults にしておくと GLES 3.0 級でも通る。
                // モバイル (M1.2) で困らないための保険でもある
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;

        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(GpuError::Incompatible)?;

        // 色の扱いを固定する。EXT-020 の前提 (spec/06-renderer.md 4 章)
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

        // グリフは物理ピクセルでラスタライズしてあるので拡大縮小しない。
        // それでも Linear にするのは、位置の端数でにじませたほうが素直なため
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
                    // 角の半径と枠線の太さ
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
                    // 角の半径 (物理 px)。**丸いアバターのために要る**
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

    /// グリフアトラスを参照する束。アトラスを作り直したら呼び直す。
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

    /// 描き先のテクスチャを取る。
    ///
    /// ⚠️ **リサイズの直後は `Outdated` がほぼ必ず返る。** そこで諦めて
    /// 帰ると、`ControlFlow::Wait` では次の入力まで窓が空白のままになる
    /// (実際に「窓の大きさを変えると真っ黒になったまま戻らない」が起きた)。
    /// 再構成したその場でもう一度だけ試す。
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
                // 最小化・隠れている。**描き直しを要求してはならない。**
                // 要求すると隠れている間じゅう回り続ける (NFR-005)
                wgpu::CurrentSurfaceTexture::Occluded => return Err(Presented::Skipped),
                other => {
                    tracing::warn!(?other, "サーフェスを取得できなかった");
                    return Err(Presented::Skipped);
                }
            }
        }
        Err(Presented::Failed)
    }

    /// 描画コマンドを送り、表示する。**表示できたかを返す。**
    ///
    /// `clear` はどの色で塗りつぶすか。テーマの `app.window` の背景色を渡す。
    ///
    /// **偽を返したら、呼び出し側はもう一度描き直しを要求する必要がある。**
    /// `ControlFlow::Wait` で回している以上、ここで諦めると次の入力が来るまで
    /// 窓が空白のままになる。
    #[must_use]
    /// ⚠️ `atlas_binds` はページごとに 1 つ。**束ねは 1 枚のテクスチャしか
    /// 指せない**ので、ページが違えば描く束も分かれる
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
                    // 潰れた切り取り。描くものはない
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
                            // ⚠️ ページが無ければ描かない。**1 枚目で
                            // 代用すると、別の字が出る**
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

/// 探索するバックエンドを**優先順に**並べたもの。
///
/// [`spec/06-renderer.md`] 10.1 の表そのままである。
/// Windows で Vulkan を探索しないのは S1 の発見による。
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

/// [`CANDIDATES`] の順にアダプタを選ぶ。
///
/// `request_adapter` に任せるとどれが選ばれるかは wgpu の都合で決まる。
/// Windows で GL が DX12 より 16 倍軽い以上、そこは任せられない。
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
    // 絞り込みで取れなければ、何でもよいから探す。
    // WGPU_BACKEND で明示的に指定された場合もここに来る
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
                // ストレートアルファ。EXT-024 (半透明合成の保証)
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            // 頂点バッファを持たず、vertex_index から 4 頂点を作る
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        // 深度を使わない。描画順がそのまま重なり順 (EXT-024)
        depth_stencil: None,
        // MSAA を使わない。fwidth による解析的 AA で足り、DPI に自動追従する
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}
