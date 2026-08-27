//! `wlr-screencopy` capturer for compositor-owned outputs that have no DRM scanout.
//!
//! niri's virtual outputs are rendered on demand for screencopy clients: on a
//! `capture_output` request the compositor renders the output with its GPU renderer,
//! copies it into our `wl_shm` buffer and signals `ready`. There is no PipeWire,
//! portal or DMA-BUF negotiation involved, which is what makes this path work on
//! NVIDIA where the PipeWire route did not.
//!
//! Capture is pipelined: [`TraitCapturer::frame`] returns the frame whose copy the
//! previous call pre-requested and immediately pre-requests the next one into a
//! second buffer, so the compositor renders and copies frame N+1 while the caller
//! converts and encodes frame N. At 5K the request-to-ready time is ~9 ms, which a
//! serial request would add to every encode cycle.

use std::io;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::wl_output::WlOutput;
use sctk::reexports::client::protocol::wl_shm;
use sctk::reexports::client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use sctk::reexports::protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use sctk::reexports::protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::shm::slot::{Buffer, SlotPool};
use sctk::shm::{Shm, ShmHandler};
use sctk::{delegate_output, delegate_registry, delegate_shm, registry_handlers};

use crate::PixelBuffer;
use crate::{Frame, Pixfmt, TraitCapturer};

#[derive(Default)]
struct Pending {
    params: Option<(u32, u32, u32, wl_shm::Format)>,
    y_invert: bool,
    done: Option<Result<(), String>>,
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pending: Pending,
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(State);
delegate_output!(State);
delegate_shm!(State);

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as sctk::reexports::client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                if let WEnum::Value(format) = format {
                    // Prefer the first shm format offered; niri offers exactly one.
                    if state.pending.params.is_none() {
                        state.pending.params = Some((width, height, stride, format));
                    }
                }
            }
            Event::Flags { flags } => {
                if let WEnum::Value(flags) = flags {
                    state.pending.y_invert =
                        flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert);
                }
            }
            Event::Ready { .. } => state.pending.done = Some(Ok(())),
            Event::Failed => {
                state.pending.done = Some(Err("compositor reported screencopy failure".into()))
            }
            _ => {}
        }
    }
}

/// Captures one named output through `zwlr_screencopy_manager_v1`.
pub struct ScreencopyCapturer {
    _conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    manager: ZwlrScreencopyManagerV1,
    output: WlOutput,
    name: String,
    /// One single-buffer pool per slot: niri validates a screencopy shm buffer by
    /// requiring the whole pool length to equal exactly one frame (stride x height),
    /// so the two buffers cannot share a pool.
    pools: [SlotPool; 2],
    /// Two buffers, used alternately: while the caller holds one frame's pixels, the
    /// pre-requested next copy lands in the other buffer.
    buffers: [Option<(Buffer, (u32, u32, u32, wl_shm::Format))>; 2],
    /// The pre-requested capture, if one is pending: its frame proxy and the index of
    /// the buffer its copy writes into.
    in_flight: Option<(ZwlrScreencopyFrameV1, usize)>,
    next_idx: usize,
    /// Geometry this capturer was built for; a mismatch means the output was resized and
    /// the video service must rebuild with the new size.
    session_size: (u32, u32),
    /// wlr-screencopy `overlay_cursor` flag: 1 composites the cursor into the
    /// frames, 0 leaves it out (the client then draws its own pointer).
    overlay_cursor: i32,
    flipped: Vec<u8>,
}

fn other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

impl ScreencopyCapturer {
    /// `width`/`height` are the physical size the display was advertised with.
    pub fn new(
        output_name: &str,
        width: usize,
        height: usize,
        overlay_cursor: bool,
    ) -> io::Result<Self> {
        let conn = Connection::connect_to_env().map_err(other)?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(other)?;
        let qh = queue.handle();
        let shm = Shm::bind(&globals, &qh).map_err(other)?;
        let manager: ZwlrScreencopyManagerV1 = globals
            .bind(&qh, 1..=3, ())
            .map_err(|e| other(format!("compositor offers no wlr-screencopy: {e}")))?;
        let mut state = State {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            shm,
            pending: Pending::default(),
        };
        // Two roundtrips: one for the wl_output globals, one for their name events.
        queue.roundtrip(&mut state).map_err(other)?;
        queue.roundtrip(&mut state).map_err(other)?;

        let output = state
            .output_state
            .outputs()
            .find(|o| {
                state
                    .output_state
                    .info(o)
                    .and_then(|i| i.name)
                    .is_some_and(|n| n == output_name)
            })
            .ok_or_else(|| other(format!("no wl_output named {output_name:?}")))?;

        let pool_len = (width * height * 4).max(4096);
        let pools = [
            SlotPool::new(pool_len, &state.shm).map_err(other)?,
            SlotPool::new(pool_len, &state.shm).map_err(other)?,
        ];
        Ok(Self {
            _conn: conn,
            queue,
            qh,
            state,
            manager,
            output,
            name: output_name.to_owned(),
            pools,
            buffers: [None, None],
            in_flight: None,
            next_idx: 0,
            session_size: (width as u32, height as u32),
            overlay_cursor: overlay_cursor as i32,
            flipped: Vec::new(),
        })
    }

    /// Issue a capture request into the next buffer: wait for the buffer params,
    /// reject a resized output, (re)create the target buffer if needed and send
    /// `copy`. The compositor then renders and fills the buffer asynchronously;
    /// completion is picked up by a later wait on `is_done`.
    fn start_capture(&mut self, deadline: Instant) -> io::Result<()> {
        self.state.pending = Pending::default();
        let frame = self
            .manager
            .capture_output(self.overlay_cursor, &self.output, &self.qh, ());
        if let Err(e) = self.wait_until(deadline, has_params) {
            frame.destroy();
            return Err(e);
        }
        let params = self.state.pending.params.expect("params present");
        let (width, height, stride, format) = params;

        // A resized output advertises its new size here, before any copy. Reject it now
        // (destroying the frame) so the video service rebuilds at the new geometry; copying
        // into a stale-sized buffer would draw a wlr protocol error and kill the connection.
        if (width, height) != self.session_size {
            frame.destroy();
            return Err(other(format!(
                "screencopy: output {} changed geometry ({}x{} -> {}x{}); rebuilding",
                self.name, self.session_size.0, self.session_size.1, width, height
            )));
        }

        let idx = self.next_idx;
        if self.buffers[idx].as_ref().map(|(_, p)| *p) != Some(params) {
            let (buffer, _) = self.pools[idx]
                .create_buffer(width as i32, height as i32, stride as i32, format)
                .map_err(other)?;
            self.buffers[idx] = Some((buffer, params));
        }
        let (buffer, _) = self.buffers[idx].as_ref().expect("buffer present");
        frame.copy(buffer.wl_buffer());
        // Flush now: the whole point of the pre-request is that the compositor
        // renders during the caller's convert+encode window, and without a flush
        // the copy request would sit in the client send buffer until the next
        // dispatch.
        if let Err(e) = self.queue.flush() {
            frame.destroy();
            return Err(other(e));
        }
        self.in_flight = Some((frame, idx));
        self.next_idx = 1 - idx;
        Ok(())
    }

    /// Dispatch events until `ready(&state)` or the deadline passes (`WouldBlock`).
    fn wait_until(&mut self, deadline: Instant, ready: fn(&State) -> bool) -> io::Result<()> {
        loop {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(other)?;
            if ready(&self.state) {
                return Ok(());
            }
            self.queue.flush().map_err(other)?;
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe {
                libc::poll(
                    &mut pfd,
                    1,
                    remaining.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if n == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            guard.read().map_err(other)?;
        }
    }
}

fn has_params(s: &State) -> bool {
    s.pending.params.is_some()
}

fn is_done(s: &State) -> bool {
    s.pending.done.is_some()
}

impl TraitCapturer for ScreencopyCapturer {
    fn frame<'a>(&'a mut self, timeout: Duration) -> io::Result<Frame<'a>> {
        let deadline = Instant::now() + timeout;

        // First call (or recovery after an aborted request): capture serially.
        if self.in_flight.is_none() {
            self.start_capture(deadline)?;
        }

        // Wait for the pending copy. A timeout leaves it pending so the next call
        // resumes the same wait (WouldBlock is the video loop's idle path); any
        // other failure aborts it so the next call starts clean.
        if let Err(e) = self.wait_until(deadline, is_done) {
            if e.kind() != io::ErrorKind::WouldBlock {
                if let Some((frame, _)) = self.in_flight.take() {
                    frame.destroy();
                }
            }
            return Err(e);
        }
        let (frame, idx) = self.in_flight.take().expect("in-flight present");
        frame.destroy();
        let done = self.state.pending.done.take();
        let y_invert = self.state.pending.y_invert;
        if let Some(Err(msg)) = done {
            return Err(other(format!("screencopy of {}: {msg}", self.name)));
        }
        let (_, (width, height, stride, format)) =
            self.buffers[idx].as_ref().expect("buffer present");
        let (width, height, stride, format) = (*width, *height, *stride, *format);

        // Pre-request the next frame into the other buffer. A geometry change must
        // surface so the video service rebuilds at the new size; any other failure
        // just degrades the next call to a serial capture.
        match self.start_capture(deadline) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }

        let pixfmt = match format {
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Pixfmt::BGRA,
            wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Pixfmt::RGBA,
            f => return Err(other(format!("unsupported screencopy shm format {f:?}"))),
        };
        let (buffer, _) = self.buffers[idx].as_ref().expect("buffer present");
        let canvas = self.pools[idx]
            .canvas(buffer)
            .ok_or_else(|| other("screencopy buffer is not mapped"))?;
        let len = (stride * height) as usize;
        let data: &[u8] = &canvas[..len];
        let (w, h) = (width as usize, height as usize);
        if y_invert {
            self.flipped.clear();
            self.flipped.reserve(len);
            for row in data.chunks_exact(stride as usize).rev() {
                self.flipped.extend_from_slice(row);
            }
            return Ok(Frame::PixelBuffer(PixelBuffer::new(
                &self.flipped,
                pixfmt,
                w,
                h,
            )));
        }
        Ok(Frame::PixelBuffer(PixelBuffer::new(data, pixfmt, w, h)))
    }
}
