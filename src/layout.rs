//! Pure layout math for the lock screen: where each line and the panel go.
//!
//! Nothing here draws. The functions take font metrics and a measuring
//! closure, so the positions are unit-tested without an X server:
//!
//! - every line is centred on the prompt output's centre by its ink, not by
//!   its advance width or a character count (see `TextRenderer::ink`);
//! - the `cava` layout is one column with a fixed vertical rhythm. Slots that
//!   come and go (the pause music line, a failure message, Caps Lock) are
//!   reserved up front, so the entry never moves when they appear;
//! - long text wraps or is cut with an ellipsis to the column's width.

use crate::display::Area;

/// The spectrum's bar count (`cava::BARS`).
const BARS: i32 = 48;
/// Rows reserved under the entry: a failure message on up to two lines,
/// the failed-attempt count and Caps Lock.
pub const STATUS_ROWS: usize = 4;
/// Lines a failure or PAM message may wrap to.
pub const MESSAGE_LINES: usize = 2;
const ELLIPSIS: char = '…';

/// Vertical font metrics in pixels (ascent and descent are positive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// The base font's line height: the unit of the vertical rhythm.
    pub line: i32,
    pub ascent: i32,
    pub descent: i32,
    /// The clock's inked height above and below the baseline (digits).
    pub clock_ascent: i32,
    pub clock_descent: i32,
    /// The info line's (smaller) font.
    pub info_ascent: i32,
    pub info_descent: i32,
}

/// What the `cava` layout has to make room for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slots {
    pub hostname: bool,
    /// The pause music is enabled: its info line gets a slot under the panel.
    pub music: bool,
}

/// A rectangle in root coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    #[cfg(test)]
    pub fn bottom(self) -> i32 {
        self.y + self.height
    }

    #[cfg(test)]
    pub fn right(self) -> i32 {
        self.x + self.width
    }

    /// The rectangle as X11 wants it, clamped to the protocol's ranges.
    pub fn x11(self) -> (i16, i16, u16, u16) {
        (
            clamp16(self.x),
            clamp16(self.y),
            self.width.clamp(1, i32::from(u16::MAX)) as u16,
            self.height.clamp(1, i32::from(u16::MAX)) as u16,
        )
    }
}

pub fn clamp16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// The `cava` layout: baselines (y) of each line and the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CavaLayout {
    /// Horizontal centre of the column.
    pub centre: i32,
    /// The widest any text line may be.
    pub column: i32,
    pub clock: i32,
    pub hostname: Option<i32>,
    pub panel: Rect,
    pub info: Option<i32>,
    pub locked: i32,
    pub entry: i32,
    pub away: i32,
    /// Baselines of the reserved status rows.
    pub status: Vec<i32>,
    /// Top and bottom of everything, status rows included.
    pub top: i32,
    pub bottom: i32,
}

/// Bar gap the spectrum uses at this width (`cava::paint`).
fn bar_gap(width: i32) -> i32 {
    if width >= 600 {
        2
    } else {
        1
    }
}

/// The widest spectrum not wider than `want` whose bars and gaps fill it
/// exactly, so its guides and baseline end where the outer bars do.
pub fn snap_spectrum(want: i32) -> i32 {
    let mut width = want.max(BARS);
    loop {
        let gap = bar_gap(width);
        let bar = ((width - gap * (BARS - 1)) / BARS).max(1);
        let used = bar * BARS + gap * (BARS - 1);
        if used <= width || width <= BARS {
            // A narrower width can switch to the 1 px gap; the result must
            // still be what `cava::paint` computes for it.
            if bar_gap(used) == gap {
                return used;
            }
        }
        width -= 1;
    }
}

fn round(v: f32) -> i32 {
    v.round() as i32
}

/// Lay out the `cava` layout on `area`.
///
/// The column, top to bottom: clock, hostname, panel, music info, `locked`,
/// entry, away time, then the status rows. The block from the clock to the
/// away time is centred a little above the middle of the output (the optical
/// centre); the status rows hang below it, inside the output, so a message
/// never pushes the entry.
pub fn cava(area: Area, m: &Metrics, slots: Slots) -> CavaLayout {
    let w = i32::from(area.width).max(1);
    let h = i32::from(area.height).max(1);
    let u = m.line.max(1) as f32;
    let margin = round(u * 1.5);
    let centre = i32::from(area.x) + w / 2;
    let panel_width = snap_spectrum(
        ((w as f32 * 0.58) as i32)
            .clamp(320, 1100)
            .min(w - 2 * margin),
    );
    // Text lines wrap at about 72 characters, never wider than the panel.
    let column = panel_width.min(m.line * 36).max(1);
    let mut panel_height = ((h as f32 * 0.16) as i32).clamp(96, 200);

    // Offsets from the top of the block.
    let place = |panel_height: i32| {
        let clock = m.clock_ascent;
        let mut cur = clock + m.clock_descent;
        let hostname = slots.hostname.then(|| {
            let base = cur + round(u * 0.6) + m.ascent;
            cur = base + m.descent;
            base
        });
        let panel_top = cur + round(u * 1.4);
        cur = panel_top + panel_height;
        let info = slots.music.then(|| {
            let base = cur + round(u * 0.8) + m.info_ascent;
            cur = base + m.info_descent;
            base
        });
        let locked = cur + round(if slots.music { u * 1.3 } else { u * 1.6 }) + m.ascent;
        let entry = locked + round(u * 1.5);
        let away = entry + round(u * 1.5);
        let block = away + m.descent;
        let status: Vec<i32> = (0..STATUS_ROWS as i32)
            .map(|i| away + round(u * 1.9) + i * round(u * 1.25))
            .collect();
        let bottom = status.last().copied().unwrap_or(away) + m.descent;
        (
            clock, hostname, panel_top, info, locked, entry, away, block, status, bottom,
        )
    };
    // A short output gives up panel height before anything overlaps.
    let mut laid = place(panel_height);
    while laid.9 > h - 2 * margin && panel_height > 32 {
        panel_height -= 4;
        laid = place(panel_height);
    }
    let (clock, hostname, panel_top, info, locked, entry, away, block, status, bottom) = laid;
    // Optical centre: the block's middle sits at 47 % of the output.
    let mut top = i32::from(area.y) + round(h as f32 * 0.47) - block / 2;
    // Keep the status rows on the output, then the clock.
    let limit = i32::from(area.y) + h - margin;
    if top + bottom > limit {
        top = limit - bottom;
    }
    top = top.max(i32::from(area.y) + margin.min(h / 20));
    let panel = Rect {
        x: centre - panel_width / 2,
        y: top + panel_top,
        width: panel_width,
        height: panel_height,
    };
    CavaLayout {
        centre,
        column,
        clock: top + clock,
        hostname: hostname.map(|b| top + b),
        panel,
        info: info.map(|b| top + b),
        locked: top + locked,
        entry: top + entry,
        away: top + away,
        status: status.into_iter().map(|b| top + b).collect(),
        top,
        bottom: top + bottom,
    }
}

/// `text` cut to at most `max` pixels with an ellipsis.
pub fn ellipsize(text: &str, max: i32, width: &mut dyn FnMut(&str) -> i32) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cut: String = chars.iter().collect();
        let candidate = format!("{}{ELLIPSIS}", cut.trim_end());
        if width(&candidate) <= max {
            return candidate;
        }
    }
    String::new()
}

/// `text` wrapped at spaces to `max` pixels, on at most `lines` lines; the
/// last one ends in an ellipsis when text was left out.
pub fn fit(text: &str, max: i32, lines: usize, width: &mut dyn FnMut(&str) -> i32) -> Vec<String> {
    let mut out = crate::quotes::wrap(text, max, width);
    if out.len() <= lines.max(1) {
        return crate::quotes::balanced(text, max, width);
    }
    let rest = out.split_off(lines.max(1) - 1).join(" ");
    out.push(ellipsize(&format!("{rest}{ELLIPSIS}"), max, width));
    out
}

/// The entry's left edge: the ink of `reference` (the empty prompt) centred
/// on `centre`. The label and the echo then start at the same x whatever is
/// typed, so the line never slides while typing.
pub fn entry_left(centre: i32, reference_ink: Option<(i32, i32)>, reference_width: i32) -> i32 {
    match reference_ink {
        Some((left, right)) => centre - (left + right) / 2,
        None => centre - reference_width / 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(size: i32) -> Metrics {
        // JetBrains Mono-like proportions.
        let line = size * 132 / 100;
        Metrics {
            line,
            ascent: size * 102 / 100,
            descent: size * 30 / 100,
            clock_ascent: size * 24 / 10 * 73 / 100,
            clock_descent: 1,
            info_ascent: size * 8 / 10,
            info_descent: size * 24 / 100,
        }
    }

    fn area(x: i16, y: i16, width: u16, height: u16) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    fn all_slots() -> [Slots; 4] {
        [
            Slots {
                hostname: false,
                music: false,
            },
            Slots {
                hostname: true,
                music: false,
            },
            Slots {
                hostname: false,
                music: true,
            },
            Slots {
                hostname: true,
                music: true,
            },
        ]
    }

    /// Every element as a (top, bottom) band, in order down the column.
    fn bands(l: &CavaLayout, m: &Metrics) -> Vec<(i32, i32)> {
        let mut b = vec![(l.clock - m.clock_ascent, l.clock + m.clock_descent)];
        if let Some(h) = l.hostname {
            b.push((h - m.ascent, h + m.descent));
        }
        b.push((l.panel.y, l.panel.bottom()));
        if let Some(i) = l.info {
            b.push((i - m.info_ascent, i + m.info_descent));
        }
        for y in [l.locked, l.entry, l.away]
            .into_iter()
            .chain(l.status.iter().copied())
        {
            b.push((y - m.ascent, y + m.descent));
        }
        b
    }

    #[test]
    fn nothing_overlaps_and_everything_stays_on_the_output() {
        for (w, h) in [
            (800, 600),
            (1280, 800),
            (1920, 1080),
            (1920, 1200),
            (2560, 1440),
            (3840, 2160),
        ] {
            for size in [11, 15, 22, 30] {
                let m = metrics(size);
                for slots in all_slots() {
                    let l = cava(area(0, 0, w, h), &m, slots);
                    let b = bands(&l, &m);
                    for pair in b.windows(2) {
                        assert!(pair[0].1 < pair[1].0, "{w}x{h} {size}px {slots:?}: {b:?}");
                    }
                    // The block always fits; the reserved status rows fit
                    // too unless the font is huge for the output.
                    let last = if size * 24 <= i32::from(h) {
                        b.len() - 1
                    } else {
                        b.len() - 1 - STATUS_ROWS
                    };
                    assert!(
                        b[0].0 >= 0 && b[last].1 <= i32::from(h),
                        "{w}x{h} {size}px: {b:?}"
                    );
                    assert!(l.panel.x >= 0 && l.panel.right() <= i32::from(w));
                    assert!(l.column <= i32::from(w));
                }
            }
        }
    }

    #[test]
    fn the_block_is_centred_on_the_prompt_output() {
        let m = metrics(15);
        // othala: the prompt goes on DisplayPort-4, below DisplayPort-5.
        let lower = area(0, 1440, 2560, 1440);
        let l = cava(
            lower,
            &m,
            Slots {
                hostname: false,
                music: true,
            },
        );
        assert_eq!(l.centre, 1280);
        assert_eq!(l.panel.x + l.panel.width / 2, 1280);
        assert!(l.top > 1440 && l.bottom < 2880);
        // The block (clock to away) sits around 47 % of the output.
        let mid = (l.clock - m.clock_ascent + l.away + m.descent) / 2;
        let want = 1440 + 1440 * 47 / 100;
        assert!((mid - want).abs() <= 2, "{mid} vs {want}");
        // The same output alone gives the same layout, shifted.
        let alone = cava(
            area(0, 0, 2560, 1440),
            &m,
            Slots {
                hostname: false,
                music: true,
            },
        );
        assert_eq!(alone.entry + 1440, l.entry);
        let side = cava(
            area(1920, 0, 1920, 1200),
            &m,
            Slots {
                hostname: true,
                music: false,
            },
        );
        assert_eq!(side.centre, 1920 + 960);
    }

    #[test]
    fn the_entry_never_moves_with_messages_or_music() {
        // Messages use reserved rows, and the music slot depends only on the
        // setting, so the entry's baseline is fixed for the whole lock.
        let m = metrics(15);
        for slots in all_slots() {
            let a = cava(area(0, 0, 1920, 1200), &m, slots);
            let b = cava(area(0, 0, 1920, 1200), &m, slots);
            assert_eq!(a, b);
            assert_eq!(a.status.len(), STATUS_ROWS);
            assert!(a.status[0] > a.away);
        }
    }

    #[test]
    fn the_rhythm_is_even_between_the_text_lines() {
        let m = metrics(15);
        let l = cava(
            area(0, 0, 1920, 1200),
            &m,
            Slots {
                hostname: false,
                music: false,
            },
        );
        assert_eq!(l.entry - l.locked, l.away - l.entry);
        let gaps: Vec<i32> = l.status.windows(2).map(|p| p[1] - p[0]).collect();
        assert!(gaps.windows(2).all(|g| g[0] == g[1]));
    }

    #[test]
    fn spectrum_widths_fill_exactly_with_bars() {
        for want in [320, 599, 600, 601, 777, 1100, 1113] {
            let w = snap_spectrum(want);
            let gap = bar_gap(w);
            let bar = (w - gap * (BARS - 1)) / BARS;
            assert!(w <= want && want - w < BARS + gap, "{want} -> {w}");
            assert_eq!(bar * BARS + gap * (BARS - 1), w, "{want} -> {w}");
        }
    }

    fn mono(text: &str) -> i32 {
        text.chars().count() as i32 * 9
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let mut width = mono;
        assert_eq!(ellipsize("short", 90, &mut width), "short");
        let cut = ellipsize("a rather long line of text", 90, &mut width);
        assert!(mono(&cut) <= 90 && cut.ends_with('…'), "{cut}");
        assert_eq!(ellipsize("abc", 0, &mut width), "");
    }

    #[test]
    fn long_messages_wrap_to_two_lines_within_the_column() {
        let mut width = mono;
        let text = "Authentication failure: the account is locked due to 3 failed login attempts; it unlocks again in 10 minutes";
        let lines = fit(text, 400, MESSAGE_LINES, &mut width);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| mono(l) <= 400), "{lines:?}");
        assert!(lines[1].ends_with('…'));
        assert_eq!(fit("fits", 400, 2, &mut width), vec!["fits".to_string()]);
    }

    #[test]
    fn the_entry_starts_where_the_full_reference_is_centred() {
        // "password:" inked from 1 to 161: the left edge is fixed.
        assert_eq!(entry_left(960, Some((1, 161)), 162), 960 - 81);
        assert_eq!(entry_left(960, None, 162), 960 - 81);
    }
}
