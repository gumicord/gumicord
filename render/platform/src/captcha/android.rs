//! The Android captcha host: a `WebView` with a small Kotlin answer object.
//!
//! No activity change: the view is built from the running activity over
//! JNI and added to its content, then removed again. The page answers
//! through `CaptchaBridge` (one `type:payload` string per post); Rust
//! polls it from a nested pump built on public looper APIs, so the UI
//! thread keeps serving the webview while the modal waits.

use jni::objects::{JObject, JString, JValue};
use jni::{jni_sig, jni_str};
use winit::window::Window;

use super::page::{Bridge, Outcome, html};
use super::{CaptchaChallenge, CaptchaError, CaptchaHost, SolvedCaptcha};

/// Name the page posts through, matching `CaptchaBridge.kt`.
const BRIDGE_NAME: &str = "CaptchaBridge";
/// The page origin the challenge is presented against.
const BASE_URL: &str = "https://discord.com";
/// `ViewGroup.LayoutParams.MATCH_PARENT`.
const MATCH_PARENT: i32 = -1;
/// The modal paints its own background; the view starts dark behind it.
const DARK: i32 = 0xFF31_3338u32 as i32;

/// An Android [`CaptchaHost`].
#[derive(Debug, Default)]
pub struct AndroidCaptcha;

impl From<jni::errors::Error> for CaptchaError {
    fn from(e: jni::errors::Error) -> Self {
        // JNI failures carry no user-actionable detail; the login retry
        // says the challenge failed, which is what to try instead.
        CaptchaError::Open(format!("the challenge view failed: {e}"))
    }
}

impl CaptchaHost for AndroidCaptcha {
    fn solve(
        &mut self,
        _parent: &Window,
        challenge: CaptchaChallenge,
    ) -> Result<SolvedCaptcha, CaptchaError> {
        let ctx = ndk_context::android_context();
        // Safe: mirrors init_tls_verifier in app/android; the host set
        // both pointers up before this ran.
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
        vm.attach_current_thread(|env| {
            let activity = unsafe { JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
            solve_with(env, &activity, &challenge)
        })
    }
}

fn solve_with(
    env: &mut jni::Env<'_>,
    activity: &JObject<'_>,
    challenge: &CaptchaChallenge,
) -> Result<SolvedCaptcha, CaptchaError> {
    let webview = env.new_object(
        jni_str!("android/webkit/WebView"),
        jni_sig!("(Landroid/content/Context;)V"),
        &[JValue::from(activity)],
    )?;
    let settings = env
        .call_method(
            &webview,
            jni_str!("getSettings"),
            jni_sig!("()Landroid/webkit/WebSettings;"),
            &[],
        )?
        .l()?;
    env.call_method(
        &settings,
        jni_str!("setJavaScriptEnabled"),
        jni_sig!("(Z)V"),
        &[JValue::Bool(true)],
    )?;
    env.call_method(
        &webview,
        jni_str!("setBackgroundColor"),
        jni_sig!("(I)V"),
        &[JValue::Int(DARK)],
    )?;
    let bridge = env.new_object(
        jni_str!("dev/gumicord/app/CaptchaBridge"),
        jni_sig!("()V"),
        &[],
    )?;
    let name = env.new_string(BRIDGE_NAME)?;
    env.call_method(
        &webview,
        jni_str!("addJavascriptInterface"),
        jni_sig!("(Ljava/lang/Object;Ljava/lang/String;)V"),
        &[JValue::from(&bridge), JValue::from(&name)],
    )?;
    let (base, page, mime, encoding) = (
        env.new_string(BASE_URL)?,
        env.new_string(html(challenge, Bridge::JsInterface(BRIDGE_NAME)))?,
        env.new_string("text/html")?,
        env.new_string("utf-8")?,
    );
    env.call_method(
        &webview,
        jni_str!("loadDataWithBaseURL"),
        jni_sig!(
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"
        ),
        &[
            JValue::from(&base),
            JValue::from(&page),
            JValue::from(&mime),
            JValue::from(&encoding),
            JValue::from(&JObject::null()),
        ],
    )?;
    let window = env
        .call_method(
            activity,
            jni_str!("getWindow"),
            jni_sig!("()Landroid/view/Window;"),
            &[],
        )?
        .l()?;
    let decor = env
        .call_method(
            &window,
            jni_str!("getDecorView"),
            jni_sig!("()Landroid/view/View;"),
            &[],
        )?
        .l()?;
    let params = env.new_object(
        jni_str!("android/view/ViewGroup$LayoutParams"),
        jni_sig!("(II)V"),
        &[JValue::Int(MATCH_PARENT), JValue::Int(MATCH_PARENT)],
    )?;
    env.call_method(
        &decor,
        jni_str!("addView"),
        jni_sig!("(Landroid/view/View;Landroid/view/ViewGroup$LayoutParams;)V"),
        &[JValue::from(&webview), JValue::from(&params)],
    )?;

    // The modal: each turn first asks the bridge, then delivers one looper
    // message. Blocking in `next` is ordinary (that is what the outer loop
    // does); user input is a message, so waiting cannot deadlock it.
    let answer: Result<String, CaptchaError> = loop {
        if let Some(body) = poll(env, &bridge)? {
            match Outcome::from_body(&body) {
                Some(Outcome::Solved(token)) => break Ok(token),
                Some(Outcome::Cancel) => break Err(CaptchaError::Cancelled),
                Some(Outcome::Failed) => {
                    break Err(CaptchaError::Open(
                        "the challenge could not be completed".to_owned(),
                    ));
                }
                Some(Outcome::Expired) => {
                    eval(env, &webview, "hcaptcha.reset(0);")?;
                }
                None => {}
            }
            continue;
        }
        if !pump_once(env)? {
            break Err(CaptchaError::Cancelled);
        }
    };

    // Leaving either way: the view goes away with the modal, solved or not.
    env.call_method(
        &decor,
        jni_str!("removeView"),
        jni_sig!("(Landroid/view/View;)V"),
        &[JValue::from(&webview)],
    )?;
    env.call_method(&webview, jni_str!("destroy"), jni_sig!("()V"), &[])?;
    answer.map(|solution| SolvedCaptcha { solution })
}

/// The bridge's oldest answer, if the page posted one.
fn poll(env: &mut jni::Env<'_>, bridge: &JObject<'_>) -> Result<Option<String>, CaptchaError> {
    let answer = env
        .call_method(
            bridge,
            jni_str!("pollMessage"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    if answer.is_null() {
        return Ok(None);
    }
    let answer: JString = env.cast_local::<JString>(answer)?;
    Ok(Some(answer.try_to_string(env)?))
}

/// Run one script without an answer: the call itself is the effect.
fn eval(env: &mut jni::Env<'_>, webview: &JObject<'_>, script: &str) -> Result<(), CaptchaError> {
    let script = env.new_string(script)?;
    env.call_method(
        webview,
        jni_str!("evaluateJavascript"),
        jni_sig!("(Ljava/lang/String;Landroid/webkit/ValueCallback;)V"),
        &[JValue::from(&script), JValue::from(&JObject::null())],
    )?;
    Ok(())
}

/// One nested turn: take the next looper message and deliver it. A null
/// message means the looper is quitting; the modal is moot then.
fn pump_once(env: &mut jni::Env<'_>) -> Result<bool, CaptchaError> {
    let queue = env
        .call_static_method(
            jni_str!("android/os/Looper"),
            jni_str!("myQueue"),
            jni_sig!("()Landroid/os/MessageQueue;"),
            &[],
        )?
        .l()?;
    let message = env
        .call_method(
            &queue,
            jni_str!("next"),
            jni_sig!("()Landroid/os/Message;"),
            &[],
        )?
        .l()?;
    if message.is_null() {
        return Ok(false);
    }
    let target = env
        .call_method(
            &message,
            jni_str!("getTarget"),
            jni_sig!("()Landroid/os/Handler;"),
            &[],
        )?
        .l()?;
    env.call_method(
        &target,
        jni_str!("dispatchMessage"),
        jni_sig!("(Landroid/os/Message;)V"),
        &[JValue::from(&message)],
    )?;
    env.call_method(&message, jni_str!("recycle"), jni_sig!("()V"), &[])?;
    Ok(true)
}
