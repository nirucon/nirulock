//! The `demo` layout: the lock screen as a C64 demo.
//!
//! Every monitor gets the same scene in two bands, with plain black between
//! them:
//!
//! - at the top, a big chunky NIRUX logo built from NIRUSID's 8x8 pixel font
//!   ([`crate::pixfont`]), each font pixel a block, wobbling row by row
//!   over raster bars that swing up and down through the logo, in front of
//!   a parallax starfield;
//! - at the bottom, a sine scroller that carries the clock, the away time,
//!   the hostname (only with `show_hostname = true`), the tune that plays
//!   and a quote from the `quotes` collections, framed by copper lines.
//!
//! The prompt monitor has the big clock, the password entry, the away time
//! and the status lines in the black middle, drawn with the theme font
//! exactly as in the other layouts, on black: nothing animated ever passes
//! behind or over them, so they stay sharp and easy to read. A failed
//! attempt turns the raster bars to the warning colour while the message
//! shows.
//!
//! The colours come from the active NIRUX theme: its accent (the text colour
//! when the accent is too dim on black), two neighbours of the accent on
//! the colour wheel, the warning colour and the text colour, each shaded
//! from black to a near-white shine like the bars on a C64. A grey theme
//! gives a grey demo. While music plays (the CAVA spectrum the other layouts
//! show), the bars and the logo flash on the beat.
//!
//! It runs at [`FRAME`] (25 frames a second, half the PAL rate) and draws
//! only the two bands: solid rectangles filled by the X server, a few dozen
//! requests a frame, and one copy of each band to the screen. With the
//! display off nothing is drawn and the loop sleeps.
//!
//! The scene is computed by [`scene`], free of I/O so it can be tested.

use crate::{core, display::Area, layout, pixfont, Lock, Result};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::{ChangeGCAux, ConnectionExt as _, Rectangle};

/// The frame interval.
pub const FRAME: Duration = Duration::from_millis(40);
/// The longest frame interval, for very large screens.
pub const SLOW_FRAME: Duration = Duration::from_millis(66);
/// Band pixels a frame that still run at [`FRAME`]: two 1920x1080
/// monitors. Beyond that the interval grows (to [`SLOW_FRAME`] at most), so
/// a 4K screen without a GPU-accelerated X server costs about as much as
/// two full HD ones.
const FULL_RATE_PIXELS: i64 = 2 * 1920 * 560;
/// The logo.
pub const LOGO: &str = "NIRUX";
/// The scroller's speed in font pixels a second (three characters).
const SCROLL_SPEED: f32 = 24.0;
/// Raster bars in the logo band.
const BARS: usize = 4;
/// Stripes per raster bar, dark to shine to dark.
const STRIPES: usize = 11;
/// Scroller colours in the cycle.
const SCROLL_COLOURS: usize = 16;
/// At most this many rectangles in one request.
const MAX_RECTS: usize = 4096;

/// One star: where it starts (0..1 of the screen width), its height in the
/// band (0..1) and its layer (0 far and slow, 2 near and fast).
#[derive(Clone, Copy, Debug)]
struct Star {
    x: f32,
    y: f32,
    layer: u8,
}

/// One monitor's scene geometry.
#[derive(Clone, Debug)]
pub struct Screen {
    x: i32,
    w: i32,
    /// The logo band and the scroller band: (top, height).
    top: (i32, i32),
    bottom: (i32, i32),
    /// The logo's block size and its top left corner.
    logo: i32,
    logo_left: i32,
    logo_top: i32,
    /// The scroller's block size, the text's centre line and the sine's
    /// amplitude, in pixels.
    scroll: i32,
    scroll_mid: i32,
    amp: i32,
    /// The size of a star pixel.
    dot: i32,
    stars_top: Vec<Star>,
    stars_bottom: Vec<Star>,
}

impl Screen {
    fn bands(&self) -> [(i16, i16, u16, u16); 2] {
        [
            rect16(self.x, self.top.0, self.w, self.top.1),
            rect16(self.x, self.bottom.0, self.w, self.bottom.1),
        ]
    }

    /// How wide the screen is in scroller font pixels.
    fn cells(&self) -> i32 {
        self.w / self.scroll.max(1) + 1
    }
}

/// Where the prompt monitor's lines go.
#[derive(Clone, Copy, Debug)]
pub struct Spots {
    pub info: i16,
    pub clock: i16,
    pub entry: i16,
    pub away: i16,
    pub rows: i32,
    pub step: i32,
    pub limit: i32,
}

/// Lay out one monitor. `block` is the height the prompt lines need in the
/// middle (0 on the other monitors).
pub fn arrange(area: Area, block: i32, seed: u64) -> Screen {
    let (x, y) = (i32::from(area.x), i32::from(area.y));
    let (w, h) = (i32::from(area.width).max(1), i32::from(area.height).max(1));
    let margin = (h * 4 / 100).max(4);
    let bottom_h = (h * 15 / 100).clamp(40, 260);
    let mut top_h = h * 34 / 100;
    // The middle must hold the prompt lines: the logo band gives way first.
    let room = h - 2 * margin - bottom_h - top_h;
    if room < block {
        top_h = (top_h - (block - room)).max(h * 14 / 100).max(24);
    }
    let top = (y + margin, top_h);
    let bottom = (y + h - margin - bottom_h, bottom_h);
    // The logo: 40 by 8 font pixels, room for the wobble at the sides.
    let logo = ((w * 70 / 100) / 42).min(top_h * 62 / 100 / 8).max(2);
    let logo_left = x + (w - logo * 40) / 2;
    let logo_top = top.0 + (top_h - logo * 8) / 2;
    let scroll = (bottom_h * 50 / 100 / 8).max(2);
    let amp = ((bottom_h - scroll * 8) / 2 - scroll / 2).max(0);
    let scroll_mid = bottom.0 + bottom_h / 2;
    let dot = (h / 540).max(1);
    let mut rng = seed | 1;
    let mut stars = |area: i32| {
        let count = (area / 9000).clamp(12, 140) as usize;
        (0..count)
            .map(|i| Star {
                x: unit(&mut rng),
                y: unit(&mut rng),
                layer: (i % 3) as u8,
            })
            .collect::<Vec<_>>()
    };
    let stars_top = stars(w * top_h);
    let stars_bottom = stars(w * bottom_h);
    Screen {
        x,
        w,
        top,
        bottom,
        logo,
        logo_left,
        logo_top,
        scroll,
        scroll_mid,
        amp,
        dot,
        stars_top,
        stars_bottom,
    }
}

/// The prompt lines in the middle of `s`, centred between the bands. `u`
/// is the theme font's line height, `clock_ascent` the big clock's.
pub fn spots(s: &Screen, u: i32, clock_ascent: i32, info_ascent: i32, music: bool) -> Spots {
    let (mid_top, mid_bottom) = (s.top.0 + s.top.1, s.bottom.0);
    let block = prompt_block(u, clock_ascent, music);
    let start = (mid_top + (mid_bottom - mid_top - block) / 2).max(mid_top + u / 2);
    let info = start + info_ascent;
    let clock = start + if music { u * 2 } else { 0 } + clock_ascent;
    let entry = clock + u * 2;
    let away = entry + u * 3 / 2;
    Spots {
        info: layout::clamp16(info),
        clock: layout::clamp16(clock),
        entry: layout::clamp16(entry),
        away: layout::clamp16(away),
        rows: away + u * 19 / 10,
        step: u * 5 / 4,
        limit: mid_bottom - u / 2,
    }
}

/// The height of the prompt lines: the info line, the clock, the entry,
/// the away time and two status rows (more fit when there is room).
pub fn prompt_block(u: i32, clock_ascent: i32, music: bool) -> i32 {
    (if music { u * 2 } else { 0 }) + clock_ascent + u * 2 + u * 3 / 2 + u * 19 / 10 + u * 5 / 4
}

/// What the music and the lock state do to the scene this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fx {
    /// 0..1, 1 on a beat, fading.
    pub flash: f32,
    /// A failed attempt: the bars take the warning colour.
    pub wrong: bool,
}

/// The theme's colours for the scene.
#[derive(Clone, Debug)]
pub struct Colours {
    /// The bars' base colours.
    bars: [u32; BARS],
    logo: u32,
    warning: u32,
    scroll: [u32; SCROLL_COLOURS],
    stars: [u32; 3],
    copper: [u32; 6],
}

impl Colours {
    pub fn new(accent: u32, foreground: u32, muted: u32, warning: u32) -> Self {
        let left = rotate_hue(accent, -38.0);
        let right = rotate_hue(accent, 42.0);
        let bars = [accent, right, warning, left];
        let ring = [accent, right, foreground, warning, left, accent];
        let mut scroll = [0; SCROLL_COLOURS];
        for (i, colour) in scroll.iter_mut().enumerate() {
            let at = i as f32 / SCROLL_COLOURS as f32 * (ring.len() - 1) as f32;
            let k = at.floor() as usize;
            *colour = lighten(mix(ring[k], ring[k + 1], at - k as f32), 0.18);
        }
        let stars = [
            mix(0x000000, muted, 0.45),
            mix(0x000000, foreground, 0.7),
            lighten(foreground, 0.5),
        ];
        let copper = [
            shade(accent, 0.35),
            shade(accent, 0.7),
            lighten(accent, 0.35),
            lighten(right, 0.35),
            shade(right, 0.7),
            shade(right, 0.35),
        ];
        Self {
            bars,
            logo: accent,
            warning,
            scroll,
            stars,
            copper,
        }
    }
}

/// Draw calls in order: a colour and its rectangles.
pub type Ops = Vec<(u32, Vec<Rectangle>)>;

fn push(ops: &mut Ops, colour: u32, rect: Option<Rectangle>) {
    let Some(rect) = rect else {
        return;
    };
    match ops.last_mut() {
        Some((c, rects)) if *c == colour => rects.push(rect),
        _ => ops.push((colour, vec![rect])),
    }
}

/// `(x, y, w, h)` cut to `(cx, cy, cw, ch)`; `None` when nothing is left.
fn clip(x: i32, y: i32, w: i32, h: i32, band: (i32, i32, i32, i32)) -> Option<Rectangle> {
    let (cx, cy, cw, ch) = band;
    let (x0, y0) = (x.max(cx), y.max(cy));
    let (x1, y1) = ((x + w).min(cx + cw), (y + h).min(cy + ch));
    (x1 > x0 && y1 > y0).then(|| {
        let (x, y, width, height) = rect16(x0, y0, x1 - x0, y1 - y0);
        Rectangle {
            x,
            y,
            width,
            height,
        }
    })
}

/// The two bands of one monitor at `t` seconds, the scroller `pos` font
/// pixels in.
pub fn scene(s: &Screen, t: f32, pos: f32, text: &str, c: &Colours, fx: Fx) -> (Ops, Ops) {
    let top_band = (s.x, s.top.0, s.w, s.top.1);
    let bottom_band = (s.x, s.bottom.0, s.w, s.bottom.1);
    let mut top = Ops::new();
    let mut bottom = Ops::new();
    // Black only where no bar covers the band: no pixel is filled twice
    // for nothing, which is most of the X server's work.
    let mut y = s.top.0;
    let mut spans = bar_spans(top_band, t);
    spans.sort_unstable();
    for (top_y, height) in spans {
        push(&mut top, 0x000000, clip(s.x, y, s.w, top_y - y, top_band));
        y = y.max(top_y + height);
    }
    push(
        &mut top,
        0x000000,
        clip(s.x, y, s.w, s.top.0 + s.top.1 - y, top_band),
    );
    push(
        &mut bottom,
        0x000000,
        clip(s.x, s.bottom.0, s.w, s.bottom.1, bottom_band),
    );
    stars(&mut top, s, &s.stars_top, top_band, t, c);
    stars(&mut bottom, s, &s.stars_bottom, bottom_band, t, c);
    raster(&mut top, s, top_band, t, c, fx);
    logo(&mut top, s, top_band, t, c, fx);
    copper(&mut top, s, top_band, t, c);
    copper(&mut bottom, s, bottom_band, t, c);
    scroller(&mut bottom, s, bottom_band, t, pos, text, c, fx);
    (top, bottom)
}

fn stars(
    ops: &mut Ops,
    s: &Screen,
    list: &[Star],
    band: (i32, i32, i32, i32),
    t: f32,
    c: &Colours,
) {
    let (_, by, _, bh) = band;
    for layer in 0..3u8 {
        let speed = [0.018, 0.045, 0.1][usize::from(layer)] * s.w as f32;
        let size = s.dot * (i32::from(layer) + 1);
        for star in list.iter().filter(|star| star.layer == layer) {
            let x = (star.x * s.w as f32 - t * speed).rem_euclid(s.w as f32) as i32;
            let y = by + (star.y * (bh - size).max(1) as f32) as i32;
            push(
                ops,
                c.stars[usize::from(layer)],
                clip(s.x + x, y, size, size, band),
            );
        }
    }
}

/// The raster bars' stripe height, bar height and swing in a band.
fn bar_size(band: (i32, i32, i32, i32)) -> (i32, i32, f32) {
    let bh = band.3;
    let stripe = (bh / 6 / STRIPES as i32).max(2);
    let bar = stripe * STRIPES as i32;
    (stripe, bar, ((bh - bar) / 2).max(0) as f32)
}

fn bar_phase(i: usize, t: f32) -> f32 {
    t * 1.3 + i as f32 * (std::f32::consts::TAU / BARS as f32)
}

fn bar_centre(band: (i32, i32, i32, i32), i: usize, t: f32) -> i32 {
    let (_, _, swing) = bar_size(band);
    band.1 + band.3 / 2 + (bar_phase(i, t).sin() * swing) as i32
}

/// Every bar's rows: (top, height).
fn bar_spans(band: (i32, i32, i32, i32), t: f32) -> Vec<(i32, i32)> {
    let (_, bar, _) = bar_size(band);
    (0..BARS)
        .map(|i| (bar_centre(band, i, t) - bar / 2, bar))
        .collect()
}

fn raster(ops: &mut Ops, s: &Screen, band: (i32, i32, i32, i32), t: f32, c: &Colours, fx: Fx) {
    let (stripe, bar, _) = bar_size(band);
    // Far bars first, so the near ones pass in front: the copper twist.
    let mut order: Vec<(f32, usize)> = (0..BARS).map(|i| (bar_phase(i, t).cos(), i)).collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (depth, i) in order {
        let centre = bar_centre(band, i, t);
        let base = if fx.wrong { c.warning } else { c.bars[i] };
        // Far bars are dimmer; a beat lifts them all.
        let near = 0.62 + 0.38 * (depth + 1.0) / 2.0;
        for k in 0..STRIPES {
            // A round bar: dark edges, full colour, a near-white shine.
            let level = (std::f32::consts::PI * (k as f32 + 0.5) / STRIPES as f32).sin();
            let mut colour = mix(0x000000, base, (0.12 + 0.88 * level) * near);
            if k == STRIPES / 2 {
                colour = lighten(base, 0.55 * near);
            } else if k + 1 == STRIPES / 2 || k == STRIPES / 2 + 1 {
                colour = lighten(colour, 0.2 * near);
            }
            colour = lighten(colour, fx.flash * 0.45);
            let y = centre - bar / 2 + stripe * k as i32;
            push(ops, colour, clip(s.x, y, s.w, stripe, band));
        }
    }
}

fn logo(ops: &mut Ops, s: &Screen, band: (i32, i32, i32, i32), t: f32, c: &Colours, fx: Fx) {
    let p = s.logo;
    let gap = if p >= 8 { (p / 10).max(1) } else { 0 };
    let lift = (fx.flash * p as f32 * 0.4) as i32;
    let runs = pixfont::runs(LOGO);
    let wobble = |row: i32| ((t * 2.2 + row as f32 * 0.62).sin() * p as f32 * 0.9) as i32;
    // A black outline and a drop shadow first, so the logo stands out from
    // bars of its own colour.
    let edge = (p / 8).max(1);
    let drop = (p / 3).max(1);
    for &(x, row, len) in &runs {
        let px = s.logo_left + x * p + wobble(row);
        let py = s.logo_top + row * p - lift;
        push(
            ops,
            0x000000,
            clip(px - edge, py - edge, len * p + edge * 2, p + edge * 2, band),
        );
        push(ops, 0x000000, clip(px + drop, py + drop, len * p, p, band));
    }
    // A shine runs down the rows now and then.
    let shine = (t * 5.0).rem_euclid(14.0) as i32;
    for row in 0..8 {
        let mut colour = mix(lighten(c.logo, 0.62), shade(c.logo, 0.62), row as f32 / 7.0);
        if row == shine {
            colour = lighten(colour, 0.75);
        }
        colour = lighten(colour, fx.flash * 0.35);
        for &(x, _, len) in runs.iter().filter(|r| r.1 == row) {
            for cell in x..x + len {
                let px = s.logo_left + cell * p + wobble(row);
                let py = s.logo_top + row * p - lift;
                push(ops, colour, clip(px, py, p - gap, p - gap, band));
            }
        }
    }
}

fn copper(ops: &mut Ops, s: &Screen, band: (i32, i32, i32, i32), t: f32, c: &Colours) {
    let (_, by, _, bh) = band;
    let line = s.dot.max(1);
    let shift = (t * 3.0) as usize;
    for (edge, y0) in [(0, by), (1, by + bh - line * 3)] {
        for k in 0..3 {
            let colour = c.copper[(k + edge * 3 + shift) % c.copper.len()];
            push(
                ops,
                colour,
                clip(s.x, y0 + k as i32 * line, s.w, line, band),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scroller(
    ops: &mut Ops,
    s: &Screen,
    band: (i32, i32, i32, i32),
    t: f32,
    pos: f32,
    text: &str,
    c: &Colours,
    fx: Fx,
) {
    let q = s.scroll;
    let amp = s.amp as f32 * (1.0 + 0.2 * fx.flash);
    let start = s.x + s.w - (pos * q as f32) as i32;
    // Each colour once, all its columns in one request.
    let mut columns: Vec<Vec<Rectangle>> = vec![Vec::new(); SCROLL_COLOURS];
    for (index, ch) in text.chars().enumerate() {
        let left = start + index as i32 * 8 * q;
        if left + 8 * q <= s.x {
            continue;
        }
        if left >= s.x + s.w {
            break;
        }
        let glyph = pixfont::glyph(ch);
        for col in 0..8 {
            let x = left + col * q;
            if x + q <= s.x || x >= s.x + s.w {
                continue;
            }
            let along = (x - s.x) as f32 / s.w.max(1) as f32;
            let dy = ((along * std::f32::consts::TAU * 1.4 + t * 2.6).sin() * amp) as i32;
            let hue = ((along * 24.0 + t * 5.0) as usize) % SCROLL_COLOURS;
            let mut row = 0;
            while row < 8 {
                if glyph[row] & (0x80 >> col) == 0 {
                    row += 1;
                    continue;
                }
                let first = row;
                while row < 8 && glyph[row] & (0x80 >> col) != 0 {
                    row += 1;
                }
                let y = s.scroll_mid - 4 * q + dy + first as i32 * q;
                if let Some(r) = clip(x, y, q, (row - first) as i32 * q, band) {
                    columns[hue].push(r);
                }
            }
        }
    }
    for (hue, rects) in columns.into_iter().enumerate() {
        if !rects.is_empty() {
            ops.push((c.scroll[hue], rects));
        }
    }
}

/// The scroller's text: NIRUX, the `facts` (clock, away time, hostname),
/// the quote in quotation marks with its attribution, then the tune as
/// its own segment, `NOW PLAYING  ARTIST - TITLE`, and the password hint.
/// The quote and the tune never touch: a quote right after the tune read
/// as the tune's description. Plain uppercase ASCII for the pixel font.
pub fn scroller_text(
    facts: &[String],
    quote: Option<(String, String)>,
    tune: Option<&str>,
) -> String {
    let mut parts = vec!["NIRUX".to_string()];
    parts.extend(facts.iter().cloned());
    if let Some((words, attribution)) = quote {
        let words = words.trim().trim_matches(['"', '\u{201c}', '\u{201d}']);
        let mut q = format!("\"{words}\"");
        if !attribution.trim().is_empty() {
            let who = attribution
                .trim()
                .trim_start_matches(['-', '\u{2014}', '\u{2013}', ' ']);
            q.push_str("  - ");
            q.push_str(who);
        }
        parts.push(q);
    }
    if let Some(tune) = tune.map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(format!("NOW PLAYING  {tune}"));
    }
    parts.push("TYPE YOUR PASSWORD TO COME BACK".into());
    let text = format!("{}   ***", parts.join("   ***   "));
    pixfont::plain(&text).to_uppercase()
}

/// The frame interval for `pixels` band pixels a frame.
pub fn frame_for(pixels: i64) -> Duration {
    if pixels <= FULL_RATE_PIXELS {
        return FRAME;
    }
    let scaled = FRAME.as_secs_f64() * pixels as f64 / FULL_RATE_PIXELS as f64;
    Duration::from_secs_f64(scaled.min(SLOW_FRAME.as_secs_f64()))
}

/// The scene's state between frames.
#[derive(Default)]
pub struct State {
    screens: Vec<Screen>,
    /// The frame interval for these screens.
    frame: Duration,
    /// When the next frame is due, once the scene has been drawn.
    next: Option<Instant>,
    /// The scroller's text and when it started.
    text: String,
    since: Option<Instant>,
    flash: f32,
    bass: f32,
    last: Option<Instant>,
}

/// Follow the bass for the flash: a jump over its running average lights
/// it, and it fades over a third of a second.
pub fn beat(flash: f32, bass: f32, average: f32, dt: f32) -> (f32, f32) {
    let faded = flash * (0.0005f32).powf(dt.clamp(0.0, 1.0));
    let lit = if bass > 0.22 && bass > average * 1.25 + 0.06 {
        1.0
    } else {
        faded
    };
    let k = (dt * 4.0).clamp(0.0, 1.0);
    (lit, average + (bass - average) * k)
}

impl Lock {
    /// Lay the scene out on every monitor and the prompt lines on the
    /// prompt monitor.
    pub(crate) fn demo_layout(&mut self, area: Area) -> Spots {
        let screens = if self.outputs.is_empty() {
            vec![area]
        } else {
            self.outputs.clone()
        };
        let u = i32::from(self.renderer.line_height()).max(1);
        let clock_ascent = self
            .clock_renderer
            .ink("0123456789-:")
            .map_or(i32::from(self.clock_renderer.ascent()), |i| -i.1);
        let info_ascent = i32::from(self.info_renderer.ascent());
        let music = self.music.is_some();
        let block = prompt_block(u, clock_ascent, music);
        let mut out = None;
        self.demo.screens.clear();
        for (index, screen) in screens.into_iter().enumerate() {
            let prompt = screen == area;
            let s = arrange(
                screen,
                if prompt { block } else { 0 },
                0x5eed + index as u64,
            );
            if prompt {
                out = Some(spots(&s, u, clock_ascent, info_ascent, music));
            }
            self.demo.screens.push(s);
        }
        let pixels = self
            .demo
            .screens
            .iter()
            .map(|s| i64::from(s.w) * i64::from(s.top.1 + s.bottom.1))
            .sum();
        self.demo.frame = frame_for(pixels);
        out.unwrap_or(Spots {
            info: 0,
            clock: 0,
            entry: 0,
            away: 0,
            rows: 0,
            step: u,
            limit: 0,
        })
    }

    /// The scroller's next text: the clock, the away time, the hostname
    /// when it may show, a quote and the tune ([`scroller_text`]).
    fn demo_text(&mut self) -> String {
        let mut facts = vec![
            core::clock_text(chrono::Local::now()),
            format!(
                "LOCKED, {}",
                core::away_text(self.started.elapsed().as_secs())
            ),
        ];
        if self.cfg.show_hostname {
            let host = core::hostname();
            if !host.is_empty() {
                facts.push(host);
            }
        }
        let quote = self
            .visual
            .as_mut()
            .and_then(crate::visual::Visual::next_quote)
            .map(|q| (q.lines.join(" "), q.attribution.join(", ")));
        scroller_text(&facts, quote, self.music_tune.as_deref())
    }

    /// Draw both bands of every monitor as they are now into the backing
    /// pixmap, without copying them.
    pub(crate) fn demo_paint(&mut self) -> Result<()> {
        let now = Instant::now();
        let widest = self
            .demo
            .screens
            .iter()
            .map(Screen::cells)
            .max()
            .unwrap_or(0);
        let length = self.demo.text.chars().count() as i32 * 8;
        let pos = self.demo.since.map_or(f32::MAX, |since| {
            now.saturating_duration_since(since).as_secs_f32() * SCROLL_SPEED
        });
        if pos > (length + widest) as f32 {
            self.demo.text = self.demo_text();
            self.demo.since = Some(now);
        }
        let pos = self.demo.since.map_or(0.0, |since| {
            now.saturating_duration_since(since).as_secs_f32() * SCROLL_SPEED
        });
        // The beat, from the spectrum while music plays.
        let dt = self.demo.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.demo.last = Some(now);
        let bass = self
            .visual
            .as_mut()
            .and_then(crate::visual::Visual::spectrum)
            .map_or(0.0, |values| {
                let low = &values[..values.len().min(6)];
                low.iter().sum::<f32>() / low.len().max(1) as f32
            });
        (self.demo.flash, self.demo.bass) = beat(self.demo.flash, bass, self.demo.bass, dt);
        let pal = &self.cfg.palette;
        let accent = crate::ascii::banner_colour(pal.focus, pal.foreground, pal.background);
        let colours = Colours::new(accent, pal.foreground, pal.muted, self.cfg.warning);
        let fx = Fx {
            flash: self.demo.flash,
            wrong: self.state == crate::State::Wrong,
        };
        let t = self.started.elapsed().as_secs_f32();
        let s = &self.surface;
        for screen in &self.demo.screens {
            let (top, bottom) = scene(screen, t, pos, &self.demo.text, &colours, fx);
            for (colour, rects) in top.iter().chain(bottom.iter()) {
                s.c.change_gc(s.gc, &ChangeGCAux::new().foreground(*colour))?;
                for chunk in rects.chunks(MAX_RECTS) {
                    s.c.poly_fill_rectangle(s.back, s.gc, chunk)?;
                }
            }
        }
        let frame = self.demo.frame.max(FRAME);
        // Keep the cadence: the next frame is due one interval after this
        // one was, unless the loop fell behind.
        let due = self
            .demo
            .next
            .map_or(now, |due| due.max(now.checked_sub(frame).unwrap_or(now)));
        self.demo.next = Some(due + frame);
        Ok(())
    }

    /// The next frame, when it is due: both bands, drawn and copied.
    /// `display_on` false holds the scene as it is.
    pub(crate) fn demo_tick(&mut self, display_on: bool) -> Result<()> {
        if !display_on || self.demo.next.map_or(true, |due| Instant::now() < due) {
            return Ok(());
        }
        self.demo_paint()?;
        for screen in &self.demo.screens {
            for (x, y, w, h) in screen.bands() {
                self.surface.present_rect(x, y, w, h)?;
            }
        }
        Ok(())
    }

    /// How long the loop may sleep before the next frame.
    pub(crate) fn demo_timeout(&self, display_on: bool) -> Option<Duration> {
        if !display_on || self.cfg.layout != crate::config::Layout::Demo {
            return None;
        }
        Some(self.demo.next.map_or(Duration::ZERO, |due| {
            due.saturating_duration_since(Instant::now())
        }))
    }
}

fn rect16(x: i32, y: i32, w: i32, h: i32) -> (i16, i16, u16, u16) {
    (
        layout::clamp16(x),
        layout::clamp16(y),
        w.clamp(0, i32::from(u16::MAX)) as u16,
        h.clamp(0, i32::from(u16::MAX)) as u16,
    )
}

fn unit(rng: &mut u64) -> f32 {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    (*rng >> 40) as f32 / (1u64 << 24) as f32
}

fn split(colour: u32) -> (f32, f32, f32) {
    (
        ((colour >> 16) & 255) as f32,
        ((colour >> 8) & 255) as f32,
        (colour & 255) as f32,
    )
}

fn join(r: f32, g: f32, b: f32) -> u32 {
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    (c(r) << 16) | (c(g) << 8) | c(b)
}

pub(crate) fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = split(a);
    let (br, bg, bb) = split(b);
    join(ar + (br - ar) * t, ag + (bg - ag) * t, ab + (bb - ab) * t)
}

pub(crate) fn lighten(colour: u32, t: f32) -> u32 {
    mix(colour, 0xffffff, t)
}

/// Towards black by `1 - level`.
pub(crate) fn shade(colour: u32, level: f32) -> u32 {
    mix(0x000000, colour, level)
}

/// The colour turned `degrees` around the colour wheel, its lightness and
/// saturation kept: a grey stays grey.
pub fn rotate_hue(colour: u32, degrees: f32) -> u32 {
    let (r, g, b) = split(colour);
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta < 1e-4 {
        return colour;
    }
    let hue = if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let hue = (hue + degrees).rem_euclid(360.0);
    let x = delta * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (delta, x, 0.0),
        1 => (x, delta, 0.0),
        2 => (0.0, delta, x),
        3 => (0.0, x, delta),
        4 => (x, 0.0, delta),
        _ => (delta, 0.0, x),
    };
    join((r + min) * 255.0, (g + min) * 255.0, (b + min) * 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i16, y: i16, width: u16, height: u16) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    fn inside(ops: &Ops, band: (i32, i32, i32, i32)) -> bool {
        let (bx, by, bw, bh) = band;
        ops.iter().flat_map(|(_, r)| r).all(|r| {
            let (x, y) = (i32::from(r.x), i32::from(r.y));
            x >= bx
                && y >= by
                && x + i32::from(r.width) <= bx + bw
                && y + i32::from(r.height) <= by + bh
        })
    }

    #[test]
    fn the_scene_never_leaves_its_bands_or_its_monitor() {
        let c = Colours::new(0x88c0d0, 0xeceff4, 0x9aa5b1, 0xd7b36a);
        for a in [
            area(0, 0, 1920, 1080),
            area(1920, 0, 1280, 1024),
            area(0, 0, 800, 600),
            area(-1366, 200, 1366, 768),
            area(0, 0, 3840, 2160),
        ] {
            let s = arrange(a, 300, 7);
            let text = "NIRUX   ***   2026-11-26 12:00   ***   ÅÄÖ";
            for step in 0..50 {
                let t = step as f32 * 0.37;
                let fx = Fx {
                    flash: (step % 3) as f32 / 2.0,
                    wrong: step % 7 == 0,
                };
                let (top, bottom) = scene(&s, t, t * SCROLL_SPEED, text, &c, fx);
                assert!(inside(&top, (s.x, s.top.0, s.w, s.top.1)), "{a:?} top");
                assert!(
                    inside(&bottom, (s.x, s.bottom.0, s.w, s.bottom.1)),
                    "{a:?} bottom"
                );
                assert!(top.len() < 200 && bottom.len() < 200, "few requests");
            }
            // The bands stay apart and on the monitor.
            let y = i32::from(a.y);
            assert!(s.top.0 >= y && s.top.0 + s.top.1 < s.bottom.0);
            assert!(s.bottom.0 + s.bottom.1 <= y + i32::from(a.height));
        }
    }

    #[test]
    fn the_prompt_fits_between_the_bands() {
        for (w, h, u) in [
            (1920, 1080, 27),
            (1366, 768, 27),
            (1024, 600, 30),
            (3840, 2160, 40),
        ] {
            let clock = u * 2;
            let block = prompt_block(u, clock, true);
            let s = arrange(area(0, 0, w, h), block, 1);
            let p = spots(&s, u, clock, u, true);
            let top_end = s.top.0 + s.top.1;
            assert!(
                i32::from(p.info) - u > top_end,
                "{w}x{h}: info under the logo band"
            );
            assert!(i32::from(p.clock) - clock >= top_end);
            assert!(
                i32::from(p.away) + u / 2 < s.bottom.0,
                "{w}x{h}: away above the scroller"
            );
            assert!(p.rows <= p.limit, "{w}x{h}: one status row fits");
            // The logo is big and inside its band.
            assert!(s.logo * 40 <= i32::from(w) * 3 / 4);
            assert!(s.logo_top >= s.top.0 && s.logo_top + s.logo * 8 <= top_end);
        }
    }

    #[test]
    fn the_colours_follow_the_theme() {
        // A grey theme stays grey; the warning colour stays itself.
        let grey = Colours::new(0xa0a0a0, 0xe5e5e5, 0xa8a8a8, 0xd7b36a);
        for bar in [grey.bars[0], grey.bars[1], grey.bars[3]] {
            assert_eq!(bar, 0xa0a0a0);
        }
        assert_eq!(grey.bars[2], 0xd7b36a);
        assert_eq!(rotate_hue(0xff0000, 120.0), 0x00ff00);
        assert_eq!(rotate_hue(0xff0000, -120.0), 0x0000ff);
        assert_eq!(rotate_hue(0x808080, 77.0), 0x808080);
        // A failed attempt paints every bar in the warning colour.
        let s = arrange(area(0, 0, 1280, 800), 0, 3);
        let wrong = Fx {
            flash: 0.0,
            wrong: true,
        };
        let (top, _) = scene(&s, 1.0, 0.0, "", &grey, wrong);
        let shine = lighten(0xd7b36a, 0.55 * 0.62);
        let any_warning = top.iter().any(|(c, _)| {
            let (r, g, b) = split(*c);
            let (sr, sg, sb) = split(shine);
            (r - sr).abs() < 60.0 && (g - sg).abs() < 60.0 && (b - sb).abs() < 60.0
        });
        assert!(any_warning);
    }

    #[test]
    fn large_screens_lower_the_frame_rate_within_limits() {
        assert_eq!(frame_for(0), FRAME);
        assert_eq!(frame_for(1920 * 560), FRAME);
        assert_eq!(frame_for(FULL_RATE_PIXELS), FRAME);
        let k4 = frame_for(3840 * 1120 * 2);
        assert!(k4 > FRAME && k4 <= SLOW_FRAME, "{k4:?}");
        assert_eq!(frame_for(i64::from(u32::MAX)), SLOW_FRAME);
    }

    #[test]
    fn a_beat_flashes_and_fades() {
        let (flash, average) = beat(0.0, 0.8, 0.2, 0.04);
        assert_eq!(flash, 1.0);
        assert!(average > 0.2 && average < 0.8);
        // Steady sound: no new flash, the old one fades within a second.
        let (mut f, mut a) = (1.0, 0.5);
        for _ in 0..25 {
            (f, a) = beat(f, 0.5, a, 0.04);
        }
        assert!(f < 0.01, "{f}");
        // Silence never flashes.
        assert_eq!(beat(0.0, 0.0, 0.0, 0.04).0, 0.0);
    }

    #[test]
    fn the_scroller_names_the_tune_apart_from_the_quote() {
        let facts = vec![
            "2026-11-26  06:19".to_string(),
            "LOCKED, AFK 0 MIN".to_string(),
        ];
        let quote = Some((
            "Keep thyself then simple, good, pure.".to_string(),
            "- Marcus Aurelius, Meditations VI.30".to_string(),
        ));
        let text = scroller_text(&facts, quote, Some("NIRUX - Gimle"));
        assert_eq!(
            text,
            "NIRUX   ***   2026-11-26  06:19   ***   LOCKED, AFK 0 MIN   ***   \
             \"KEEP THYSELF THEN SIMPLE, GOOD, PURE.\"  - MARCUS AURELIUS, MEDITATIONS VI.30   ***   \
             NOW PLAYING  NIRUX - GIMLE   ***   TYPE YOUR PASSWORD TO COME BACK   ***"
        );
        // The tune segment is only artist and title, and nothing but the
        // password hint follows it.
        let after = text.split("NOW PLAYING  ").nth(1).unwrap();
        assert!(after.starts_with("NIRUX - GIMLE   ***   TYPE YOUR PASSWORD"));
        assert!(!text.contains("ESC STOPS") && !text.contains("NIRUX 11"));
        // Without music or quotes.
        assert_eq!(
            scroller_text(&[], None, None),
            "NIRUX   ***   TYPE YOUR PASSWORD TO COME BACK   ***"
        );
        assert!(!scroller_text(&[], None, Some("  ")).contains("NOW PLAYING"));
    }

    #[test]
    fn the_scroller_moves_and_the_text_scrolls_in_from_the_right() {
        let c = Colours::new(0x88c0d0, 0xeceff4, 0x9aa5b1, 0xd7b36a);
        let s = arrange(area(0, 0, 1280, 720), 0, 5);
        let rects = |pos: f32| -> usize {
            let mut ops = Ops::new();
            let band = (s.x, s.bottom.0, s.w, s.bottom.1);
            scroller(&mut ops, &s, band, 0.0, pos, "NIRUX", &c, Fx::default());
            ops.iter().map(|(_, r)| r.len()).sum()
        };
        assert_eq!(rects(0.0), 0, "the text starts off the right edge");
        assert!(rects(40.0) > 0);
        assert_eq!(rects((s.cells() + 41) as f32), 0, "and leaves on the left");
    }
}
