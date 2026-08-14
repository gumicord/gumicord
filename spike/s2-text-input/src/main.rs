//! スパイク S2: テキスト描画 + 日本語 IME + アクセシビリティ
//!
//! 検証する仮説 (spec/08-spike-plan.md):
//!   自前レンダラのテキスト入力欄で、日本語をインライン変換で入力できる。
//!   かつ、OS のスクリーンリーダーが UI を読み上げられる。
//!
//! これが成立しなければ Gumicord はチャットクライアントとして成立しない。
//! ADR-0001 の見直し条件に直結する、最高リスクのスパイク。
//!
//! 検証項目:
//!   2-1 テキスト入力欄 (キャレット・選択範囲・折り返し)
//!   2-2 未確定文字列 (preedit) を下線付きで表示
//!   2-3 変換候補ウィンドウがキャレット直下に出る
//!   2-4 変換確定・取り消し・部分変換
//!   2-5 日本語 / 中国語 / 韓国語
//!   2-6 accesskit でスクリーンリーダーが読み上げる
//!   2-8 絵文字 (ZWJ) と結合文字の表示
//!
//! 画面を直接見られない環境でも判定できるよう、IME イベントはすべて標準出力に記録する。

mod text;

use std::sync::Arc;
use std::time::Instant;

use accesskit::{Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use accesskit_winit::{Adapter, Event as AccessKitEvent};
use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use text::GlyphAtlas;

const FLOATS_PER_RECT: usize = 12;
const FLOATS_PER_GLYPH: usize = 16;
const MAX_RECTS: usize = 8192;
const MAX_GLYPHS: usize = 32768;

const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 22.0;
const INPUT_H: f32 = 44.0;
const PAD: f32 = 16.0;

const ROOT_ID: NodeId = NodeId(0);
const INPUT_ID: NodeId = NodeId(1);
const LOG_ID: NodeId = NodeId(2);

/// PLT-002 / PLT-004 の描画確認用サンプル。
/// 整形 (shaping)・フォントフォールバック・双方向・結合文字・ZWJ を一度に踏む。
const SAMPLES: &[(&str, &str)] = &[
    ("日本語", "こんにちは、世界。ひらがな・カタカナ・漢字"),
    ("中文", "你好，世界。简体字と繁體字"),
    ("한국어", "안녕하세요, 세계"),
    ("Emoji ZWJ", "👨‍👩‍👧‍👦 👩🏽‍💻 🇯🇵 ❤️‍🔥"),
    ("العربية (RTL)", "مرحبا بالعالم"),
    ("देवनागरी", "नमस्ते दुनिया"),
    ("Latin", "The quick brown fox — ligatures: ffi fl"),
];

// ============================================================ エディタ

/// 最小限のテキスト入力状態。
/// preedit (未確定文字列) を確定テキストとは別に持つのが IME 対応の要点。
#[derive(Default)]
struct Editor {
    /// 確定済みテキスト
    text: String,
    /// text 内のバイトオフセット
    cursor: usize,
    /// IME の未確定文字列。cursor の位置に挿入されて表示される
    preedit: String,
    /// preedit 内のキャレット範囲 (バイト)。部分変換で使う
    preedit_cursor: Option<(usize, usize)>,
    /// 送信済みメッセージ
    sent: Vec<String>,
}

impl Editor {
    /// 画面に表示される文字列 = 確定テキストの cursor 位置に preedit を挿入したもの
    fn display(&self) -> String {
        let mut s = String::with_capacity(self.text.len() + self.preedit.len());
        s.push_str(&self.text[..self.cursor]);
        s.push_str(&self.preedit);
        s.push_str(&self.text[self.cursor..]);
        s
    }

    /// display() 内でのキャレットのバイト位置
    fn caret_byte(&self) -> usize {
        match self.preedit_cursor {
            Some((start, _)) => self.cursor + start,
            None => self.cursor + self.preedit.len(),
        }
    }

    /// display() 内での preedit の範囲
    fn preedit_range(&self) -> Option<(usize, usize)> {
        if self.preedit.is_empty() {
            None
        } else {
            Some((self.cursor, self.cursor + self.preedit.len()))
        }
    }

    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut i = self.cursor - 1;
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        self.text.replace_range(i..self.cursor, "");
        self.cursor = i;
    }

    fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let mut i = self.cursor + 1;
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        self.text.replace_range(self.cursor..i, "");
    }

    fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut i = self.cursor - 1;
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        self.cursor = i;
    }

    fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let mut i = self.cursor + 1;
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        self.cursor = i;
    }

    fn submit(&mut self) {
        if self.text.trim().is_empty() {
            return;
        }
        println!("[SEND]    {:?}", self.text);
        self.sent.push(std::mem::take(&mut self.text));
        self.cursor = 0;
    }
}

// ============================================================ GPU

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    rect_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    rect_buf: wgpu::Buffer,
    glyph_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    atlas_bg: wgpu::BindGroup,
    backend: wgpu::Backend,
}

// ============================================================ App

struct App {
    window: Option<Arc<Window>>,
    adapter: Option<Adapter>,
    gpu: Option<Gpu>,
    font_system: FontSystem,
    swash: SwashCache,
    atlas: Option<GlyphAtlas>,
    buffer: Option<Buffer>,
    editor: Editor,
    /// IME イベントのログ。検証の証跡として画面にも出す
    ime_log: Vec<String>,
    scale: f32,
    rects: Vec<f32>,
    glyphs: Vec<f32>,
    first_frame: bool,
    t0: Instant,
    /// set_ime_cursor_area を毎フレーム呼ばないための前回値
    last_ime_area: Option<(f64, f64)>,
    proxy: winit::event_loop::EventLoopProxy<AccessKitEvent>,
}

impl App {
    fn new(proxy: winit::event_loop::EventLoopProxy<AccessKitEvent>) -> Self {
        let t_font = Instant::now();
        let font_system = FontSystem::new();
        println!(
            "[MEASURE] font_system_init = {:.1}ms  (システムフォントの列挙)",
            t_font.elapsed().as_secs_f32() * 1000.0
        );
        Self {
            window: None,
            adapter: None,
            gpu: None,
            font_system,
            swash: SwashCache::new(),
            atlas: None,
            buffer: None,
            editor: Editor::default(),
            ime_log: Vec::new(),
            scale: 1.0,
            rects: Vec::with_capacity(MAX_RECTS * FLOATS_PER_RECT),
            glyphs: Vec::with_capacity(MAX_GLYPHS * FLOATS_PER_GLYPH),
            first_frame: true,
            t0: Instant::now(),
            last_ime_area: None,
            proxy,
        }
    }

    fn log_ime(&mut self, s: String) {
        println!("[IME]     {s}");
        self.ime_log.push(s);
        if self.ime_log.len() > 12 {
            self.ime_log.remove(0);
        }
    }

    // ---------------------------------------------------------------- GPU

    fn init_gpu(&mut self, window: Arc<Window>) {
        let t = Instant::now();
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        // S1 の発見: この検証機の Vulkan ICD はプロセスごと落ちる
        #[cfg(target_os = "windows")]
        if std::env::var("WGPU_BACKEND").is_err() {
            desc.backends = wgpu::Backends::GL | wgpu::Backends::DX12;
        }
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("GPU アダプタが見つからない");
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("s2"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .unwrap();

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("s2"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let rect_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rects"),
            size: (MAX_RECTS * FLOATS_PER_RECT * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let glyph_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glyphs"),
            size: (MAX_GLYPHS * FLOATS_PER_GLYPH * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let atlas = GlyphAtlas::new(&device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let atlas_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
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
        let atlas_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);

        let rect_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_bgl)],
            immediate_size: 0,
        });
        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect"),
            layout: Some(&rect_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_rect"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_RECT * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_rect"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let text_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_bgl), Some(&atlas_bgl)],
            immediate_size: 0,
        });
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text"),
            layout: Some(&text_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_text"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_GLYPH * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_text"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        println!(
            "[gpu]     backend={:?}  adapter={}  init={:.1}ms",
            info.backend,
            info.name,
            t.elapsed().as_secs_f32() * 1000.0
        );

        self.atlas = Some(atlas);
        self.gpu = Some(Gpu {
            surface,
            device,
            queue,
            config,
            rect_pipeline,
            text_pipeline,
            globals_buf,
            rect_buf,
            glyph_buf,
            globals_bg,
            atlas_bg,
            backend: info.backend,
        });
    }

    // ---------------------------------------------------------------- 描画

    fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4], r: f32) {
        if self.rects.len() / FLOATS_PER_RECT >= MAX_RECTS {
            return;
        }
        self.rects
            .extend_from_slice(&[x, y, w, h, c[0], c[1], c[2], c[3], r, 0.0, 0.0, 0.0]);
    }

    /// テキストを整形してグリフインスタンスを積む。戻り値は描画高さ。
    fn draw_text(&mut self, s: &str, x: f32, y: f32, max_w: f32, color: [f32; 4]) -> f32 {
        let scale = self.scale;
        let mut buf = Buffer::new(
            &mut self.font_system,
            Metrics::new(FONT_SIZE * scale, LINE_HEIGHT * scale),
        );
        buf.set_wrap(Wrap::WordOrGlyph);
        buf.set_size(Some(max_w), None);
        buf.set_text(
            s,
            &Attrs::new(),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.font_system, false);

        let mut height = 0.0f32;
        let mut out: Vec<f32> = Vec::new();
        {
            let Some(gpu) = &self.gpu else { return 0.0 };
            let Some(atlas) = &mut self.atlas else {
                return 0.0;
            };
            for run in buf.layout_runs() {
                height = height.max(run.line_top + run.line_height);
                for g in run.glyphs {
                    let p = g.physical((x, y + run.line_y), 1.0);
                    let Some(e) = atlas.get(&gpu.queue, &mut self.font_system, &mut self.swash, p.cache_key)
                    else {
                        continue;
                    };
                    if e.w == 0 {
                        continue;
                    }
                    let gx = (p.x + e.left) as f32;
                    let gy = (p.y - e.top) as f32;
                    out.extend_from_slice(&[
                        gx,
                        gy,
                        e.w as f32,
                        e.h as f32,
                        e.uv[0],
                        e.uv[1],
                        e.uv[2],
                        e.uv[3],
                        color[0],
                        color[1],
                        color[2],
                        color[3],
                        if e.is_color { 1.0 } else { 0.0 },
                        0.0,
                        0.0,
                        0.0,
                    ]);
                }
            }
        }
        if self.glyphs.len() / FLOATS_PER_GLYPH + out.len() / FLOATS_PER_GLYPH <= MAX_GLYPHS {
            self.glyphs.extend_from_slice(&out);
        }
        height
    }

    /// display 文字列中のバイト位置 → キャレットの x 座標 (物理px, 入力欄内の相対)
    fn caret_x(&mut self, display: &str, byte: usize, max_w: f32) -> f32 {
        let scale = self.scale;
        let mut buf = Buffer::new(
            &mut self.font_system,
            Metrics::new(FONT_SIZE * scale, LINE_HEIGHT * scale),
        );
        buf.set_wrap(Wrap::None);
        buf.set_size(Some(max_w), None);
        buf.set_text(
            display,
            &Attrs::new(),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.font_system, false);

        let mut end_x = 0.0f32;
        for run in buf.layout_runs() {
            end_x = run.line_w;
            for g in run.glyphs {
                if byte <= g.start {
                    return g.x;
                }
                if byte < g.end {
                    return g.x;
                }
            }
        }
        end_x
    }

    fn build_scene(&mut self) {
        self.rects.clear();
        self.glyphs.clear();
        let Some(gpu) = &self.gpu else { return };
        let (pw, ph) = (gpu.config.width as f32, gpu.config.height as f32);
        let s = self.scale;
        let pad = PAD * s;
        let ih = INPUT_H * s;

        self.push_rect(0.0, 0.0, pw, ph, [0.10, 0.10, 0.14, 1.0], 0.0);

        // ---- 左: 多言語サンプル (PLT-002 / PLT-004) ----
        let col_w = (pw * 0.5 - pad * 1.5).max(200.0);
        let mut y = pad;
        self.draw_text("── 整形の確認 ──", pad, y, col_w, [0.55, 0.60, 0.85, 1.0]);
        y += LINE_HEIGHT * s * 1.5;
        for (label, sample) in SAMPLES {
            self.draw_text(label, pad, y, col_w, [0.50, 0.52, 0.65, 1.0]);
            y += LINE_HEIGHT * s;
            let h = self.draw_text(sample, pad, y, col_w, [0.90, 0.90, 0.95, 1.0]);
            y += h.max(LINE_HEIGHT * s) + 8.0 * s;
        }

        // ---- 右: IME イベントログ (2-2 〜 2-4 の証跡) ----
        let rx = pw * 0.5 + pad * 0.5;
        let mut ry = pad;
        self.draw_text("── IME イベント ──", rx, ry, col_w, [0.55, 0.60, 0.85, 1.0]);
        ry += LINE_HEIGHT * s * 1.5;
        let logs = self.ime_log.clone();
        for line in &logs {
            self.draw_text(line, rx, ry, col_w, [0.75, 0.78, 0.85, 1.0]);
            ry += LINE_HEIGHT * s;
        }

        // ---- 送信済みメッセージ ----
        let sent = self.editor.sent.clone();
        let mut my = ph - ih - pad * 2.0;
        for m in sent.iter().rev().take(6) {
            my -= LINE_HEIGHT * s + 4.0 * s;
            self.draw_text(&format!("▸ {m}"), pad, my, pw - pad * 2.0, [0.85, 0.88, 0.95, 1.0]);
        }

        // ---- 入力欄 (2-1) ----
        let iy = ph - ih - pad;
        let iw = pw - pad * 2.0;
        self.push_rect(pad, iy, iw, ih, [0.15, 0.15, 0.20, 1.0], 10.0 * s);

        let display = self.editor.display();
        let tx = pad + 12.0 * s;
        let ty = iy + (ih - LINE_HEIGHT * s) * 0.5;
        let inner_w = iw - 24.0 * s;

        if display.is_empty() {
            self.draw_text(
                "メッセージを入力 (日本語入力で変換してください)",
                tx,
                ty,
                inner_w,
                [0.45, 0.45, 0.55, 1.0],
            );
        } else {
            self.draw_text(&display, tx, ty, inner_w, [0.95, 0.95, 1.0, 1.0]);
        }

        // ---- preedit の下線 (2-2) ----
        if let Some((ps, pe)) = self.editor.preedit_range() {
            let x0 = self.caret_x(&display, ps, inner_w);
            let x1 = self.caret_x(&display, pe, inner_w);
            let uy = ty + LINE_HEIGHT * s - 2.0 * s;
            // 未確定全体: 細い下線
            self.push_rect(
                tx + x0,
                uy,
                (x1 - x0).max(1.0),
                1.0 * s,
                [0.60, 0.65, 0.95, 1.0],
                0.0,
            );
            // 部分変換の対象範囲: 太い下線 (2-4)
            if let Some((cs, ce)) = self.editor.preedit_cursor {
                if ce > cs {
                    let a = self.caret_x(&display, self.editor.cursor + cs, inner_w);
                    let b = self.caret_x(&display, self.editor.cursor + ce, inner_w);
                    self.push_rect(
                        tx + a,
                        uy - 1.0 * s,
                        (b - a).max(1.0),
                        3.0 * s,
                        [0.75, 0.80, 1.0, 1.0],
                        0.0,
                    );
                }
            }
        }

        // ---- キャレット ----
        let cb = self.editor.caret_byte();
        let cx = self.caret_x(&display, cb, inner_w);
        let blink = (self.t0.elapsed().as_secs_f32() * 1.5).sin() > -0.3;
        if blink {
            self.push_rect(
                tx + cx,
                ty + 2.0 * s,
                1.5 * s,
                LINE_HEIGHT * s - 4.0 * s,
                [0.90, 0.92, 1.0, 1.0],
                0.0,
            );
        }

        // ---- 2-3: 変換候補ウィンドウの位置を IME に伝える ----
        //
        // ■ 検証中の仮説
        //   当初は毎フレーム呼んでいたが、変換候補ウィンドウが入力欄に出なかった。
        //   (a) 60Hz で呼びすぎて IME が追従できない
        //   (b) Windows 10 の Microsoft IME は TSF ベースで、winit が使う IMM32 の
        //       ImmSetCandidateWindow を候補位置に反映しない
        //   → まず (a) を潰すため、位置が実際に変わったときだけ呼ぶ。
        let px = (tx + cx) as f64;
        let py = iy as f64;
        let changed = match self.last_ime_area {
            Some((lx, ly)) => (lx - px).abs() > 1.0 || (ly - py).abs() > 1.0,
            None => true,
        };
        if changed {
            self.last_ime_area = Some((px, py));
            if let Some(w) = &self.window {
                w.set_ime_cursor_area(
                    winit::dpi::PhysicalPosition::new(px, py),
                    winit::dpi::PhysicalSize::new(2.0 * s as f64, ih as f64),
                );
            }
            println!("[IME-POS] set_ime_cursor_area(x={px:.0}, y={py:.0}, h={ih:.0})");
        }
    }

    fn render(&mut self) {
        self.build_scene();
        let rect_n = (self.rects.len() / FLOATS_PER_RECT) as u32;
        let glyph_n = (self.glyphs.len() / FLOATS_PER_GLYPH) as u32;

        let Some(gpu) = &self.gpu else { return };
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return;
            }
            _ => return,
        };

        gpu.queue.write_buffer(
            &gpu.globals_buf,
            0,
            bytemuck::cast_slice(&[gpu.config.width as f32, gpu.config.height as f32, 0.0, 0.0]),
        );
        if rect_n > 0 {
            gpu.queue
                .write_buffer(&gpu.rect_buf, 0, bytemuck::cast_slice(&self.rects));
        }
        if glyph_n > 0 {
            gpu.queue
                .write_buffer(&gpu.glyph_buf, 0, bytemuck::cast_slice(&self.glyphs));
        }

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
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
            pass.set_bind_group(0, &gpu.globals_bg, &[]);
            pass.set_pipeline(&gpu.rect_pipeline);
            pass.set_vertex_buffer(0, gpu.rect_buf.slice(..));
            pass.draw(0..4, 0..rect_n);

            pass.set_pipeline(&gpu.text_pipeline);
            pass.set_bind_group(1, &gpu.atlas_bg, &[]);
            pass.set_vertex_buffer(0, gpu.glyph_buf.slice(..));
            pass.draw(0..4, 0..glyph_n);
        }
        gpu.queue.submit(Some(enc.finish()));
        gpu.queue.present(frame);

        if self.first_frame {
            self.first_frame = false;
            let uploaded = self.atlas.as_ref().map(|a| a.uploaded).unwrap_or(0);
            println!("[MEASURE] cold_start_to_first_frame = {:.1}ms", self.t0.elapsed().as_secs_f32() * 1000.0);
            println!("[MEASURE] backend = {:?}", gpu.backend);
            println!("[MEASURE] glyphs_in_atlas = {uploaded}");
            println!("[MEASURE] rects = {rect_n}, glyph_quads = {glyph_n}");
            println!();
            println!("  ■ 検証手順");
            println!("    1. 半角/全角キーで日本語入力に切り替える");
            println!("    2. 「にほんご」と打つ → 未確定文字列が下線付きで出るか");
            println!("    3. スペースで変換 → 変換候補ウィンドウがキャレット直下に出るか");
            println!("    4. スペース連打で候補を送る → 表示が追従するか");
            println!("    5. ←→ で文節を移動 → 部分変換の太い下線が動くか");
            println!("    6. Enter で確定 → Commit イベントが出るか");
            println!("    7. Esc で変換取り消し");
            println!("    8. もう一度 Enter で送信");
            println!("    9. ナレーター (Win+Ctrl+Enter) で入力欄が読み上げられるか");
            println!("   10. Ctrl+Q で終了");
            println!();
        }
    }

    // ---------------------------------------------------------------- a11y

    fn a11y_tree(&self) -> TreeUpdate {
        let mut root = Node::new(Role::Window);
        root.set_label("Gumicord スパイク S2");
        root.set_children(vec![INPUT_ID, LOG_ID]);

        // PLT-003: 入力欄をテキスト入力として公開する
        let mut input = Node::new(Role::TextInput);
        input.set_label("メッセージ入力");
        input.set_value(self.editor.display());
        root.set_children(vec![INPUT_ID, LOG_ID]);

        let mut log = Node::new(Role::Label);
        log.set_label("IME イベントログ");
        log.set_value(self.ime_log.join(" / "));

        TreeUpdate {
            nodes: vec![(ROOT_ID, root), (INPUT_ID, input), (LOG_ID, log)],
            tree: Some(Tree::new(ROOT_ID)),
            tree_id: TreeId::ROOT,
            focus: INPUT_ID,
        }
    }

    fn push_a11y(&mut self) {
        let update = self.a11y_tree();
        if let Some(a) = &mut self.adapter {
            a.update_if_active(|| update);
        }
    }
}

impl ApplicationHandler<AccessKitEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // accesskit のアダプタは「ウィンドウが初めて表示される前」に作る必要がある。
        // したがって不可視で作り、アダプタ生成後に可視化する。
        let attrs = Window::default_attributes()
            .with_title("Gumicord スパイク S2 — IME とアクセシビリティ")
            .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 560.0))
            .with_visible(false);
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        self.scale = window.scale_factor() as f32;

        let adapter = Adapter::with_event_loop_proxy(event_loop, &window, self.proxy.clone());
        self.adapter = Some(adapter);

        // PLT-001: IME を有効にする。これを呼ばないと Ime イベントが一切来ない
        window.set_ime_allowed(true);
        println!("[window]  set_ime_allowed(true)  scale_factor={}", self.scale);

        self.init_gpu(window.clone());
        window.set_visible(true);
        window.request_redraw();
        self.window = Some(window);
        self.push_a11y();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AccessKitEvent) {
        use accesskit_winit::WindowEvent as AkEvent;
        match event.window_event {
            AkEvent::InitialTreeRequested => {
                println!("[a11y]    InitialTreeRequested — スクリーンリーダーが接続しました");
                let update = self.a11y_tree();
                if let Some(a) = &mut self.adapter {
                    a.update_if_active(|| update);
                }
            }
            AkEvent::ActionRequested(req) => {
                println!("[a11y]    ActionRequested: {:?}", req.action);
            }
            AkEvent::AccessibilityDeactivated => {
                println!("[a11y]    AccessibilityDeactivated");
            }
        }
        let _ = event_loop;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let (Some(a), Some(w)) = (&mut self.adapter, &self.window) {
            a.process_event(w, &event);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    if size.width > 0 && size.height > 0 {
                        gpu.config.width = size.width;
                        gpu.config.height = size.height;
                        gpu.surface.configure(&gpu.device, &gpu.config);
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                println!("[MEASURE] scale_factor_changed = {scale_factor}");
                self.scale = scale_factor as f32;
            }

            // ========================================================
            // PLT-001: IME のインライン変換。S2 の中心。
            // ========================================================
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Enabled => {
                        self.log_ime("Enabled".into());
                    }
                    Ime::Preedit(text, cursor) => {
                        // 未確定文字列。変換中はここが毎回更新される
                        self.log_ime(format!("Preedit {text:?} cursor={cursor:?}"));
                        self.editor.preedit = text;
                        self.editor.preedit_cursor = cursor;
                    }
                    Ime::Commit(text) => {
                        // 変換確定
                        self.log_ime(format!("Commit {text:?}"));
                        self.editor.preedit.clear();
                        self.editor.preedit_cursor = None;
                        self.editor.insert(&text);
                    }
                    Ime::Disabled => {
                        self.log_ime("Disabled".into());
                        self.editor.preedit.clear();
                        self.editor.preedit_cursor = None;
                    }
                }
                self.push_a11y();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                // 変換中のキー入力は IME が握っている。横取りしない
                let composing = !self.editor.preedit.is_empty();

                match &event.logical_key {
                    Key::Named(NamedKey::Backspace) if !composing => self.editor.backspace(),
                    Key::Named(NamedKey::Delete) if !composing => self.editor.delete(),
                    Key::Named(NamedKey::ArrowLeft) if !composing => self.editor.move_left(),
                    Key::Named(NamedKey::ArrowRight) if !composing => self.editor.move_right(),
                    Key::Named(NamedKey::Home) if !composing => self.editor.cursor = 0,
                    Key::Named(NamedKey::End) if !composing => {
                        self.editor.cursor = self.editor.text.len()
                    }
                    Key::Named(NamedKey::Enter) if !composing => self.editor.submit(),
                    Key::Character(c) if !composing => {
                        // Ctrl+Q で終了
                        if c == "q" || c == "Q" {
                            // 修飾キーの判定は省略。Ctrl 押下中のみ text が None になる
                            if event.text.is_none() {
                                self.report();
                                event_loop.exit();
                                return;
                            }
                        }
                        if let Some(t) = &event.text {
                            self.editor.insert(t);
                        }
                    }
                    Key::Named(NamedKey::Space) if !composing => {
                        if let Some(t) = &event.text {
                            self.editor.insert(t);
                        }
                    }
                    _ => {}
                }
                self.push_a11y();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => {
                self.render();
                // キャレット点滅のため継続描画する。実装では点滅タイマーのみ再描画する
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            _ => {}
        }
    }
}

impl App {
    fn report(&self) {
        println!();
        println!("======== S2 測定結果 ========");
        println!("[MEASURE] atlas_glyphs   = {}", self.atlas.as_ref().map(|a| a.uploaded).unwrap_or(0));
        println!("[MEASURE] ime_events     = {}", self.ime_log.len());
        println!("[MEASURE] sent_messages  = {}", self.editor.sent.len());
        println!("=============================");
    }
}

fn main() {
    let event_loop = EventLoop::<AccessKitEvent>::with_user_event().build().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);
    event_loop.run_app(&mut app).unwrap();
}
