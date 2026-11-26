//! The `satie` layout: a Commodore 64 loading screen of Erik Satie.
//!
//! Like a game loading from tape, the screen says PRESS PLAY ON TAPE,
//! searches and finds ERIK SATIE, and then the picture comes in: the bitmap
//! character cell by cell, top to bottom, in two shades while the border
//! flickers with loader stripes, then the colours cell by cell, until it
//! says READY. and the border settles. Every [`RELOAD`] it loads again.
//!
//! The picture is Henri Manuel's portrait of Satie from about 1920 (public
//! domain, see `docs/CREDITS.md`), converted offline by
//! `tools/satie-picture.py` to a 160x200 multicolour bitmap (pixels twice
//! as wide as tall) with six brightness levels after ordered dithering,
//! 16,000 bytes in the binary. The levels become colours of the active
//! theme at run time: black, a dark grey, the accent in three shades and
//! white. A text strip under the picture carries the title, the clock, the
//! away time and the tune, in NIRUSID's 8x8 font.
//!
//! The canvas is drawn with PutImage, only the rows that changed, so the
//! loaded picture costs nothing to keep on the screen; the border is a few
//! rectangles. The prompt lines sit on black below the picture as in the
//! `attract` layout. A failed attempt turns the border to the warning
//! colour. While music plays the settled border brightens a little on the
//! beat. With the display off nothing is drawn.
//!
//! NIRUX's arrangement of Satie's Gymnopedie No. 1 plays as the pause music
//! when it is installed ([`TUNE`]), on its own, again and again.

use crate::{
    chunky::{self, slot::*, spots, Canvas, Palette, Screen},
    demo::{self, Spots},
    display::Area,
    layout, Lock, Result,
};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    ChangeGCAux, ConnectionExt as _, ImageFormat, ImageOrder, Rectangle,
};

/// The canvas: the 160x200 picture as 320x200 square pixels, and two text
/// lines under it.
pub const W: i32 = 320;
pub const H: i32 = 224;
/// The picture in its own wide pixels.
pub const PW: i32 = 160;
pub const PH: i32 = 200;
/// The tune that plays with this layout when NIRUX has installed it, in
/// NIRUX's music folder.
pub const TUNE: &str = "nirux-12-gymnopedie.ogg";
/// The picture, packed two levels to a byte.
const PICTURE: &[u8] = include_bytes!("../assets/satie.bin");
const LEVELS: u8 = 6;
/// Character cells, 4 wide pixels by 8 rows, like the C64's.
const CELL_W: i32 = 4;
const CELL_H: i32 = 8;
const CELLS: u32 = ((PW / CELL_W) * (PH / CELL_H)) as u32;
/// The loading story, in seconds from the start of a load.
const TAPE: f32 = 1.5;
const SEARCH: f32 = 3.5;
const FOUND: f32 = 5.0;
const BITMAP: f32 = 13.0;
const COLOUR: f32 = 6.0;
/// The picture loads again this long after the last load began.
pub const RELOAD: f32 = 300.0;
/// Frame intervals: while loading or following the beat, and at rest.
const RESTING: Duration = Duration::from_millis(250);
/// At most this many image bytes in one request.
const MAX_IMAGE_BYTES: usize = 200_000;

/// The brightness levels, unpacked.
fn picture() -> &'static [u8] {
    static P: OnceLock<Vec<u8>> = OnceLock::new();
    P.get_or_init(|| {
        PICTURE
            .iter()
            .flat_map(|b| [b >> 4, b & 15])
            .map(|l| l.min(LEVELS - 1))
            .collect()
    })
}

/// Where a load is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Tape,
    Search,
    Found,
    /// So many cells of the bitmap are in.
    Bitmap(u32),
    /// The bitmap is in and so many cells have their colours.
    Colour(u32),
    Ready,
}

impl Stage {
    pub fn at(t: f32) -> Self {
        let t = t.rem_euclid(RELOAD);
        if t < TAPE {
            Self::Tape
        } else if t < SEARCH {
            Self::Search
        } else if t < FOUND {
            Self::Found
        } else if t < FOUND + BITMAP {
            Self::Bitmap(((t - FOUND) / BITMAP * CELLS as f32) as u32)
        } else if t < FOUND + BITMAP + COLOUR {
            Self::Colour(((t - FOUND - BITMAP) / COLOUR * CELLS as f32) as u32)
        } else {
            Self::Ready
        }
    }

    /// The loader stripes run in the border.
    pub fn loading(self) -> bool {
        matches!(self, Self::Bitmap(_) | Self::Colour(_))
    }
}

/// The words in the text strip.
#[derive(Clone, Copy, Debug, Default)]
pub struct Texts<'a> {
    /// "HH:MM".
    pub clock: &'a str,
    /// "AFK 3 MIN".
    pub away: &'a str,
    /// "Artist - Title" of the tune that plays.
    pub tune: Option<&'a str>,
}

/// The colour of brightness level `level`, in the bitmap's two shades
/// while it loads, or in full colour.
fn shade(level: u8, coloured: bool) -> u8 {
    if coloured {
        [BLACK, GREY_DARK, ACCENT_DARK, ACCENT, ACCENT_LIGHT, WHITE][usize::from(level.min(5))]
    } else {
        [BLACK, GREY_DARK, GREY_DARK, GREY, GREY_LIGHT, GREY_LIGHT][usize::from(level.min(5))]
    }
}

/// Draw the screen `t` seconds into a load.
pub fn render(c: &mut Canvas, t: f32, texts: Texts) {
    c.clear(BLACK);
    let stage = Stage::at(t);
    let (bitmap, colour) = match stage {
        Stage::Tape | Stage::Search | Stage::Found => (0, 0),
        Stage::Bitmap(n) => (n, 0),
        Stage::Colour(n) => (CELLS, n),
        Stage::Ready => (CELLS, CELLS),
    };
    let pic = picture();
    let columns = (PW / CELL_W) as u32;
    for cell in 0..bitmap.min(CELLS) {
        let (cx, cy) = ((cell % columns) as i32, (cell / columns) as i32);
        let coloured = cell < colour;
        for y in cy * CELL_H..(cy + 1) * CELL_H {
            let row = c.row_mut(y);
            for x in cx * CELL_W..(cx + 1) * CELL_W {
                let level = pic[(y * PW + x) as usize];
                let colour = shade(level, coloured);
                row[(x * 2) as usize] = colour;
                row[(x * 2 + 1) as usize] = colour;
            }
        }
    }
    let plain = |s: &str| crate::pixfont::plain(s).to_uppercase();
    let line1 = PH + 3;
    let line2 = PH + 14;
    c.text(4, line1, "ERIK SATIE 1866-1925", ACCENT_LIGHT, 1);
    c.text(W - 4 - 11 * 8, line1, "GYMNOPEDIES", WHITE, 1);
    let status = match stage {
        Stage::Tape => "PRESS PLAY ON TAPE".to_string(),
        Stage::Search => "SEARCHING".to_string(),
        Stage::Found => "FOUND ERIK SATIE".to_string(),
        Stage::Bitmap(_) | Stage::Colour(_) => {
            let dots = ".".repeat(((t * 2.0) as usize) % 4);
            format!("LOADING{dots}")
        }
        Stage::Ready => match texts.tune {
            Some(tune) => plain(tune),
            None => format!("READY.  {}", plain(texts.away)),
        },
    };
    let clock = plain(texts.clock);
    let room = 38 - clock.chars().count();
    let status: String = status.chars().take(room).collect();
    c.text(4, line2, &status, GREY_LIGHT, 1);
    c.text(
        W - 4 - clock.chars().count() as i32 * 8,
        line2,
        &clock,
        WHITE,
        1,
    );
}

/// The loader stripes for frame `frame`, `height` border units tall: (top,
/// height, colour) from top to bottom, covering it all.
pub fn stripes(frame: u64, height: i32) -> Vec<(i32, i32, u8)> {
    const COLOURS: [u8; 6] = [ACCENT_LIGHT, COOL, FLASH, ACCENT_DARK, COOL_LIGHT, WARNING];
    let mut rng = crate::arcade::Rng::new(frame.wrapping_mul(0x9e37_79b9) ^ 0x5a7e);
    let mut out = Vec::new();
    let mut y = 0;
    let mut k = rng.below(COLOURS.len() as u32) as usize;
    while y < height {
        let h = (2 + rng.below(9) as i32).min(height - y);
        out.push((y, h, COLOURS[k % COLOURS.len()]));
        k += 1 + (rng.below(2) as usize);
        y += h;
    }
    out
}

/// Lay out one monitor: a C64-like border, an eighth of the canvas wide.
pub fn arrange(area: Area, block: i32) -> Screen {
    chunky::arrange(area, block, W, H, |scale| scale * 20)
}

/// The layout's state between frames.
#[derive(Default)]
pub struct State {
    screens: Vec<Screen>,
    frame: Duration,
    next: Option<Instant>,
    start: Option<Instant>,
    frames: u64,
    canvas: Option<Canvas>,
    shown: Option<Canvas>,
    flash: f32,
    bass: f32,
    last: Option<Instant>,
    /// The border as drawn: a flat colour, or `None` for stripes.
    border: Option<Option<u32>>,
}

impl Lock {
    pub(crate) fn satie_layout(&mut self, area: Area) -> Spots {
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
        let block = demo::prompt_block(u, clock_ascent, music);
        let mut out = None;
        self.satie.screens.clear();
        for screen in screens {
            let prompt = screen == area;
            let s = arrange(screen, if prompt { block } else { 0 });
            if prompt {
                out = Some(spots(&s, u, clock_ascent, info_ascent, music));
            }
            self.satie.screens.push(s);
        }
        let pixels = self
            .satie
            .screens
            .iter()
            .filter_map(Screen::outer)
            .map(|(_, _, w, h)| i64::from(w) * i64::from(h))
            .sum();
        self.satie.frame = demo::frame_for(pixels);
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

    /// Draw the screen as it is now into the backing pixmap: everything
    /// with `full`, otherwise what changed. Returns whether the border
    /// changed and the canvas rows drawn.
    pub(crate) fn satie_paint(&mut self, full: bool) -> Result<(bool, Option<(i32, i32)>)> {
        let now = Instant::now();
        let start = *self.satie.start.get_or_insert(now);
        let t = now.saturating_duration_since(start).as_secs_f32();
        let dt = self.satie.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.satie.last = Some(now);
        self.satie.frames += 1;
        let spectrum = self
            .visual
            .as_mut()
            .and_then(crate::visual::Visual::spectrum)
            .map(|values| {
                let low = &values[..values.len().min(6)];
                low.iter().sum::<f32>() / low.len().max(1) as f32
            });
        let following = spectrum.is_some();
        let st = &mut self.satie;
        (st.flash, st.bass) = demo::beat(st.flash, spectrum.unwrap_or(0.0), st.bass, dt);
        let clock = chrono::Local::now().format("%H:%M").to_string();
        let away = crate::core::away_text(self.started.elapsed().as_secs());
        let canvas = st.canvas.get_or_insert_with(|| Canvas::new(W, H));
        render(
            canvas,
            t,
            Texts {
                clock: &clock,
                away: &away,
                tune: self.music_tune.as_deref(),
            },
        );
        let rows = chunky::changed_rows(canvas, if full { None } else { st.shown.as_ref() });
        let pal = &self.cfg.palette;
        let accent = crate::ascii::banner_colour(pal.focus, pal.foreground, pal.background);
        let palette = Palette::new(accent, pal.foreground, pal.muted, self.cfg.warning);
        let stage = Stage::at(t);
        let border = if self.state == crate::State::Wrong {
            Some(self.cfg.warning)
        } else if stage.loading() {
            None
        } else {
            let base = palette.0[usize::from(ACCENT_DARK)];
            let lit = palette.0[usize::from(ACCENT)];
            // Quantised, so a fading beat redraws the border a few times.
            let k = (st.flash * 4.0).round() / 8.0;
            Some(demo::mix(base, lit, k))
        };
        let border_changed = full || border.is_none() || st.border != Some(border);
        let s = &self.surface;
        let big_endian = s.c.setup().image_byte_order == ImageOrder::MSB_FIRST;
        for screen in &st.screens {
            let (Some(place), Some((ox, oy, ow, oh))) = (screen.place, screen.outer()) else {
                continue;
            };
            if let Some((y0, y1)) = rows {
                let line = (W * place.sx * place.sy * 4).max(1) as usize;
                let step = (MAX_IMAGE_BYTES / line).max(1) as i32;
                let mut y = y0;
                while y < y1 {
                    let end = (y + step).min(y1);
                    let (bytes, w, h) =
                        chunky::image_rows(canvas, &palette, &place, y, end, big_endian);
                    s.c.put_image(
                        ImageFormat::Z_PIXMAP,
                        s.back,
                        s.gc,
                        w,
                        h,
                        layout::clamp16(place.x),
                        layout::clamp16(place.y + y * place.sy),
                        0,
                        s.depth,
                        &bytes,
                    )?;
                    y = end;
                }
            }
            if border_changed {
                let b = screen.bezel;
                let (_, iy, _, ih) = place.rect(W, H);
                let rect = |x: i32, y: i32, w: i32, h: i32| Rectangle {
                    x: layout::clamp16(x),
                    y: layout::clamp16(y),
                    width: w.clamp(0, i32::from(u16::MAX)) as u16,
                    height: h.clamp(0, i32::from(u16::MAX)) as u16,
                };
                // A band of the border from y0 to y1: full width above and
                // below the picture, the two sides beside it.
                let band = |y0: i32, y1: i32| -> Vec<Rectangle> {
                    let mut out = Vec::new();
                    let (a, z) = (y0.max(iy), y1.min(iy + ih));
                    if a >= z {
                        out.push(rect(ox, y0, ow, y1 - y0));
                        return out;
                    }
                    if y0 < a {
                        out.push(rect(ox, y0, ow, a - y0));
                    }
                    out.push(rect(ox, a, b, z - a));
                    out.push(rect(ox + ow - b, a, b, z - a));
                    if z < y1 {
                        out.push(rect(ox, z, ow, y1 - z));
                    }
                    out
                };
                match border {
                    Some(colour) => {
                        s.c.change_gc(s.gc, &ChangeGCAux::new().foreground(colour))?;
                        s.c.poly_fill_rectangle(s.back, s.gc, &band(oy, oy + oh))?;
                    }
                    None => {
                        let unit = place.sy.max(1);
                        for (y, h, colour) in stripes(st.frames, (oh + unit - 1) / unit) {
                            let (y0, y1) = (oy + y * unit, (oy + (y + h) * unit).min(oy + oh));
                            s.c.change_gc(
                                s.gc,
                                &ChangeGCAux::new().foreground(palette.0[usize::from(colour)]),
                            )?;
                            s.c.poly_fill_rectangle(s.back, s.gc, &band(y0, y1))?;
                        }
                    }
                }
            }
        }
        st.border = Some(border);
        let old = st.shown.take();
        st.shown = st.canvas.take();
        st.canvas = old;
        let frame = if stage.loading() || following || st.flash > 0.01 {
            st.frame.max(demo::FRAME)
        } else {
            RESTING
        };
        let due = st
            .next
            .map_or(now, |due| due.max(now.checked_sub(frame).unwrap_or(now)));
        st.next = Some(due + frame);
        Ok((border_changed, rows))
    }

    pub(crate) fn satie_tick(&mut self, display_on: bool) -> Result<()> {
        if !display_on || self.satie.next.map_or(true, |due| Instant::now() < due) {
            return Ok(());
        }
        let (border, rows) = self.satie_paint(false)?;
        for screen in &self.satie.screens {
            let (Some(place), Some((ox, oy, ow, oh))) = (screen.place, screen.outer()) else {
                continue;
            };
            let (x, y, w, h) = if border {
                (ox, oy, ow, oh)
            } else if let Some((y0, y1)) = rows {
                (
                    place.x,
                    place.y + y0 * place.sy,
                    W * place.sx,
                    (y1 - y0) * place.sy,
                )
            } else {
                continue;
            };
            self.surface.present_rect(
                layout::clamp16(x),
                layout::clamp16(y),
                w.clamp(0, i32::from(u16::MAX)) as u16,
                h.clamp(0, i32::from(u16::MAX)) as u16,
            )?;
        }
        Ok(())
    }

    pub(crate) fn satie_timeout(&self, display_on: bool) -> Option<Duration> {
        if !display_on || self.cfg.layout != crate::config::Layout::Satie {
            return None;
        }
        Some(self.satie.next.map_or(Duration::ZERO, |due| {
            due.saturating_duration_since(Instant::now())
        }))
    }
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

    #[test]
    fn the_picture_is_the_converted_portrait() {
        assert_eq!(PICTURE.len(), (PW * PH / 2) as usize);
        let p = picture();
        assert_eq!(p.len(), (PW * PH) as usize);
        assert!(p.iter().all(|&l| l < LEVELS));
        // A photograph: every level is used, dark and light both plenty.
        for level in 0..LEVELS {
            let n = p.iter().filter(|&&l| l == level).count();
            assert!(n > 500, "level {level}: {n} pixels");
        }
    }

    #[test]
    fn the_load_tells_its_story_in_order_and_starts_again() {
        let mut last = Stage::Tape;
        let mut seen = vec![last];
        let mut cells = 0;
        let mut t = 0.0;
        while t < RELOAD {
            let s = Stage::at(t);
            if std::mem::discriminant(&s) != std::mem::discriminant(&last) {
                seen.push(s);
                cells = 0;
            }
            if let Stage::Bitmap(n) | Stage::Colour(n) = s {
                assert!(n >= cells && n <= CELLS, "{t}: {n}");
                cells = n;
            }
            last = s;
            t += 0.04;
        }
        let kinds: Vec<_> = seen.iter().map(std::mem::discriminant).collect();
        let want = [
            Stage::Tape,
            Stage::Search,
            Stage::Found,
            Stage::Bitmap(0),
            Stage::Colour(0),
            Stage::Ready,
        ];
        assert_eq!(
            kinds,
            want.iter().map(std::mem::discriminant).collect::<Vec<_>>()
        );
        assert_eq!(Stage::at(RELOAD + 0.1), Stage::Tape);
        assert!(Stage::at(FOUND + 1.0).loading() && !Stage::at(RELOAD - 1.0).loading());
    }

    #[test]
    fn the_picture_loads_cell_by_cell_and_deterministically() {
        let texts = Texts {
            clock: "12:34",
            away: "afk 3 min",
            tune: None,
        };
        let mut a = Canvas::new(W, H);
        let mut b = Canvas::new(W, H);
        render(&mut a, FOUND + BITMAP / 2.0, texts);
        render(&mut b, FOUND + BITMAP / 2.0, texts);
        assert_eq!(a, b);
        // Half the bitmap: the top half has the picture in two shades, the
        // bottom is still black.
        let lit = |c: &Canvas, y0: i32, y1: i32| {
            (y0..y1)
                .flat_map(|y| (0..W).map(move |x| (x, y)))
                .filter(|&(x, y)| c.get(x, y) != BLACK)
                .count()
        };
        assert!(lit(&a, 0, 90) > 5000);
        assert_eq!(lit(&a, 110, PH), 0);
        assert!(a.px[..(W * 90) as usize]
            .iter()
            .all(|&c| [BLACK, GREY_DARK, GREY, GREY_LIGHT].contains(&c)));
        // Loaded: the whole picture in the theme's ramp, wide pixels.
        let mut done = Canvas::new(W, H);
        render(&mut done, RELOAD - 1.0, texts);
        assert!(lit(&done, 150, PH) > 3000);
        for y in 0..PH {
            for x in (0..W).step_by(2) {
                assert_eq!(done.get(x, y), done.get(x + 1, y));
            }
        }
        // While loading only rows of one band of cells change a frame.
        let mut before = Canvas::new(W, H);
        render(&mut before, FOUND + 4.0, texts);
        render(&mut a, FOUND + 4.04, texts);
        let (y0, y1) = chunky::changed_rows(&a, Some(&before)).unwrap();
        assert!(y1 - y0 <= 2 * CELL_H || y0 >= PH, "{y0}..{y1}");
        // At rest nothing changes from frame to frame.
        let mut again = Canvas::new(W, H);
        render(&mut again, RELOAD - 0.5, texts);
        assert_eq!(chunky::changed_rows(&again, Some(&done)), None);
    }

    #[test]
    fn the_strip_shows_the_tune_as_artist_and_title() {
        let mut c = Canvas::new(W, H);
        let texts = Texts {
            clock: "12:34",
            away: "afk 3 min",
            tune: Some("Erik Satie - Gymnopedie No. 1"),
        };
        render(&mut c, RELOAD - 1.0, texts);
        let mut want = Canvas::new(W, H);
        want.text(4, PH + 14, "ERIK SATIE - GYMNOPEDIE NO. 1", GREY_LIGHT, 1);
        for x in 0..240 {
            for y in PH + 14..PH + 22 {
                assert_eq!(c.get(x, y), want.get(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn stripes_cover_the_border_and_change_every_frame() {
        for frame in 0..50 {
            let s = stripes(frame, 300);
            assert_eq!(s.first().unwrap().0, 0);
            let mut y = 0;
            for &(top, h, _) in &s {
                assert_eq!(top, y);
                assert!(h > 0);
                y += h;
            }
            assert_eq!(y, 300);
        }
        assert_ne!(stripes(1, 300), stripes(2, 300));
        assert_eq!(stripes(7, 300), stripes(7, 300));
    }

    #[test]
    fn the_picture_and_border_stay_off_the_prompt() {
        for (a, u) in [
            (area(0, 0, 1920, 1080), 26),
            (area(0, 0, 2560, 1440), 34),
            (area(0, 0, 1366, 768), 22),
            (area(1920, -200, 1280, 1024), 24),
            (area(0, 0, 3840, 2160), 52),
        ] {
            let block = demo::prompt_block(u, u * 2, true);
            let s = arrange(a, block);
            let (x, y, w, h) = s.outer().expect("fits");
            let (ax, ay) = (i32::from(a.x), i32::from(a.y));
            assert!(x >= ax && y >= ay);
            assert!(x + w <= ax + i32::from(a.width) && y + h <= ay + i32::from(a.height));
            assert!(y + h <= s.prompt_top);
            assert!(s.prompt_bottom - s.prompt_top >= block, "{a:?}");
        }
        assert_eq!(arrange(area(0, 0, 1920, 1080), 280).place.unwrap().sx, 2);
    }
}
