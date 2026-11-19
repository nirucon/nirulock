//! The `ascii` and `ascii-cava` layouts ("NIRUX ascii"): the NIRUX banner
//! from NIRUSCREENSAVER's `ascii` scene, typed out slowly when the screen
//! locks, with "A MINIMAL X11 DESKTOP" under it and a block cursor that
//! blinks after the line once the typing is done. Every monitor shows the
//! banner, centred and sized for it; only the prompt monitor has the clock,
//! the password entry and the away time below. `ascii-cava` adds the CAVA
//! spectrum between the two, exactly as wide as the banner, while music
//! plays.
//!
//! It is drawn like an old terminal: the character just typed glows white
//! for a moment and settles into the theme's accent colour (the text colour
//! when the accent is too dim on the background), and on a large banner a
//! dark scanline runs through every third pixel row of the characters.
//!
//! The password prompt works from the first moment; the typing never waits
//! for it or holds it up. Only what changes is drawn and copied to the
//! screen: a character while typing, the cursor's cell when it blinks. With
//! the display off the cursor stops blinking.

use crate::{core::TextRenderer, display::Area, layout, palette::Palette, Lock, Result};
use std::collections::HashMap;
use std::time::Duration;
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{ChangeGCAux, ConnectionExt as _, Rectangle},
};

/// The banner: seven rows, all the same width. Kept identical to
/// NIRUSCREENSAVER's `ascii` scene.
pub const BANNER: [&str; 7] = [
    "##    ##  ####  #######   ##    ##  ##    ##",
    "###   ##   ##   ##    ##  ##    ##   ##  ## ",
    "####  ##   ##   ##    ##  ##    ##    ####  ",
    "## ## ##   ##   #######   ##    ##     ##   ",
    "##  ####   ##   ##  ##    ##    ##    ####  ",
    "##   ###   ##   ##   ##   ##    ##   ##  ## ",
    "##    ##  ####  ##    ##   ######   ##    ##",
];

/// The line under the banner.
pub const TAGLINE: &str = "A MINIMAL X11 DESKTOP";

/// Typing pace, the same as NIRUSCREENSAVER's scene: the banner one `#` at
/// a time (spaces are skipped, the cursor jumps them), then a breath, then
/// the line at a slower, typed rhythm.
const TYPE_BANNER: Duration = Duration::from_millis(28);
const TYPE_LINE: Duration = Duration::from_millis(70);
const BREATH: Duration = Duration::from_millis(500);
/// The cursor's half period once the text is done: on, then off.
const BLINK: Duration = Duration::from_millis(530);
/// How often the loop wakes while typing.
const FRAME: Duration = Duration::from_millis(16);
/// Scanlines only on a banner at least this tall per row; on a small one
/// they would eat the characters.
const SCANLINE_MIN_ROW: i32 = 24;

/// Every `#` of the banner as (row, column), in typing order.
pub fn order() -> Vec<(usize, usize)> {
    BANNER
        .iter()
        .enumerate()
        .flat_map(|(r, row)| {
            row.chars()
                .enumerate()
                .filter(|(_, ch)| *ch == '#')
                .map(move |(c, _)| (r, c))
        })
        .collect()
}

/// How much is typed at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reveal {
    /// `#` characters of the banner, in [`order`].
    pub hashes: usize,
    /// Characters of the line.
    pub line: usize,
    /// Everything is typed.
    pub done: bool,
    /// The cursor shows (always while typing, every other half period after).
    pub cursor: bool,
}

/// The typing `elapsed` after the lock.
pub fn reveal(elapsed: Duration) -> Reveal {
    let total = order().len();
    let banner_time = TYPE_BANNER * total as u32;
    let line_total = TAGLINE.chars().count();
    if elapsed < banner_time {
        let hashes = (elapsed.as_millis() / TYPE_BANNER.as_millis()) as usize;
        return Reveal {
            hashes,
            line: 0,
            done: false,
            cursor: true,
        };
    }
    let line_start = banner_time + BREATH;
    let line_time = TYPE_LINE * line_total as u32;
    if elapsed < line_start + line_time {
        let line = elapsed
            .checked_sub(line_start)
            .map_or(0, |d| (d.as_millis() / TYPE_LINE.as_millis()) as usize + 1)
            .min(line_total);
        return Reveal {
            hashes: total,
            line,
            done: false,
            cursor: true,
        };
    }
    let since = elapsed - (line_start + line_time);
    Reveal {
        hashes: total,
        line: line_total,
        done: true,
        cursor: (since.as_millis() / BLINK.as_millis()) % 2 == 0,
    }
}

/// When the typing ends.
pub fn typing_time() -> Duration {
    TYPE_BANNER * order().len() as u32 + BREATH + TYPE_LINE * TAGLINE.chars().count() as u32
}

/// How long until [`reveal`] changes after `elapsed`.
pub fn next_change(elapsed: Duration) -> Duration {
    let end = typing_time();
    if elapsed < end {
        return FRAME;
    }
    let into = (elapsed - end).as_millis() % BLINK.as_millis();
    BLINK - Duration::from_millis(into as u64)
}

/// The banner rows with `hashes` characters typed: each row up to its last
/// typed `#`, the spaces before it kept so every `#` sits in its column.
pub fn banner_rows(hashes: usize) -> Vec<String> {
    let typed: Vec<(usize, usize)> = order().into_iter().take(hashes).collect();
    BANNER
        .iter()
        .enumerate()
        .map(
            |(r, row)| match typed.iter().rev().find(|(tr, _)| *tr == r) {
                Some((_, c)) => row.chars().take(c + 1).collect(),
                None => String::new(),
            },
        )
        .collect()
}

/// Where the cursor stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    /// In the banner, on (row, column).
    Banner(usize, usize),
    /// On the line, before character N.
    Line(usize),
}

/// The cursor's cell for a reveal: right after the last typed `#`, at the
/// start of the line during the breath, after the line's last character
/// while it is typed, and one space after it when done.
pub fn cursor(r: Reveal) -> Cursor {
    let order = order();
    if r.hashes < order.len() {
        return match r.hashes.checked_sub(1).map(|i| order[i]) {
            None => Cursor::Banner(0, 0),
            Some((row, col)) => Cursor::Banner(row, col + 1),
        };
    }
    Cursor::Line(if r.done { r.line + 1 } else { r.line })
}

/// The banner's font size for a monitor `width` x `height`: `share` of the
/// width, never so tall that the banner takes more than `max_height` of the
/// height. `advance` is the font's advance width per pixel of size (about
/// 0.6 for a monospace font) and `line` its line height per pixel of size.
pub fn banner_size(
    width: u16,
    height: u16,
    share: f32,
    max_height: f32,
    advance: f32,
    line: f32,
) -> f32 {
    let columns = BANNER[0].chars().count() as f32;
    let by_width = f32::from(width) * share / (columns * advance.max(0.1));
    let by_height = f32::from(height) * max_height / (BANNER.len() as f32 * line.max(0.1));
    by_width.min(by_height).clamp(6.0, 96.0)
}

/// Relative luminance of `0xRRGGBB` (WCAG).
fn luminance(colour: u32) -> f32 {
    let channel = |shift: u32| {
        let c = ((colour >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG contrast between two colours, 1 to 21.
fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The banner colour: the theme's accent when it reads well on the
/// background (contrast 4.5 or more), otherwise the text colour.
pub fn banner_colour(accent: u32, foreground: u32, background: u32) -> u32 {
    if contrast(accent, background) >= 4.5 {
        accent
    } else {
        foreground
    }
}

/// The banner and line laid out on one monitor.
#[derive(Clone, Copy, Debug)]
struct Screen {
    /// The banner font's size, as `f32::to_bits`, the renderer cache key.
    key: u32,
    left: i32,
    top: i32,
    /// Banner row height, ascent and advance (monospace).
    row: i32,
    ascent: i32,
    adv: i32,
    /// The line: left edge, baseline, advance per character, ascent and
    /// descent.
    tag_left: i32,
    tag_y: i32,
    tag_adv: i32,
    tag_ascent: i32,
    tag_descent: i32,
}

impl Screen {
    fn banner_rect(&self) -> (i32, i32, i32, i32) {
        let w = self.adv * BANNER[0].chars().count() as i32;
        (self.left, self.top, w, self.row * BANNER.len() as i32)
    }

    /// The whole text block, for copying to the screen.
    fn block(&self) -> (i16, i16, u16, u16) {
        let (x, y, w, _) = self.banner_rect();
        let tag_right = self.tag_left + self.tag_adv * (TAGLINE.chars().count() as i32 + 3);
        let left = x.min(self.tag_left) - self.adv;
        let right = (x + w).max(tag_right) + self.adv;
        let bottom = self.tag_y + self.tag_descent + 2;
        rect16(left, y, right - left, bottom - y)
    }

    fn cursor_rect(&self, at: Cursor) -> (i16, i16, u16, u16) {
        match at {
            Cursor::Banner(r, c) => {
                let h = self.row * 8 / 10;
                rect16(
                    self.left + self.adv * c as i32,
                    self.top + self.row * r as i32 + (self.row - h) / 2,
                    self.adv,
                    h,
                )
            }
            Cursor::Line(i) => rect16(
                self.tag_left + self.tag_adv * i as i32,
                self.tag_y - self.tag_ascent,
                self.tag_adv,
                self.tag_ascent + self.tag_descent / 2,
            ),
        }
    }
}

fn rect16(x: i32, y: i32, w: i32, h: i32) -> (i16, i16, u16, u16) {
    (
        layout::clamp16(x),
        layout::clamp16(y),
        w.clamp(1, i32::from(u16::MAX)) as u16,
        h.clamp(1, i32::from(u16::MAX)) as u16,
    )
}

/// Where the prompt monitor's other lines go, baselines in root coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Spots {
    /// The spectrum panel (`ascii-cava`).
    pub panel: Option<(i16, i16, u16, u16)>,
    /// The pause music's info line.
    pub info: i16,
    pub clock: i16,
    pub entry: i16,
    pub away: i16,
    /// The first status row and the step between rows.
    pub rows: i32,
    pub step: i32,
    /// No row below this baseline.
    pub limit: i32,
}

/// The layouts' state between frames.
#[derive(Default)]
pub struct State {
    screens: Vec<Screen>,
    renderers: HashMap<u32, TextRenderer>,
    /// What is drawn in the backing pixmap now.
    drawn: Option<Reveal>,
    /// The cursor cell as drawn, if it shows.
    cursor: Option<Cursor>,
}

impl Lock {
    /// Lay the banner out on every monitor and the prompt lines on the
    /// prompt monitor. `panel` reserves the spectrum (`ascii-cava`).
    pub(crate) fn ascii_layout(&mut self, area: Area, panel: bool) -> Spots {
        let screens = if self.outputs.is_empty() {
            vec![area]
        } else {
            self.outputs.clone()
        };
        let base = self.cfg.font_size.max(1.0);
        let line = i32::from(self.renderer.line_height()).max(1);
        let advance = f32::from(self.renderer.width("#")) / base;
        let line_ratio = line as f32 / base;
        let tag_adv = i32::from(self.renderer.width("A"));
        let tag_ascent = i32::from(self.renderer.ascent());
        let tag_descent = i32::from(self.renderer.descent());
        let music = self.music.is_some();
        let u = line;
        let mut spots = None;
        self.ascii.screens.clear();
        for screen in screens {
            let prompt = screen == area;
            let (share, max_height) = match (prompt, panel) {
                (true, true) => (0.46, 0.2),
                (true, false) => (0.5, 0.26),
                (false, _) => (0.6, 0.32),
            };
            let size = banner_size(
                screen.width,
                screen.height,
                share,
                max_height,
                advance,
                line_ratio,
            );
            let key = size.to_bits();
            let renderer = self
                .ascii
                .renderers
                .entry(key)
                .or_insert_with(|| self.renderer.at_size(size));
            let row = i32::from(renderer.line_height()).max(1);
            let ascent = i32::from(renderer.ascent());
            let adv = i32::from(renderer.width("#")).max(1);
            let banner_w = adv * BANNER[0].chars().count() as i32;
            let banner_h = row * BANNER.len() as i32;
            let (sx, sy) = (i32::from(screen.x), i32::from(screen.y));
            let (sw, sh) = (i32::from(screen.width), i32::from(screen.height));
            let centre = sx + sw / 2;
            let tag_block = u * 3 / 4 + u;
            let panel_h = (sh * 11 / 100).clamp(56, 150);
            let panel_block = if prompt && panel {
                u * 3 / 2 + panel_h + if music { u * 5 / 2 } else { u * 7 / 4 }
            } else if prompt {
                u * 5 / 2
            } else {
                0
            };
            let prompt_block = if prompt { u + u * 3 } else { 0 };
            let block = banner_h + tag_block + panel_block + prompt_block;
            let top = (sy + sh * 46 / 100 - block / 2).max(sy + u);
            let tag_y = top + banner_h + u * 3 / 4 + tag_ascent;
            let tag_w = i32::from(self.renderer.width(TAGLINE));
            self.ascii.screens.push(Screen {
                key,
                left: centre - banner_w / 2,
                top,
                row,
                ascent,
                adv,
                tag_left: centre - tag_w / 2,
                tag_y,
                tag_adv,
                tag_ascent,
                tag_descent,
            });
            if prompt {
                let mut y = tag_y + tag_descent;
                let panel_rect = panel.then(|| {
                    let width = layout::snap_spectrum(banner_w.min(sw - 2 * u).max(64));
                    let r = rect16(centre - width / 2, y + u * 3 / 2, width, panel_h);
                    y += u * 3 / 2 + panel_h;
                    r
                });
                let info = if panel {
                    y + u * 11 / 10 + tag_ascent / 2
                } else {
                    tag_y + u * 5 / 4
                };
                let clock = if panel {
                    y + if music { u * 5 / 2 } else { u * 7 / 4 } + tag_ascent
                } else {
                    tag_y + u * 5 / 2
                };
                let entry = clock + u * 3 / 2;
                let away = entry + u * 3 / 2;
                spots = Some(Spots {
                    panel: panel_rect,
                    info: layout::clamp16(info),
                    clock: layout::clamp16(clock),
                    entry: layout::clamp16(entry),
                    away: layout::clamp16(away),
                    rows: away + u * 19 / 10,
                    step: u * 5 / 4,
                    limit: sy + sh - u,
                });
            }
        }
        // The fallback is never used: the prompt area is one of the screens.
        spots.unwrap_or(Spots {
            panel: None,
            info: 0,
            clock: 0,
            entry: 0,
            away: 0,
            rows: 0,
            step: u,
            limit: 0,
        })
    }

    /// Draw the banners, lines and cursor as they stand now into the
    /// backing pixmap (after a full clear), without copying them.
    pub(crate) fn ascii_paint(&mut self, pal: &Palette) -> Result<()> {
        let r = reveal(self.started.elapsed());
        let accent = banner_colour(pal.focus, pal.foreground, pal.background);
        let rows = banner_rows(r.hashes);
        let line: String = TAGLINE.chars().take(r.line).collect();
        let glow =
            (!r.done && r.hashes < order().len() && r.hashes > 0).then(|| order()[r.hashes - 1]);
        for screen in self.ascii.screens.clone() {
            let Some(banner) = self.ascii.renderers.get_mut(&screen.key) else {
                continue;
            };
            for (i, text) in rows.iter().enumerate() {
                if text.is_empty() {
                    continue;
                }
                banner.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    layout::clamp16(screen.left),
                    layout::clamp16(screen.top + screen.ascent + screen.row * i as i32),
                    text,
                    accent,
                    pal.background,
                )?;
            }
            if let Some((gr, gc)) = glow {
                draw_hash(self, &screen, gr, gc, pal.foreground, pal.background)?;
            }
            if screen.row >= SCANLINE_MIN_ROW {
                let (x, y, w, h) = screen.banner_rect();
                scanlines(self, x, y, w, h, pal.background)?;
            }
            if !line.is_empty() {
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    layout::clamp16(screen.tag_left),
                    layout::clamp16(screen.tag_y),
                    &line,
                    pal.muted,
                    pal.background,
                )?;
            }
            if r.cursor {
                let (x, y, w, h) = screen.cursor_rect(cursor(r));
                fill(self, x, y, w, h, cursor_colour(r, pal, accent))?;
            }
        }
        self.ascii.drawn = Some(r);
        self.ascii.cursor = r.cursor.then(|| cursor(r));
        Ok(())
    }

    /// Advance the typing or the cursor's blink, drawing and copying only
    /// what changed. `display_on` false holds everything as it is.
    pub(crate) fn ascii_tick(&mut self, display_on: bool) -> Result<()> {
        if !display_on || self.ascii.screens.is_empty() {
            return Ok(());
        }
        let Some(before) = self.ascii.drawn else {
            return Ok(());
        };
        let r = reveal(self.started.elapsed());
        if r == before {
            return Ok(());
        }
        let pal = self.cfg.palette.clone();
        let accent = banner_colour(pal.focus, pal.foreground, pal.background);
        let order = order();
        let old_cursor = self.ascii.cursor;
        let new_cursor = r.cursor.then(|| cursor(r));
        for screen in self.ascii.screens.clone() {
            if let Some(at) = old_cursor {
                let (x, y, w, h) = screen.cursor_rect(at);
                fill(self, x, y, w, h, pal.background)?;
            }
            // The character typed before settles into the accent; the new
            // one glows white until the next.
            if before.hashes > 0 && r.hashes > before.hashes {
                let (row, col) = order[before.hashes - 1];
                draw_hash(self, &screen, row, col, accent, pal.background)?;
            }
            for i in before.hashes..r.hashes {
                let (row, col) = order[i];
                let colour = if i + 1 == r.hashes && r.hashes < order.len() {
                    pal.foreground
                } else {
                    accent
                };
                draw_hash(self, &screen, row, col, colour, pal.background)?;
            }
            for (i, ch) in TAGLINE.chars().enumerate().take(r.line).skip(before.line) {
                let mut buf = [0u8; 4];
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    layout::clamp16(screen.tag_left + screen.tag_adv * i as i32),
                    layout::clamp16(screen.tag_y),
                    ch.encode_utf8(&mut buf),
                    pal.muted,
                    pal.background,
                )?;
            }
            if let Some(at) = new_cursor {
                let (x, y, w, h) = screen.cursor_rect(at);
                fill(self, x, y, w, h, cursor_colour(r, &pal, accent))?;
            }
            let blink_only = r.hashes == before.hashes
                && r.line == before.line
                && old_cursor.map_or(true, |at| at == cursor(r));
            let (x, y, w, h) = if blink_only {
                // Only the blink, in the same cell: copy that cell.
                screen.cursor_rect(cursor(r))
            } else {
                screen.block()
            };
            self.surface.present_rect(x, y, w, h)?;
        }
        self.ascii.drawn = Some(r);
        self.ascii.cursor = new_cursor;
        Ok(())
    }

    /// How long the loop may sleep before the ascii layouts change.
    pub(crate) fn ascii_timeout(&self, display_on: bool) -> Option<Duration> {
        (self.cfg.layout.is_ascii() && display_on).then(|| next_change(self.started.elapsed()))
    }
}

fn cursor_colour(r: Reveal, pal: &Palette, accent: u32) -> u32 {
    if r.done {
        accent
    } else {
        pal.foreground
    }
}

/// One `#` of the banner at (row, column), with its scanlines.
fn draw_hash(lock: &mut Lock, s: &Screen, row: usize, col: usize, fg: u32, bg: u32) -> Result<()> {
    let x = s.left + s.adv * col as i32;
    let top = s.top + s.row * row as i32;
    if let Some(banner) = lock.ascii.renderers.get_mut(&s.key) {
        banner.draw_left(
            &lock.surface.c,
            lock.surface.gc,
            lock.surface.back,
            lock.surface.depth,
            layout::clamp16(x),
            layout::clamp16(top + s.ascent),
            "#",
            fg,
            bg,
        )?;
    }
    if s.row >= SCANLINE_MIN_ROW {
        scanlines(lock, x, top, s.adv, s.row, bg)?;
    }
    Ok(())
}

/// Dark lines through every third pixel row of a rectangle: the CRT look.
fn scanlines(lock: &Lock, x: i32, y: i32, w: i32, h: i32, bg: u32) -> Result<()> {
    let lines: Vec<Rectangle> = (0..h)
        .filter(|dy| (y + dy) % 3 == 2)
        .map(|dy| {
            let (x, y, w, _) = rect16(x, y + dy, w, 1);
            Rectangle {
                x,
                y,
                width: w,
                height: 1,
            }
        })
        .collect();
    if lines.is_empty() {
        return Ok(());
    }
    let s = &lock.surface;
    s.c.change_gc(s.gc, &ChangeGCAux::new().foreground(bg))?;
    s.c.poly_fill_rectangle(s.back, s.gc, &lines)?;
    Ok(())
}

fn fill(lock: &Lock, x: i16, y: i16, w: u16, h: u16, colour: u32) -> Result<()> {
    let s = &lock.surface;
    s.c.change_gc(s.gc, &ChangeGCAux::new().foreground(colour))?;
    s.c.poly_fill_rectangle(
        s.back,
        s.gc,
        &[Rectangle {
            x,
            y,
            width: w,
            height: h,
        }],
    )?;
    let _ = s.c.flush();
    Ok(())
}

#[cfg(test)]
fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_is_rectangular_and_reads_nirux() {
        let w = BANNER[0].chars().count();
        assert!(BANNER.iter().all(|r| r.chars().count() == w));
        assert!(BANNER
            .iter()
            .all(|r| r.chars().all(|c| c == '#' || c == ' ')));
        assert_eq!(w, 44);
    }

    #[test]
    fn typing_is_slow_then_the_cursor_blinks() {
        assert_eq!(reveal(at(0)).hashes, 0);
        assert!(reveal(at(0)).cursor);
        let total = order().len();
        let banner = TYPE_BANNER * total as u32;
        assert_eq!(reveal(banner + at(100)).hashes, total);
        assert_eq!(reveal(banner + at(100)).line, 0, "a breath first");
        assert_eq!(reveal(banner + BREATH).line, 1);
        let end = typing_time();
        // About seven seconds: slow, like the screensaver.
        assert!(
            end > Duration::from_secs(5) && end < Duration::from_secs(10),
            "{end:?}"
        );
        let r = reveal(end);
        assert!(r.done && r.cursor && r.line == TAGLINE.chars().count());
        assert!(!reveal(end + BLINK).cursor);
        assert!(reveal(end + BLINK * 2).cursor);
        // Typing frames, then the next blink edge.
        assert_eq!(next_change(at(10)), FRAME);
        assert_eq!(next_change(end + at(30)), BLINK - at(30));
    }

    #[test]
    fn rows_keep_their_columns_and_skip_nothing() {
        assert!(banner_rows(0).iter().all(String::is_empty));
        let first = banner_rows(2);
        assert_eq!(first[0], "##");
        // The third # of row 0 is in column 7: the spaces before it are kept.
        assert_eq!(banner_rows(3)[0], "##    #");
        let all = banner_rows(order().len());
        for (row, full) in all.iter().zip(BANNER) {
            assert_eq!(row.as_str(), full.trim_end());
        }
    }

    #[test]
    fn the_cursor_follows_the_text() {
        assert_eq!(cursor(reveal(at(0))), Cursor::Banner(0, 0));
        assert_eq!(cursor(reveal(TYPE_BANNER * 3)), Cursor::Banner(0, 7));
        let banner = TYPE_BANNER * order().len() as u32;
        assert_eq!(cursor(reveal(banner)), Cursor::Line(0));
        let end = typing_time();
        assert_eq!(
            cursor(reveal(end)),
            Cursor::Line(TAGLINE.chars().count() + 1)
        );
    }

    #[test]
    fn the_banner_fits_every_monitor() {
        for (w, h) in [
            (1280u16, 800u16),
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
            (1080, 1920),
        ] {
            let size = banner_size(w, h, 0.46, 0.2, 0.6, 1.2);
            assert!(44.0 * 0.6 * size <= f32::from(w) * 0.47, "{w}x{h}: {size}");
            assert!(7.0 * 1.2 * size <= f32::from(h) * 0.21, "{w}x{h}: {size}");
        }
    }

    #[test]
    fn a_dim_accent_falls_back_to_the_text_colour() {
        assert_eq!(banner_colour(0x5a5a60, 0xe5e5e5, 0x000000), 0xe5e5e5);
        assert_eq!(banner_colour(0x7fbf6a, 0xe5e5e5, 0x000000), 0x7fbf6a);
    }
}
