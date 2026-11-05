//! Is music playing? A PulseAudio client, which PipeWire also serves through
//! `pipewire-pulse`, that decides when the CAVA spectrum is worth starting.
//!
//! The client runs on its own thread with libpulse's own main loop and is
//! entirely event driven:
//!
//! - It subscribes to sink-input events (new, change, remove) and keeps a
//!   count of the playback streams that are not corked. Nothing else wakes it
//!   while that count is zero.
//! - Only while such a stream exists does it open a peak monitor on the
//!   default sink's monitor source (`PA_STREAM_PEAK_DETECT`, ten peaks a
//!   second delivered five times a second). The monitor does not keep a
//!   suspended sink awake.
//! - [`AudioGate`] turns those peaks into "playing": loud for
//!   [`START_AFTER`], and silent again after [`STOP_AFTER`]. An idle stream
//!   that is always present but nearly silent (a MIDI synthesizer waiting
//!   for notes) never crosses [`PEAK_FLOOR`].
//!
//! libpulse is loaded at run time with `dlopen`, so neither the build nor the
//! program needs it. Without it, or without a sound server, the caller sees
//! [`Heard::Silent`] and shows quotes instead; the client retries with a
//! growing back-off and never blocks the caller. Shared verbatim with
//! NIRUSCREENSAVER, like `cava.rs`.

use std::{
    ffi::{c_char, c_int, c_void},
    io::Write,
    os::unix::net::UnixStream,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// A peak above this (linear, full scale 1.0; about -54 dBFS) is sound.
pub const PEAK_FLOOR: f32 = 0.002;
/// How long the peak must stay above the floor before music counts as playing.
pub const START_AFTER: Duration = Duration::from_millis(300);
/// How long it must stay silent (or without a playing stream) before music
/// counts as stopped.
pub const STOP_AFTER: Duration = Duration::from_secs(5);
/// Peaks per second from the monitor, and how many arrive per wake-up.
const PEAK_RATE: u32 = 10;
const PEAKS_PER_READ: u32 = 2;

/// What the detector currently hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// No sound server, no stream that is not corked, or the detector is
    /// paused: certainly no music.
    Silent,
    /// A stream is playing but has not been loud for [`START_AFTER`] yet.
    Listening,
    /// Music: loud for [`START_AFTER`], and not yet silent for [`STOP_AFTER`].
    Playing,
}

impl Heard {
    fn from_u8(value: u8) -> Self {
        match value {
            2 => Self::Playing,
            1 => Self::Listening,
            _ => Self::Silent,
        }
    }
}

/// The start/stop decision, free of I/O so that it can be tested.
#[derive(Debug, Default)]
pub struct AudioGate {
    playing: bool,
    loud_since: Option<Instant>,
    quiet_since: Option<Instant>,
}

impl AudioGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Record one observation: whether the latest peak was above the floor.
    /// "No playing stream" is a quiet observation. Returns true when
    /// `playing` changed.
    pub fn observe(&mut self, loud: bool, now: Instant) -> bool {
        if loud {
            self.quiet_since = None;
            let since = *self.loud_since.get_or_insert(now);
            if !self.playing && now.saturating_duration_since(since) >= START_AFTER {
                self.playing = true;
                return true;
            }
        } else {
            self.loud_since = None;
            let since = *self.quiet_since.get_or_insert(now);
            if self.playing && now.saturating_duration_since(since) >= STOP_AFTER {
                self.playing = false;
                return true;
            }
        }
        false
    }

    /// Forget everything (the detector was paused): not playing.
    pub fn reset(&mut self) -> bool {
        let was = self.playing;
        *self = Self::default();
        was
    }

    /// When a decision falls due without further observations: the end of
    /// the silence window while playing. Peaks arrive on their own while a
    /// stream plays, so nothing else needs a timer.
    pub fn deadline(&self) -> Option<Instant> {
        if self.playing {
            self.quiet_since.map(|since| since + STOP_AFTER)
        } else {
            None
        }
    }
}

/// State shared between the caller and the client thread.
struct Shared {
    heard: AtomicU8,
    enabled: AtomicBool,
    quit: AtomicBool,
    /// The running `pa_mainloop`, for `pa_mainloop_wakeup`; 0 when none.
    mainloop: Mutex<usize>,
    wakeup: Mutex<Option<unsafe extern "C" fn(*mut c_void)>>,
}

/// Handle to the detector thread. Dropping it stops the thread.
pub struct Audio {
    shared: Arc<Shared>,
    thread: Option<thread::Thread>,
}

impl Audio {
    /// Start the detector. `wake` is written to (one byte, nonblocking)
    /// whenever [`Audio::heard`] changes.
    pub fn start(app: &str, wake: Option<UnixStream>) -> Self {
        let shared = Arc::new(Shared {
            // Undecided until the detector has looked.
            heard: AtomicU8::new(Heard::Listening as u8),
            enabled: AtomicBool::new(true),
            quit: AtomicBool::new(false),
            mainloop: Mutex::new(0),
            wakeup: Mutex::new(None),
        });
        let name = format!("{app}\0");
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("niru-audio".into())
            .spawn(move || client(&thread_shared, name, wake))
            .ok()
            .map(|handle| handle.thread().clone());
        Self { shared, thread }
    }

    pub fn heard(&self) -> Heard {
        Heard::from_u8(self.shared.heard.load(Ordering::Acquire))
    }

    /// Pause (display off) or resume the detector. Paused, it keeps only its
    /// idle subscription: no peak monitor runs and it reports silence.
    pub fn set_enabled(&self, enabled: bool) {
        if self.shared.enabled.swap(enabled, Ordering::AcqRel) != enabled {
            self.poke();
        }
    }

    fn poke(&self) {
        let wakeup = self.shared.wakeup.lock().ok().and_then(|w| *w);
        if let (Some(wakeup), Ok(mainloop)) = (wakeup, self.shared.mainloop.lock()) {
            if *mainloop != 0 {
                // SAFETY: the client thread clears `mainloop` under this lock
                // before it frees the main loop, and pa_mainloop_wakeup is
                // meant to be called from another thread.
                unsafe { wakeup(*mainloop as *mut c_void) };
            }
        }
        if let Some(thread) = self.thread.as_ref() {
            thread.unpark();
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        // Not joined: the thread frees its own resources and exits after
        // its current wait, and a lock that unlocks must never wait for it.
        self.shared.quit.store(true, Ordering::Release);
        self.poke();
    }
}

// ---- libpulse, loaded at run time -------------------------------------------

/// A NUL-terminated C string literal (`c"..."` needs Rust 1.77).
macro_rules! cstr {
    ($s:literal) => {
        concat!($s, "\0").as_ptr().cast::<c_char>()
    };
}

type Context = c_void;
type Stream = c_void;
type Mainloop = c_void;

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct ChannelMap {
    channels: u8,
    map: [c_int; 32],
}

#[repr(C)]
struct CVolume {
    channels: u8,
    values: [u32; 32],
}

/// `pa_sink_input_info` up to `corked`, the last field read here; the
/// struct is only ever read through a pointer libpulse hands out.
#[repr(C)]
struct SinkInputInfo {
    index: u32,
    name: *const c_char,
    owner_module: u32,
    client: u32,
    sink: u32,
    sample_spec: SampleSpec,
    channel_map: ChannelMap,
    volume: CVolume,
    buffer_usec: u64,
    sink_usec: u64,
    resample_method: *const c_char,
    driver: *const c_char,
    mute: c_int,
    proplist: *mut c_void,
    corked: c_int,
}

#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

const CONTEXT_NOAUTOSPAWN: u32 = 0x0001;
const CONTEXT_READY: c_int = 4;
const CONTEXT_FAILED: c_int = 5;
const CONTEXT_TERMINATED: c_int = 6;
const STREAM_READY: c_int = 2;
const STREAM_FAILED: c_int = 3;
const STREAM_TERMINATED: c_int = 4;
const STREAM_DONT_MOVE: u32 = 0x0200;
const STREAM_PEAK_DETECT: u32 = 0x0800;
const STREAM_ADJUST_LATENCY: u32 = 0x2000;
const STREAM_DONT_INHIBIT_AUTO_SUSPEND: u32 = 0x8000;
const SAMPLE_FLOAT32NE: c_int = if cfg!(target_endian = "little") { 5 } else { 6 };
const SUBSCRIPTION_MASK_SINK_INPUT: u32 = 0x0004;
const SUBSCRIPTION_FACILITY_MASK: u32 = 0x000f;
const SUBSCRIPTION_FACILITY_SINK_INPUT: u32 = 0x0002;

type NotifyCb = unsafe extern "C" fn(*mut c_void, *mut c_void);
type SubscribeCb = unsafe extern "C" fn(*mut Context, u32, u32, *mut c_void);
type SinkInputCb = unsafe extern "C" fn(*mut Context, *const SinkInputInfo, c_int, *mut c_void);
type RequestCb = unsafe extern "C" fn(*mut Stream, usize, *mut c_void);

/// The libpulse functions this client uses.
struct Pulse {
    _lib: *mut c_void,
    mainloop_new: unsafe extern "C" fn() -> *mut Mainloop,
    mainloop_free: unsafe extern "C" fn(*mut Mainloop),
    mainloop_get_api: unsafe extern "C" fn(*mut Mainloop) -> *mut c_void,
    mainloop_prepare: unsafe extern "C" fn(*mut Mainloop, c_int) -> c_int,
    mainloop_poll: unsafe extern "C" fn(*mut Mainloop) -> c_int,
    mainloop_dispatch: unsafe extern "C" fn(*mut Mainloop) -> c_int,
    mainloop_wakeup: unsafe extern "C" fn(*mut Mainloop),
    context_new: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut Context,
    context_unref: unsafe extern "C" fn(*mut Context),
    context_connect: unsafe extern "C" fn(*mut Context, *const c_char, u32, *const c_void) -> c_int,
    context_disconnect: unsafe extern "C" fn(*mut Context),
    context_get_state: unsafe extern "C" fn(*const Context) -> c_int,
    context_set_state_callback: unsafe extern "C" fn(*mut Context, Option<NotifyCb>, *mut c_void),
    context_set_subscribe_callback:
        unsafe extern "C" fn(*mut Context, Option<SubscribeCb>, *mut c_void),
    context_subscribe:
        unsafe extern "C" fn(*mut Context, u32, *const c_void, *mut c_void) -> *mut c_void,
    context_get_sink_input_info_list:
        unsafe extern "C" fn(*mut Context, Option<SinkInputCb>, *mut c_void) -> *mut c_void,
    operation_unref: unsafe extern "C" fn(*mut c_void),
    stream_new: unsafe extern "C" fn(
        *mut Context,
        *const c_char,
        *const SampleSpec,
        *const c_void,
    ) -> *mut Stream,
    stream_connect_record:
        unsafe extern "C" fn(*mut Stream, *const c_char, *const BufferAttr, u32) -> c_int,
    stream_set_read_callback: unsafe extern "C" fn(*mut Stream, Option<RequestCb>, *mut c_void),
    stream_set_state_callback: unsafe extern "C" fn(*mut Stream, Option<NotifyCb>, *mut c_void),
    stream_get_state: unsafe extern "C" fn(*const Stream) -> c_int,
    stream_peek: unsafe extern "C" fn(*mut Stream, *mut *const c_void, *mut usize) -> c_int,
    stream_drop: unsafe extern "C" fn(*mut Stream) -> c_int,
    stream_disconnect: unsafe extern "C" fn(*mut Stream) -> c_int,
    stream_unref: unsafe extern "C" fn(*mut Stream),
}

/// A symbol's address as the function pointer type of the field it fills.
///
/// # Safety
/// `T` must be the `extern "C" fn` type of the symbol's C prototype.
unsafe fn function<T: Copy>(symbol: *mut c_void) -> T {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    std::mem::transmute_copy(&symbol)
}

impl Pulse {
    fn open() -> Option<Self> {
        // SAFETY: dlopen/dlsym with NUL-terminated names; every symbol is
        // cast to the prototype of libpulse's public, stable C API.
        unsafe {
            let lib = libc::dlopen(cstr!("libpulse.so.0"), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if lib.is_null() {
                return None;
            }
            macro_rules! sym {
                ($name:literal) => {{
                    let symbol = libc::dlsym(lib, concat!($name, "\0").as_ptr().cast());
                    if symbol.is_null() {
                        libc::dlclose(lib);
                        return None;
                    }
                    function(symbol)
                }};
            }
            Some(Self {
                mainloop_new: sym!("pa_mainloop_new"),
                mainloop_free: sym!("pa_mainloop_free"),
                mainloop_get_api: sym!("pa_mainloop_get_api"),
                mainloop_prepare: sym!("pa_mainloop_prepare"),
                mainloop_poll: sym!("pa_mainloop_poll"),
                mainloop_dispatch: sym!("pa_mainloop_dispatch"),
                mainloop_wakeup: sym!("pa_mainloop_wakeup"),
                context_new: sym!("pa_context_new"),
                context_unref: sym!("pa_context_unref"),
                context_connect: sym!("pa_context_connect"),
                context_disconnect: sym!("pa_context_disconnect"),
                context_get_state: sym!("pa_context_get_state"),
                context_set_state_callback: sym!("pa_context_set_state_callback"),
                context_set_subscribe_callback: sym!("pa_context_set_subscribe_callback"),
                context_subscribe: sym!("pa_context_subscribe"),
                context_get_sink_input_info_list: sym!("pa_context_get_sink_input_info_list"),
                operation_unref: sym!("pa_operation_unref"),
                stream_new: sym!("pa_stream_new"),
                stream_connect_record: sym!("pa_stream_connect_record"),
                stream_set_read_callback: sym!("pa_stream_set_read_callback"),
                stream_set_state_callback: sym!("pa_stream_set_state_callback"),
                stream_get_state: sym!("pa_stream_get_state"),
                stream_peek: sym!("pa_stream_peek"),
                stream_drop: sym!("pa_stream_drop"),
                stream_disconnect: sym!("pa_stream_disconnect"),
                stream_unref: sym!("pa_stream_unref"),
                _lib: lib,
            })
        }
    }
}

/// One connection's state, owned by the client thread. Callbacks receive a
/// pointer to it and run only inside `pa_mainloop_dispatch` on that thread.
struct Session<'a> {
    pa: &'a Pulse,
    context: *mut Context,
    stream: *mut Stream,
    /// Streams that are not corked, from the last complete list.
    playing_streams: usize,
    /// Count in progress while a list query runs.
    counting: usize,
    querying: bool,
    /// A complete stream list has arrived since connecting.
    listed: bool,
    /// A sink-input event arrived while a query was running.
    dirty: bool,
    ready: bool,
    failed: bool,
    stream_failed_at: Option<Instant>,
    /// Loudest peak since the last observation, if any arrived.
    peak: Option<f32>,
}

unsafe extern "C" fn on_context_state(context: *mut c_void, data: *mut c_void) {
    let session = &mut *(data as *mut Session);
    match (session.pa.context_get_state)(context) {
        CONTEXT_READY => session.ready = true,
        CONTEXT_FAILED | CONTEXT_TERMINATED => session.failed = true,
        _ => {}
    }
}

unsafe extern "C" fn on_subscribe(_: *mut Context, event: u32, _: u32, data: *mut c_void) {
    if event & SUBSCRIPTION_FACILITY_MASK == SUBSCRIPTION_FACILITY_SINK_INPUT {
        (*(data as *mut Session)).dirty = true;
    }
}

unsafe extern "C" fn on_sink_input(
    _: *mut Context,
    info: *const SinkInputInfo,
    eol: c_int,
    data: *mut c_void,
) {
    let session = &mut *(data as *mut Session);
    if eol != 0 {
        // eol < 0 is an error; keep the last good count then.
        if eol > 0 {
            session.playing_streams = session.counting;
            session.listed = true;
        }
        session.counting = 0;
        session.querying = false;
        return;
    }
    if !info.is_null() && (*info).corked == 0 {
        session.counting += 1;
    }
}

unsafe extern "C" fn on_stream_state(stream: *mut c_void, data: *mut c_void) {
    let session = &mut *(data as *mut Session);
    if matches!(
        (session.pa.stream_get_state)(stream),
        STREAM_FAILED | STREAM_TERMINATED
    ) && stream == session.stream
    {
        session.stream_failed_at = Some(Instant::now());
    }
}

unsafe extern "C" fn on_read(stream: *mut Stream, _: usize, data: *mut c_void) {
    let session = &mut *(data as *mut Session);
    loop {
        let mut bytes: *const c_void = ptr::null();
        let mut length = 0usize;
        if (session.pa.stream_peek)(stream, &mut bytes, &mut length) < 0 || length == 0 {
            return;
        }
        if !bytes.is_null() {
            let samples = std::slice::from_raw_parts(bytes.cast::<u8>(), length);
            let peak = peak_of(samples);
            session.peak = Some(session.peak.map_or(peak, |p| p.max(peak)));
        }
        // A hole (null data) is dropped like data.
        (session.pa.stream_drop)(stream);
    }
}

/// The largest absolute value among native-endian f32 samples.
fn peak_of(bytes: &[u8]) -> f32 {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]).abs())
        .filter(|v| v.is_finite())
        .fold(0.0, f32::max)
}

impl Session<'_> {
    unsafe fn query(&mut self) {
        if self.querying || !self.ready {
            self.dirty |= self.querying;
            return;
        }
        self.dirty = false;
        self.querying = true;
        self.counting = 0;
        let data = self as *mut Session as *mut c_void;
        let op =
            (self.pa.context_get_sink_input_info_list)(self.context, Some(on_sink_input), data);
        if op.is_null() {
            self.querying = false;
        } else {
            (self.pa.operation_unref)(op);
        }
    }

    unsafe fn open_monitor(&mut self) {
        let spec = SampleSpec {
            format: SAMPLE_FLOAT32NE,
            rate: PEAK_RATE,
            channels: 1,
        };
        let stream = (self.pa.stream_new)(self.context, cstr!("peak"), &spec, ptr::null());
        if stream.is_null() {
            self.stream_failed_at = Some(Instant::now());
            return;
        }
        let data = self as *mut Session as *mut c_void;
        (self.pa.stream_set_state_callback)(stream, Some(on_stream_state), data);
        (self.pa.stream_set_read_callback)(stream, Some(on_read), data);
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 4 * PEAKS_PER_READ,
        };
        let flags = STREAM_PEAK_DETECT
            | STREAM_ADJUST_LATENCY
            | STREAM_DONT_MOVE
            | STREAM_DONT_INHIBIT_AUTO_SUSPEND;
        self.stream = stream;
        if (self.pa.stream_connect_record)(stream, cstr!("@DEFAULT_MONITOR@"), &attr, flags) < 0 {
            self.close_monitor();
            self.stream_failed_at = Some(Instant::now());
        }
    }

    unsafe fn close_monitor(&mut self) {
        if self.stream.is_null() {
            return;
        }
        let stream = std::mem::replace(&mut self.stream, ptr::null_mut());
        (self.pa.stream_set_state_callback)(stream, None, ptr::null_mut());
        (self.pa.stream_set_read_callback)(stream, None, ptr::null_mut());
        if (self.pa.stream_get_state)(stream) == STREAM_READY {
            (self.pa.stream_disconnect)(stream);
        }
        (self.pa.stream_unref)(stream);
        self.peak = None;
    }
}

/// How long a failed peak monitor waits before it is opened again.
const MONITOR_RETRY: Duration = Duration::from_secs(10);
/// Reconnection back-off after the sound server went away or never came.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

fn publish(shared: &Shared, wake: &mut Option<UnixStream>, heard: Heard) {
    let value = heard as u8;
    if shared.heard.swap(value, Ordering::AcqRel) != value {
        if let Some(wake) = wake.as_mut() {
            let _ = wake.write(&[1]);
        }
    }
}

fn client(shared: &Shared, name: String, mut wake: Option<UnixStream>) {
    let Some(pa) = Pulse::open() else {
        publish(shared, &mut wake, Heard::Silent);
        return;
    };
    if let Ok(mut wakeup) = shared.wakeup.lock() {
        *wakeup = Some(pa.mainloop_wakeup);
    }
    let mut retry = RETRY_MIN;
    while !shared.quit.load(Ordering::Acquire) {
        let connected_at = Instant::now();
        // SAFETY: one connection at a time, all on this thread.
        unsafe { connection(&pa, shared, &name, &mut wake) };
        publish(shared, &mut wake, Heard::Silent);
        if shared.quit.load(Ordering::Acquire) {
            break;
        }
        // A connection that lasted resets the back-off.
        if connected_at.elapsed() > RETRY_MAX {
            retry = RETRY_MIN;
        }
        thread::park_timeout(retry);
        retry = (retry * 2).min(RETRY_MAX);
    }
}

unsafe fn connection(pa: &Pulse, shared: &Shared, name: &str, wake: &mut Option<UnixStream>) {
    let mainloop = (pa.mainloop_new)();
    if mainloop.is_null() {
        return;
    }
    let api = (pa.mainloop_get_api)(mainloop);
    let context = (pa.context_new)(api, name.as_ptr().cast());
    if context.is_null() {
        (pa.mainloop_free)(mainloop);
        return;
    }
    if let Ok(mut guard) = shared.mainloop.lock() {
        *guard = mainloop as usize;
    }
    let mut session = Box::new(Session {
        pa,
        context,
        stream: ptr::null_mut(),
        playing_streams: 0,
        counting: 0,
        querying: false,
        listed: false,
        dirty: false,
        ready: false,
        failed: false,
        stream_failed_at: None,
        peak: None,
    });
    let data = &mut *session as *mut Session as *mut c_void;
    (pa.context_set_state_callback)(context, Some(on_context_state), data);
    (pa.context_set_subscribe_callback)(context, Some(on_subscribe), data);
    let mut gate = AudioGate::new();
    let mut subscribed = false;
    if (pa.context_connect)(context, ptr::null(), CONTEXT_NOAUTOSPAWN, ptr::null()) >= 0 {
        loop {
            if shared.quit.load(Ordering::Acquire) || session.failed {
                break;
            }
            let now = Instant::now();
            if session.ready && !subscribed {
                subscribed = true;
                let op = (pa.context_subscribe)(
                    context,
                    SUBSCRIPTION_MASK_SINK_INPUT,
                    ptr::null(),
                    ptr::null_mut(),
                );
                if !op.is_null() {
                    (pa.operation_unref)(op);
                }
                session.dirty = true;
            }
            if session.dirty {
                session.query();
            }
            let enabled = shared.enabled.load(Ordering::Acquire);
            let want_monitor = enabled && session.playing_streams > 0;
            if !session.stream.is_null() && (!want_monitor || session.stream_failed_at.is_some()) {
                session.close_monitor();
            }
            let retry_due = session.stream_failed_at.map_or(true, |at| {
                now.saturating_duration_since(at) >= MONITOR_RETRY
            });
            if want_monitor && session.stream.is_null() && retry_due {
                session.stream_failed_at = None;
                session.open_monitor();
            }
            let heard = if !enabled {
                gate.reset();
                Heard::Silent
            } else {
                match session.peak.take() {
                    Some(peak) => {
                        gate.observe(peak > PEAK_FLOOR, now);
                    }
                    None if session.stream.is_null() => {
                        gate.observe(false, now);
                    }
                    // The monitor is open but nothing arrived this time.
                    None => {}
                }
                if gate.playing() {
                    Heard::Playing
                } else if session.playing_streams > 0 || !session.listed {
                    Heard::Listening
                } else {
                    Heard::Silent
                }
            };
            publish(shared, wake, heard);
            // Sleep until libpulse has work, the gate's silence window ends,
            // the monitor may be retried, or the caller pokes the loop.
            let mut deadline = gate.deadline();
            if want_monitor && session.stream.is_null() {
                if let Some(at) = session.stream_failed_at {
                    let retry = at + MONITOR_RETRY;
                    deadline = Some(deadline.map_or(retry, |d| d.min(retry)));
                }
            }
            let timeout = deadline.map_or(-1, |d| {
                d.saturating_duration_since(Instant::now())
                    .as_micros()
                    .clamp(1000, i32::MAX as u128) as c_int
            });
            if (pa.mainloop_prepare)(mainloop, timeout) < 0
                || (pa.mainloop_poll)(mainloop) < 0
                || (pa.mainloop_dispatch)(mainloop) < 0
            {
                break;
            }
        }
    }
    session.close_monitor();
    (pa.context_set_state_callback)(context, None, ptr::null_mut());
    (pa.context_set_subscribe_callback)(context, None, ptr::null_mut());
    (pa.context_disconnect)(context);
    (pa.context_unref)(context);
    if let Ok(mut guard) = shared.mainloop.lock() {
        *guard = 0;
    }
    (pa.mainloop_free)(mainloop);
    drop(session);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loud_stream_plays_after_the_start_window() {
        let t = Instant::now();
        let mut gate = AudioGate::new();
        assert!(!gate.observe(true, t));
        assert!(!gate.observe(true, t + Duration::from_millis(200)));
        assert!(!gate.playing());
        assert!(gate.observe(true, t + START_AFTER));
        assert!(gate.playing());
    }

    #[test]
    fn a_short_blip_does_not_start_the_spectrum() {
        let t = Instant::now();
        let mut gate = AudioGate::new();
        gate.observe(true, t);
        gate.observe(true, t + Duration::from_millis(200));
        gate.observe(false, t + Duration::from_millis(250));
        // The window starts again from the next loud peak.
        gate.observe(true, t + Duration::from_millis(400));
        assert!(!gate.observe(true, t + Duration::from_millis(600)));
        assert!(!gate.playing());
        assert!(gate.observe(true, t + Duration::from_millis(700)));
    }

    #[test]
    fn a_nearly_silent_stream_never_plays() {
        // An idle synthesizer: peaks far below the floor, forever.
        let t = Instant::now();
        let mut gate = AudioGate::new();
        for n in 0..10_000u64 {
            let peak = 0.0003 * ((n % 7) as f32 / 7.0);
            gate.observe(peak > PEAK_FLOOR, t + Duration::from_millis(200 * n));
            assert!(!gate.playing());
        }
        assert_eq!(gate.deadline(), None);
    }

    #[test]
    fn silence_stops_after_the_stop_window_and_pauses_keep_playing() {
        let t = Instant::now();
        let mut gate = AudioGate::new();
        gate.observe(true, t);
        gate.observe(true, t + START_AFTER);
        assert!(gate.playing());
        // A two-second gap between tracks keeps the spectrum.
        let quiet = t + Duration::from_secs(1);
        gate.observe(false, quiet);
        assert_eq!(gate.deadline(), Some(quiet + STOP_AFTER));
        assert!(!gate.observe(false, quiet + Duration::from_secs(2)));
        gate.observe(true, quiet + Duration::from_millis(2100));
        assert!(gate.playing());
        assert_eq!(gate.deadline(), None);
        // Then real silence.
        let stop = quiet + Duration::from_secs(3);
        gate.observe(false, stop);
        assert!(!gate.observe(false, stop + Duration::from_millis(4900)));
        assert!(gate.observe(false, stop + STOP_AFTER));
        assert!(!gate.playing());
        assert_eq!(gate.deadline(), None);
    }

    #[test]
    fn a_reset_forgets_everything() {
        let t = Instant::now();
        let mut gate = AudioGate::new();
        gate.observe(true, t);
        gate.observe(true, t + START_AFTER);
        assert!(gate.reset());
        assert!(!gate.playing());
        assert!(!gate.reset());
        // A fresh start window afterwards.
        assert!(!gate.observe(true, t + Duration::from_secs(1)));
    }

    #[test]
    fn peaks_are_read_as_native_floats() {
        let mut bytes = Vec::new();
        for v in [0.1f32, -0.5, 0.25, f32::NAN] {
            bytes.extend_from_slice(&v.to_ne_bytes());
        }
        assert_eq!(peak_of(&bytes), 0.5);
        assert_eq!(peak_of(&[]), 0.0);
        assert_eq!(peak_of(&[1, 2, 3]), 0.0);
    }

    /// The `corked` field must sit where libpulse puts it.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn sink_input_info_layout_matches_libpulse() {
        let info = std::mem::MaybeUninit::<SinkInputInfo>::uninit();
        let base = info.as_ptr() as usize;
        // SAFETY: only field addresses are taken, nothing is read.
        let corked = unsafe { ptr::addr_of!((*info.as_ptr()).corked) } as usize;
        let volume = unsafe { ptr::addr_of!((*info.as_ptr()).volume) } as usize;
        assert_eq!(volume - base, 172);
        assert_eq!(corked - base, 352);
    }
}
