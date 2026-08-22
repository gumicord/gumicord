//! ウィンドウとイベントループ。
//!
//! # 独自タイトルバー (`PLT-020`, `PLT-021`)
//!
//! OS 標準のタイトルバーを使わない。テーマが全プラットフォームで同じ見た目に
//! なる範囲を、ウィンドウの縁まで広げるためである。
//!
//! 代わりに、標準タイトルバーが提供していた操作を自前で補う必要がある。
//!
//! | 失うもの | 補い方 |
//! |---|---|
//! | ドラッグでの移動 | `chrome.titlebar` の当たりで `drag_window` |
//! | 端のドラッグでのリサイズ | 縁 6px の当たりで `drag_resize_window` |
//! | 最小化・最大化・閉じる | `chrome.titlebar.control` の `key` で分岐 |
//! | リサイズカーソル | 当たりに応じて `set_cursor` |
//!
//! ⚠️ `PLT-022` (スナップレイアウト) はまだ調べていない。Windows で最大化
//! ボタンにポインタを置いたときに出る配置候補は、標準タイトルバーの機能である。
//!
//! ## サーフェスの大きさがずれると、上端と右端が切れる
//!
//! **タイトルバーが消えて描画範囲が狭く見えたら、まずこれを疑う。**
//!
//! GL バックエンドの原点は左下である。サーフェスをウィンドウより大きく
//! 構成したまま描くと、UI の上端は見えている領域より**上**へ写り、右端は
//! はみ出す。`chrome.titlebar` は上端にあるので真っ先に消える。
//!
//! リサイズの通知を 1 回でも取りこぼすとこの状態になるので、
//! [`Host::redraw`] は**描く直前にウィンドウの実寸からサーフェスを合わせる**。
//! `GUMICORD_LOG=debug` にすると「サーフェスを作り直す from=… to=…」が出るので、
//! ずれていればそこで分かる。
//!
//! ⚠️ 別件として、**Windows は枠なしウィンドウを最大化するとクライアント領域を
//! 画面の外へ 8px ほどはみ出させる**。正しい直し方は `WM_NCCALCSIZE` を扱う
//! ことで、Windows 固有の層として P7 に積んである。
//!
//! # 再描画は待って行う
//!
//! S1 は計測のために `ControlFlow::Poll` で回していたが、常時描画は
//! `NFR-005` (非アクティブ時に描画を止める) と両立しない。ここでは `Wait` に
//! し、状態が変わったときだけ再描画を要求する ([`spec/06-renderer.md`] 9.2)。

use std::sync::Arc;

use gumicord_render::{Hit, Presented, Renderer, ScrollGrab, Size};
use gumicord_uitree::{Key, NodeId, UiNode};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

/// ウィンドウの縁で、リサイズの掴みしろとする幅 (論理 px)
const RESIZE_BORDER: f32 = 6.0;
/// ホイール 1 段の移動量 (論理 px)。行数で来る環境のための換算
const LINE_SCROLL: f32 = 48.0;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("イベントループを作れない: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    #[error("ウィンドウを作れない: {0}")]
    Window(#[from] winit::error::OsError),
    #[error("GPU の準備に失敗した: {0}")]
    Gpu(#[from] gumicord_render::GpuError),
}

/// 1 フレームを組み立てるときに分かっている情報。
#[derive(Debug, Clone, Copy)]
pub struct FrameCx {
    /// 表示領域 (論理 px)。テーマの `when.maxWidth` の照合にも使う
    pub viewport: Size,
    pub scale: f32,
}

/// プラットフォーム層から見たアプリケーション。
///
/// **ここに OS の型は現れない。** `winit` を [`gumicord_app`] へ漏らさないため
/// である ([`spec/02-architecture.md`])。
pub trait Application {
    /// UITree を組み立てて返す。**スタイル解決まで済ませること。**
    ///
    /// パイプラインの [3] から [6] がこの中で起きる。
    fn build(&mut self, cx: &FrameCx) -> UiNode;

    /// ポインタの下のノードが変わった。再描画が要るなら `true`。
    ///
    /// `hits` は手前から順に並ぶ。ポインタが窓の外へ出たときは空。
    fn hover_changed(&mut self, hits: &[Hit]) -> bool;

    /// 左ボタンが押された。再描画が要るなら `true`。
    ///
    /// タイトルバーの操作はここへ来る前にプラットフォーム層が処理する。
    fn pressed(&mut self, hits: &[Hit]) -> bool;

    fn title(&self) -> String;
}

/// ウィンドウを開いてアプリを走らせる。**返ってくるのは終了時である。**
pub fn run(app: impl Application + 'static) -> Result<(), PlatformError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut host = Host {
        app: Box::new(app),
        window: None,
        renderer: None,
        cursor: (0.0, 0.0),
        zone: Zone::Client,
        maximized: false,
        scroll_grab: None,
        first_frame: true,
    };
    event_loop.run_app(&mut host)?;
    Ok(())
}

/// ポインタが何の上にあるか。**ウィンドウ操作にだけ関わる区分**である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Client,
    Titlebar,
    Control(&'static str),
    Resize(ResizeDirection),
}

struct Host {
    app: Box<dyn Application>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// ポインタの位置 (論理 px)
    cursor: (f32, f32),
    zone: Zone,
    maximized: bool,
    /// 掴んでいるスクロールバー。押している間だけ入る
    scroll_grab: Option<ScrollGrab>,
    first_frame: bool,
}

impl Host {
    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// ポインタの下にあるノードを手前から。
    fn hits(&self) -> Vec<Hit> {
        let Some(r) = &self.renderer else {
            return Vec::new();
        };
        r.hit_test(self.cursor.0, self.cursor.1).cloned().collect()
    }

    /// ウィンドウ操作に関わる領域かを判定する。
    ///
    /// 縁のリサイズだけは UITree の外側の話なので座標で見る。それ以外は
    /// **UITree の安定 ID で見る**。タイトルバーの位置や大きさをテーマが
    /// 変えても、ここが追随できるようにするためである。
    fn zone_at(&self, hits: &[Hit]) -> Zone {
        if !self.maximized
            && let Some(r) = &self.renderer
        {
            let v = r.viewport();
            let (x, y) = self.cursor;
            let left = x < RESIZE_BORDER;
            let right = x > v.w - RESIZE_BORDER;
            let top = y < RESIZE_BORDER;
            let bottom = y > v.h - RESIZE_BORDER;
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
                return Zone::Resize(d);
            }
        }

        for h in hits {
            if h.id == NodeId::ChromeTitlebarControl
                && let Some(Key::Slot(slot)) = h.key
            {
                return Zone::Control(slot);
            }
            if h.id == NodeId::ChromeTitlebar {
                return Zone::Titlebar;
            }
        }
        Zone::Client
    }

    fn apply_cursor(&self) {
        let Some(w) = &self.window else { return };
        let icon = match self.zone {
            Zone::Resize(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            Zone::Resize(ResizeDirection::East | ResizeDirection::West) => CursorIcon::EwResize,
            Zone::Resize(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => {
                CursorIcon::NwseResize
            }
            Zone::Resize(ResizeDirection::NorthEast | ResizeDirection::SouthWest) => {
                CursorIcon::NeswResize
            }
            _ => CursorIcon::Default,
        };
        w.set_cursor(icon);
    }

    fn redraw(&mut self) {
        // ⚠️ **描く直前に窓の実寸からサーフェスを合わせる。**
        //
        // `Resized` を取りこぼすとサーフェスの大きさが古いままになり、
        // `get_current_texture` が `Outdated` を返し続ける。再構成しても
        // 古い大きさで作り直すだけなので、窓が空白のまま回り続ける。
        // 実際に「大きさを変えると真っ白のまま戻らない」が起きた。
        //
        // `inner_size()` の呼び出しは 1 フレームに 1 回で、大きさが同じなら
        // `resize` は何もしない。
        let size = self.window.as_ref().map(|w| w.inner_size());
        if let (Some(size), Some(r)) = (size, &mut self.renderer) {
            r.resize(size.width, size.height);
        }

        let (stats, backend) = {
            let Some(r) = &mut self.renderer else { return };
            let cx = FrameCx {
                viewport: r.viewport(),
                scale: r.scale(),
            };
            tracing::trace!(w = cx.viewport.w, h = cx.viewport.h, "描画");
            let tree = self.app.build(&cx);
            (r.render(&tree), r.backend())
        };

        // ⚠️ **表示できなかったら、もう一度描き直しを要求する。**
        // `ControlFlow::Wait` で回している以上、ここで諦めると次の入力が
        // 来るまで窓が空白のままになる。リサイズ直後は実際にそうなった。
        //
        // 隠れているだけ (`Skipped`) のときは要求しない。要求すると
        // 最小化している間じゅう回り続ける (`NFR-005`)。
        if stats.presented == Presented::Failed {
            self.request_redraw();
            return;
        }

        // ⚠️ **ホバーは配置が変われば変わる。ポインタが動いたときだけでは足りない。**
        //
        // スクロールすると行がカーソルの下を通り過ぎるのに、当たり判定は
        // 前のフレームの配置に対して答える。描き終えたここで見直さないと、
        // ハイライトが通り過ぎた行に貼り付いたままになる。
        //
        // 変わったときだけ再描画を要求するので、次のフレームで落ち着く。
        if self.app.hover_changed(&self.hits()) {
            self.request_redraw();
        }

        if self.first_frame && stats.presented == Presented::Yes {
            self.first_frame = false;
            tracing::info!(
                ?backend,
                nodes = stats.nodes,
                rects = stats.rects,
                glyphs = stats.glyphs,
                draw_calls = stats.draw_calls,
                "最初のフレームを描いた"
            );
        }
    }
}

impl ApplicationHandler for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        // PLT-020: OS 標準のタイトルバーを使わない
        let attrs = Window::default_attributes()
            .with_title(self.app.title())
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0))
            .with_decorations(false);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!(%e, "ウィンドウを作れなかった");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        match Renderer::new(window.clone().into(), size.width, size.height, scale) {
            Ok(r) => self.renderer = Some(r),
            Err(e) => {
                tracing::error!(%e, "GPU を初期化できなかった");
                event_loop.exit();
                return;
            }
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                tracing::debug!(w = size.width, h = size.height, "リサイズ");
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                self.request_redraw();
            }

            // PLT-009: DPI 変更・ディスプレイ間の移動への追従
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(r) = &mut self.renderer {
                    r.set_scale(scale_factor as f32);
                }
                self.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.renderer.as_ref().map_or(1.0, Renderer::scale) as f64;
                self.cursor = ((position.x / scale) as f32, (position.y / scale) as f32);

                // スクロールバーを引いている間は、それ以外を見ない。
                // 摘みから指がはみ出しても掴んだままにする (OS の作法)
                if let (Some(grab), Some(r)) = (self.scroll_grab, &mut self.renderer) {
                    if r.drag_scrollbar(&grab, self.cursor.1) {
                        self.request_redraw();
                    }
                    return;
                }

                let hits = self.hits();
                let zone = self.zone_at(&hits);
                if zone != self.zone {
                    self.zone = zone;
                    self.apply_cursor();
                }
                if self.app.hover_changed(&hits) {
                    self.request_redraw();
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if self.app.hover_changed(&[]) {
                    self.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * LINE_SCROLL,
                    MouseScrollDelta::PixelDelta(p) => {
                        let scale = self.renderer.as_ref().map_or(1.0, Renderer::scale) as f64;
                        -(p.y / scale) as f32
                    }
                };
                let hits = self.hits();
                let Some(r) = &mut self.renderer else { return };
                // ポインタの下にある、もっとも手前のスクロール領域を動かす
                let target = hits
                    .iter()
                    .find(|h| gumicord_render::intrinsic(h.id).scroll)
                    .map(|h| h.id);
                if let Some(id) = target
                    && r.scroll_by(id, dy)
                {
                    self.request_redraw();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(w) = self.window.clone() else { return };
                match self.zone {
                    // PLT-021: ドラッグでの移動
                    Zone::Titlebar => {
                        if let Err(e) = w.drag_window() {
                            tracing::warn!(%e, "ウィンドウをドラッグできなかった");
                        }
                    }
                    // PLT-021: 端のドラッグでのリサイズ
                    Zone::Resize(dir) => {
                        if let Err(e) = w.drag_resize_window(dir) {
                            tracing::warn!(%e, "ウィンドウをリサイズできなかった");
                        }
                    }
                    Zone::Control("minimize") => w.set_minimized(true),
                    Zone::Control("maximize") => {
                        self.maximized = !self.maximized;
                        w.set_maximized(self.maximized);
                    }
                    Zone::Control("close") => event_loop.exit(),
                    Zone::Control(other) => {
                        tracing::debug!(slot = other, "知らないタイトルバーのボタン");
                    }
                    Zone::Client => {
                        // スクロールバーはアプリより先に見る。
                        // 一覧の中身と重なっているので、どちらかしか反応できない
                        let grabbed = self
                            .renderer
                            .as_mut()
                            .and_then(|r| r.grab_scrollbar(self.cursor.0, self.cursor.1));
                        if let Some(g) = grabbed {
                            self.scroll_grab = Some(g);
                            self.request_redraw();
                            return;
                        }

                        let hits = self.hits();
                        if self.app.pressed(&hits) {
                            self.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.scroll_grab = None;
            }

            // ⚠️ **ここでフォーカスを見てはならない。** 非アクティブな窓でも、
            // 大きさが変わったり内容が変わったりすれば描き直す必要がある。
            // 見ないと、隣に並べた窓が空白のままになる。
            //
            // `NFR-005` (非アクティブ時に描画を止める) を満たしているのは
            // `ControlFlow::Wait` と「変化したときだけ再描画を要求する」ほうで
            // あって、ここではない。何も起きなければそもそも呼ばれない。
            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }
}
