//! Linux virtual displays backed by niri's compositor-owned virtual outputs.
//!
//! The DRM capture backend enumerates physical connectors; a niri virtual output has
//! no connector, so it is appended to that list here and captured through
//! wlr-screencopy instead of the `_drm` stream. Display indices therefore run
//! `[DRM outputs…, virtual outputs…]`, and every consumer that indexes the list by
//! display index (capturer construction, resolution changes) goes through the same
//! `drm_capturer::get_display_infos()` snapshot.

use crate::platform::niri_ipc;
use hbb_common::config::{self, Config};
use hbb_common::message_proto::{DisplayInfo, Resolution};
use hbb_common::{log, ResultType};
use std::sync::Mutex;

/// Whether the cursor is composited into the captured frames. Default off:
/// the client draws its own zero-latency pointer instead of the one-RTT-late
/// embedded one (and pure cursor motion stops generating video frames). Set
/// the host option `niri-embed-cursor=Y` to restore the old behavior without
/// a rebuild.
fn embed_cursor() -> bool {
    // Explicit equality: `option2bool` would default this to true.
    Config::get_option("niri-embed-cursor") == "Y"
}

/// One `DisplayInfo` per virtual output that is on.
pub(super) fn display_infos() -> Vec<DisplayInfo> {
    niri_ipc::virtual_outputs()
        .into_iter()
        .filter_map(|o| {
            let (w, h, _) = o.mode?;
            let l = o.logical?;
            let scale = if l.width > 0 {
                f64::from(w) / f64::from(l.width)
            } else {
                1.0
            };
            Some(DisplayInfo {
                x: l.x,
                y: l.y,
                width: i32::from(w),
                height: i32::from(h),
                name: o.name,
                online: true,
                cursor_embedded: embed_cursor(),
                // 0x0 is the client's "virtual display" marker: it enables the custom
                // resolution entry in the display menu.
                original_resolution: Some(Resolution {
                    width: 0,
                    height: 0,
                    ..Default::default()
                })
                .into(),
                scale,
                ..Default::default()
            })
        })
        .collect()
}

pub(super) fn append(infos: &mut Vec<DisplayInfo>) {
    let virtuals = display_infos();
    if !virtuals.is_empty() {
        log::debug!(
            "niri: appending {} virtual display(s) after {} DRM display(s)",
            virtuals.len(),
            infos.len()
        );
        infos.extend(virtuals);
    }
}

/// A capturer for `display_idx` if it refers to a niri virtual output; `None` lets the
/// caller continue with the DRM/PipeWire paths.
pub(super) fn capturer_info(
    display_idx: usize,
) -> Option<ResultType<super::video_service::CapturerInfo>> {
    // Resolve through the advertised (synced) list first: while a session
    // takeover has the physical outputs off, a fresh DRM enumeration comes up
    // empty and can flip the DRM cache to Unavailable, but the sync list keeps
    // the index space the client is using.
    let info = super::display_service::get_display_info(display_idx).or_else(|| {
        super::drm_capturer::get_display_infos()?
            .get(display_idx)
            .cloned()
    })?;
    let ndisplay = {
        let synced = super::display_service::get_sync_displays().len();
        let n = if synced > 0 {
            synced
        } else {
            super::drm_capturer::get_display_infos()
                .map(|v| v.len())
                .unwrap_or(0)
        };
        n.max(display_idx + 1)
    };
    let output = niri_ipc::find_virtual(&info.name)?;
    let (w, h, _) = output.mode?;
    let (width, height) = (usize::from(w), usize::from(h));
    log::info!(
        "niri: capturing virtual output {} ({}x{} at {},{}) via wlr-screencopy",
        info.name,
        width,
        height,
        info.x,
        info.y
    );
    let capturer = match scrap::wayland::screencopy::ScreencopyCapturer::new(
        &info.name,
        width,
        height,
        embed_cursor(),
    ) {
        Ok(c) => c,
        Err(e) => return Some(Err(e.into())),
    };
    Some(Ok(super::video_service::CapturerInfo {
        origin: (info.x, info.y),
        width,
        height,
        ndisplay,
        current: display_idx,
        privacy_mode_id: 0,
        _capturer_privacy_mode_id: 0,
        capturer: Box::new(capturer),
    }))
}

// ---- Session takeover -------------------------------------------------------
//
// Windows-RDP-like semantics, gated by the host option
// `allow-niri-session-takeover`: while at least one remote desktop session is
// authorized, the physical outputs are turned off so every workspace lives on
// the (client-resizable) virtual output — the remote user sees the same
// windows the console had. On release the physical outputs come back, niri
// returns the workspaces it displaced, and any windowed workspace still on a
// virtual output is moved to the first restored physical output so nothing
// stays stranded on an invisible display.
//
// The RustDesk display list is deliberately left alone: the DRM cache keeps
// its last-known physical entry while the output is off, so display indices
// stay stable for the whole session (no plug-out dialogs, no index shifts).
//
// Crash safety is layered: a sentinel file written before outputs go off, an
// orphan restore on `--server` start (the service respawns it within ~1 s),
// and a systemd ExecStopPost hook on the host reading the same sentinel.

const TAKEOVER_OPTION: &str = "allow-niri-session-takeover";
const TAKEOVER_SENTINEL: &str = "rustdesk-niri-takeover.json";
const VIRTUAL_OUTPUT_NAME: &str = "rustdesk-virtual-1";

static TAKEOVER: Mutex<Option<SessionTakeover>> = Mutex::new(None);
// Bumped by every release so an acquire that was still doing IPC when its
// session vanished restores instead of engaging a takeover nobody owns.
static RELEASE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct SessionTakeover {
    disabled: Vec<String>,
}

fn sentinel_path() -> std::path::PathBuf {
    niri_ipc::runtime_dir().join(TAKEOVER_SENTINEL)
}

fn takeover_enabled() -> bool {
    config::option2bool(TAKEOVER_OPTION, &Config::get_option(TAKEOVER_OPTION))
}

/// First-authorized-remote-session hook. Never blocks or fails the login: all
/// niri IPC runs on a named thread, and any error just leaves the session
/// without a takeover.
pub(super) fn session_takeover_acquire() {
    if !takeover_enabled() {
        return;
    }
    let epoch = RELEASE_EPOCH.load(std::sync::atomic::Ordering::SeqCst);
    std::thread::Builder::new()
        .name("niri-takeover".to_owned())
        .spawn(move || acquire_blocking(epoch))
        .ok();
}

fn acquire_blocking(epoch: u64) {
    let mut guard = TAKEOVER.lock().unwrap();
    if guard.is_some() {
        return;
    }
    if RELEASE_EPOCH.load(std::sync::atomic::Ordering::SeqCst) != epoch {
        // The session that requested this takeover is already gone.
        return;
    }
    let outs = match niri_ipc::outputs() {
        Ok(o) => o,
        Err(e) => {
            log::warn!("niri takeover skipped: {e}");
            return;
        }
    };
    let has_virtual = outs
        .iter()
        .any(|o| o.is_virtual() && o.mode.is_some() && o.logical.is_some());
    if !has_virtual {
        // The current client logged in without the virtual display in its
        // PeerInfo; engaging now would strand it on a dark output. Create the
        // output for the next session and leave this one alone.
        match niri_ipc::create_virtual_output(VIRTUAL_OUTPUT_NAME, 1920, 1080, 60, Some(2.0)) {
            Ok(name) => log::info!(
                "niri takeover: created missing virtual output {name}; deferred to the next session"
            ),
            Err(e) => log::warn!("niri takeover: no virtual output and creating one failed: {e}"),
        }
        return;
    }
    let candidates: Vec<String> = outs
        .iter()
        .filter(|o| !o.is_virtual() && o.mode.is_some())
        .map(|o| o.name.clone())
        .collect();
    if candidates.is_empty() {
        *guard = Some(SessionTakeover { disabled: Vec::new() });
        return;
    }
    // Sentinel first: a crash between here and the restore must leave enough
    // state behind for the orphan/ExecStopPost recovery. Turning an already-on
    // output on is harmless, so over-listing is fine.
    if let Err(e) = std::fs::write(
        sentinel_path(),
        serde_json::json!({ "outputs": candidates }).to_string(),
    ) {
        log::warn!("niri takeover: failed to write sentinel: {e}");
    }
    let mut disabled = Vec::new();
    for name in &candidates {
        match niri_ipc::set_output_enabled(name, false) {
            Ok(()) => disabled.push(name.clone()),
            Err(e) => log::warn!("niri takeover: failed to turn off {name}: {e}"),
        }
    }
    if disabled.is_empty() {
        let _ = std::fs::remove_file(sentinel_path());
        return;
    }
    if RELEASE_EPOCH.load(std::sync::atomic::Ordering::SeqCst) != epoch {
        // The session vanished while the outputs were being turned off.
        restore_outputs(disabled, false);
        return;
    }
    log::info!("niri: session takeover engaged; turned off {disabled:?}");
    *guard = Some(SessionTakeover { disabled });
}

/// Last-remote-session-closed hook; restoration runs on a named thread.
pub(super) fn session_takeover_release() {
    RELEASE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *TAKEOVER.lock().unwrap() = None;
}

impl Drop for SessionTakeover {
    fn drop(&mut self) {
        let disabled = std::mem::take(&mut self.disabled);
        std::thread::Builder::new()
            .name("niri-takeover-restore".to_owned())
            .spawn(move || restore_outputs(disabled, true))
            .ok();
    }
}

fn restore_outputs(names: Vec<String>, consolidate: bool) {
    for name in &names {
        if let Err(e) = niri_ipc::set_output_enabled(name, true) {
            log::error!("niri takeover: failed to restore {name}: {e}");
        }
    }
    let _ = std::fs::remove_file(sentinel_path());
    if names.is_empty() {
        return;
    }
    log::info!("niri: session takeover released; turned on {names:?}");
    if consolidate {
        // Give niri a moment to re-enable the outputs and move the workspaces
        // it displaced back before consolidating the rest.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        consolidate_workspaces_to(&names);
    }
    // Re-probe the DRM verdict so a takeover-time Unavailable flip cannot
    // outlive the takeover.
    super::drm_capturer::warm_availability();
}

/// Move every windowed workspace still parked on a virtual output to the first
/// restored physical output, so nothing stays on an invisible display after
/// the session ends.
fn consolidate_workspaces_to(physical: &[String]) {
    let Some(target) = physical.first() else {
        return;
    };
    let virtuals: Vec<String> = niri_ipc::virtual_outputs()
        .into_iter()
        .map(|o| o.name)
        .collect();
    let list = match niri_ipc::workspaces() {
        Ok(l) => l,
        Err(e) => {
            log::warn!("niri takeover: workspace consolidation skipped: {e}");
            return;
        }
    };
    for ws in list {
        let on_virtual = ws
            .output
            .as_deref()
            .is_some_and(|o| virtuals.iter().any(|v| v == o));
        if ws.has_windows && on_virtual {
            match niri_ipc::move_workspace_to_monitor(ws.id, target) {
                Ok(()) => log::info!(
                    "niri takeover: moved workspace {} from a virtual output to {target}",
                    ws.id
                ),
                Err(e) => log::warn!("niri takeover: failed to move workspace {}: {e}", ws.id),
            }
        }
    }
}

/// `--server` start: if a previous process died mid-takeover, its sentinel
/// names the outputs to turn back on.
pub(super) fn restore_orphaned_takeover() {
    std::thread::Builder::new()
        .name("niri-takeover-orphan".to_owned())
        .spawn(|| {
            let path = sentinel_path();
            let Ok(data) = std::fs::read_to_string(&path) else {
                return;
            };
            let names: Vec<String> = serde_json::from_str::<serde_json::Value>(&data)
                .ok()
                .and_then(|v| {
                    Some(
                        v.get("outputs")?
                            .as_array()?
                            .iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect(),
                    )
                })
                .unwrap_or_default();
            if names.is_empty() {
                let _ = std::fs::remove_file(&path);
                return;
            }
            log::info!("niri takeover: restoring orphaned outputs {names:?}");
            restore_outputs(names, true);
        })
        .ok();
}
