//! The challenge page every captcha host shows.
//!
//! One page for every OS: the widget setup (site key, enterprise data,
//! dark theme) never differs, only the way the page talks back to Rust
//! does. `wry` hosts listen on `window.ipc`, while the Android host
//! owns a small Kotlin object instead. The message keeps the
//! `type:payload` shape on both, so [`Outcome`] parses either.

use super::CaptchaChallenge;

/// How the page hands its answer back to Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bridge {
    /// `wry` IPC: `window.ipc.postMessage(message)`.
    /// Every host but Android posts through it.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    Wry,
    /// A Kotlin object on `window`: `window.NAME.post(message)`.
    /// Constructed on Android only; elsewhere the page is tested through it.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    JsInterface(&'static str),
}

/// The challenge page and its bridge back to Rust. `data-host` appears on
/// both the widget and the script tag, matching Discord's enterprise
/// setup. The token is URL-safe so a colon split is unambiguous.
pub fn html(challenge: &CaptchaChallenge, bridge: Bridge) -> String {
    let rqdata = match &challenge.rqdata {
        Some(r) => format!("hcaptcha.setData({r:?});"),
        None => String::new(),
    };
    let post = match bridge {
        Bridge::Wry => "window.ipc.postMessage(msg);".to_owned(),
        Bridge::JsInterface(name) => format!("window.{name}.post(msg);"),
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
    function post(msg) {{
      {2}
    }}
    function onLoad() {{
      {1}
      hcaptcha.render(document.querySelector('.h-captcha'), {{
        theme: 'dark',
        callback: function (token) {{
          post('solved:' + token);
        }},
        'expired-callback': function () {{
          post('expired');
        }},
        'error-callback': function () {{
          document.getElementById('hint').textContent = 'エラーが発生しました。ウィンドウを閉じてやり直してください。';
          post('failed');
        }}
      }});
    }}
    document.getElementById('cancel').addEventListener('click', function () {{
      post('cancel');
    }});
  </script>
</body>
</html>"#,
        challenge.site_key, rqdata, post
    )
}

/// Parse what the page posted back (`type:payload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Solved(String),
    Expired,
    Failed,
    Cancel,
}

impl Outcome {
    pub fn from_body(body: &str) -> Option<Outcome> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge() -> CaptchaChallenge {
        CaptchaChallenge {
            site_key: "site-key".to_owned(),
            rqdata: Some("rq-data".to_owned()),
        }
    }

    /// The widget carries the site key against Discord's host on both
    /// bridges; only the way back to Rust differs.
    #[test]
    fn the_page_names_the_challenge_and_its_bridge() {
        for bridge in [Bridge::Wry, Bridge::JsInterface("CaptchaBridge")] {
            let page = html(&challenge(), bridge);
            assert!(page.contains("data-sitekey=\"site-key\""), "{bridge:?}");
            assert!(page.contains("data-host=\"discord.com\""), "{bridge:?}");
            assert!(
                page.contains("hcaptcha.setData(\"rq-data\");"),
                "{bridge:?}"
            );
        }
        assert!(
            html(&challenge(), Bridge::Wry).contains("window.ipc.postMessage(msg);"),
            "wry posts through ipc"
        );
        assert!(
            html(&challenge(), Bridge::JsInterface("CaptchaBridge"))
                .contains("window.CaptchaBridge.post(msg);"),
            "android posts through its object"
        );
    }

    /// Without enterprise data there is nothing to set before rendering.
    #[test]
    fn without_rqdata_there_is_no_setup_call() {
        let plain = CaptchaChallenge {
            site_key: "site-key".to_owned(),
            rqdata: None,
        };
        assert!(!html(&plain, Bridge::Wry).contains("setData"));
    }

    /// Every posted message parses; an empty token or a stranger does not.
    #[test]
    fn posted_messages_parse_or_not() {
        assert_eq!(
            Outcome::from_body("solved:token"),
            Some(Outcome::Solved("token".to_owned()))
        );
        assert_eq!(Outcome::from_body("expired"), Some(Outcome::Expired));
        assert_eq!(Outcome::from_body("failed"), Some(Outcome::Failed));
        assert_eq!(Outcome::from_body("cancel"), Some(Outcome::Cancel));
        assert_eq!(Outcome::from_body("solved:"), None);
        assert_eq!(Outcome::from_body("hello"), None);
    }
}
