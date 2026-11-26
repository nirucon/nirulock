//! Chunky pixels for the C64-style layouts (`attract`, and any later one):
//! a small indexed canvas (16 colours, like the C64), drawn on the screen
//! integer-scaled with sharp pixels as solid rectangles filled by the X
//! server.
//!
//! [`encode`] turns the canvas into rectangles: runs of one colour along a
//! row, merged down the rows while they repeat, so flat areas cost one
//! rectangle each. Given the previous frame it encodes only the rows that
//! changed, so a still picture costs nothing to keep on the screen.
//!
//! [`Palette`] derives the 16 colours from the active NIRUX theme.
//!
//! Everything here is pure (no X requests) so that it can be tested.

use crate::{
    demo::{self, Spots},
    display::Area,
    layout, pixfont,
};
use x11rb::protocol::xproto::Rectangle;

/// Colours in a canvas.
pub const COLOURS: usize = 16;

/// Palette slots: what each index is for. The game draws with these.
pub mod slot {
    pub const BLACK: u8 = 0;
    pub const WHITE: u8 = 1;
    pub const ACCENT_DARK: u8 = 2;
    pub const ACCENT: u8 = 3;
    pub const ACCENT_LIGHT: u8 = 4;
    pub const COOL_DARK: u8 = 5;
    pub const COOL: u8 = 6;
    pub const COOL_LIGHT: u8 = 7;
    pub const WARM_DARK: u8 = 8;
    pub const WARM: u8 = 9;
    pub const WARM_LIGHT: u8 = 10;
    pub const WARNING: u8 = 11;
    pub const FLASH: u8 = 12;
    pub const GREY_DARK: u8 = 13;
    pub const GREY: u8 = 14;
    pub const GREY_LIGHT: u8 = 15;
}

/// An indexed picture, `w` by `h` logical pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        Self {
            w,
            h,
            px: vec![0; (w.max(0) * h.max(0)) as usize],
        }
    }

    pub fn clear(&mut self, colour: u8) {
        self.px.fill(colour);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0;
        }
        self.px[(y * self.w + x) as usize]
    }

    pub fn set(&mut self, x: i32, y: i32, colour: u8) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            self.px[(y * self.w + x) as usize] = colour;
        }
    }

    /// Row `y`, which must be inside the canvas.
    pub fn row_mut(&mut self, y: i32) -> &mut [u8] {
        let w = self.w as usize;
        let at = y as usize * w;
        &mut self.px[at..at + w]
    }

    /// A filled rectangle, cut to the canvas.
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, colour: u8) {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for row in y0..y1 {
            let at = (row * self.w) as usize;
            self.px[at + x0 as usize..at + x1 as usize].fill(colour);
        }
    }

    /// A sprite: rows of hex digits (palette indices), `.` transparent.
    /// `wide` doubles every pixel across, like the C64's multicolour
    /// sprites. `tint` draws every lit pixel in one colour (a hit flash).
    pub fn sprite(&mut self, x: i32, y: i32, rows: &[&str], wide: bool, tint: Option<u8>) {
        let step = if wide { 2 } else { 1 };
        for (dy, row) in rows.iter().enumerate() {
            for (dx, c) in row.bytes().enumerate() {
                let Some(colour) = (c as char).to_digit(16) else {
                    continue;
                };
                let colour = tint.unwrap_or(colour as u8);
                let px = x + dx as i32 * step;
                let py = y + dy as i32;
                self.set(px, py, colour);
                if wide {
                    self.set(px + 1, py, colour);
                }
            }
        }
    }

    /// Text in the 8x8 pixel font at `scale`, its top left at (x, y).
    /// Returns the width drawn.
    pub fn text(&mut self, x: i32, y: i32, text: &str, colour: u8, scale: i32) -> i32 {
        let scale = scale.max(1);
        let mut cx = x;
        for c in text.chars() {
            let g = pixfont::glyph(c);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..8 {
                    if bits & (0x80 >> col) != 0 {
                        self.fill(
                            cx + col * scale,
                            y + row as i32 * scale,
                            scale,
                            scale,
                            colour,
                        );
                    }
                }
            }
            cx += 8 * scale;
        }
        cx - x
    }

    /// Text centred across the canvas.
    pub fn text_centred(&mut self, y: i32, text: &str, colour: u8, scale: i32) {
        let width = text.chars().count() as i32 * 8 * scale.max(1);
        self.text((self.w - width) / 2, y, text, colour, scale);
    }
}

/// A rectangle in canvas pixels: x, y, width, height.
pub type Logical = (u16, u16, u16, u16);

/// The canvas's rectangles, one list per colour. With `prev` (what is on
/// the screen now, the same size) only the rows that differ are encoded.
/// Returns the number of rectangles.
pub fn encode(cur: &Canvas, prev: Option<&Canvas>, out: &mut [Vec<Logical>; COLOURS]) -> usize {
    for list in out.iter_mut() {
        list.clear();
    }
    let prev = prev.filter(|p| p.w == cur.w && p.h == cur.h);
    let w = cur.w as usize;
    // The previous encoded row's runs: (x0, x1, colour, index in out[colour]).
    let mut open: Vec<(u16, u16, u8, usize)> = Vec::new();
    let mut next: Vec<(u16, u16, u8, usize)> = Vec::new();
    let mut count = 0;
    for y in 0..cur.h.max(0) as usize {
        let row = &cur.px[y * w..(y + 1) * w];
        if prev.is_some_and(|p| &p.px[y * w..(y + 1) * w] == row) {
            open.clear();
            continue;
        }
        next.clear();
        let mut j = 0;
        let mut x = 0;
        while x < w {
            let colour = row[x];
            let start = x;
            while x < w && row[x] == colour {
                x += 1;
            }
            let (x0, x1) = (start as u16, x as u16);
            while j < open.len() && open[j].0 < x0 {
                j += 1;
            }
            let c = usize::from(colour & 15);
            match open.get(j) {
                Some(&(ox0, ox1, oc, index)) if ox0 == x0 && ox1 == x1 && oc == colour => {
                    out[c][index].3 += 1;
                    next.push((x0, x1, colour, index));
                }
                _ => {
                    out[c].push((x0, y as u16, x1 - x0, 1));
                    next.push((x0, x1, colour, out[c].len() - 1));
                    count += 1;
                }
            }
        }
        std::mem::swap(&mut open, &mut next);
    }
    count
}

/// Where a canvas goes on the screen: its top left corner and the size of
/// one canvas pixel (`sx` across, `sy` down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub sx: i32,
    pub sy: i32,
}

impl Placement {
    /// The screen rectangle of a `w` by `h` canvas.
    pub fn rect(&self, w: i32, h: i32) -> (i32, i32, i32, i32) {
        (self.x, self.y, w * self.sx, h * self.sy)
    }

    /// A canvas rectangle on the screen.
    pub fn scaled(&self, r: Logical) -> Rectangle {
        let (x, y, w, h) = r;
        Rectangle {
            x: layout::clamp16(self.x + i32::from(x) * self.sx),
            y: layout::clamp16(self.y + i32::from(y) * self.sy),
            width: (i32::from(w) * self.sx).clamp(0, i32::from(u16::MAX)) as u16,
            height: (i32::from(h) * self.sy).clamp(0, i32::from(u16::MAX)) as u16,
        }
    }
}

/// Centre a canvas `w` pixels wide horizontally on `area` with its top at `top`.
pub fn centred(area: Area, w: i32, scale: i32, aspect: i32, top: i32) -> Placement {
    let sx = scale * aspect.max(1);
    let x = i32::from(area.x) + (i32::from(area.width) - w * sx) / 2;
    Placement {
        x,
        y: top,
        sx,
        sy: scale,
    }
}

/// One monitor: where the canvas goes (none when it does not fit), the
/// frame's thickness around it, and the black area for the prompt lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub place: Option<Placement>,
    /// The canvas size.
    pub cw: i32,
    pub ch: i32,
    pub bezel: i32,
    pub prompt_top: i32,
    pub prompt_bottom: i32,
}

impl Screen {
    /// The canvas on the screen, frame included.
    pub fn outer(&self) -> Option<(i32, i32, i32, i32)> {
        self.place.map(|p| {
            let (x, y, w, h) = p.rect(self.cw, self.ch);
            let b = self.bezel;
            (x - b, y - b, w + 2 * b, h + 2 * b)
        })
    }
}

/// Lay out one monitor for a `cw` by `ch` canvas with a frame `border(scale)`
/// screen pixels thick: the largest whole scale at which the canvas, its
/// frame and (with `block` > 0, the prompt monitor) the prompt lines below
/// it fit, the canvas and the prompt centred as one block. Without a
/// prompt the canvas is centred. A screen too small for the canvas at scale
/// 1 gets no canvas, only the prompt.
pub fn arrange(area: Area, block: i32, cw: i32, ch: i32, border: impl Fn(i32) -> i32) -> Screen {
    let y = i32::from(area.y);
    let (w, h) = (i32::from(area.width).max(1), i32::from(area.height).max(1));
    let margin = (h * 3 / 100).max(4);
    let room_h = if block > 0 {
        h - block - 3 * margin
    } else {
        h - 2 * margin
    };
    let room_w = w * 94 / 100;
    let fits = |s: i32| cw * s + 2 * border(s) <= room_w && ch * s + 2 * border(s) <= room_h;
    let Some(scale) = (1..=16).rev().find(|&s| fits(s)) else {
        return Screen {
            place: None,
            cw,
            ch,
            bezel: 0,
            prompt_top: y + margin,
            prompt_bottom: y + h - margin,
        };
    };
    let bezel = border(scale);
    let field_h = ch * scale;
    let top = if block > 0 {
        let total = field_h + 2 * bezel + margin + block;
        y + ((h - total) / 2).max(margin) + bezel
    } else {
        y + (h - field_h) / 2
    };
    Screen {
        place: Some(centred(area, cw, scale, 1, top)),
        cw,
        ch,
        bezel,
        prompt_top: top + field_h + bezel + margin,
        prompt_bottom: y + h - margin,
    }
}

/// The prompt lines in the black area of `s`.
pub fn spots(s: &Screen, u: i32, clock_ascent: i32, info_ascent: i32, music: bool) -> Spots {
    let block = demo::prompt_block(u, clock_ascent, music);
    let (top, bottom) = (s.prompt_top, s.prompt_bottom);
    let start = (top + (bottom - top - block) / 4).max(top);
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
        limit: bottom - u / 2,
    }
}

/// Rows `y0..y1` of the canvas as a 32-bit image scaled by `place`, for
/// one PutImage: `big_endian` follows the X server's image byte order.
/// Returns the bytes and the image size in screen pixels.
pub fn image_rows(
    canvas: &Canvas,
    palette: &Palette,
    place: &Placement,
    y0: i32,
    y1: i32,
    big_endian: bool,
) -> (Vec<u8>, u16, u16) {
    let (y0, y1) = (y0.clamp(0, canvas.h), y1.clamp(0, canvas.h));
    let width = (canvas.w * place.sx).max(0) as usize;
    let mut out = Vec::with_capacity(width * ((y1 - y0) * place.sy).max(0) as usize * 4);
    let mut line = Vec::with_capacity(width * 4);
    for y in y0..y1 {
        line.clear();
        for x in 0..canvas.w {
            let colour = palette.0[usize::from(canvas.get(x, y) & 15)];
            let bytes = if big_endian {
                colour.to_be_bytes()
            } else {
                colour.to_le_bytes()
            };
            for _ in 0..place.sx {
                line.extend_from_slice(&bytes);
            }
        }
        for _ in 0..place.sy {
            out.extend_from_slice(&line);
        }
    }
    let height = ((y1 - y0) * place.sy).clamp(0, i32::from(u16::MAX)) as u16;
    (out, width.min(usize::from(u16::MAX)) as u16, height)
}

/// The rows that differ between two canvases of the same size: the first
/// and one past the last; `None` when they are equal. Without `prev` every
/// row.
pub fn changed_rows(cur: &Canvas, prev: Option<&Canvas>) -> Option<(i32, i32)> {
    let Some(prev) = prev.filter(|p| p.w == cur.w && p.h == cur.h) else {
        return (cur.h > 0).then_some((0, cur.h));
    };
    let w = cur.w as usize;
    let differs = |y: usize| cur.px[y * w..(y + 1) * w] != prev.px[y * w..(y + 1) * w];
    let rows = cur.h.max(0) as usize;
    let first = (0..rows).find(|&y| differs(y))?;
    let last = (first..rows).rev().find(|&y| differs(y)).unwrap_or(first);
    Some((first as i32, last as i32 + 1))
}

/// The 16 colours, derived from the theme: black, a light white, the
/// accent and two neighbours on the colour wheel (a cool one for water and
/// a warm one for the land, see [`neighbour`], turned from the accent, or
/// from the warning colour when the accent is grey) in three shades each, the warning colour and a light flash of it,
/// and three greys from the theme's muted colour. A grey theme gives a
/// grey picture; the background is always black.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette(pub [u32; COLOURS]);

impl Palette {
    pub fn new(accent: u32, foreground: u32, muted: u32, warning: u32) -> Self {
        // A grey accent (Niru Noir) has no hue to turn: the neighbours come
        // from the warning colour then, so the picture keeps some colour.
        let source = if saturation(accent) < 0.12 {
            warning
        } else {
            accent
        };
        let cool = neighbour(source, 215.0);
        let warm = neighbour(source, 120.0);
        Self([
            0x000000,
            demo::lighten(foreground, 0.7),
            demo::shade(accent, 0.5),
            accent,
            demo::lighten(accent, 0.45),
            demo::shade(cool, 0.4),
            demo::shade(cool, 0.75),
            demo::lighten(cool, 0.4),
            demo::shade(warm, 0.28),
            demo::shade(warm, 0.55),
            demo::lighten(warm, 0.25),
            warning,
            demo::lighten(warning, 0.55),
            demo::shade(muted, 0.45),
            muted,
            demo::lighten(muted, 0.5),
        ])
    }
}

/// `colour` turned around the colour wheel in 45 degree steps (never
/// left as it is) to the hue nearest `target`: the river comes out bluish
/// and the forest greenish in most themes, in the theme's own saturation
/// and lightness.
fn neighbour(colour: u32, target: f32) -> u32 {
    let hue = hue(colour);
    let step = (1..8)
        .map(|k| k as f32 * 45.0)
        .min_by(|a, b| {
            let d = |turn: f32| {
                let x = (hue + turn - target).rem_euclid(360.0);
                x.min(360.0 - x)
            };
            d(*a).total_cmp(&d(*b))
        })
        .unwrap_or(180.0);
    demo::rotate_hue(colour, step)
}

/// The hue in degrees.
fn hue(colour: u32) -> f32 {
    let (r, g, b) = (
        ((colour >> 16) & 255) as f32,
        ((colour >> 8) & 255) as f32,
        (colour & 255) as f32,
    );
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    if delta <= 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    }
}

/// HSV saturation, 0..1.
fn saturation(colour: u32) -> f32 {
    let c = [(colour >> 16) & 255, (colour >> 8) & 255, colour & 255];
    let max = *c.iter().max().unwrap_or(&0) as f32;
    let min = *c.iter().min().unwrap_or(&0) as f32;
    if max <= 0.0 {
        0.0
    } else {
        (max - min) / max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint(out: &[Vec<Logical>; COLOURS], w: i32, h: i32) -> Canvas {
        let mut c = Canvas::new(w, h);
        c.clear(255);
        for (colour, list) in out.iter().enumerate() {
            for &(x, y, rw, rh) in list {
                for yy in y..y + rh {
                    for xx in x..x + rw {
                        assert_eq!(c.get(i32::from(xx), i32::from(yy)), 255, "drawn twice");
                        c.px[usize::from(yy) * w as usize + usize::from(xx)] = colour as u8;
                    }
                }
            }
        }
        c
    }

    #[test]
    fn the_rectangles_cover_the_canvas_exactly_once() {
        let mut c = Canvas::new(40, 30);
        c.fill(3, 4, 10, 10, 5);
        c.fill(20, 0, 5, 30, 7);
        c.text(1, 20, "NX", 1, 1);
        c.sprite(30, 2, &[".12.", "3..4"], true, None);
        let mut out: [Vec<Logical>; COLOURS] = Default::default();
        let n = encode(&c, None, &mut out);
        assert_eq!(paint(&out, 40, 30), c);
        // Flat areas merge down the rows: a 10x10 block is one rectangle.
        assert!(out[5].contains(&(3, 4, 10, 10)), "{:?}", out[5]);
        assert!(n < 120, "{n}");
        // Nothing changed: nothing to draw.
        assert_eq!(encode(&c, Some(&c.clone()), &mut out), 0);
        // One changed row: only that row.
        let prev = c.clone();
        c.set(0, 10, 9);
        let n = encode(&c, Some(&prev), &mut out);
        assert!(out.iter().flatten().all(|r| r.1 == 10 && r.3 == 1));
        assert_eq!(out.iter().flatten().map(|r| r.2).sum::<u16>(), 40);
        assert!(n >= 2);
    }

    #[test]
    fn images_scale_rows_and_find_what_changed() {
        let mut c = Canvas::new(3, 4);
        c.set(1, 2, 3);
        let mut pal = Palette([0; COLOURS]);
        pal.0[3] = 0x112233;
        let place = Placement {
            x: 0,
            y: 0,
            sx: 2,
            sy: 3,
        };
        let (bytes, w, h) = image_rows(&c, &pal, &place, 2, 3, false);
        assert_eq!((w, h), (6, 3));
        assert_eq!(bytes.len(), 6 * 3 * 4);
        // Pixel (1, 2) is screen columns 2 and 3 of every repeated row.
        for row in 0..3 {
            for col in 0..6 {
                let at = (row * 6 + col) * 4;
                let want = if col == 2 || col == 3 { 0x33 } else { 0 };
                assert_eq!(bytes[at], want, "{row} {col}");
            }
        }
        let (be, _, _) = image_rows(&c, &pal, &place, 2, 3, true);
        assert_eq!(&be[8..12], &[0, 0x11, 0x22, 0x33]);
        let prev = c.clone();
        assert_eq!(changed_rows(&c, Some(&prev)), None);
        assert_eq!(changed_rows(&c, None), Some((0, 4)));
        c.set(0, 1, 5);
        c.set(2, 3, 5);
        assert_eq!(changed_rows(&c, Some(&prev)), Some((1, 4)));
    }

    #[test]
    fn placements_scale_whole_pixels() {
        let p = centred(
            Area {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            320,
            3,
            1,
            40,
        );
        assert_eq!(p.rect(320, 200), (1920 + 480, 40, 960, 600));
        let r = p.scaled((1, 2, 3, 4));
        assert_eq!((r.x, r.y, r.width, r.height), (2403, 46, 9, 12));
    }

    #[test]
    fn the_palette_follows_the_theme() {
        let p = Palette::new(0x5e81ac, 0xd8dee9, 0x4c566a, 0xbf616a);
        assert_eq!(p.0[0], 0);
        assert_eq!(p.0[usize::from(slot::ACCENT)], 0x5e81ac);
        assert_eq!(p.0[usize::from(slot::WARNING)], 0xbf616a);
        assert_eq!(p.0[usize::from(slot::GREY)], 0x4c566a);
        // A grey accent takes its neighbours from the warning colour.
        let noir = Palette::new(0x9a9a9a, 0xdddddd, 0x555555, 0xd8a657);
        assert_eq!(noir.0[usize::from(slot::ACCENT)], 0x9a9a9a);
        assert_ne!(
            noir.0[usize::from(slot::COOL)],
            noir.0[usize::from(slot::WARM)]
        );
        assert!(saturation(noir.0[usize::from(slot::COOL)]) > 0.2);
        // The river bluish, the forest greenish.
        for accent in [0x5e81ac, 0xbf616a, 0xa3be8c, 0xd8a657] {
            let p = Palette::new(accent, 0xd8dee9, 0x4c566a, 0xbf616a);
            let water = hue(p.0[usize::from(slot::COOL)]);
            let forest = hue(p.0[usize::from(slot::WARM)]);
            assert!((water - 215.0).abs() <= 45.0, "{accent:06x}: water {water}");
            assert!(
                (forest - 120.0).abs() <= 45.0,
                "{accent:06x}: forest {forest}"
            );
        }
        // An all-grey theme stays grey.
        let g = Palette::new(0x888888, 0xdddddd, 0x555555, 0x999999);
        for c in g.0 {
            let (r, gg, b) = ((c >> 16) & 255, (c >> 8) & 255, c & 255);
            assert!(r == gg && gg == b, "{c:06x}");
        }
    }
}
