//! The `wry` captcha host: WKWebView on macOS/iOS, WebKitGTK on Linux.
//!
//! Same shape as the Windows host: show the shared page as a modal, wait
//! on a channel, and pump the platform queue in between. Only the pumping
//! differs: `NSRunLoop` slices on Apple, `gtk` iterations on Linux. iOS
//! has no child webviews, so the challenge takes the whole screen there;
//! elsewhere it covers the parent window like any modal dialog.

use std::sync::mpsc::{self, Receiver, Sender};

use winit::window::Window;

use super::page::{Bridge, Outcome, html};
use super::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha};

/// A `wry`-backed [`CaptchaHost`].
#[derive(Debug, Default)]
pub struct WryCaptcha;

impl CaptchaHost for WryCaptcha {
    fn solve(
        &mut self,
        parent: &Window,
        challenge: CaptchaChallenge,
    ) -> Result<SolvedCaptcha, CaptchaError> {
        let (tx, rx): (Sender<Outcome>, Receiver<Outcome>) = mpsc::channel();
        let handler = {
            let tx = tx.clone();
            move |req: wry::http::Request<String>| {
                if let Some(outcome) = Outcome::from_body(req.body()) {
                    let _ = tx.send(outcome);
                }
            }
        };
        let builder = wry::WebViewBuilder::new()
            .with_html(html(&challenge, Bridge::Wry))
            .with_ipc_handler(handler);
        // iOS has no child webviews; the challenge takes the screen, with
        // its own cancel button for the way back.
        #[cfg(target_os = "ios")]
        let webview = builder
            .build(parent)
            .map_err(|e| CaptchaError::Open(e.to_string()))?;
        // Linux without X11 (Wayland) cannot hang a child anywhere; decide
        // the seat before building.
        #[cfg(target_os = "linux")]
        gtk::init().map_err(|e| CaptchaError::Open(e.to_string()))?;
        #[cfg(target_os = "linux")]
        let child = {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};

            parent
                .window_handle()
                .map(|h| {
                    matches!(
                        h.handle().as_ref(),
                        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_)
                    )
                })
                .unwrap_or(false)
        };
        // Wayland: a top-level window of our own. It cannot hang off the
        // parent, but the challenge stays in-app; the compositor places it.
        // Kept until the modal below ends.
        #[cfg(target_os = "linux")]
        let wayland: Option<gtk::Window> = if child {
            None
        } else {
            use gtk::prelude::GtkWindowExt;

            let scale = parent.scale_factor();
            let size = parent.inner_size().to_logical::<f64>(scale);
            let dialog = gtk::Window::new(gtk::WindowType::Toplevel);
            dialog.set_title("セキュリティ確認");
            dialog.set_default_size(size.width as i32, size.height as i32);
            Some(dialog)
        };
        // A child covering the parent window, like a modal dialog.
        #[cfg(not(target_os = "ios"))]
        let webview = {
            // The Wayland window answers first and closes itself: the
            // shared tail below only serves the child seat.
            #[cfg(target_os = "linux")]
            if let Some(dialog) = &wayland {
                use gtk::prelude::{GtkWindowExt, WidgetExt};
                use wry::WebViewBuilderExtUnix;

                let webview = builder
                    .build_gtk(dialog)
                    .map_err(|e| CaptchaError::Open(e.to_string()))?;
                dialog.show_all();
                let answer = pump(&rx, &webview);
                dialog.close();
                return answer;
            }
            let scale = parent.scale_factor();
            let size = parent.inner_size().to_logical::<f64>(scale);
            let bounds = wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(size.width, size.height).into(),
            };
            builder
                .with_bounds(bounds)
                .build_as_child(parent)
                .map_err(|e| CaptchaError::Open(e.to_string()))?
        };
        // Kept alive for the whole modal: dropping it closes the webview.
        pump(&rx, &webview)
    }
}

/// Wait for the page, pumping the Apple runloop in slices. Nesting is
/// ordinary here: the outer loop simply resumes when this returns.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn pump(rx: &Receiver<Outcome>, webview: &wry::WebView) -> Result<SolvedCaptcha, CaptchaError> {
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};

    let runloop = NSRunLoop::currentRunLoop();
    // The mode is an extern static outside Rust's control.
    let mode = unsafe { NSDefaultRunLoopMode };
    loop {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
        let _ = runloop.runMode_beforeDate(mode, &until);
        match rx.try_recv() {
            Ok(Outcome::Solved(token)) => return Ok(SolvedCaptcha { solution: token }),
            Ok(Outcome::Cancel) => return Err(CaptchaError::Cancelled),
            Ok(Outcome::Expired) => {
                let _ = webview.evaluate_script("hcaptcha.reset(0);");
            }
            Ok(Outcome::Failed) => {
                return Err(CaptchaError::Open(
                    "the challenge could not be completed".to_string(),
                ));
            }
            Err(_) => continue,
        }
    }
}

/// Wait for the page, driving GTK alongside.
#[cfg(target_os = "linux")]
fn pump(rx: &Receiver<Outcome>, webview: &wry::WebView) -> Result<SolvedCaptcha, CaptchaError> {
    loop {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
        match rx.try_recv() {
            Ok(Outcome::Solved(token)) => return Ok(SolvedCaptcha { solution: token }),
            Ok(Outcome::Cancel) => return Err(CaptchaError::Cancelled),
            Ok(Outcome::Expired) => {
                let _ = webview.evaluate_script("hcaptcha.reset(0);");
            }
            Ok(Outcome::Failed) => {
                return Err(CaptchaError::Open(
                    "the challenge could not be completed".to_string(),
                ));
            }
            Err(_) => continue,
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
