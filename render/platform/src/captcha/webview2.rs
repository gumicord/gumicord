//! The WebView2 captcha host.
//!
//! A captcha needs to be solved by a real browser, and showing it as a modal is
//! the least jarring way to keep a login inside the app: WebView2 is embedded
//! as a child window that covers the main window, loads a page embedding
//! hCaptcha, and posts its token back over wry's IPC. The thread pumping
//! messages (the main thread) loops until a result arrives, exactly like a
//! native modal dialog.
//!
//! hCaptcha's enterprise mode limits which origin may verify a token: Discord
//! returns a site key (and on demand `rqdata`) that we present against
//! `data-host="discord.com"`. Whether that satisfies the challenge is verified
//! live; see ADR-0007.

use std::sync::mpsc::{self, Receiver, Sender};

use winit::window::Window;

use super::page::{Bridge, Outcome, html};
use super::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha};

/// A WebView2-backed [`CaptchaHost`].
#[derive(Debug, Default)]
pub struct WebView2Captcha;

impl CaptchaHost for WebView2Captcha {
    fn solve(
        &mut self,
        parent: &Window,
        challenge: CaptchaChallenge,
    ) -> Result<SolvedCaptcha, CaptchaError> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, MSG, TranslateMessage,
        };

        let (tx, rx): (Sender<Outcome>, Receiver<Outcome>) = mpsc::channel();
        let handler = {
            let tx = tx.clone();
            move |req: wry::http::Request<String>| {
                if let Some(outcome) = Outcome::from_body(req.body()) {
                    let _ = tx.send(outcome);
                }
            }
        };

        // Kept alive for the whole modal: dropping it closes the webview.
        let webview = wry::WebViewBuilder::new()
            .with_html(html(&challenge, Bridge::Wry))
            .with_ipc_handler(handler)
            .build_as_child(parent)
            .map_err(|e| CaptchaError::Open(e.to_string()))?;

        // A nested message pump. Dispatching the thread's messages is safe: the
        // window procedure winit installed answers the main window, and the
        // WebView2 child answers its own; both go through DispatchMessageW.
        loop {
            let mut msg = unsafe { std::mem::zeroed::<MSG>() };
            let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if r == 0 {
                // WM_QUIT: the app is shutting down; the captcha is moot.
                return Err(CaptchaError::Cancelled);
            }
            if r == -1 {
                return Err(CaptchaError::Open("the message loop failed".to_string()));
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
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
        }
    }
}
