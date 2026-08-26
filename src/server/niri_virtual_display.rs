//! Linux virtual displays backed by niri's compositor-owned virtual outputs.
//!
//! The DRM capture backend enumerates physical connectors; a niri virtual output has
//! no connector, so it is appended to that list here and captured through
//! wlr-screencopy instead of the `_drm` stream. Display indices therefore run
//! `[DRM outputs…, virtual outputs…]`, and every consumer that indexes the list by
//! display index (capturer construction, resolution changes) goes through the same
//! `drm_capturer::get_display_infos()` snapshot.

use crate::platform::niri_ipc;
use hbb_common::message_proto::{DisplayInfo, Resolution};
use hbb_common::{log, ResultType};

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
                // screencopy is requested with the cursor overlaid.
                cursor_embedded: true,
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
    let all = super::drm_capturer::get_display_infos()?;
    let info = all.get(display_idx)?;
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
    let capturer =
        match scrap::wayland::screencopy::ScreencopyCapturer::new(&info.name, width, height) {
            Ok(c) => c,
            Err(e) => return Some(Err(e.into())),
        };
    Some(Ok(super::video_service::CapturerInfo {
        origin: (info.x, info.y),
        width,
        height,
        ndisplay: all.len(),
        current: display_idx,
        privacy_mode_id: 0,
        _capturer_privacy_mode_id: 0,
        capturer: Box::new(capturer),
    }))
}
