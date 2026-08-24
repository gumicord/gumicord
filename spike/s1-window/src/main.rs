//! Spike S1: a window, a custom title bar, and measurements.
//!
//! The hypothesis: winit and wgpu can give a window without the OS title
//! bar, in less memory and starting faster than the official client.
//!
//! Covers drawing, the custom title bar, parity with the OS window
//! controls, not drawing while inactive, DPI changes, and the draw method.
//!
//! Throwaway code: the result is the numbers, not this.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{ResizeDirection, Window, WindowId};

/// When the process started, taken on the first line of main.
static T0: OnceLock<Instant> = OnceLock::new();

fn t0() -> Instant {
    *T0.get().expect("T0 は main で初期化される")
}

/// The title bar height, in logical pixels.
const TITLEBAR_H: f64 = 32.0;
/// How wide the resize edge is, in logical pixels.
const RESIZE_BORDER: f64 = 6.0;
/// The width of one window button, in logical pixels.
const BUTTON_W: f64 = 46.0;

/// Floats per instance: rect(4) + color(4) + radius(1) + pad(3).
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
    /// Nothing is drawn while inactive.
    focused: bool,
    cursor: PhysicalPosition<f64>,
    hover: HitZone,
    maximized: bool,
    scale: f64,
    /// Scratch space for the instances being drawn.
    instances: Vec<f32>,
    /// Quits after GUMICORD_BENCH_SECS, so a run needs nobody watching.
    bench_secs: Option<f32>,
    bench_start: Option<Instant>,
    should_exit: bool,
    /// Moves the content each frame, standing in for scrolling.
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

    // ---------------------------------------------------------------- GPU setup

    fn init_gpu(&mut self, window: Arc<Window>) {
        let t_gpu = Instant::now();

        // WGPU_BACKEND selects a backend, so they can be compared.
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();

        // What S1 found (2026-08-14):
        // on this machine (Intel HD Graphics 520) the Vulkan ICD segfaults while
        // the instance is being created and takes the process with it. wgpu tries
        // Vulkan first, so the default search cannot even start.
        // An unsupported backend does not merely make request_adapter return None:
        // a broken driver takes the process down. The product must narrow the
        // search per OS.
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
        // Colour handling is pinned, so every platform draws the same result.
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(config.format);
        // Fifo = VSync, which is what the responsiveness targets rest on.
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        // --- Pipeline ---
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

    // ---------------------------------------------------------------- Hit testing

    /// The regions that make the custom title bar behave like the OS one.
    fn hit_test(&self, x: f64, y: f64) -> HitZone {
        let Some(gpu) = &self.gpu else {
            return HitZone::Client;
        };
        let w = gpu.config.width as f64 / self.scale;
        let h = gpu.config.height as f64 / self.scale;
        let (lx, ly) = (x / self.scale, y / self.scale);

        // No resize edge while maximised, as the OS does.
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
            // From the right: close, maximise, minimise.
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

    // ---------------------------------------------------------------- Drawing

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

        // --- Background ---
        self.push_rect(0.0, 0.0, pw, ph, [0.10, 0.10, 0.14, 1.0], 0.0);

        // --- The custom title bar ---
        self.push_rect(0.0, 0.0, pw, tb, [0.07, 0.07, 0.10, 1.0], 0.0);

        // Hover on the window buttons.
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

        // The glyphs stand in as rectangles; text is S2.
        let g = [0.85, 0.85, 0.90, 1.0];
        let cy = tb * 0.5;
        // Minimise: a line.
        self.push_rect(pw - btn * 2.5 - 5.0 * s, cy, 10.0 * s, 1.0 * s, g, 0.0);
        // Maximise: a frame.
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
        // Close: a small square instead of a cross.
        self.push_rect(pw - btn * 0.5 - 5.0 * s, cy - 5.0 * s, 10.0 * s, 10.0 * s, g, 5.0 * s);

        // --- The client area: three panes of rounded rectangles ---
        // For the draw-method evaluation: as many instances as a real UI needs.
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
        // A message row: avatar, name, two lines of body, a reaction chip.
        // The content moves with the scroll, to measure how it keeps up.
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
                // Reaction chip.
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

        // Where the batcher gives out. GUMICORD_STRESS adds rounded rectangles
        // to see how much headroom a real UI leaves.
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

        // The input field.
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
        // The content moves every frame, to measure scrolling.
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

        // --- Measurements ---
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

        // No OS title bar. The dragging, resizing and maximising that costs is
        // put back in window_event.
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

            // Following a DPI change, and a move between displays.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                println!("[MEASURE] scale_factor_changed = {scale_factor}");
                self.scale = scale_factor;
                if let Some(w) = &self.window {
                    let s = w.inner_size();
                    self.resize(s.width, s.height);
                }
            }

            // Drawing stops while inactive.
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
                    // Drag to move.
                    HitZone::Titlebar => {
                        if let Err(e) = w.drag_window() {
                            eprintln!("[MEASURE] drag_window 失敗: {e}");
                        }
                    }
                    // Drag an edge to resize.
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

            // Double-click the title bar to toggle maximised.
            WindowEvent::DoubleTapGesture { .. } => {}

            WindowEvent::RedrawRequested => {
                self.render();
                if self.should_exit {
                    self.report();
                    event_loop.exit();
                    return;
                }
                // A benchmark keeps drawing while inactive, since nobody is watching.
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
    // Drawn continuously to measure frame time; the real client waits.
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    event_loop.run_app(&mut app).expect("イベントループが異常終了");
}
