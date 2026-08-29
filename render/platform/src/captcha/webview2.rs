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

use super::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha};

/// The challenge page and its bridge back to Rust. `data-host` appears on both
/// the widget and the script tag, matching Discord's enterprise setup. The page
/// talks to Rust through `window.ipc.postMessage("type:payload")`; the token is
/// URL-safe so a colon split is unambiguous.
fn html(challenge: &CaptchaChallenge) -> String {
    let rqdata = match &challenge.rqdata {
        Some(r) => format!("hcaptcha.setData({r:?});"),
        None => String::new(),
    };
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>セキュリティ確認</title>
<style>
  html, body {{ margin: 0; height: 100%; background: #313338; }}
  body {{ display: flex; flex-direction: column; align-items: center; justify-content: center;
         gap: 14px; font-family: sans-serif; color: #f2f3f5; }}
  .box {{ background: #fff; border-radius: 8px; padding: 4px; }}
  .h-captcha {{ min-width: 304px; min-height: 78px; }}
  #cancel {{ background: #4e5058; color: #f2f3f5; border: 0; border-radius: 6px;
             padding: 8px 20px; font-size: 14px; cursor: pointer; }}
  #hint {{ font-size: 12px; color: #b5bac1; }}
</style>
</head>
<body>
  <div class="box"><div class="h-captcha"
    data-sitekey="{0}"
    data-host="discord.com"
    data-theme="dark"></div></div>
  <button id="cancel">キャンセル</button>
  <div id="hint"></div>
  <script src="https://hcaptcha.com/1/api.js?render=explicit&onload=onLoad" data-host="discord.com"></script>
  <script>
    function onLoad() {{
      {1}
      hcaptcha.render(document.querySelector('.h-captcha'), {{
        theme: 'dark',
        callback: function (token) {{
          window.ipc.postMessage('solved:' + token);
        }},
        'expired-callback': function () {{
          window.ipc.postMessage('expired');
        }},
        'error-callback': function () {{
          document.getElementById('hint').textContent = 'エラーが発生しました。ウィンドウを閉じてやり直してください。';
          window.ipc.postMessage('failed');
        }}
      }});
    }}
    document.getElementById('cancel').addEventListener('click', function () {{
      window.ipc.postMessage('cancel');
    }});
  </script>
</body>
</html>"#,
        challenge.site_key, rqdata
    )
}

/// Parse what the page posted back over IPC (`type:payload`).
enum Outcome {
    Solved(String),
    Expired,
    Failed,
    Cancel,
}

impl Outcome {
    fn from_body(body: &str) -> Option<Outcome> {
        let (ty, payload) = body.split_once(':').unwrap_or((body, ""));
        match ty {
            "solved" if !payload.is_empty() => Some(Outcome::Solved(payload.to_string())),
            "solved" => None,
            "expired" => Some(Outcome::Expired),
            "failed" => Some(Outcome::Failed),
            "cancel" => Some(Outcome::Cancel),
            _ => None,
        }
    }
}

/// A WebView2-backed [`CaptchaHost`].
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
            .with_html(html(&challenge))
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
