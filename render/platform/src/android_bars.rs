//! Android's system bars.
//!
//! The window is edge-to-edge, so the surface runs behind both bars: the
//! status bar is transparent and the content is pushed under it by the
//! same root padding iOS uses, while the navigation bar is put away rather
//! than left lying over the composer.
//!
//! The window calls happen in `android_on_create`, which android-activity
//! runs on the Java main thread while `Activity.onCreate` is on the stack.
//! `android_main` is another thread and the view hierarchy rejects work
//! from it, so nothing here may be moved there.

use jni::objects::{JObject, JValue};
use jni::{jni_sig, jni_str};

/// `View.SYSTEM_UI_FLAG_HIDE_NAVIGATION`: the navigation bar goes away.
const HIDE_NAVIGATION: i32 = 0x2;
/// `View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY`: it stays away and returns only
/// as an overlay for a swipe from the edge, never as a resize.
const IMMERSIVE_STICKY: i32 = 0x10;
/// `WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`: the
/// same behaviour, spelled the API 30 way.
const TRANSIENT_BARS: i32 = 0x2;
/// `WindowInsets.Type.navigationBars()`.
const NAVIGATION_BARS: i32 = 0x2;
/// The SDK from which `WindowInsetsController` and the `Insets` accessors
/// exist. Below it the view flags and the old accessors do the same work.
const MODERN: i32 = 30;

/// Prepares the window for edge-to-edge: content behind the bars, and the
/// navigation bar out of the way. The bars' own colour is the theme's job.
///
/// Java main thread only.
pub fn prepare(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    // `from_raw` asserts on null, so the pointers are checked before it.
    if vm.is_null() || activity.is_null() {
        tracing::warn!("onCreate handed over no JVM; the bars stay as they are");
        return;
    }
    // Safe: the shell hands over pointers owned by the running `onCreate`.
    let vm = unsafe { jni::JavaVM::from_raw(vm.cast()) };
    let done = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let activity = unsafe { JObject::from_raw(env, activity.cast()) };
        prepare_with(env, &activity)
    });
    if let Err(e) = done {
        tracing::warn!(%e, "the system bars could not be prepared");
    }
}

/// Top, right, bottom and left in logical pixels, for the root padding.
///
/// Reads only, so any thread may ask; zeros before the first window.
pub fn safe_insets() -> (f32, f32, f32, f32) {
    let ctx = ndk_context::android_context();
    // `from_raw` asserts on null, and a shell that never set the context
    // has exactly that.
    if ctx.vm().is_null() || ctx.context().is_null() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    // Safe: the shell set both pointers up before the first frame ran.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| -> Result<(f32, f32, f32, f32), jni::errors::Error> {
        let activity = unsafe { JObject::from_raw(env, ctx.context() as jni::sys::jobject) };
        Ok(read_insets(env, &activity).unwrap_or_default())
    })
    .unwrap_or_default()
}

fn prepare_with(env: &mut jni::Env<'_>, activity: &JObject<'_>) -> Result<(), jni::errors::Error> {
    let window = window_of(env, activity)?;
    let decor = decor_of(env, &window)?;
    // Content behind the bars. WindowCompat takes the version branch:
    // layout flags below 30, the window call from 30 on.
    env.call_static_method(
        jni_str!("androidx/core/view/WindowCompat"),
        jni_str!("setDecorFitsSystemWindows"),
        jni_sig!("(Landroid/view/Window;Z)V"),
        &[JValue::from(&window), JValue::Bool(false)],
    )?;

    if sdk_int(env)? < MODERN {
        // Below 30 there is no controller; the view flags say the same
        // thing. The layout flags WindowCompat has just set have to
        // survive, so the current mask is kept and only the hiding ones
        // are added.
        let flags = env
            .call_method(
                &decor,
                jni_str!("getSystemUiVisibility"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        env.call_method(
            &decor,
            jni_str!("setSystemUiVisibility"),
            jni_sig!("(I)V"),
            &[JValue::Int(flags | HIDE_NAVIGATION | IMMERSIVE_STICKY)],
        )?;
        return Ok(());
    }

    let controller = env
        .call_method(
            &decor,
            jni_str!("getWindowInsetsController"),
            jni_sig!("()Landroid/view/WindowInsetsController;"),
            &[],
        )?
        .l()?;
    if controller.is_null() {
        return Ok(());
    }
    env.call_method(
        &controller,
        jni_str!("setSystemBarsBehavior"),
        jni_sig!("(I)V"),
        &[JValue::Int(TRANSIENT_BARS)],
    )?;
    env.call_method(
        &controller,
        jni_str!("hide"),
        jni_sig!("(I)V"),
        &[JValue::Int(NAVIGATION_BARS)],
    )?;
    Ok(())
}

fn read_insets(
    env: &mut jni::Env<'_>,
    activity: &JObject<'_>,
) -> Result<(f32, f32, f32, f32), jni::errors::Error> {
    let window = window_of(env, activity)?;
    let decor = decor_of(env, &window)?;
    let roots = env
        .call_method(
            &decor,
            jni_str!("getRootWindowInsets"),
            jni_sig!("()Landroid/view/WindowInsets;"),
            &[],
        )?
        .l()?;
    if roots.is_null() {
        return Ok((0.0, 0.0, 0.0, 0.0));
    }
    let density = density_of(env, activity)?;

    if sdk_int(env)? < MODERN {
        let top = env
            .call_method(
                &roots,
                jni_str!("getSystemWindowInsetTop"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        let right = env
            .call_method(
                &roots,
                jni_str!("getSystemWindowInsetRight"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        let bottom = env
            .call_method(
                &roots,
                jni_str!("getSystemWindowInsetBottom"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        let left = env
            .call_method(
                &roots,
                jni_str!("getSystemWindowInsetLeft"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        return Ok((
            top as f32 / density,
            right as f32 / density,
            bottom as f32 / density,
            left as f32 / density,
        ));
    }

    // The status bar is always there, and asking about the visible set
    // would move the header every time the bars are swept away.
    let system_bars = env
        .call_static_method(
            jni_str!("android/view/WindowInsets$Type"),
            jni_str!("systemBars"),
            jni_sig!("()I"),
            &[],
        )?
        .i()?
        | env
            .call_static_method(
                jni_str!("android/view/WindowInsets$Type"),
                jni_str!("displayCutout"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
    let stable: JObject = env
        .call_method(
            &roots,
            jni_str!("getInsetsIgnoringVisibility"),
            jni_sig!("(I)Landroid/graphics/Insets;"),
            &[JValue::Int(system_bars)],
        )?
        .l()?;
    // The navigation bar this app hides takes no room; while it is swept
    // back in for a moment, the bottom clears it.
    let nav_bars = env
        .call_static_method(
            jni_str!("android/view/WindowInsets$Type"),
            jni_str!("navigationBars"),
            jni_sig!("()I"),
            &[],
        )?
        .i()?;
    let shown: JObject = env
        .call_method(
            &roots,
            jni_str!("getInsets"),
            jni_sig!("(I)Landroid/graphics/Insets;"),
            &[JValue::Int(nav_bars)],
        )?
        .l()?;

    let top = env
        .get_field(&stable, jni_str!("top"), jni_sig!("I"))?
        .i()?;
    let right = env
        .get_field(&stable, jni_str!("right"), jni_sig!("I"))?
        .i()?;
    let bottom = env
        .get_field(&shown, jni_str!("bottom"), jni_sig!("I"))?
        .i()?;
    let left = env
        .get_field(&stable, jni_str!("left"), jni_sig!("I"))?
        .i()?;
    Ok((
        top as f32 / density,
        right as f32 / density,
        bottom as f32 / density,
        left as f32 / density,
    ))
}

fn window_of<'local>(
    env: &mut jni::Env<'local>,
    activity: &JObject<'_>,
) -> Result<JObject<'local>, jni::errors::Error> {
    env.call_method(
        activity,
        jni_str!("getWindow"),
        jni_sig!("()Landroid/view/Window;"),
        &[],
    )?
    .l()
}

fn decor_of<'local>(
    env: &mut jni::Env<'local>,
    window: &JObject<'_>,
) -> Result<JObject<'local>, jni::errors::Error> {
    env.call_method(
        window,
        jni_str!("getDecorView"),
        jni_sig!("()Landroid/view/View;"),
        &[],
    )?
    .l()
}

/// Pixels per logical pixel; the frame works in the latter everywhere.
fn density_of(env: &mut jni::Env<'_>, activity: &JObject<'_>) -> Result<f32, jni::errors::Error> {
    let resources = env
        .call_method(
            activity,
            jni_str!("getResources"),
            jni_sig!("()Landroid/content/res/Resources;"),
            &[],
        )?
        .l()?;
    let metrics = env
        .call_method(
            &resources,
            jni_str!("getDisplayMetrics"),
            jni_sig!("()Landroid/util/DisplayMetrics;"),
            &[],
        )?
        .l()?;
    env.get_field(&metrics, jni_str!("density"), jni_sig!("F"))?
        .f()
}

fn sdk_int(env: &mut jni::Env<'_>) -> Result<i32, jni::errors::Error> {
    env.get_static_field(
        jni_str!("android/os/Build$VERSION"),
        jni_str!("SDK_INT"),
        jni_sig!("I"),
    )?
    .i()
}
