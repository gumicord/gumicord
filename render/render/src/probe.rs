//! GPU backend probing in child processes.
//!
//! A broken driver segfaults while the instance is created, taking the whole
//! process down. Each candidate backend is therefore created in a child that
//! may die; the parent only trusts backends whose child reported back.
//!
//! The child is this same binary, run with `--probe-gpu=<backend>`. It prints
//! one JSON line and exits; anything else means that backend is unusable.
//!
//! Fresh crashes are remembered on disk, so a known-bad backend is skipped
//! without spawning. Remembered crashes expire, so a fixed driver is retried.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Set to `0` to skip probing (a crashing driver then takes the client down
/// again, as before). For broken environments and debugging only.
const PROBE_ENV: &str = "GUMICORD_GPU_PROBE";
const PROBE_ARG: &str = "--probe-gpu=";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const CACHE_FILE: &str = "probe.json";
/// Remembered crashes count for this long before a retry.
const EXCLUSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Report {
    backend: String,
    adapters: Vec<String>,
}

fn backend_name(backend: wgpu::Backends) -> Option<&'static str> {
    if backend == wgpu::Backends::GL {
        Some("gl")
    } else if backend == wgpu::Backends::VULKAN {
        Some("vulkan")
    } else if backend == wgpu::Backends::DX12 {
        Some("dx12")
    } else if backend == wgpu::Backends::METAL {
        Some("metal")
    } else {
        None
    }
}

fn backend_of(name: &str) -> Option<wgpu::Backends> {
    match name {
        "gl" => Some(wgpu::Backends::GL),
        "vulkan" => Some(wgpu::Backends::VULKAN),
        "dx12" => Some(wgpu::Backends::DX12),
        "metal" => Some(wgpu::Backends::METAL),
        _ => None,
    }
}

/// Runs the child side. Returns true when the arguments held a probe request,
/// so `main` exits instead of starting the client.
pub fn run_probe(args: &[String]) -> bool {
    let Some(arg) = args.iter().find(|a| a.starts_with(PROBE_ARG)) else {
        return false;
    };
    // The parent's explicit choice must not leak in: the child probes exactly
    // the backend it was asked for.
    unsafe { std::env::remove_var("WGPU_BACKEND") };
    let name = &arg[PROBE_ARG.len()..];
    match backend_of(name) {
        Some(backend) => {
            let adapters = probe_backend(backend);
            let report = Report {
                backend: name.to_owned(),
                adapters,
            };
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
        }
        None => eprintln!("unknown gpu backend for probing: {name}"),
    }
    true
}

fn probe_backend(backend: wgpu::Backends) -> Vec<String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    desc.backends = backend;
    let instance = wgpu::Instance::new(desc);
    pollster::block_on(instance.enumerate_adapters(backend))
        .iter()
        .map(|a| a.get_info().name.clone())
        .collect()
}

/// Backends whose probe child reported back.
///
/// Empty when none did; when no child could even start, everything is
/// trusted, matching the old behaviour. `cache_dir` remembers fresh crashes.
pub fn surviving_backends(
    candidates: &[wgpu::Backends],
    cache_dir: Option<&Path>,
) -> wgpu::Backends {
    if std::env::var("WGPU_BACKEND").is_ok() || std::env::var(PROBE_ENV).as_deref() == Ok("0") {
        return wgpu::Backends::all();
    }
    let mut excluded = load_exclusions(cache_dir);
    let mut wanted: Vec<(&str, wgpu::Backends)> = Vec::new();
    for backend in candidates {
        let Some(name) = backend_name(*backend) else {
            continue;
        };
        if excluded.contains_key(name) {
            continue;
        }
        wanted.push((name, *backend));
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(%e, "cannot re-execute for gpu probing; trusting all backends");
            return wgpu::Backends::all();
        }
    };
    // One child per backend, all at once: a segfault in one must not hold up
    // the others, and serial probing would stack their startups.
    let mut running = Vec::new();
    for (name, _) in &wanted {
        let child = std::process::Command::new(&exe)
            .arg(format!("{PROBE_ARG}{name}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Ok(child) => running.push((name.to_string(), child)),
            Err(e) => tracing::warn!(backend = name, %e, "gpu probe did not start"),
        }
    }
    if running.is_empty() && !wanted.is_empty() {
        tracing::warn!("no gpu probe started; trusting all backends");
        return wgpu::Backends::all();
    }
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut ok = wgpu::Backends::empty();
    let mut pending = running;
    while !pending.is_empty() {
        let mut rest = Vec::new();
        for (name, mut child) in pending {
            match child.try_wait() {
                Ok(Some(status)) => match child.wait_with_output() {
                    Ok(out) if status.success() => match parse_report(&out.stdout, &name) {
                        Some(report) => {
                            tracing::debug!(backend = name, adapters = ?report.adapters, "gpu probe survived");
                            excluded.remove(&name);
                            ok |= backend_of(&name).unwrap_or(wgpu::Backends::empty());
                        }
                        None => {
                            tracing::warn!(backend = name, "gpu probe spoke garbage; excluding it");
                            exclude(&mut excluded, &name);
                        }
                    },
                    _ => {
                        tracing::warn!(backend = name, status = ?status, "gpu probe failed; excluding it");
                        exclude(&mut excluded, &name);
                    }
                },
                Ok(None) => {
                    if Instant::now() >= deadline {
                        tracing::warn!(backend = name, "gpu probe timed out; excluding it");
                        let _ = child.kill();
                        exclude(&mut excluded, &name);
                    } else {
                        rest.push((name, child));
                    }
                }
                Err(e) => {
                    tracing::warn!(backend = name, %e, "cannot poll the gpu probe; excluding it");
                    exclude(&mut excluded, &name);
                }
            }
        }
        pending = rest;
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    save_exclusions(cache_dir, &excluded);
    ok
}

fn parse_report(stdout: &[u8], name: &str) -> Option<Report> {
    let report: Report = serde_json::from_slice(stdout).ok()?;
    (report.backend == name).then_some(report)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn exclusions_path(dir: &Path) -> PathBuf {
    dir.join(CACHE_FILE)
}

fn load_exclusions(dir: Option<&Path>) -> HashMap<String, u64> {
    let empty = HashMap::new();
    let Some(dir) = dir else { return empty };
    let Ok(text) = std::fs::read_to_string(exclusions_path(dir)) else {
        return empty;
    };
    let mut map: HashMap<String, u64> = serde_json::from_str(&text).unwrap_or_default();
    let now = now_secs();
    map.retain(|_, at| at.saturating_add(EXCLUSION_TTL.as_secs()) > now);
    map
}

fn exclude(excluded: &mut HashMap<String, u64>, name: &str) {
    excluded.insert(name.to_owned(), now_secs());
}

fn save_exclusions(dir: Option<&Path>, excluded: &HashMap<String, u64>) {
    let Some(dir) = dir else { return };
    let Ok(text) = serde_json::to_string(excluded) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(exclusions_path(dir), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_round_trip() {
        for (name, backend) in [
            ("gl", wgpu::Backends::GL),
            ("vulkan", wgpu::Backends::VULKAN),
            ("dx12", wgpu::Backends::DX12),
            ("metal", wgpu::Backends::METAL),
        ] {
            assert_eq!(backend_name(backend), Some(name));
            assert_eq!(backend_of(name), Some(backend));
        }
        assert_eq!(backend_of("direct3d"), None);
        assert_eq!(backend_name(wgpu::Backends::all()), None);
    }

    #[test]
    fn reports_parse_or_not() {
        let good = br#"{"backend":"gl","adapters":["Mesa"]}"#;
        assert_eq!(
            parse_report(good, "gl"),
            Some(Report {
                backend: "gl".to_owned(),
                adapters: vec!["Mesa".to_owned()],
            })
        );
        assert!(parse_report(good, "vulkan").is_none(), "wrong backend");
        assert!(parse_report(b"not json", "gl").is_none());
        assert!(parse_report(b"", "gl").is_none());
    }

    #[test]
    fn non_probe_arguments_pass_through() {
        assert!(!run_probe(&[]));
        assert!(!run_probe(&["gumicord".to_owned()]));
    }

    #[test]
    fn probing_turns_off_on_request() {
        // SAFETY: no other test touches this variable, and with probing off
        // nothing spawns that could read the environment concurrently.
        unsafe {
            std::env::set_var(PROBE_ENV, "0");
        }
        let backends = surviving_backends(&[wgpu::Backends::GL], None);
        unsafe {
            std::env::remove_var(PROBE_ENV);
        }
        assert_eq!(backends, wgpu::Backends::all());
    }
}
