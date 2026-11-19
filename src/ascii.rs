//! The `ascii` layout ("NIRUX ascii"): the NIRUX banner from NIRUSCREENSAVER's
//! `ascii` scene, typed out once when the screen locks, with "A MINIMAL X11
//! DESKTOP" under it and the password prompt below. Every monitor shows the
//! banner, centred and sized for it; only the prompt monitor has the prompt.
//!
//! The typing runs for about two seconds after the lock, then the screen is
//! still and the loop is back to waking once a minute for the clock. The
//! banner takes the theme's accent colour when it reads well on the
//! background, otherwise the text colour.
//!
//! Nothing here draws or touches input; it is layout and timing only.

use std::time::Duration;

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

/// The block cursor drawn after the last typed character.
pub const CURSOR: &str = "█";

/// Typing pace: quick for the banner, so the prompt is not kept waiting,
/// calmer for the line.
const TYPE_BANNER: Duration = Duration::from_millis(5);
const TYPE_LINE: Duration = Duration::from_millis(45);
/// The pause between the banner and the line.
const BREATH: Duration = Duration::from_millis(250);
/// Frame interval while typing.
pub const FRAME: Duration = Duration::from_millis(16);

/// Characters in the banner, spaces included (they are typed too, so each
/// row ends where it should).
pub fn banner_chars() -> usize {
    BANNER.iter().map(|r| r.chars().count()).sum()
}

/// How much is typed `elapsed` after the lock: characters of the banner
/// (row after row) and of the line, and whether typing is done.
pub fn reveal(elapsed: Duration) -> (usize, usize, bool) {
    let total = banner_chars();
    let banner_time = TYPE_BANNER * total as u32;
    if elapsed < banner_time {
        return (
            (elapsed.as_millis() / TYPE_BANNER.as_millis()) as usize,
            0,
            false,
        );
    }
    let line_start = banner_time + BREATH;
    if elapsed < line_start {
        return (total, 0, false);
    }
    let line_total = TAGLINE.chars().count();
    let typed = ((elapsed - line_start).as_millis() / TYPE_LINE.as_millis()) as usize + 1;
    if typed >= line_total {
        (total, line_total, true)
    } else {
        (total, typed, false)
    }
}

/// The banner rows with `shown` characters typed, in order.
pub fn banner_rows(shown: usize) -> Vec<String> {
    let mut left = shown;
    BANNER
        .iter()
        .map(|row| {
            let n = row.chars().count().min(left);
            left -= n;
            row.chars().take(n).collect()
        })
        .collect()
}

/// When typing is done.
#[cfg(test)]
pub fn duration() -> Duration {
    TYPE_BANNER * banner_chars() as u32
        + BREATH
        + TYPE_LINE * (TAGLINE.chars().count() as u32).saturating_sub(1)
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
        // Same text as NIRUSCREENSAVER's scene.
        assert_eq!(w, 44);
    }

    #[test]
    fn typing_goes_banner_then_line_and_ends() {
        assert_eq!(reveal(Duration::ZERO), (0, 0, false));
        let (b, l, done) = reveal(Duration::from_millis(500));
        assert!(b > 0 && b < banner_chars() && l == 0 && !done);
        let after_banner = TYPE_BANNER * banner_chars() as u32;
        assert_eq!(
            reveal(after_banner + Duration::from_millis(100)),
            (banner_chars(), 0, false)
        );
        let (_, l, done) = reveal(after_banner + BREATH);
        assert_eq!((l, done), (1, false));
        assert_eq!(
            reveal(duration()),
            (banner_chars(), TAGLINE.chars().count(), true)
        );
        assert!(reveal(Duration::from_secs(3600)).2);
        // Short enough not to keep the prompt waiting.
        assert!(duration() < Duration::from_millis(2800), "{:?}", duration());
    }

    #[test]
    fn rows_fill_in_order() {
        let rows = banner_rows(50);
        assert_eq!(rows[0], BANNER[0]);
        assert_eq!(rows[1].chars().count(), 6);
        assert!(rows[2..].iter().all(String::is_empty));
        assert_eq!(
            banner_rows(banner_chars()),
            BANNER.map(String::from).to_vec()
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
            let size = banner_size(w, h, 0.55, 0.3, 0.6, 1.2);
            assert!(44.0 * 0.6 * size <= f32::from(w) * 0.56, "{w}x{h}: {size}");
            assert!(7.0 * 1.2 * size <= f32::from(h) * 0.31, "{w}x{h}: {size}");
        }
        assert!(
            banner_size(2560, 1440, 0.55, 0.3, 0.6, 1.2)
                > banner_size(1280, 800, 0.55, 0.3, 0.6, 1.2)
        );
    }

    #[test]
    fn a_dim_accent_falls_back_to_the_text_colour() {
        // Niru Noir's focus grey on black is too dim; a green accent is not.
        assert_eq!(banner_colour(0x5a5a60, 0xe5e5e5, 0x000000), 0xe5e5e5);
        assert_eq!(banner_colour(0x7fbf6a, 0xe5e5e5, 0x000000), 0x7fbf6a);
    }
}
