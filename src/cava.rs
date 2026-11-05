//! Optional CAVA spectrum for the idle and lock screens.
//!
//! CAVA is started as a child process with a temporary config that emits raw
//! 16-bit mono bars on stdout at a fixed sensitivity; the level normalization
//! CAVA's `autosens` would do happens here, with a noise floor (see [`Gain`]). The rendering is done here, in the same 24/32-bit
//! pixel format as the text renderer, so the bars follow the active NIRUX theme
//! without requiring a compositor or a second window.
//!
//! Power: the spectrum pauses whenever it cannot be seen or has nothing to
//! show. With the display off (DPMS not `On`) the CAVA child is stopped and
//! reaped and one static frame is drawn; it restarts as soon as the display
//! is on again. While the display is on but every bar has been silent for
//! two seconds, the 30 fps repaint stops after one flat frame, and the reader
//! thread wakes the caller on the first non-silent sample. CAVA's own
//! `sleep_timer` stops its FFT during the same silence. The DPMS watch and
//! the [`PowerGate`] state machine below are self-contained so that
//! NIRUSCREENSAVER can copy this file verbatim.

use crate::{core::Surface, palette::Palette};
use std::{
    fs,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use x11rb::{
    connection::{Connection, RequestConnection},
    protocol::{
        dpms::{self, ConnectionExt as _},
        xproto::{ConnectionExt, ImageFormat, ImageOrder},
        Event,
    },
};

pub const BARS: usize = 48;

const CAVA_CONFIG: &str = "\
[general]\n\
bars = 48\n\
framerate = 30\n\
autosens = 0\n\
sensitivity = 100\n\
sleep_timer = 2\n\
\n\
[output]\n\
method = raw\n\
raw_target = /dev/stdout\n\
data_format = binary\n\
bit_format = 16bit\n\
channels = mono\n\
\n\
[smoothing]\n\
noise_reduction = 77\n";

/// A running CAVA child and the latest complete frame it produced.
struct Visualizer {
    child: Child,
    bars: Arc<Mutex<Vec<u16>>>,
    stop: Arc<AtomicBool>,
    /// Set while the caller has stopped repainting for silence; the reader
    /// then signals `wake` on the first non-silent frame.
    quiet: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl Visualizer {
    fn start(wake: Option<UnixStream>) -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "niru-cava-{}-{}-{stamp}.conf",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, CAVA_CONFIG).ok()?;
        let mut command = Command::new("cava");
        command
            .arg("-p")
            .arg(&path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Stop CAVA if this process dies without dropping the visualizer
        // (killed, or `process::exit`). With `sleep_timer` a silent CAVA no
        // longer writes, so it would not die of SIGPIPE and could linger.
        // SAFETY: prctl is async-signal-safe and touches no Rust state.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                let _ = fs::remove_file(&path);
                return None;
            }
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&path);
            return None;
        };
        let bars = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let quiet = Arc::new(AtomicBool::new(false));
        let thread_bars = Arc::clone(&bars);
        let thread_stop = Arc::clone(&stop);
        let thread_quiet = Arc::clone(&quiet);
        let mut wake = wake;
        let config = path.clone();
        let reader = thread::Builder::new()
            .name("niru-cava-reader".into())
            .spawn(move || {
                let mut bytes = [0u8; BARS * 2];
                let mut frame = [0u16; BARS];
                let mut config = Some(config);
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if stdout.read_exact(&mut bytes).is_err() {
                        break;
                    }
                    // CAVA has read its config once it produces a frame; remove
                    // it now, so not even a killed lock leaves it behind.
                    if let Some(config) = config.take() {
                        let _ = fs::remove_file(config);
                    }
                    // CAVA writes 16-bit values in native byte order.
                    for (value, pair) in frame.iter_mut().zip(bytes.chunks_exact(2)) {
                        *value = u16::from_ne_bytes([pair[0], pair[1]]);
                    }
                    if let Ok(mut guard) = thread_bars.lock() {
                        guard.clear();
                        guard.extend_from_slice(&frame);
                    }
                    if !silent_frame(&frame) && thread_quiet.swap(false, Ordering::AcqRel) {
                        if let Some(wake) = wake.as_mut() {
                            let _ = wake.write(&[1]);
                        }
                    }
                }
            })
            .ok();
        if reader.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&path);
            return None;
        }
        Some(Self {
            child,
            bars,
            stop,
            quiet,
            reader,
            path,
        })
    }

    fn is_running(&mut self) -> bool {
        !self.stop.load(Ordering::Relaxed) && matches!(self.child.try_wait(), Ok(None))
    }

    fn bars(&self) -> Vec<u16> {
        self.bars
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// CAVA plus a short attack/release filter, so pauses and beats do not make the
/// bars jump between adjacent frames.
pub struct Cava {
    visualizer: Option<Visualizer>,
    spectrum: Spectrum,
    last: Instant,
    next_retry: Instant,
    gate: PowerGate,
    /// Read end of the reader's silence wake-up; the write end is cloned
    /// into each reader thread.
    wake: Option<(UnixStream, UnixStream)>,
    trace: Option<Trace>,
}

impl Cava {
    pub fn new() -> Self {
        let now = Instant::now();
        let wake = UnixStream::pair().ok().and_then(|(read, write)| {
            read.set_nonblocking(true).ok()?;
            write.set_nonblocking(true).ok()?;
            Some((read, write))
        });
        let visualizer = Visualizer::start(Self::writer(&wake));
        Self {
            visualizer,
            spectrum: Spectrum::new(),
            last: now,
            next_retry: now + Duration::from_secs(2),
            gate: PowerGate::new(),
            wake,
            trace: Trace::from_env(),
        }
    }

    fn writer(wake: &Option<(UnixStream, UnixStream)>) -> Option<UnixStream> {
        wake.as_ref().and_then(|(_, write)| write.try_clone().ok())
    }

    /// True while the spectrum should be repainted at 30 fps: a CAVA child
    /// is believed to be running, the display is on and there is sound.
    pub fn animating(&self) -> bool {
        self.visualizer.is_some() && self.gate.activity() == Activity::Live
    }

    /// The descriptor to wait on while silent: readable on the first
    /// non-silent sample.
    pub fn wake_fd(&self) -> Option<RawFd> {
        if self.gate.activity() != Activity::Silent {
            return None;
        }
        self.wake.as_ref().map(|(read, _)| read.as_raw_fd())
    }

    /// Consume a wake-up from the reader and resume the 30 fps repaint.
    pub fn woke(&mut self) {
        if let Some((read, _)) = self.wake.as_mut() {
            let mut buf = [0u8; 16];
            while matches!(read.read(&mut buf), Ok(n) if n > 0) {}
        }
        self.gate.sample(false, Instant::now());
    }

    /// Follow the display power state. Off stops and reaps the CAVA child;
    /// on starts it again immediately.
    pub fn set_display(&mut self, on: bool) {
        if !self.gate.display(on) {
            return;
        }
        if on {
            self.last = Instant::now();
            if self.visualizer.is_none() {
                self.visualizer = Visualizer::start(Self::writer(&self.wake));
                self.next_retry = Instant::now() + Duration::from_secs(2);
            }
        } else {
            // Drop kills and reaps the child and joins the reader.
            self.visualizer = None;
        }
        self.spectrum.flat();
    }

    /// Latest smoothed bar heights in the range 0.0..=1.0. Flat while paused.
    pub fn values(&mut self) -> &[f32] {
        let now = Instant::now();
        if self.gate.activity() == Activity::DisplayOff {
            self.last = now;
            self.spectrum.flat();
            return self.traced(now, 'O');
        }
        self.maintain(now);
        let raw = self
            .visualizer
            .as_ref()
            .map(Visualizer::bars)
            .unwrap_or_default();
        if self.gate.sample(silent_frame(&raw), now) && self.gate.activity() == Activity::Silent {
            if let Some(visualizer) = self.visualizer.as_ref() {
                visualizer.quiet.store(true, Ordering::Release);
                // A sample that arrived between the read above and the flag
                // would otherwise be missed until the next one.
                if !silent_frame(&visualizer.bars()) {
                    self.gate.sample(false, now);
                    visualizer.quiet.store(false, Ordering::Release);
                }
            }
        }
        let dt = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .clamp(0.0, 0.2);
        self.last = now;
        if self.gate.activity() == Activity::Silent {
            // The release has already brought every bar down during the
            // silence window, so this flat frame is not a visible step.
            self.spectrum.flat();
            return self.traced(now, 'S');
        }
        self.spectrum.frame(&raw, dt);
        self.traced(now, 'L')
    }

    /// The shown bars, written to the `NIRU_CAVA_TRACE` file when set.
    fn traced(&mut self, now: Instant, state: char) -> &[f32] {
        if let Some(trace) = self.trace.as_mut() {
            trace.frame(now, state, self.spectrum.shown());
        }
        self.spectrum.shown()
    }

    fn maintain(&mut self, now: Instant) {
        if let Some(visualizer) = self.visualizer.as_mut() {
            if !visualizer.is_running() {
                self.visualizer = None;
                self.next_retry = now + Duration::from_secs(2);
            }
        }
        if self.visualizer.is_none() && now >= self.next_retry {
            self.visualizer = Visualizer::start(Self::writer(&self.wake));
            self.next_retry = now + Duration::from_secs(2);
        }
    }
}

/// Optional per-frame record of the shown bar heights, for measuring motion
/// and frame pacing: `NIRU_CAVA_TRACE=/path` appends one line per frame with
/// the milliseconds since start, the activity (`L`ive, `S`ilent, display
/// `O`ff) and the 48 heights. It holds nothing but those numbers.
struct Trace {
    out: io::BufWriter<fs::File>,
    start: Instant,
}

impl Trace {
    fn from_env() -> Option<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = std::env::var_os("NIRU_CAVA_TRACE").filter(|p| !p.is_empty())?;
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .ok()?;
        Some(Self {
            out: io::BufWriter::new(file),
            start: Instant::now(),
        })
    }

    fn frame(&mut self, now: Instant, state: char, shown: &[f32]) {
        let ms = now.saturating_duration_since(self.start).as_secs_f64() * 1000.0;
        let mut line = format!("{ms:.3} {state}");
        for value in shown {
            line.push_str(&format!(" {value:.4}"));
        }
        line.push('\n');
        let _ = self.out.write_all(line.as_bytes());
        let _ = self.out.flush();
    }
}

/// The shown bar heights: the [`Gain`]-normalized CAVA levels eased with a
/// fast attack and a slower release. Free of I/O so that it can be tested.
///
/// Every change of the shown bars, including the first frame after a
/// silence pause or a display wake-up, goes through the same easing, so the
/// bars always rise from where they were instead of snapping to a frame.
#[derive(Debug)]
pub struct Spectrum {
    shown: Vec<f32>,
    gain: Gain,
    /// Set by a flat (paused) frame: the next frame eases over one nominal
    /// frame interval, whatever time passed during the pause.
    resume: bool,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    /// The nominal frame interval (CAVA and the repaint run at 30 fps).
    pub const FRAME: f32 = 1.0 / 30.0;
    /// Attack and release time constants, in seconds: the NIRUAMP envelope.
    const ATTACK: f32 = 0.045;
    const RELEASE: f32 = 0.14;

    pub fn new() -> Self {
        Self {
            shown: vec![0.0; BARS],
            gain: Gain::new(),
            resume: true,
        }
    }

    pub fn shown(&self) -> &[f32] {
        &self.shown
    }

    /// A paused frame: every bar flat. The level reference is kept.
    pub fn flat(&mut self) -> &[f32] {
        self.shown.iter_mut().for_each(|v| *v = 0.0);
        self.resume = true;
        &self.shown
    }

    /// Ease towards one raw CAVA frame that arrived `dt` seconds after the
    /// last one.
    pub fn frame(&mut self, raw: &[u16], dt: f32) -> &[f32] {
        let dt = if std::mem::take(&mut self.resume) {
            Self::FRAME
        } else {
            dt
        };
        let targets = self.gain.targets(raw, dt);
        // Same attack/release envelope as NIRUAMP: fast up, slower down, at
        // the same 30 fps sampling interval.
        let attack = 1.0 - (-dt / Self::ATTACK).exp();
        let release = 1.0 - (-dt / Self::RELEASE).exp();
        for (index, shown) in self.shown.iter_mut().enumerate() {
            let target = targets.get(index).copied().unwrap_or(0.0);
            let factor = if target >= *shown { attack } else { release };
            *shown += (target - *shown) * factor;
            if *shown < 0.002 {
                *shown = 0.0;
            }
        }
        &self.shown
    }
}

/// Raw 16-bit bars below this are noise, not sound (about -66 dB). An idle
/// stream measured 4 at most; quiet music is in the hundreds.
const SILENCE_FLOOR: u16 = 32;

/// A frame with every bar below the noise floor.
fn silent_frame(frame: &[u16]) -> bool {
    frame.iter().all(|value| *value < SILENCE_FLOOR)
}

/// Level normalization in place of CAVA's `autosens`.
///
/// With `autosens` CAVA raises its gain without limit during silence until
/// the noise of an idle stream fills the panel, so "no audio" looked like
/// sound and the spectrum never paused. CAVA now runs at a fixed sensitivity
/// and this scales the bars instead: the loudest recent bar is shown where
/// `autosens` put it (about 85 % before the 0.62 curve), the reference decays slowly
/// so quiet passages grow, and the gain is capped so that anything below the
/// noise floor stays flat.
///
/// Two details keep the motion continuous. A soft knee above the noise floor
/// lets a bar grow from zero instead of appearing at a few percent of the
/// panel. And the reference only decays while there is sound: silence keeps
/// the level of the last music, and a fresh start uses a typical music
/// level, so the first frames after a pause are not drawn at full gain.
#[derive(Debug)]
pub struct Gain {
    peak: f32,
}

impl Default for Gain {
    fn default() -> Self {
        Self::new()
    }
}

impl Gain {
    /// The smallest reference, which caps the gain.
    const PEAK_MIN: f32 = 400.0;
    /// The reference before any sound: a quiet music level, about -24 dB.
    const PEAK_START: f32 = 4000.0;
    /// Where the loudest recent bar is drawn.
    const HEADROOM: f32 = 0.85;
    /// Time constant of the reference's decay, in seconds.
    const DECAY: f32 = 4.0;
    /// Width of the soft knee above [`SILENCE_FLOOR`], in raw units: a bar
    /// reaches its full curve at four times the floor.
    const KNEE: f32 = 3.0 * SILENCE_FLOOR as f32;

    pub fn new() -> Self {
        Self {
            peak: Self::PEAK_START,
        }
    }

    /// Bar heights in 0.0..=1.0 for one raw frame, `dt` seconds after the last.
    pub fn targets(&mut self, raw: &[u16], dt: f32) -> Vec<f32> {
        let max = raw.iter().copied().max().map(f32::from).unwrap_or(0.0);
        self.peak = if max > self.peak {
            max
        } else if max < f32::from(SILENCE_FLOOR) {
            // Silence says nothing about how loud the next sound will be.
            self.peak
        } else {
            (self.peak * (-dt / Self::DECAY).exp()).max(Self::PEAK_MIN)
        };
        raw.iter().map(|value| self.level(*value)).collect()
    }

    /// One bar: the 0.62 curve of the normalized level, faded in by a
    /// smoothstep over the knee so it is continuous (and flat) at the floor.
    fn level(&self, value: u16) -> f32 {
        if value < SILENCE_FLOOR {
            return 0.0;
        }
        let curve = (f32::from(value) * Self::HEADROOM / self.peak)
            .min(1.0)
            .powf(0.62);
        let knee = (f32::from(value - SILENCE_FLOOR) / Self::KNEE).min(1.0);
        curve * knee * knee * (3.0 - 2.0 * knee)
    }
}

/// How long every bar must stay silent before the repaint stops.
pub const SILENCE_AFTER: Duration = Duration::from_secs(2);
/// The DPMS state is polled at most this often when the server cannot send
/// DPMS events.
pub const DPMS_POLL: Duration = Duration::from_secs(2);

/// What the spectrum is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// Sound and a lit display: repaint at 30 fps.
    Live,
    /// The display is on but there has been no sound for [`SILENCE_AFTER`]:
    /// one flat frame, then no repaints until a non-silent sample.
    Silent,
    /// DPMS is not `On`: CAVA is stopped and one static frame is shown.
    DisplayOff,
}

/// The pause state machine, free of I/O so that it can be tested.
#[derive(Debug)]
pub struct PowerGate {
    display_on: bool,
    silent: bool,
    silent_since: Option<Instant>,
}

impl Default for PowerGate {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerGate {
    pub fn new() -> Self {
        Self {
            display_on: true,
            silent: false,
            silent_since: None,
        }
    }

    pub fn activity(&self) -> Activity {
        if !self.display_on {
            Activity::DisplayOff
        } else if self.silent {
            Activity::Silent
        } else {
            Activity::Live
        }
    }

    /// Record the display power state. Returns true when it changed. Turning
    /// the display on starts a fresh silence window.
    pub fn display(&mut self, on: bool) -> bool {
        if on == self.display_on {
            return false;
        }
        self.display_on = on;
        self.silent = false;
        self.silent_since = None;
        true
    }

    /// Record whether the latest sample was silent. Returns true when the
    /// activity changed between `Live` and `Silent`.
    pub fn sample(&mut self, silent: bool, now: Instant) -> bool {
        if !self.display_on {
            return false;
        }
        if !silent {
            self.silent_since = None;
            return std::mem::replace(&mut self.silent, false);
        }
        let since = *self.silent_since.get_or_insert(now);
        if !self.silent && now.saturating_duration_since(since) >= SILENCE_AFTER {
            self.silent = true;
            return true;
        }
        false
    }
}

/// Watches the display power (DPMS) state.
///
/// DPMS 1.2 servers send `DPMSInfoNotify`; older servers are polled with
/// `DPMSInfo`, at most every [`DPMS_POLL`] and immediately on input (input is
/// what wakes a display). A server without DPMS counts as always on.
pub struct DisplayPower {
    available: bool,
    events: bool,
    on: bool,
    next_poll: Instant,
    last_input_check: Option<Instant>,
}

impl DisplayPower {
    pub fn new<C: RequestConnection>(c: &C) -> Self {
        let version = c
            .dpms_get_version(1, 2)
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        let available = version.is_some();
        let events = version
            .as_ref()
            .is_some_and(|v| (v.server_major_version, v.server_minor_version) >= (1, 2))
            && c.dpms_select_input(dpms::EventMask::INFO_NOTIFY)
                .ok()
                .and_then(|cookie| cookie.check().ok())
                .is_some();
        let mut power = Self {
            available,
            events,
            on: true,
            next_poll: Instant::now() + DPMS_POLL,
            last_input_check: None,
        };
        if available {
            power.on = Self::query(c).unwrap_or(true);
        }
        power
    }

    /// How the state is followed; for the diagnostic log.
    pub fn mode(&self) -> &'static str {
        match (self.available, self.events) {
            (false, _) => "no DPMS",
            (true, true) => "DPMS events",
            (true, false) => "DPMS polling",
        }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    fn query<C: RequestConnection>(c: &C) -> Option<bool> {
        let info = c.dpms_info().ok()?.reply().ok()?;
        Some(!info.state || info.power_level == dpms::DPMSMode::ON)
    }

    fn set(&mut self, on: Option<bool>) -> Option<bool> {
        let on = on?;
        (on != std::mem::replace(&mut self.on, on)).then_some(on)
    }

    /// A DPMS event; returns the new state when it changed.
    pub fn event(&mut self, event: &Event) -> Option<bool> {
        match event {
            Event::DpmsInfoNotify(e) => {
                self.set(Some(!e.state || e.power_level == dpms::DPMSMode::ON))
            }
            _ => None,
        }
    }

    /// Poll when due (servers without events); returns a changed state.
    pub fn poll<C: RequestConnection>(&mut self, c: &C, now: Instant) -> Option<bool> {
        if !self.available || self.events || now < self.next_poll {
            return None;
        }
        self.next_poll = now + DPMS_POLL;
        self.set(Self::query(c))
    }

    /// Input arrived. While the display is off, check at once (at most every
    /// 250 ms), so the spectrum is back within a second of waking.
    pub fn input<C: RequestConnection>(&mut self, c: &C, now: Instant) -> Option<bool> {
        if !self.available || self.on {
            return None;
        }
        if self
            .last_input_check
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(250))
        {
            return None;
        }
        self.last_input_check = Some(now);
        self.set(Self::query(c))
    }

    /// When the next poll is due, for the event loop's timeout.
    pub fn deadline(&self) -> Option<Instant> {
        (self.available && !self.events).then_some(self.next_poll)
    }
}

fn split(colour: u32) -> (u32, u32, u32) {
    ((colour >> 16) & 255, (colour >> 8) & 255, colour & 255)
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = split(a);
    let (br, bg, bb) = split(b);
    let channel =
        |a: u32, b: u32| (f32::from(a as u8) * (1.0 - t) + f32::from(b as u8) * t).round() as u32;
    (channel(ar, br) << 16) | (channel(ag, bg) << 8) | channel(ab, bb)
}

/// Lift a theme colour towards white by `t`, independent of hue. This is the
/// same peak/glow derivation NIRUAMP uses for its visualizer.
fn lighten(colour: u32, t: f32) -> u32 {
    mix(colour, 0xffffff, t)
}

/// Keep the visualizer recognisably part of the current NIRUX theme: dim low
/// bars, focus-coloured mid bars, and a light focus peak for loud bars.
fn bar_colour(pal: &Palette, value: f32) -> u32 {
    let led = lighten(pal.focus, 0.30);
    let led_dim = mix(led, mix(pal.background, pal.foreground, 0.065), 0.80);
    if value > 0.82 {
        lighten(pal.focus, 0.55)
    } else if value > 0.45 {
        pal.focus
    } else {
        led_dim
    }
}

/// Paint one spectrum panel into the screen's backing pixmap.
pub fn paint(
    surface: &Surface,
    pal: &Palette,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    values: &[f32],
) -> crate::Result<()> {
    let w = usize::from(width);
    let h = usize::from(height);
    if w == 0 || h == 0 {
        return Ok(());
    }
    let mut pixels = vec![pal.background; w * h];

    // Faint horizontal guides keep the panel readable even during silence.
    let guide = mix(pal.border, pal.background, 0.60);
    for numerator in 1..=3 {
        let row = h * numerator / 4;
        if row < h {
            for pixel in &mut pixels[row * w..(row + 1) * w] {
                *pixel = guide;
            }
        }
    }
    let baseline = mix(pal.focus, pal.background, 0.35);
    for pixel in &mut pixels[(h - 1) * w..] {
        *pixel = baseline;
    }

    let count = values.len();
    if count > 0 {
        let gap = if w >= 600 { 2 } else { 1 };
        let usable = w.saturating_sub(gap * count.saturating_sub(1));
        let bar_width = (usable / count).max(1);
        let used = bar_width * count + gap * count.saturating_sub(1);
        let start = (w.saturating_sub(used)) / 2;
        let max_bar = h.saturating_sub(4).max(1);
        for (index, &value) in values.iter().enumerate() {
            let value = value.clamp(0.0, 1.0);
            if value <= 0.001 {
                continue;
            }
            let bar_height = ((value * max_bar as f32).round() as usize).clamp(1, max_bar);
            let left = start + index * (bar_width + gap);
            let top = h - 2 - bar_height;
            let colour = bar_colour(pal, value);
            for row in top..h - 2 {
                for column in left..left + bar_width {
                    if bar_height > 4
                        && row < top + 2
                        && (column == left || column == left + bar_width - 1)
                    {
                        continue;
                    }
                    pixels[row * w + column] = colour;
                }
            }
            if value > 0.18 {
                let cap = lighten(pal.focus, 0.20);
                for column in left..left + bar_width {
                    if top < h {
                        pixels[top * w + column] = cap;
                    }
                }
            }
        }
    }

    let order = surface.c.setup().image_byte_order;
    let mut bytes = Vec::with_capacity(w * h * 4);
    for pixel in pixels {
        if order == ImageOrder::MSB_FIRST {
            bytes.extend_from_slice(&pixel.to_be_bytes());
        } else {
            bytes.extend_from_slice(&pixel.to_le_bytes());
        }
    }
    surface.c.put_image(
        ImageFormat::Z_PIXMAP,
        surface.back,
        surface.gc,
        width,
        height,
        x,
        y,
        0,
        surface.depth,
        &bytes,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_pauses_after_the_window_and_sound_resumes() {
        let start = Instant::now();
        let mut gate = PowerGate::new();
        assert_eq!(gate.activity(), Activity::Live);
        assert!(!gate.sample(true, start));
        assert!(!gate.sample(true, start + Duration::from_millis(1900)));
        assert_eq!(gate.activity(), Activity::Live);
        assert!(gate.sample(true, start + SILENCE_AFTER));
        assert_eq!(gate.activity(), Activity::Silent);
        assert!(gate.sample(false, start + Duration::from_secs(5)));
        assert_eq!(gate.activity(), Activity::Live);
    }

    #[test]
    fn a_single_sound_sample_restarts_the_silence_window() {
        let start = Instant::now();
        let mut gate = PowerGate::new();
        gate.sample(true, start);
        gate.sample(false, start + Duration::from_millis(1500));
        assert!(!gate.sample(true, start + Duration::from_millis(2500)));
        assert_eq!(gate.activity(), Activity::Live);
        assert!(gate.sample(true, start + Duration::from_millis(4500)));
    }

    #[test]
    fn display_off_wins_and_on_starts_fresh() {
        let start = Instant::now();
        let mut gate = PowerGate::new();
        gate.sample(true, start);
        gate.sample(true, start + SILENCE_AFTER);
        assert_eq!(gate.activity(), Activity::Silent);
        assert!(gate.display(false));
        assert!(!gate.display(false));
        assert_eq!(gate.activity(), Activity::DisplayOff);
        // Samples while off change nothing.
        assert!(!gate.sample(false, start + Duration::from_secs(3)));
        assert_eq!(gate.activity(), Activity::DisplayOff);
        assert!(gate.display(true));
        assert_eq!(gate.activity(), Activity::Live);
    }

    #[test]
    fn silent_frames_allow_noise_below_the_floor() {
        assert!(silent_frame(&[0; BARS]));
        assert!(silent_frame(&[SILENCE_FLOOR - 1; BARS]));
        assert!(silent_frame(&[]));
        let mut frame = [0u16; BARS];
        frame[7] = 180;
        assert!(!silent_frame(&frame));
    }

    #[test]
    fn gain_normalizes_like_autosens_but_keeps_noise_flat() {
        let mut gain = Gain::new();
        // Loud and quiet music end up at the same height once settled.
        let loud = gain.targets(&[20_000, 2_000, 0], 0.033);
        assert!((loud[0] - Gain::HEADROOM.powf(0.62)).abs() < 1e-4);
        assert!(loud[1] > 0.0 && loud[1] < loud[0]);
        assert_eq!(loud[2], 0.0);
        for _ in 0..30 * 60 {
            gain.targets(&[2_000, 200, 0], 0.033);
        }
        let quiet = gain.targets(&[2_000, 200, 0], 0.033);
        assert!((quiet[0] - Gain::HEADROOM.powf(0.62)).abs() < 1e-3);
        // Noise never grows into bars, however long it lasts.
        for _ in 0..30 * 120 {
            gain.targets(&[4, 9, SILENCE_FLOOR - 1], 0.033);
        }
        assert!(gain
            .targets(&[4, 9, SILENCE_FLOOR - 1], 0.033)
            .iter()
            .all(|v| *v == 0.0));
        // The gain is capped: a sound just above the floor stays small.
        let faint = gain.targets(&[SILENCE_FLOOR + 8, 0, 0], 0.033);
        assert!(faint[0] > 0.0 && faint[0] < 0.05);
    }

    /// A music frame with a spread of levels, like a real spectrum.
    fn music() -> Vec<u16> {
        (0..BARS)
            .map(|i| (20_000 - i * 400) as u16)
            .collect::<Vec<_>>()
    }

    /// Check one rise from flat bars to a steady `target`: every frame is a
    /// step of at most 55 % of the target, never backwards, and the bars
    /// settle within a few frames like the 0.17 envelope.
    fn assert_eased_rise(frames: &[Vec<f32>], target: &[f32]) {
        let mut previous = vec![0.0; BARS];
        for (n, frame) in frames.iter().enumerate() {
            for i in 0..BARS {
                let step = frame[i] - previous[i];
                assert!(step >= -1e-6, "frame {n} bar {i} fell during a rise");
                assert!(
                    step <= 0.55 * target[i] + 1e-6,
                    "frame {n} bar {i} jumped {step} of {}",
                    target[i]
                );
            }
            previous.clone_from(frame);
        }
        for i in 0..BARS {
            assert!((previous[i] - target[i]).abs() <= 0.05 * target[i] + 1e-3);
        }
    }

    #[test]
    fn music_after_a_silence_pause_eases_up_instead_of_snapping() {
        let mut spectrum = Spectrum::new();
        for _ in 0..90 {
            spectrum.frame(&music(), Spectrum::FRAME);
        }
        let target = spectrum.shown().to_vec();
        // The silence window: CAVA's bars fall to nothing, the release
        // follows them down, then the repaint pauses on a flat frame.
        for _ in 0..60 {
            spectrum.frame(&[0; BARS], Spectrum::FRAME);
        }
        assert!(spectrum.shown().iter().all(|v| *v == 0.0));
        spectrum.flat();
        // Seconds pass while paused; the first frame after the wake-up
        // still eases over one frame interval.
        let frames: Vec<Vec<f32>> = (0..6)
            .map(|n| {
                let dt = if n == 0 { 0.2 } else { Spectrum::FRAME };
                spectrum.frame(&music(), dt).to_vec()
            })
            .collect();
        // The 0.17 envelope: 1 - e^(-33/45), about 52 % per frame.
        let first = 1.0 - (-Spectrum::FRAME / Spectrum::ATTACK).exp();
        assert!((frames[0][0] - first * target[0]).abs() < 0.01 * target[0]);
        assert_eased_rise(&frames, &target);
    }

    #[test]
    fn the_first_frames_and_a_display_wake_up_ease_too() {
        let mut spectrum = Spectrum::new();
        let frames: Vec<Vec<f32>> = (0..6)
            .map(|_| spectrum.frame(&music(), 0.2).to_vec())
            .collect();
        let mut settled = Spectrum::new();
        for _ in 0..90 {
            settled.frame(&music(), Spectrum::FRAME);
        }
        // Only the first frame is held to one frame interval; a late frame
        // after that legitimately moves further, so compare the first.
        assert!(frames[0][0] <= 0.55 * settled.shown()[0]);
        spectrum.flat();
        let frames: Vec<Vec<f32>> = (0..6)
            .map(|_| spectrum.frame(&music(), Spectrum::FRAME).to_vec())
            .collect();
        assert_eased_rise(&frames, settled.shown());
    }

    #[test]
    fn bars_near_the_floor_are_small_and_grow_continuously_from_zero() {
        let mut gain = Gain::new();
        // A long quiet passage drives the gain to its maximum.
        for _ in 0..30 * 60 {
            gain.targets(&[SILENCE_FLOOR + 4; BARS], 0.033);
        }
        assert_eq!(gain.peak, Gain::PEAK_MIN);
        assert_eq!(gain.level(SILENCE_FLOOR - 1), 0.0);
        assert_eq!(gain.level(SILENCE_FLOOR), 0.0);
        assert!(gain.level(SILENCE_FLOOR + 8) < 0.01);
        assert!(gain.level(2 * SILENCE_FLOOR) < 0.08);
        let mut previous = 0.0;
        for value in SILENCE_FLOOR..=2_000 {
            let level = gain.level(value);
            assert!(level >= previous, "not monotonic at {value}");
            assert!(
                level - previous < 0.01,
                "a step of {} at {value}",
                level - previous
            );
            previous = level;
        }
        // During music the gain is lower, so the knee only touches bars
        // below a few percent of the panel.
        let mut gain = Gain::new();
        gain.targets(&[20_000], 0.033);
        assert!(gain.level(4 * SILENCE_FLOOR) < 0.05);
    }

    #[test]
    fn silence_keeps_the_level_reference() {
        let mut gain = Gain::new();
        assert_eq!(gain.peak, Gain::PEAK_START);
        gain.targets(&music(), 0.033);
        assert_eq!(gain.peak, 20_000.0);
        for _ in 0..30 * 600 {
            gain.targets(&[0; BARS], 0.033);
        }
        assert_eq!(gain.peak, 20_000.0);
        // Sound decays it as before, so quiet passages still grow.
        gain.targets(&[2_000], 1.0);
        assert!(gain.peak < 20_000.0);
    }
}
