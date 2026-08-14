//! スパイク S1: ウィンドウ + カスタムタイトルバー + 性能実測
//!
//! 検証する仮説 (spec/08-spike-plan.md):
//!   winit + wgpu で、OS 標準のタイトルバーを排した独自ウィンドウを作り、
//!   公式 Discord より小さいメモリと速い起動を達成できる。
//!
//! 検証項目: 1-1 描画 / 1-2 独自タイトルバー / 1-3 ウィンドウ操作の等価性
//!           1-4 非アクティブ時の描画停止 / 1-5 DPI 追従 / 1-6 描画方式の評価
//!
//! このコードは捨てる前提。成果物は測定値であってコードではない。

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{ResizeDirection, Window, WindowId};

/// プロセス起動時刻。main の最初の行で確定させる。
static T0: OnceLock<Instant> = OnceLock::new();

fn t0() -> Instant {
    *T0.get().expect("T0 は main で初期化される")
}

/// 独自タイトルバーの高さ (論理ピクセル)。PLT-020
const TITLEBAR_H: f64 = 32.0;
/// ウィンドウ端のリサイズ判定幅 (論理ピクセル)。PLT-021
const RESIZE_BORDER: f64 = 6.0;
/// ウィンドウ操作ボタン 1 個の幅 (論理ピクセル)
const BUTTON_W: f64 = 46.0;

/// 1 インスタンスあたりの f32 個数: rect(4) + color(4) + radius(1) + pad(3)
const FLOATS_PER_INSTANCE: usize = 12;
const MAX_INSTANCES: usize = 262_144;

#[derive(Clone, Copy, PartialEq, Debug)]
enum HitZone {
    Client,
    Titlebar,
    Minimize,
    Maximize,
    Close,
    Resize(ResizeDirection),
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    backend: wgpu::Backend,
    adapter_name: String,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    first_frame_done: bool,
    frame_times: Vec<f32>,
    last_frame: Option<Instant>,
    /// NFR-005: 非アクティブ時は描画しない
    focused: bool,
    cursor: PhysicalPosition<f64>,
    hover: HitZone,
    maximized: bool,
    scale: f64,
    /// 描画するインスタンスの一時バッファ
    instances: Vec<f32>,
    /// GUMICORD_BENCH_SECS 秒で自動終了する。無人で計測するため。
    bench_secs: Option<f32>,
    bench_start: Option<Instant>,
    should_exit: bool,
    /// スクロールを模擬してフレームごとに内容を動かす (NFR-003 の検証)
    scroll: f32,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            first_frame_done: false,
            frame_times: Vec::with_capacity(200_000),
            last_frame: None,
            focused: true,
            cursor: PhysicalPosition::new(0.0, 0.0),
            hover: HitZone::Client,
            maximized: false,
            scale: 1.0,
            instances: Vec::with_capacity(MAX_INSTANCES * FLOATS_PER_INSTANCE),
            bench_secs: std::env::var("GUMICORD_BENCH_SECS")
                .ok()
                .and_then(|s| s.parse().ok()),
            bench_start: None,
            should_exit: false,
            scroll: 0.0,
        }
    }

    // ---------------------------------------------------------------- GPU 初期化

    fn init_gpu(&mut self, window: Arc<Window>) {
        let t_gpu = Instant::now();

        // WGPU_BACKEND 環境変数でバックエンドを切り替えて比較できるようにする
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();

        // ■ S1 の発見 (2026-08-14)
        // この検証機 (Intel HD Graphics 520) では、Vulkan ICD がインスタンス生成中に
        // セグメンテーション違反を起こし、プロセスごと落ちる。
        // wgpu の既定バックエンド探索は Vulkan を先に触るため、既定のままだと起動できない。
        //
        // 「対応していないバックエンドは request_adapter が None を返してくれる」という
        // 前提は成り立たない。壊れたドライバはプロセスを道連れにする。
        // → 製品では探索対象を OS ごとに明示的に絞る必要がある。
        #[cfg(target_os = "windows")]
        if std::env::var("WGPU_BACKEND").is_err() {
            desc.backends = wgpu::Backends::DX12 | wgpu::Backends::GL;
        }

        let instance = wgpu::Instance::new(desc);

        let surface = instance
            .create_surface(window.clone())
            .expect("surface の作成に失敗");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("対応する GPU アダプタが見つからない");

        let info = adapter.get_info();

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gumicord-spike"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .expect("デバイスの取得に失敗");

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("surface が adapter に対応していない");
        // 色の扱いを固定する。EXT-020 (全 PF で同一の描画結果) の前提。
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(config.format);
        // Fifo = VSync。NFR-003 / NFR-004 の追従性はここに依存する。
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        // --- パイプライン ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rounded-rect"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (MAX_INSTANCES * FLOATS_PER_INSTANCE * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rounded-rect"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_INSTANCE * 4) as u64,
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
                            format: wgpu::VertexFormat::Float32,
                            offset: 32,
                            shader_location: 2,
                        },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        println!(
            "[gpu]     backend={:?}  adapter={}  ({:?})  init={:.1}ms",
            info.backend,
            info.name,
            info.device_type,
            t_gpu.elapsed().as_secs_f32() * 1000.0
        );

        self.gpu = Some(Gpu {
            surface,
            device,
            queue,
            config,
            pipeline,
            globals_buf,
            instance_buf,
            bind_group,
            backend: info.backend,
            adapter_name: info.name.clone(),
        });
    }

    // ---------------------------------------------------------------- ヒットテスト

    /// PLT-021: OS 標準タイトルバーと等価な操作を成立させるための領域判定。
    fn hit_test(&self, x: f64, y: f64) -> HitZone {
        let Some(gpu) = &self.gpu else {
            return HitZone::Client;
        };
        let w = gpu.config.width as f64 / self.scale;
        let h = gpu.config.height as f64 / self.scale;
        let (lx, ly) = (x / self.scale, y / self.scale);

        // 最大化中はリサイズ判定を行わない (OS 標準と同じ挙動)
        if !self.maximized {
            let left = lx < RESIZE_BORDER;
            let right = lx > w - RESIZE_BORDER;
            let top = ly < RESIZE_BORDER;
            let bottom = ly > h - RESIZE_BORDER;
            let dir = match (top, bottom, left, right) {
                (true, _, true, _) => Some(ResizeDirection::NorthWest),
                (true, _, _, true) => Some(ResizeDirection::NorthEast),
                (_, true, true, _) => Some(ResizeDirection::SouthWest),
                (_, true, _, true) => Some(ResizeDirection::SouthEast),
                (true, ..) => Some(ResizeDirection::North),
                (_, true, ..) => Some(ResizeDirection::South),
                (_, _, true, _) => Some(ResizeDirection::West),
                (_, _, _, true) => Some(ResizeDirection::East),
                _ => None,
            };
            if let Some(d) = dir {
                return HitZone::Resize(d);
            }
        }

        if ly < TITLEBAR_H {
            // 右端から 閉じる / 最大化 / 最小化 の順
            if lx > w - BUTTON_W {
                return HitZone::Close;
            }
            if lx > w - BUTTON_W * 2.0 {
                return HitZone::Maximize;
            }
            if lx > w - BUTTON_W * 3.0 {
                return HitZone::Minimize;
            }
            return HitZone::Titlebar;
        }
        HitZone::Client
    }

    fn apply_cursor_icon(&self) {
        use winit::window::CursorIcon;
        let Some(w) = &self.window else { return };
        let icon = match self.hover {
            HitZone::Resize(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            HitZone::Resize(ResizeDirection::East | ResizeDirection::West) => CursorIcon::EwResize,
            HitZone::Resize(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => {
                CursorIcon::NwseResize
            }
            HitZone::Resize(ResizeDirection::NorthEast | ResizeDirection::SouthWest) => {
                CursorIcon::NeswResize
            }
            _ => CursorIcon::Default,
        };
        w.set_cursor(icon);
    }

    // ---------------------------------------------------------------- 描画

    fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4], r: f32) {
        if self.instances.len() / FLOATS_PER_INSTANCE >= MAX_INSTANCES {
            return;
        }
        self.instances
            .extend_from_slice(&[x, y, w, h, c[0], c[1], c[2], c[3], r, 0.0, 0.0, 0.0]);
    }

    fn build_scene(&mut self) {
        self.instances.clear();
        let Some(gpu) = &self.gpu else { return };
        let (pw, ph) = (gpu.config.width as f32, gpu.config.height as f32);
        let s = self.scale as f32;
        let tb = TITLEBAR_H as f32 * s;
        let btn = BUTTON_W as f32 * s;

        // --- 背景 ---
        self.push_rect(0.0, 0.0, pw, ph, [0.10, 0.10, 0.14, 1.0], 0.0);

        // --- 独自タイトルバー (PLT-020) ---
        self.push_rect(0.0, 0.0, pw, tb, [0.07, 0.07, 0.10, 1.0], 0.0);

        // ウィンドウ操作ボタンのホバー表示
        let hover_btn = |z: HitZone, cur: HitZone| if z == cur { 1.0f32 } else { 0.0 };
        let h = self.hover;
        let close_a = hover_btn(HitZone::Close, h);
        let max_a = hover_btn(HitZone::Maximize, h);
        let min_a = hover_btn(HitZone::Minimize, h);
        if close_a > 0.0 {
            self.push_rect(pw - btn, 0.0, btn, tb, [0.77, 0.15, 0.20, 1.0], 0.0);
        }
        if max_a > 0.0 {
            self.push_rect(pw - btn * 2.0, 0.0, btn, tb, [1.0, 1.0, 1.0, 0.10], 0.0);
        }
        if min_a > 0.0 {
            self.push_rect(pw - btn * 3.0, 0.0, btn, tb, [1.0, 1.0, 1.0, 0.10], 0.0);
        }

        // ボタンのグリフを矩形で代用 (テキスト描画は S2 の範囲)
        let g = [0.85, 0.85, 0.90, 1.0];
        let cy = tb * 0.5;
        // 最小化: 横線
        self.push_rect(pw - btn * 2.5 - 5.0 * s, cy, 10.0 * s, 1.0 * s, g, 0.0);
        // 最大化: 枠
        self.push_rect(pw - btn * 1.5 - 5.0 * s, cy - 5.0 * s, 10.0 * s, 10.0 * s, g, 1.0 * s);
        self.push_rect(
            pw - btn * 1.5 - 4.0 * s,
            cy - 4.0 * s,
            8.0 * s,
            8.0 * s,
            if close_a > 0.0 || max_a > 0.0 {
                [0.15, 0.15, 0.20, 1.0]
            } else {
                [0.07, 0.07, 0.10, 1.0]
            },
            0.0,
        );
        // 閉じる: ×の代わりに小さい四角
        self.push_rect(pw - btn * 0.5 - 5.0 * s, cy - 5.0 * s, 10.0 * s, 10.0 * s, g, 5.0 * s);

        // --- クライアント領域: Discord 風の 3 ペインを角丸矩形で敷き詰める ---
        // 1-6 の評価用。実際の UI と同程度のインスタンス数を出して負荷を見る。
        let pad = 8.0 * s;
        let guild_w = 72.0 * s;
        let chan_w = 240.0 * s;
        let y0 = tb + pad;
        let body_h = ph - y0 - pad;

        self.push_rect(pad, y0, guild_w, body_h, [0.06, 0.06, 0.09, 1.0], 12.0 * s);
        for i in 0..12 {
            let y = y0 + pad + i as f32 * (48.0 + 8.0) * s;
            if y + 48.0 * s > y0 + body_h {
                break;
            }
            self.push_rect(
                pad + 12.0 * s,
                y,
                48.0 * s,
                48.0 * s,
                [0.20, 0.22, 0.35, 1.0],
                24.0 * s,
            );
        }

        let cx = pad * 2.0 + guild_w;
        self.push_rect(cx, y0, chan_w, body_h, [0.08, 0.08, 0.12, 1.0], 12.0 * s);
        for i in 0..20 {
            let y = y0 + pad + i as f32 * 34.0 * s;
            if y + 28.0 * s > y0 + body_h {
                break;
            }
            self.push_rect(
                cx + 8.0 * s,
                y,
                chan_w - 16.0 * s,
                28.0 * s,
                [1.0, 1.0, 1.0, 0.04],
                6.0 * s,
            );
        }

        let mx = cx + chan_w + pad;
        let mw = pw - mx - pad;
        self.push_rect(mx, y0, mw, body_h, [0.09, 0.09, 0.13, 1.0], 12.0 * s);
        // メッセージ行: アバター + 名前 + 本文 2 行 + リアクションチップ。
        // NFR-003 の検証のためスクロールで内容を動かす。
        let row_h = 72.0 * s;
        let off = self.scroll % row_h;
        let mut y = y0 + pad - off;
        let mut i = 0usize;
        while y < y0 + body_h {
            if y + row_h > y0 && y > y0 {
                self.push_rect(
                    mx + 12.0 * s,
                    y,
                    40.0 * s,
                    40.0 * s,
                    [0.25, 0.28, 0.40, 1.0],
                    20.0 * s,
                );
                let tx = mx + 64.0 * s;
                self.push_rect(
                    tx,
                    y + 2.0 * s,
                    120.0 * s,
                    12.0 * s,
                    [0.55, 0.60, 0.85, 1.0],
                    3.0 * s,
                );
                self.push_rect(
                    tx + 128.0 * s,
                    y + 3.0 * s,
                    64.0 * s,
                    10.0 * s,
                    [1.0, 1.0, 1.0, 0.20],
                    3.0 * s,
                );
                let wlen = 200.0 + ((i * 137) % 400) as f32;
                self.push_rect(
                    tx,
                    y + 22.0 * s,
                    (wlen * s).min(mw - 88.0 * s),
                    12.0 * s,
                    [1.0, 1.0, 1.0, 0.14],
                    3.0 * s,
                );
                self.push_rect(
                    tx,
                    y + 38.0 * s,
                    ((wlen * 0.6) * s).min(mw - 88.0 * s),
                    12.0 * s,
                    [1.0, 1.0, 1.0, 0.14],
                    3.0 * s,
                );
                // リアクションチップ
                for k in 0..(i % 4) {
                    self.push_rect(
                        tx + k as f32 * 48.0 * s,
                        y + 54.0 * s,
                        42.0 * s,
                        20.0 * s,
                        [1.0, 1.0, 1.0, 0.08],
                        10.0 * s,
                    );
                }
            }
            y += row_h;
            i += 1;
        }

        // 1-6: 自前バッチャの限界を測る。GUMICORD_STRESS で追加の角丸矩形を敷き詰める。
        // 実際の UI が必要とするインスタンス数に対してどれだけ余裕があるかを見る。
        if let Ok(n) = std::env::var("GUMICORD_STRESS") {
            let n: usize = n.parse().unwrap_or(0);
            let cols = 64usize;
            let cw = mw / cols as f32;
            let chh = 14.0 * s;
            for k in 0..n {
                let col = k % cols;
                let row = k / cols;
                let x = mx + col as f32 * cw;
                let yy = y0 + (row as f32 * chh + self.scroll * 0.5) % body_h;
                self.push_rect(
                    x + 1.0,
                    yy,
                    cw - 2.0,
                    chh - 2.0,
                    [
                        0.3 + (k % 7) as f32 * 0.1,
                        0.3 + (k % 5) as f32 * 0.1,
                        0.5,
                        0.35,
                    ],
                    3.0 * s,
                );
            }
        }

        // 入力欄
        let ih = 44.0 * s;
        self.push_rect(
            mx + 12.0 * s,
            y0 + body_h - ih - 8.0 * s,
            mw - 24.0 * s,
            ih,
            [0.14, 0.14, 0.19, 1.0],
            10.0 * s,
        );
    }

    fn render(&mut self) {
        // NFR-003: スクロール中の追従性を測るため毎フレーム内容を動かす
        self.scroll += 2.0;
        self.build_scene();

        let instance_count = (self.instances.len() / FLOATS_PER_INSTANCE) as u32;
        let Some(gpu) = &self.gpu else { return };

        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return;
            }
            other => {
                // Timeout / Occluded / Validation
                if !matches!(other, wgpu::CurrentSurfaceTexture::Occluded) {
                    eprintln!("[render] surface: {other:?}");
                }
                return;
            }
        };

        gpu.queue.write_buffer(
            &gpu.globals_buf,
            0,
            bytemuck::cast_slice(&[
                gpu.config.width as f32,
                gpu.config.height as f32,
                0.0,
                0.0,
            ]),
        );
        gpu.queue
            .write_buffer(&gpu.instance_buf, 0, bytemuck::cast_slice(&self.instances));

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &gpu.bind_group, &[]);
            pass.set_vertex_buffer(0, gpu.instance_buf.slice(..));
            pass.draw(0..4, 0..instance_count);
        }
        gpu.queue.submit(Some(encoder.finish()));
        gpu.queue.present(frame);

        // --- 計測 ---
        let now = Instant::now();
        if !self.first_frame_done {
            self.first_frame_done = true;
            println!(
                "[MEASURE] cold_start_to_first_frame = {:.1}ms",
                t0().elapsed().as_secs_f32() * 1000.0
            );
            println!("[MEASURE] backend = {:?}", gpu.backend);
            println!("[MEASURE] adapter = {}", gpu.adapter_name);
            println!("[MEASURE] instances_per_frame = {instance_count}");
            self.bench_start = Some(now);
            if let Some(sec) = self.bench_secs {
                println!("[bench]   {sec}秒後に自動終了して統計を出力します");
            }
            println!();
            println!("  操作: タイトルバーをドラッグして移動 / ダブルクリックで最大化");
            println!("        端をドラッグでリサイズ / 右上のボタンで最小化・最大化・閉じる");
            println!("        Esc または閉じるで終了し、フレーム統計を出力します");
            println!();
        } else if let Some(prev) = self.last_frame {
            self.frame_times.push((now - prev).as_secs_f32() * 1000.0);
        }
        self.last_frame = Some(now);

        if let (Some(sec), Some(start)) = (self.bench_secs, self.bench_start) {
            if now.duration_since(start).as_secs_f32() >= sec {
                self.should_exit = true;
            }
        }
    }

    fn report(&mut self) {
        println!();
        println!("======== S1 測定結果 ========");
        if self.frame_times.is_empty() {
            println!("[MEASURE] フレームサンプルなし");
            return;
        }
        self.frame_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = self.frame_times.len();
        let pct = |p: f32| self.frame_times[(((n - 1) as f32 * p) as usize).min(n - 1)];
        let mean: f32 = self.frame_times.iter().sum::<f32>() / n as f32;
        println!("[MEASURE] frames           = {n}");
        println!("[MEASURE] frame_time_mean  = {mean:.2}ms  ({:.0} fps)", 1000.0 / mean);
        println!("[MEASURE] frame_time_p50   = {:.2}ms", pct(0.50));
        println!("[MEASURE] frame_time_p95   = {:.2}ms", pct(0.95));
        println!("[MEASURE] frame_time_p99   = {:.2}ms", pct(0.99));
        println!("[MEASURE] frame_time_max   = {:.2}ms", self.frame_times[n - 1]);
        println!("=============================");
    }

    fn resize(&mut self, w: u32, h: u32) {
        if let Some(gpu) = &mut self.gpu {
            if w > 0 && h > 0 {
                gpu.config.width = w;
                gpu.config.height = h;
                gpu.surface.configure(&gpu.device, &gpu.config);
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let t_win = Instant::now();

        // PLT-020: OS 標準のタイトルバーを使わない。
        // 失われるドラッグ移動・リサイズ・最大化は window_event 側で補う (PLT-021)。
        let attrs = Window::default_attributes()
            .with_title("Gumicord スパイク S1")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0))
            .with_decorations(false);

        let window = Arc::new(event_loop.create_window(attrs).expect("ウィンドウ作成に失敗"));
        self.scale = window.scale_factor();
        println!(
            "[window]  作成 {:.1}ms  scale_factor={}",
            t_win.elapsed().as_secs_f32() * 1000.0,
            self.scale
        );

        self.init_gpu(window.clone());
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.report();
                event_loop.exit();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if event.state.is_pressed() && event.logical_key == Key::Named(NamedKey::Escape) {
                    self.report();
                    event_loop.exit();
                }
            }

            WindowEvent::Resized(size) => self.resize(size.width, size.height),

            // PLT-009: DPI 変更・ディスプレイ間移動への追従
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                println!("[MEASURE] scale_factor_changed = {scale_factor}");
                self.scale = scale_factor;
                if let Some(w) = &self.window {
                    let s = w.inner_size();
                    self.resize(s.width, s.height);
                }
            }

            // NFR-005: 非アクティブ時は描画を止める
            WindowEvent::Focused(f) => {
                self.focused = f;
                println!("[event]   focused = {f}  (描画{})", if f { "再開" } else { "停止" });
                if f {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                let z = self.hit_test(position.x, position.y);
                if z != self.hover {
                    self.hover = z;
                    self.apply_cursor_icon();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(w) = self.window.clone() else { return };
                match self.hover {
                    // PLT-021: ドラッグ移動
                    HitZone::Titlebar => {
                        if let Err(e) = w.drag_window() {
                            eprintln!("[MEASURE] drag_window 失敗: {e}");
                        }
                    }
                    // PLT-021: 端のドラッグでリサイズ
                    HitZone::Resize(dir) => {
                        if let Err(e) = w.drag_resize_window(dir) {
                            eprintln!("[MEASURE] drag_resize_window 失敗: {e}");
                        }
                    }
                    HitZone::Minimize => w.set_minimized(true),
                    HitZone::Maximize => {
                        self.maximized = !self.maximized;
                        w.set_maximized(self.maximized);
                    }
                    HitZone::Close => {
                        self.report();
                        event_loop.exit();
                    }
                    HitZone::Client => {}
                }
            }

            // PLT-021: タイトルバーのダブルクリックで最大化トグル
            WindowEvent::DoubleTapGesture { .. } => {}

            WindowEvent::RedrawRequested => {
                self.render();
                if self.should_exit {
                    self.report();
                    event_loop.exit();
                    return;
                }
                // ベンチ中は非アクティブでも描画を続ける (無人計測のため)
                if self.focused || self.bench_secs.is_some() {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}

fn main() {
    let _ = T0.set(Instant::now());

    let event_loop = EventLoop::new().expect("イベントループの作成に失敗");
    // フレーム時間を測るため連続描画する。実際のクライアントでは Wait にする。
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    event_loop.run_app(&mut app).expect("イベントループが異常終了");
}
