//! The `attract` layout: the lock screen as an arcade cabinet in attract
//! mode. [`crate::arcade`]'s NIRUX RAID plays itself on a 320x200 canvas,
//! integer-scaled with sharp pixels inside a framed "monitor", and cycles
//! like a C64 game left alone: title screen, high scores, a game the AI
//! plays until GAME OVER, and round again with a new seed.
//!
//! On the prompt monitor the playfield sits at the top and the clock, the
//! password entry, the away time and the status lines are below it on
//! plain black in the theme font, exactly as in the other layouts: the
//! game never draws there. A failed attempt turns the frame to the warning
//! colour while its message shows. Every other monitor shows the same
//! game, centred (one simulation, drawn once per monitor). A monitor too
//! small for the playfield at scale 1 above the prompt shows only the
//! prompt.
//!
//! The canvas is drawn as solid rectangles ([`chunky::encode`]): only the
//! rows that changed since the last frame, merged down the rows, at 25
//! frames a second ([`demo::FRAME`], fewer on very large screens), and
//! only the rows that changed are copied to the screen. With the display
//! off nothing is drawn and the game waits.
//!
//! Nothing typed reaches the game: it runs on its own clock from its own
//! seed. While music plays, explosions and the muzzle flash light up on
//! the beat, and the tune shows in the bottom strip as "Artist - Title".

use crate::{
    arcade,
    chunky::{self, spots, Canvas, Logical, Palette, Screen, COLOURS},
    demo::{self, Spots},
    display::Area,
    layout, Lock, Result,
};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::{ChangeGCAux, ConnectionExt as _, Rectangle};

/// At most this many rectangles in one request.
const MAX_RECTS: usize = 4096;
/// The longest stretch of game time made up after a pause.
const CATCH_UP: f32 = 0.2;

/// Lay out one monitor. `block` is the height the prompt lines need (0 on
/// the other monitors). The frame is two canvas pixels thick, 2 to 8
/// screen pixels.
pub fn arrange(area: Area, block: i32) -> Screen {
    chunky::arrange(area, block, arcade::W, arcade::H, |scale| {
        (scale * 2).clamp(2, 8)
    })
}

/// The layout's state between frames.
#[derive(Default)]
pub struct State {
    screens: Vec<Screen>,
    frame: Duration,
    next: Option<Instant>,
    game: Option<arcade::Attract>,
    canvas: Option<Canvas>,
    /// What the screen shows now, for drawing only what changed.
    shown: Option<Canvas>,
    out: [Vec<Logical>; COLOURS],
    scratch: Vec<Rectangle>,
    /// Game time not yet played, in seconds.
    pending: f32,
    last: Option<Instant>,
    flash: f32,
    bass: f32,
    /// The frame colour on the screen.
    bezel: Option<u32>,
}

fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x5eed, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id()).rotate_left(32)
}

impl Lock {
    /// Lay the playfield out on every monitor and the prompt lines on the
    /// prompt monitor.
    pub(crate) fn attract_layout(&mut self, area: Area) -> Spots {
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
        self.attract.screens.clear();
        for screen in screens {
            let prompt = screen == area;
            let s = arrange(screen, if prompt { block } else { 0 });
            if prompt {
                out = Some(spots(&s, u, clock_ascent, info_ascent, music));
            }
            self.attract.screens.push(s);
        }
        let pixels = self
            .attract
            .screens
            .iter()
            .filter_map(Screen::outer)
            .map(|(_, _, w, h)| i64::from(w) * i64::from(h))
            .sum();
        self.attract.frame = demo::frame_for(pixels);
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

    /// Play the game up to now and draw it into the backing pixmap:
    /// everything with `full` (after the screen was cleared), otherwise
    /// only the rows that changed. Returns the canvas rows drawn (first,
    /// last), if any.
    pub(crate) fn attract_paint(&mut self, full: bool) -> Result<Option<(i32, i32)>> {
        let now = Instant::now();
        let st = &mut self.attract;
        let game = st.game.get_or_insert_with(|| arcade::Attract::new(seed()));
        let dt = st.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        st.last = Some(now);
        st.pending = (st.pending + dt).min(CATCH_UP);
        let tick = 1.0 / arcade::TICKS as f32;
        while st.pending >= tick {
            game.step();
            st.pending -= tick;
        }
        // The beat, from the spectrum while music plays.
        let bass = self
            .visual
            .as_mut()
            .and_then(crate::visual::Visual::spectrum)
            .map_or(0.0, |values| {
                let low = &values[..values.len().min(6)];
                low.iter().sum::<f32>() / low.len().max(1) as f32
            });
        let st = &mut self.attract;
        (st.flash, st.bass) = demo::beat(st.flash, bass, st.bass, dt);
        let canvas = st
            .canvas
            .get_or_insert_with(|| Canvas::new(arcade::W, arcade::H));
        if let Some(game) = st.game.as_ref() {
            game.render(
                canvas,
                arcade::Extras {
                    tune: self.music_tune.as_deref(),
                    beat: st.flash,
                },
            );
        }
        let prev = if full { None } else { st.shown.as_ref() };
        chunky::encode(canvas, prev, &mut st.out);
        let pal = &self.cfg.palette;
        let accent = crate::ascii::banner_colour(pal.focus, pal.foreground, pal.background);
        let palette = Palette::new(accent, pal.foreground, pal.muted, self.cfg.warning);
        let bezel = if self.state == crate::State::Wrong {
            self.cfg.warning
        } else {
            demo::shade(accent, 0.6)
        };
        let s = &self.surface;
        let mut rows: Option<(i32, i32)> = None;
        for list in &st.out {
            for r in list {
                let (a, b) = (i32::from(r.1), i32::from(r.1 + r.3) - 1);
                rows = Some(rows.map_or((a, b), |(lo, hi)| (lo.min(a), hi.max(b))));
            }
        }
        for screen in &st.screens {
            let Some(place) = screen.place else {
                continue;
            };
            for (colour, list) in st.out.iter().enumerate() {
                if list.is_empty() {
                    continue;
                }
                s.c.change_gc(s.gc, &ChangeGCAux::new().foreground(palette.0[colour]))?;
                st.scratch.clear();
                st.scratch.extend(list.iter().map(|r| place.scaled(*r)));
                for chunk in st.scratch.chunks(MAX_RECTS) {
                    s.c.poly_fill_rectangle(s.back, s.gc, chunk)?;
                }
            }
            if full || st.bezel != Some(bezel) {
                let (x, y, w, h) = screen.outer().unwrap_or_default();
                let b = screen.bezel;
                let r = |x: i32, y: i32, w: i32, h: i32| Rectangle {
                    x: layout::clamp16(x),
                    y: layout::clamp16(y),
                    width: w.clamp(0, i32::from(u16::MAX)) as u16,
                    height: h.clamp(0, i32::from(u16::MAX)) as u16,
                };
                let frame = [
                    r(x, y, w, b),
                    r(x, y + h - b, w, b),
                    r(x, y + b, b, h - 2 * b),
                    r(x + w - b, y + b, b, h - 2 * b),
                ];
                s.c.change_gc(s.gc, &ChangeGCAux::new().foreground(bezel))?;
                s.c.poly_fill_rectangle(s.back, s.gc, &frame)?;
            }
        }
        let bezel_changed = st.bezel != Some(bezel);
        st.bezel = Some(bezel);
        // Keep the canvas as what is shown; reuse the old one next frame.
        let old = st.shown.take();
        st.shown = st.canvas.take();
        st.canvas = old;
        let frame = st.frame.max(demo::FRAME);
        let due = st
            .next
            .map_or(now, |due| due.max(now.checked_sub(frame).unwrap_or(now)));
        st.next = Some(due + frame);
        if bezel_changed && !full {
            return Ok(Some((-1, arcade::H)));
        }
        Ok(rows)
    }

    /// The next frame, when it is due: drawn, and the changed rows copied
    /// to the screen. `display_on` false holds the game as it is.
    pub(crate) fn attract_tick(&mut self, display_on: bool) -> Result<()> {
        if !display_on {
            // The game waits; no time piles up while the display is off.
            self.attract.last = None;
            return Ok(());
        }
        if self.attract.next.map_or(true, |due| Instant::now() < due) {
            return Ok(());
        }
        let Some((first, last)) = self.attract_paint(false)? else {
            return Ok(());
        };
        for screen in &self.attract.screens {
            let (Some(place), Some((x, _, w, _))) = (screen.place, screen.outer()) else {
                continue;
            };
            let b = if first < 0 { screen.bezel } else { 0 };
            let y0 = place.y + first.max(0) * place.sy - b;
            let y1 = place.y + (last.min(arcade::H - 1) + 1) * place.sy + b;
            let (x, w) = if b > 0 {
                (x, w)
            } else {
                (place.x, arcade::W * place.sx)
            };
            self.surface.present_rect(
                layout::clamp16(x),
                layout::clamp16(y0),
                w.clamp(0, i32::from(u16::MAX)) as u16,
                (y1 - y0).clamp(0, i32::from(u16::MAX)) as u16,
            )?;
        }
        Ok(())
    }

    /// How long the loop may sleep before the next frame.
    pub(crate) fn attract_timeout(&self, display_on: bool) -> Option<Duration> {
        if !display_on || self.cfg.layout != crate::config::Layout::Attract {
            return None;
        }
        Some(self.attract.next.map_or(Duration::ZERO, |due| {
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
    fn the_playfield_stays_on_its_monitor_and_off_the_prompt() {
        for (a, u) in [
            (area(0, 0, 1920, 1080), 26),
            (area(1920, 0, 2560, 1440), 34),
            (area(0, 0, 1366, 768), 22),
            (area(-1280, 200, 1280, 1024), 24),
            (area(0, 0, 3840, 2160), 52),
            (area(0, 0, 1080, 1920), 26),
            (area(0, 0, 1920, 1080), 0),
        ] {
            let block = if u > 0 {
                demo::prompt_block(u, u * 2, true)
            } else {
                0
            };
            let s = arrange(a, block);
            let (x, y, w, h) = s.outer().expect("the playfield fits");
            let (ax, ay) = (i32::from(a.x), i32::from(a.y));
            let (aw, ah) = (i32::from(a.width), i32::from(a.height));
            assert!(
                x >= ax && y >= ay && x + w <= ax + aw && y + h <= ay + ah,
                "{a:?} {s:?}"
            );
            let place = s.place.unwrap();
            assert!(place.sx >= 1 && place.sx == place.sy);
            if block > 0 {
                assert!(y + h <= s.prompt_top, "{a:?}: the frame reaches the prompt");
                assert!(
                    s.prompt_bottom - s.prompt_top >= block,
                    "{a:?}: {} for a block of {block}",
                    s.prompt_bottom - s.prompt_top
                );
                let sp = spots(&s, u, u * 2, u * 2 / 3, true);
                assert!(i32::from(sp.info) - u * 2 / 3 >= s.prompt_top, "{a:?}");
                assert!(sp.rows <= sp.limit && sp.limit <= ay + ah, "{a:?} {sp:?}");
            }
        }
        // A full HD screen gets the C64 picture three times over.
        assert_eq!(
            arrange(area(0, 0, 1920, 1080), demo::prompt_block(26, 52, true))
                .place
                .unwrap()
                .sx,
            3
        );
    }

    #[test]
    fn a_screen_too_small_shows_only_the_prompt() {
        let s = arrange(area(0, 0, 640, 400), 300);
        assert_eq!(s.place, None);
        assert_eq!(s.outer(), None);
        assert!(s.prompt_bottom - s.prompt_top >= 300);
    }
}
