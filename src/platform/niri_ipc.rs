//! Minimal client for niri's IPC socket, used to enumerate and control the
//! compositor-owned virtual outputs that back RustDesk's Linux virtual displays.
//!
//! Protocol: one JSON request per line on `$NIRI_SOCKET`, one JSON reply per line
//! (`{"Ok": …}` / `{"Err": "…"}`). Only the requests this crate needs are wrapped.
//! Virtual outputs are recognised by `make == "niri" && model == "virtual"`, which is
//! what the virtual-output backend reports for them.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use hbb_common::message_proto::Resolution;
use hbb_common::{bail, log, ResultType};
use serde_json::{json, Value};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Resolutions offered to clients next to the current one (physical pixels).
const PRESET_RESOLUTIONS: &[(i32, i32)] = &[
    (3024, 1964),
    (2560, 1600),
    (2560, 1440),
    (1920, 1200),
    (1920, 1080),
    (1680, 1050),
    (1600, 900),
    (1440, 900),
    (1280, 800),
];
const MAX_DIMENSION: i32 = 16384;

static SOCKET: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Debug, Clone)]
pub struct Logical {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
}

#[derive(Debug, Clone)]
pub struct NiriOutput {
    pub name: String,
    pub make: String,
    pub model: String,
    /// Current mode in physical pixels and millihertz; `None` if the output is off.
    pub mode: Option<(u16, u16, u32)>,
    pub logical: Option<Logical>,
}

impl NiriOutput {
    pub fn is_virtual(&self) -> bool {
        self.make == "niri" && self.model == "virtual"
    }
}

pub(crate) fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0);
    PathBuf::from(format!("/run/user/{uid}"))
}

fn candidate_sockets() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("NIRI_SOCKET") {
        v.push(PathBuf::from(p));
    }
    if let Ok(entries) = std::fs::read_dir(runtime_dir()) {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("niri.") && n.ends_with(".sock"))
            })
            .collect();
        found.sort();
        v.extend(found);
    }
    v
}

fn raw_request(path: &PathBuf, request: &Value) -> ResultType<Value> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    if reply.trim().is_empty() {
        bail!("empty reply from niri");
    }
    Ok(serde_json::from_str(&reply)?)
}

/// Send `request` to the compositor and return the payload of `{"Ok": …}`.
fn request(request: Value) -> ResultType<Value> {
    let cached = SOCKET.lock().unwrap().clone();
    let candidates = match cached {
        Some(p) => vec![p],
        None => candidate_sockets(),
    };
    let mut last_err = None;
    for path in candidates {
        match raw_request(&path, &request) {
            Ok(reply) => {
                *SOCKET.lock().unwrap() = Some(path);
                if let Some(err) = reply.get("Err") {
                    bail!("niri: {}", err.as_str().unwrap_or("unknown error"));
                }
                return Ok(reply.get("Ok").cloned().unwrap_or(Value::Null));
            }
            Err(e) => {
                *SOCKET.lock().unwrap() = None;
                last_err = Some(e);
            }
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => bail!("no niri IPC socket found in {}", runtime_dir().display()),
    }
}

/// Whether a niri compositor answers on the IPC socket.
pub fn is_available() -> bool {
    request(json!("Version")).is_ok()
}

fn parse_output(v: &Value) -> Option<NiriOutput> {
    let name = v.get("name")?.as_str()?.to_owned();
    let make = v
        .get("make")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let model = v
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let mode = v
        .get("current_mode")
        .and_then(Value::as_u64)
        .and_then(|i| v.get("modes")?.as_array()?.get(i as usize).cloned())
        .and_then(|m| {
            Some((
                m.get("width")?.as_u64()? as u16,
                m.get("height")?.as_u64()? as u16,
                m.get("refresh_rate")?.as_u64()? as u32,
            ))
        });
    let logical = v.get("logical").and_then(|l| {
        Some(Logical {
            x: l.get("x")?.as_i64()? as i32,
            y: l.get("y")?.as_i64()? as i32,
            width: l.get("width")?.as_i64()? as i32,
            height: l.get("height")?.as_i64()? as i32,
            scale: l.get("scale")?.as_f64()?,
        })
    });
    Some(NiriOutput {
        name,
        make,
        model,
        mode,
        logical,
    })
}

pub fn outputs() -> ResultType<Vec<NiriOutput>> {
    let reply = request(json!("Outputs"))?;
    let map = reply
        .get("Outputs")
        .and_then(Value::as_object)
        .ok_or_else(|| hbb_common::anyhow::anyhow!("unexpected Outputs reply"))?;
    let mut list: Vec<NiriOutput> = map.values().filter_map(parse_output).collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

/// Virtual outputs that are on (have a mode and a logical position). Empty if niri is
/// not reachable, so callers on non-niri hosts see no change.
pub fn virtual_outputs() -> Vec<NiriOutput> {
    match outputs() {
        Ok(list) => list
            .into_iter()
            .filter(|o| o.is_virtual() && o.mode.is_some() && o.logical.is_some())
            .collect(),
        Err(e) => {
            log::debug!("niri outputs unavailable: {e}");
            Vec::new()
        }
    }
}

pub fn find_virtual(name: &str) -> Option<NiriOutput> {
    virtual_outputs().into_iter().find(|o| o.name == name)
}

/// Resize a virtual output through the ordinary `custom-mode` output action.
/// `refresh_hz` `None` keeps the current refresh rate.
pub fn set_custom_mode(
    name: &str,
    width: u16,
    height: u16,
    refresh_hz: Option<f64>,
) -> ResultType<()> {
    let output = find_virtual(name)
        .ok_or_else(|| hbb_common::anyhow::anyhow!("{name} is not a niri virtual output"))?;
    let refresh = refresh_hz
        .or_else(|| output.mode.map(|(_, _, mhz)| mhz as f64 / 1000.0))
        .unwrap_or(60.0);
    log::info!("niri: setting {name} to {width}x{height}@{refresh}");
    request(json!({
        "Output": {
            "output": name,
            "action": { "CustomMode": { "mode": {
                "width": width, "height": height, "refresh": refresh
            } } }
        }
    }))?;
    Ok(())
}

/// Turn an output on or off (`niri msg output <name> on/off`). Unknown names
/// are reported by niri as `OutputWasMissing` inside an `Ok` reply, so they
/// are surfaced here as an error for the caller to log.
pub fn set_output_enabled(name: &str, on: bool) -> ResultType<()> {
    let action = if on { "On" } else { "Off" };
    log::info!("niri: turning output {name} {action}");
    let reply = request(json!({ "Output": { "output": name, "action": action } }))?;
    if reply
        .get("OutputConfigChanged")
        .and_then(Value::as_str)
        .is_some_and(|s| s == "OutputWasMissing")
    {
        bail!("niri: output {name} does not exist");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct NiriWorkspace {
    pub id: u64,
    pub output: Option<String>,
    pub has_windows: bool,
}

pub fn workspaces() -> ResultType<Vec<NiriWorkspace>> {
    let reply = request(json!("Workspaces"))?;
    let list = reply
        .get("Workspaces")
        .and_then(Value::as_array)
        .ok_or_else(|| hbb_common::anyhow::anyhow!("unexpected Workspaces reply"))?;
    Ok(list
        .iter()
        .filter_map(|w| {
            Some(NiriWorkspace {
                id: w.get("id")?.as_u64()?,
                output: w
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                has_windows: w
                    .get("active_window_id")
                    .is_some_and(|v| !v.is_null()),
            })
        })
        .collect())
}

/// Move a workspace (addressed by its stable id) to `output`. The raw socket
/// accepts `{"Id": n}` references; the CLI's index-based `--reference` does not
/// apply here.
pub fn move_workspace_to_monitor(id: u64, output: &str) -> ResultType<()> {
    request(json!({ "Action": { "MoveWorkspaceToMonitor": {
        "reference": { "Id": id }, "output": output
    } } }))?;
    Ok(())
}

pub fn create_virtual_output(
    name: &str,
    width: u16,
    height: u16,
    refresh_hz: u32,
    scale: Option<f64>,
) -> ResultType<String> {
    let reply = request(json!({ "CreateVirtualOutput": {
        "name": name, "width": width, "height": height, "refresh_rate": refresh_hz, "scale": scale
    } }))?;
    Ok(reply
        .get("VirtualOutputCreated")
        .and_then(Value::as_str)
        .unwrap_or(name)
        .to_owned())
}

pub fn remove_virtual_output(name: &str) -> ResultType<()> {
    request(json!({ "RemoveVirtualOutput": { "name": name } }))?;
    Ok(())
}

/// Resolution list advertised for a virtual output, or `None` if `name` is not one.
pub fn resolutions_for(name: &str) -> Option<Vec<Resolution>> {
    let output = find_virtual(name)?;
    let (w, h, _) = output.mode?;
    let mut list = vec![(i32::from(w), i32::from(h))];
    for &(pw, ph) in PRESET_RESOLUTIONS {
        if pw <= MAX_DIMENSION && ph <= MAX_DIMENSION && !list.contains(&(pw, ph)) {
            list.push((pw, ph));
        }
    }
    Some(
        list.into_iter()
            .map(|(width, height)| Resolution {
                width,
                height,
                ..Default::default()
            })
            .collect(),
    )
}

pub fn current_resolution_for(name: &str) -> Option<Resolution> {
    let (w, h, _) = find_virtual(name)?.mode?;
    Some(Resolution {
        width: i32::from(w),
        height: i32::from(h),
        ..Default::default()
    })
}
