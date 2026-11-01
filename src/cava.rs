//! Optional CAVA spectrum for the idle and lock screens.
//!
//! CAVA is started as a child process with a temporary config that emits raw
//! 8-bit mono bars on stdout. The rendering is done here, in the same 24/32-bit
//! pixel format as the text renderer, so the bars follow the active NIRUX theme
//! without requiring a compositor or a second window.

use crate::{core::Surface, palette::Palette};
use std::{
    fs,
    io::Read,
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
    connection::Connection,
    protocol::xproto::{ConnectionExt, ImageFormat, ImageOrder},
};

pub const BARS: usize = 48;

const CAVA_CONFIG: &str = "\
[general]\n\
bars = 48\n\
framerate = 30\n\
\n\
[output]\n\
method = raw\n\
raw_target = /dev/stdout\n\
data_format = binary\n\
bit_format = 8bit\n\
channels = mono\n\
\n\
[smoothing]\n\
noise_reduction = 77\n";

/// A running CAVA child and the latest complete frame it produced.
struct Visualizer {
    child: Child,
    bars: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl Visualizer {
    fn start() -> Option<Self> {
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
        let mut child = match Command::new("cava")
            .arg("-p")
            .arg(&path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
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
        let thread_bars = Arc::clone(&bars);
        let thread_stop = Arc::clone(&stop);
        let reader = thread::Builder::new()
            .name("niru-cava-reader".into())
            .spawn(move || {
                let mut frame = [0u8; BARS];
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if stdout.read_exact(&mut frame).is_err() {
                        break;
                    }
                    if let Ok(mut guard) = thread_bars.lock() {
                        guard.clear();
                        guard.extend_from_slice(&frame);
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
            reader,
            path,
        })
    }

    fn is_running(&mut self) -> bool {
        !self.stop.load(Ordering::Relaxed) && matches!(self.child.try_wait(), Ok(None))
    }

    fn bars(&self) -> Vec<u8> {
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
    shown: Vec<f32>,
    last: Instant,
    next_retry: Instant,
    primed: bool,
}

impl Cava {
    pub fn new() -> Self {
        let now = Instant::now();
        let visualizer = Visualizer::start();
        Self {
            visualizer,
            shown: vec![0.0; BARS],
            last: now,
            next_retry: now + Duration::from_secs(2),
            primed: false,
        }
    }

    /// True while a CAVA child is believed to be running. Used to choose the
    /// 30 fps idle refresh instead of the once-a-minute clock refresh.
    pub fn running(&self) -> bool {
        self.visualizer.is_some()
    }

    /// Latest smoothed bar heights in the range 0.0..=1.0.
    pub fn values(&mut self) -> &[f32] {
        let now = Instant::now();
        self.maintain(now);
        let raw = self
            .visualizer
            .as_ref()
            .map(Visualizer::bars)
            .unwrap_or_default();
        let dt = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .clamp(0.0, 0.2);
        self.last = now;
        if !raw.is_empty() && !self.primed {
            for (index, shown) in self.shown.iter_mut().enumerate() {
                *shown = raw
                    .get(index)
                    .map(|value| (f32::from(*value) / 255.0).powf(0.62))
                    .unwrap_or(0.0);
            }
            self.primed = true;
            return &self.shown;
        }
        // Same attack/release envelope as NIRUAMP: fast up, slower down, at
        // the same 30 fps sampling interval.
        let attack = 1.0 - (-dt / 0.045).exp();
        let release = 1.0 - (-dt / 0.14).exp();
        for (index, shown) in self.shown.iter_mut().enumerate() {
            let target = raw
                .get(index)
                .map(|value| (f32::from(*value) / 255.0).powf(0.62))
                .unwrap_or(0.0);
            let factor = if target >= *shown { attack } else { release };
            *shown += (target - *shown) * factor;
            if *shown < 0.002 {
                *shown = 0.0;
            }
        }
        &self.shown
    }

    fn maintain(&mut self, now: Instant) {
        if let Some(visualizer) = self.visualizer.as_mut() {
            if !visualizer.is_running() {
                self.visualizer = None;
                self.next_retry = now + Duration::from_secs(2);
            }
        }
        if self.visualizer.is_none() && now >= self.next_retry {
            self.visualizer = Visualizer::start();
            self.next_retry = now + Duration::from_secs(2);
        }
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
