//! Quotes for the idle and lock screens, from a file the owner edits.
//!
//! `${XDG_CONFIG_HOME:-~/.config}/niru/quotes.txt` is shared by NIRULOCK and
//! NIRUSCREENSAVER. When it is missing, either program writes the default
//! file (stanzas of Hávamál in Erik Brate's 1913 translation, see
//! `quotes.txt` next to this file) atomically and never over an existing
//! file. An unreadable or empty file shows nothing and says nothing.
//!
//! Format: quotes are separated by one or more blank lines; a quote may span
//! several lines and its line breaks are kept; a line starting with `— ` or
//! `-- ` is an attribution (drawn muted); a line starting with `#` is a
//! comment.
//!
//! The file is read on a [`Loader`] thread, never on the input or render
//! path, and read again (when its mtime changed) on request at each quote
//! change. Shared verbatim with NIRUSCREENSAVER, like `cava.rs`.

use crate::core::{Surface, TextRenderer};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::SystemTime,
};
use x11rb::protocol::xproto::{ChangeGCAux, ConnectionExt, Rectangle};

/// The default file, written when none exists.
pub const DEFAULT: &str = include_str!("quotes.txt");

/// At most this much of the file is read.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// At most this many quotes are kept.
const MAX_QUOTES: usize = 2000;
/// A quote's text is cut (with an ellipsis) beyond this many characters or
/// source lines; the layout cuts further when it still does not fit.
const MAX_CHARS: usize = 800;
const MAX_LINES: usize = 24;
/// Attribution lines kept per quote.
const MAX_ATTRIBUTION: usize = 3;
const ELLIPSIS: char = '…';

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quote {
    /// The quote's lines, as written.
    pub lines: Vec<String>,
    /// Attribution lines, normalised to start with "— ".
    pub attribution: Vec<String>,
}

/// Parse the quotes file. Never fails: anything that is not a quote is
/// skipped.
pub fn parse(text: &str) -> Vec<Quote> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut quotes = Vec::new();
    let mut current = Quote::default();
    let mut chars = 0usize;
    let finish = |current: &mut Quote, chars: &mut usize, quotes: &mut Vec<Quote>| {
        let quote = std::mem::take(current);
        *chars = 0;
        if !quote.lines.is_empty() && quotes.len() < MAX_QUOTES {
            quotes.push(quote);
        }
    };
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            finish(&mut current, &mut chars, &mut quotes);
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let attribution = trimmed
            .strip_prefix("— ")
            .or_else(|| trimmed.strip_prefix("-- "))
            .map(str::trim);
        if let Some(by) = attribution {
            if !by.is_empty() && current.attribution.len() < MAX_ATTRIBUTION {
                current
                    .attribution
                    .push(format!("— {}", cut(by, MAX_CHARS / 4)));
            }
            continue;
        }
        // A quote line after the attribution starts a new quote only after
        // a blank line; here it simply continues the text.
        if current.lines.len() >= MAX_LINES || chars >= MAX_CHARS {
            mark_cut(&mut current.lines);
            continue;
        }
        let line = cut(trimmed, MAX_CHARS - chars);
        chars += line.chars().count();
        current.lines.push(line);
    }
    finish(&mut current, &mut chars, &mut quotes);
    quotes
}

/// `text` limited to `max` characters, with an ellipsis when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push(ELLIPSIS);
    out
}

/// End the last line with an ellipsis, once.
fn mark_cut(lines: &mut [String]) {
    if let Some(last) = lines.last_mut() {
        if !last.ends_with(ELLIPSIS) {
            last.push(ELLIPSIS);
        }
    }
}

/// `${XDG_CONFIG_HOME:-~/.config}/niru/quotes.txt`.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("niru").join("quotes.txt"))
}

/// Write the default file if `path` does not exist. Atomic and never over
/// an existing file, even one another program creates at the same moment:
/// the content goes to a private temporary file that is then hard-linked
/// into place, which fails if the name is taken. Returns true when written.
pub fn ensure_default(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        Err(_) => {}
    }
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".quotes.txt.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temp);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temp)?;
        // The mode above is filtered by the umask; the file is meant to be
        // readable like any other config file.
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
        file.write_all(DEFAULT.as_bytes())?;
        file.sync_all()?;
        match fs::hard_link(&temp, path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    })();
    let _ = fs::remove_file(&temp);
    result
}

/// The file's quotes and modification time; empty when it is unreadable.
fn read(path: &Path) -> (Vec<Quote>, Option<SystemTime>) {
    let Ok(file) = fs::File::open(path) else {
        return (Vec::new(), None);
    };
    let modified = file.metadata().and_then(|m| m.modified()).ok();
    let mut bytes = Vec::new();
    if file.take(MAX_FILE_BYTES).read_to_end(&mut bytes).is_err() {
        return (Vec::new(), modified);
    }
    (parse(&String::from_utf8_lossy(&bytes)), modified)
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Reads the quotes file on its own thread.
pub struct Loader {
    results: mpsc::Receiver<Vec<Quote>>,
    requests: mpsc::Sender<()>,
}

impl Loader {
    /// Start reading; `wake` gets one byte whenever a result is ready. The
    /// first result (possibly empty) always comes.
    pub fn start(wake: Option<UnixStream>) -> Option<Self> {
        let (send_result, results) = mpsc::channel();
        let (requests, receive_request) = mpsc::channel::<()>();
        thread::Builder::new()
            .name("niru-quotes".into())
            .spawn(move || {
                let mut wake = wake;
                let path = path();
                if let Some(path) = path.as_deref() {
                    let _ = ensure_default(path);
                }
                let (quotes, mut seen) = path.as_deref().map(read).unwrap_or_default();
                if send_result.send(quotes).is_err() {
                    return;
                }
                if let Some(wake) = wake.as_mut() {
                    let _ = wake.write(&[1]);
                }
                // One check per request; nothing runs in between.
                while receive_request.recv().is_ok() {
                    let Some(path) = path.as_deref() else {
                        continue;
                    };
                    let now = modified(path);
                    if now == seen {
                        continue;
                    }
                    let (quotes, at) = read(path);
                    seen = at;
                    if send_result.send(quotes).is_err() {
                        return;
                    }
                    if let Some(wake) = wake.as_mut() {
                        let _ = wake.write(&[1]);
                    }
                }
            })
            .ok()?;
        Some(Self { results, requests })
    }

    /// Ask the thread to read the file again if it changed.
    pub fn check(&self) {
        let _ = self.requests.send(());
    }

    /// The newest result, if one arrived.
    pub fn take(&self) -> Option<Vec<Quote>> {
        self.results.try_iter().last()
    }
}

/// One laid-out quote: the font size and the lines to draw, each flagged
/// when it is an attribution.
#[derive(Clone, Debug, PartialEq)]
pub struct Arranged {
    pub size: f32,
    pub lines: Vec<(String, bool)>,
}

/// Break one line into pieces at most `max` wide (as `width` measures),
/// at spaces where possible.
pub fn wrap(line: &str, max: i32, width: &mut dyn FnMut(&str) -> i32) -> Vec<String> {
    if width(line) <= max {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split(' ').filter(|w| !w.is_empty()) {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if width(&candidate) <= max {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        // A single word wider than the line is broken by characters.
        let mut piece = String::new();
        for ch in word.chars() {
            piece.push(ch);
            if width(&piece) > max && piece.chars().count() > 1 {
                piece.pop();
                out.push(std::mem::take(&mut piece));
                piece.push(ch);
            }
        }
        current = piece;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Lay out `quote` in a box `max_w` by `max_h` pixels, trying `sizes` from
/// the largest. `width(size, text)` and `line_height(size)` measure the font.
/// At the smallest size a quote that still does not fit is cut, with an
/// ellipsis, after the last line that fits; attributions are kept.
pub fn arrange(
    quote: &Quote,
    max_w: i32,
    max_h: i32,
    sizes: &[f32],
    width: &mut dyn FnMut(f32, &str) -> i32,
    line_height: &dyn Fn(f32) -> i32,
) -> Option<Arranged> {
    let smallest = *sizes.last()?;
    for &size in sizes {
        let (body, by) = wrapped(quote, max_w, size, width);
        let lh = line_height(size).max(1);
        if height(body.len(), by.len(), lh) <= max_h {
            return Some(Arranged {
                size,
                lines: join(body, by),
            });
        }
        if size == smallest {
            let mut body = body;
            let mut by = by;
            while height(1, by.len(), lh) > max_h && !by.is_empty() {
                by.pop();
            }
            while body.len() > 1 && height(body.len(), by.len(), lh) > max_h {
                body.pop();
            }
            if height(body.len(), by.len(), lh) > max_h {
                return None;
            }
            if let Some(last) = body.last_mut() {
                if !last.ends_with(ELLIPSIS) {
                    last.push(ELLIPSIS);
                    while width(size, last) > max_w && last.chars().count() > 1 {
                        last.pop();
                        last.pop();
                        last.push(ELLIPSIS);
                    }
                }
            }
            return Some(Arranged {
                size,
                lines: join(body, by),
            });
        }
    }
    None
}

fn wrapped(
    quote: &Quote,
    max_w: i32,
    size: f32,
    width: &mut dyn FnMut(f32, &str) -> i32,
) -> (Vec<String>, Vec<String>) {
    let mut measure = |text: &str| width(size, text);
    let body = quote
        .lines
        .iter()
        .flat_map(|line| wrap(line, max_w, &mut measure))
        .collect();
    let by = quote
        .attribution
        .iter()
        .flat_map(|line| wrap(line, max_w, &mut measure))
        .collect();
    (body, by)
}

/// Pixel height of a block: the body, then half a line, then attributions.
fn height(body: usize, by: usize, lh: i32) -> i32 {
    let gap = if by > 0 { lh / 2 } else { 0 };
    (body + by) as i32 * lh + gap
}

fn join(body: Vec<String>, by: Vec<String>) -> Vec<(String, bool)> {
    body.into_iter()
        .map(|l| (l, false))
        .chain(by.into_iter().map(|l| (l, true)))
        .collect()
}

/// Fill a rectangle of the backing pixmap.
pub fn fill(
    surface: &Surface,
    colour: u32,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
) -> crate::Result<()> {
    surface
        .c
        .change_gc(surface.gc, &ChangeGCAux::new().foreground(colour))?;
    surface.c.poly_fill_rectangle(
        surface.back,
        surface.gc,
        &[Rectangle {
            x,
            y,
            width,
            height,
        }],
    )?;
    Ok(())
}

/// Draw an arranged quote centred in the rectangle, over `background`.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    surface: &Surface,
    renderer: &mut TextRenderer,
    arranged: &Arranged,
    rect: (i16, i16, u16, u16),
    text: u32,
    muted: u32,
    background: u32,
) -> crate::Result<()> {
    let (x, y, w, h) = rect;
    fill(surface, background, x, y, w, h)?;
    let lh = i32::from(renderer.line_height()).max(1);
    let by = arranged.lines.iter().filter(|(_, a)| *a).count();
    let block = height(arranged.lines.len() - by, by, lh);
    let top = i32::from(y) + (i32::from(h) - block).max(0) / 2;
    let centre = (i32::from(x) + i32::from(w) / 2) as i16;
    // The baseline sits about a fifth of a line above the line's bottom.
    let mut bottom = top;
    let mut first_attribution = true;
    for (line, attribution) in &arranged.lines {
        if *attribution && first_attribution {
            bottom += lh / 2;
            first_attribution = false;
        }
        bottom += lh;
        let baseline = (bottom - lh / 5).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        renderer.draw_centred(
            &surface.c,
            surface.gc,
            surface.back,
            surface.depth,
            centre,
            baseline,
            line,
            if *attribution { muted } else { text },
            background,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(lines: &[&str], by: &[&str]) -> Quote {
        Quote {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            attribution: by.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn blank_lines_separate_quotes_and_line_breaks_are_kept() {
        let text = "Ett\ntvå\n\n\n\nTre\n  \nFyra\n";
        assert_eq!(
            parse(text),
            vec![q(&["Ett", "två"], &[]), q(&["Tre"], &[]), q(&["Fyra"], &[])]
        );
    }

    #[test]
    fn comments_are_skipped_and_do_not_separate() {
        let text = "# rubrik\n# mer\nEtt\n# mitt i\ntvå\n\n#bara kommentar\n\n";
        assert_eq!(parse(text), vec![q(&["Ett", "två"], &[])]);
    }

    #[test]
    fn attributions_with_em_dash_or_double_hyphen() {
        let text = "Fä dör\n— Hávamál 76\n\nAnnat\n-- Någon\n\nInte -- detta\n—utan mellanslag\n";
        assert_eq!(
            parse(text),
            vec![
                q(&["Fä dör"], &["— Hávamál 76"]),
                q(&["Annat"], &["— Någon"]),
                q(&["Inte -- detta", "—utan mellanslag"], &[]),
            ]
        );
        // An attribution alone is not a quote.
        assert!(parse("— Ingen\n\n-- Ingen\n").is_empty());
    }

    #[test]
    fn crlf_and_a_byte_order_mark_are_handled() {
        let text = "\u{feff}Ett\r\ntvå\r\n— Någon\r\n\r\nTre\r\n";
        assert_eq!(
            parse(text),
            vec![q(&["Ett", "två"], &["— Någon"]), q(&["Tre"], &[])]
        );
    }

    #[test]
    fn empty_and_comment_only_files_have_no_quotes() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n\r\n  \n").is_empty());
        assert!(parse("# bara\n# kommentarer\n").is_empty());
    }

    #[test]
    fn a_very_long_quote_is_capped() {
        let long = "ord ".repeat(5000);
        let many: String = (0..200).map(|n| format!("rad {n}\n")).collect();
        let quotes = parse(&format!("{long}\n— X\n\n{many}"));
        assert_eq!(quotes.len(), 2);
        let chars: usize = quotes[0].lines.iter().map(|l| l.chars().count()).sum();
        assert!(chars <= MAX_CHARS);
        assert!(quotes[0].lines[0].ends_with(ELLIPSIS));
        assert_eq!(quotes[0].attribution, vec!["— X".to_string()]);
        assert_eq!(quotes[1].lines.len(), MAX_LINES);
        assert!(quotes[1].lines.last().unwrap().ends_with(ELLIPSIS));
    }

    #[test]
    fn the_default_file_parses_into_attributed_havamal_stanzas() {
        let quotes = parse(DEFAULT);
        assert!((15..=25).contains(&quotes.len()), "{}", quotes.len());
        for quote in &quotes {
            assert_eq!(quote.attribution.len(), 1);
            assert!(quote.attribution[0].starts_with("— Hávamál "));
            assert!(quote.attribution[0].ends_with(" (övers. Erik Brate, 1913)"));
            assert!((3..=12).contains(&quote.lines.len()));
        }
        assert_eq!(quotes[0].lines[0], "Alla dörrar,");
        assert!(DEFAULT.contains("https://runeberg.org/eddan/se-02.html"));
    }

    /// A monospace font of `size` pixels: every character is half as wide.
    fn mono(size: f32, text: &str) -> i32 {
        (text.chars().count() as f32 * size * 0.5).ceil() as i32
    }

    #[test]
    fn long_lines_wrap_at_spaces_and_long_words_by_characters() {
        let mut width = |t: &str| mono(10.0, t);
        assert_eq!(wrap("kort", 50, &mut width), vec!["kort"]);
        assert_eq!(
            wrap("ett två tre fyra fem", 50, &mut width),
            vec!["ett två", "tre fyra", "fem"]
        );
        assert_eq!(
            wrap("abcdefghijklmnop", 50, &mut width),
            vec!["abcdefghij", "klmnop"]
        );
    }

    #[test]
    fn arrange_shrinks_the_font_and_cuts_only_when_needed() {
        let quote = q(&["a"; 6], &["— b"]);
        let sizes = [20.0, 16.0, 12.0];
        let lh = |s: f32| (s * 1.25) as i32;
        let mut width = mono;
        // 6 + 1 lines at 25 px and a 12 px gap fit in 200 px.
        let a = arrange(&quote, 400, 200, &sizes, &mut width, &lh).unwrap();
        assert_eq!(a.size, 20.0);
        assert_eq!(a.lines.len(), 7);
        assert!(a.lines[6].1 && !a.lines[5].1);
        // 7 lines at 20 px plus a 10 px gap is 150 > 140; at 15 px, 112 fits.
        let a = arrange(&quote, 400, 140, &sizes, &mut width, &lh).unwrap();
        assert_eq!(a.size, 12.0);
        // Too small even at 12 px: cut with an ellipsis, attribution kept.
        let a = arrange(&quote, 400, 60, &sizes, &mut width, &lh).unwrap();
        assert_eq!(a.size, 12.0);
        assert_eq!(a.lines.len(), 3);
        assert_eq!(a.lines[1].0, "a…");
        assert_eq!(a.lines[2], ("— b".to_string(), true));
        // Nothing fits at all.
        assert!(arrange(&quote, 400, 5, &sizes, &mut width, &lh).is_none());
    }

    #[test]
    fn the_default_file_is_written_once_and_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("niru-quotes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("niru/quotes.txt");
        assert!(ensure_default(&path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT);
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        fs::write(&path, "Mitt eget citat\n").unwrap();
        assert!(!ensure_default(&path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "Mitt eget citat\n");
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(dir.join("niru")).unwrap().count(), 1);
        // An empty file stays empty and has no quotes.
        fs::write(&path, "").unwrap();
        assert!(!ensure_default(&path).unwrap());
        assert!(read(&path).0.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
