//! Quotes for the idle and lock screens, from collections the owner edits.
//!
//! `${XDG_CONFIG_HOME:-~/.config}/niru/quotes/` is shared by NIRULOCK and
//! NIRUSCREENSAVER. Each `NAME.txt` file in it is one collection, named
//! `NAME`. When the directory is missing, either program creates it with the
//! built-in collections ([`COLLECTIONS`], the files in `quotes/` of the
//! repository) atomically; it never writes into an existing directory, so a
//! collection the owner deleted stays deleted.
//!
//! The `quotes` setting selects collections by name with `*` and `?`
//! patterns (default `["*"]`, all of them). The quotes of every selected
//! collection form one pool, shown in a shuffled order: each quote once
//! before any repeats. When nothing is selected, nothing is readable or the
//! selected collections hold no quote, the built-in collections are used, so
//! the panel never ends up empty.
//!
//! The single file `niru/quotes.txt` of NIRULOCK 0.19 and NIRUSCREENSAVER
//! 0.11 still counts as the collection `quotes` once the owner edited it; the
//! untouched default is ignored.
//!
//! Format: quotes are separated by one or more blank lines; a quote may span
//! several lines and its line breaks are kept; a line starting with `— ` or
//! `-- ` is an attribution (drawn muted); a line starting with `#` is a
//! comment.
//!
//! The files are read on a [`Loader`] thread, never on the input or render
//! path, and read again on request at each quote change when a file or the
//! directory changed. Shared verbatim with NIRUSCREENSAVER, like `cava.rs`.

use crate::core::{Surface, TextRenderer};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::SystemTime,
};
use x11rb::protocol::xproto::{ChangeGCAux, ConnectionExt, Rectangle};

/// The built-in collections: written when the directory is missing, and the
/// fallback when nothing else yields a quote.
pub const COLLECTIONS: &[(&str, &str)] = &[
    ("havamal-en", include_str!("../quotes/havamal-en.txt")),
    ("havamal-sv", include_str!("../quotes/havamal-sv.txt")),
    ("stoic-en", include_str!("../quotes/stoic-en.txt")),
    ("stoic-sv", include_str!("../quotes/stoic-sv.txt")),
];

/// The legacy collection name, for `niru/quotes.txt`.
const LEGACY: &str = "quotes";
/// Length and FNV-1a 64 of the `niru/quotes.txt` that NIRULOCK 0.19 and
/// NIRUSCREENSAVER 0.11 wrote (SHA-256 635293d4…c8abf). That file, untouched,
/// is ignored: the same stanzas are in `havamal-sv`.
const LEGACY_DEFAULT: (u64, u64) = (6346, 0x503e_564b_fa32_dbb1);

/// At most this many collections are read; more are skipped.
const MAX_FILES: usize = 64;
/// A larger file is skipped.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// At most this many quotes are kept per collection, and in the pool.
const MAX_QUOTES: usize = 2000;
const MAX_POOL: usize = 10_000;
/// A quote's text is cut (with an ellipsis) beyond this many characters or
/// source lines; the layout cuts further when it still does not fit.
const MAX_CHARS: usize = 800;
const MAX_LINES: usize = 24;
/// Attribution lines kept per quote.
const MAX_ATTRIBUTION: usize = 3;
/// Patterns kept from the setting, and their length.
const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_CHARS: usize = 128;
const ELLIPSIS: char = '…';

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quote {
    /// The quote's lines, as written.
    pub lines: Vec<String>,
    /// Attribution lines, normalised to start with "— ".
    pub attribution: Vec<String>,
}

/// Parse a collection. Never fails: anything that is not a quote is skipped.
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

/// The `quotes` setting: one pattern or an array of them. Empty patterns are
/// dropped; an empty result selects nothing (the built-in collections show).
pub fn patterns(value: &toml::Value) -> Result<Vec<String>, String> {
    const ERROR: &str = "quotes must be a collection name pattern or an array of them";
    let list: Vec<&toml::Value> = match value {
        toml::Value::String(_) => vec![value],
        toml::Value::Array(items) => items.iter().collect(),
        _ => return Err(ERROR.into()),
    };
    let mut out = Vec::new();
    for item in list {
        let pattern = item.as_str().ok_or(ERROR)?.trim();
        if !pattern.is_empty() && out.len() < MAX_PATTERNS {
            out.push(pattern.chars().take(MAX_PATTERN_CHARS).collect());
        }
    }
    Ok(out)
}

/// The default `quotes` setting: every collection.
pub fn all() -> Vec<String> {
    vec!["*".to_string()]
}

/// Shell-style match of a collection name: `*` is any run of characters,
/// `?` any one character, everything else itself. Case-sensitive.
pub fn glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn selected(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| glob(p, name))
}

/// `${XDG_CONFIG_HOME:-~/.config}/niru`.
pub fn base() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("niru"))
}

/// Create `dir` with the built-in collections if it does not exist. Atomic
/// and never into an existing directory, even one another program creates at
/// the same moment: the files go to a private temporary directory that is
/// then renamed into place, which fails if the name is taken. Returns true
/// when created.
pub fn ensure_defaults(dir: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(dir) {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        Err(_) => {}
    }
    let parent = dir.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".quotes.{}.tmp", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let result = (|| {
        fs::DirBuilder::new().mode(0o755).create(&temp)?;
        // Modes are filtered by the umask; these are meant to be readable
        // like any other config files.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
        for (name, text) in COLLECTIONS {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(temp.join(format!("{name}.txt")))?;
            file.set_permissions(fs::Permissions::from_mode(0o644))?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        match fs::rename(&temp, dir) {
            Ok(()) => Ok(true),
            Err(e)
                if matches!(e.raw_os_error(), Some(libc::EEXIST | libc::ENOTEMPTY))
                    || e.kind() == io::ErrorKind::AlreadyExists =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

/// FNV-1a 64 of `bytes`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// One selected collection file, with what identifies its version.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
}

/// What [`load`] read: the directory's state and every selected file. When
/// it is unchanged, the pool is too.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    dir: Option<SystemTime>,
    sources: Vec<Source>,
}

fn source(path: PathBuf) -> Option<Source> {
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    Some(Source {
        modified: meta.modified().ok(),
        len: meta.len(),
        path,
    })
}

/// The selected collection files under `base` (`niru/`), by name, then the
/// legacy file. Only metadata is read.
pub fn snapshot(base: &Path, patterns: &[String]) -> Snapshot {
    let dir = base.join("quotes");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.'))
                .filter_map(|n| n.strip_suffix(".txt").map(str::to_string))
                .filter(|n| !n.is_empty() && selected(patterns, n))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut sources: Vec<Source> = names
        .into_iter()
        .filter_map(|n| source(dir.join(format!("{n}.txt"))))
        .take(MAX_FILES)
        .collect();
    if selected(patterns, LEGACY) {
        if let Some(legacy) = source(base.join("quotes.txt")) {
            sources.push(legacy);
        }
    }
    Snapshot {
        dir: fs::metadata(&dir).and_then(|m| m.modified()).ok(),
        sources,
    }
}

/// A file's quotes; none when it is unreadable, too large, or the untouched
/// legacy default.
fn read(path: &Path) -> Vec<Quote> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_FILE_BYTES
    {
        return Vec::new();
    }
    if (bytes.len() as u64, fnv(&bytes)) == LEGACY_DEFAULT {
        return Vec::new();
    }
    parse(&String::from_utf8_lossy(&bytes))
}

/// The pool of a snapshot, or the built-in collections when it is empty.
pub fn load(snapshot: &Snapshot, patterns: &[String]) -> Vec<Quote> {
    let mut pool = Vec::new();
    for source in &snapshot.sources {
        let room = MAX_POOL - pool.len();
        pool.extend(read(&source.path).into_iter().take(room));
    }
    if pool.is_empty() {
        pool = builtin(patterns);
    }
    pool
}

/// The built-in collections the patterns select, or all of them.
pub fn builtin(patterns: &[String]) -> Vec<Quote> {
    let chosen: Vec<&str> = COLLECTIONS
        .iter()
        .filter(|(name, _)| selected(patterns, name))
        .map(|(_, text)| *text)
        .collect();
    let texts = if chosen.is_empty() {
        COLLECTIONS.iter().map(|(_, text)| *text).collect()
    } else {
        chosen
    };
    texts.into_iter().flat_map(parse).take(MAX_POOL).collect()
}

/// A shuffled order over the pool: every quote once before any repeats, and
/// never the same quote twice in a row across a reshuffle.
pub struct Deck {
    order: Vec<usize>,
    next: usize,
    last: Option<usize>,
    rng: u64,
}

impl Deck {
    pub fn new(seed: u64) -> Self {
        Self {
            order: Vec::new(),
            next: 0,
            last: None,
            rng: seed | 1,
        }
    }

    /// Start a new order, for a new pool of quotes.
    pub fn reset(&mut self) {
        self.order.clear();
        self.next = 0;
        self.last = None;
    }

    /// The index of the next quote in a pool of `len`.
    pub fn draw(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        if self.order.len() != len || self.next >= len {
            self.order = (0..len).collect();
            for i in (1..len).rev() {
                let j = (self.random() % (i as u64 + 1)) as usize;
                self.order.swap(i, j);
            }
            if len > 1 && self.last == Some(self.order[0]) {
                let j = 1 + (self.random() % (len as u64 - 1)) as usize;
                self.order.swap(0, j);
            }
            self.next = 0;
        }
        let index = self.order[self.next];
        self.next += 1;
        self.last = Some(index);
        Some(index)
    }

    fn random(&mut self) -> u64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// Reads the collections on its own thread.
pub struct Loader {
    results: mpsc::Receiver<Vec<Quote>>,
    requests: mpsc::Sender<()>,
}

impl Loader {
    /// Start reading the collections `patterns` select; `wake` gets one byte
    /// whenever a result is ready. The first result always comes.
    pub fn start(wake: Option<UnixStream>, patterns: Vec<String>) -> Option<Self> {
        let (send_result, results) = mpsc::channel();
        let (requests, receive_request) = mpsc::channel::<()>();
        thread::Builder::new()
            .name("niru-quotes".into())
            .spawn(move || {
                let mut wake = wake;
                let base = base();
                if let Some(base) = base.as_deref() {
                    let _ = ensure_defaults(&base.join("quotes"));
                }
                let scan = || {
                    base.as_deref()
                        .map(|b| snapshot(b, &patterns))
                        .unwrap_or_default()
                };
                let mut seen = scan();
                let mut first = true;
                loop {
                    if !first {
                        // One check per request; nothing runs in between.
                        if receive_request.recv().is_err() {
                            return;
                        }
                        let now = scan();
                        if now == seen {
                            continue;
                        }
                        seen = now;
                    }
                    first = false;
                    if send_result.send(load(&seen, &patterns)).is_err() {
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

    /// Ask the thread to read the collections again if they changed.
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
    fn every_built_in_collection_parses_into_attributed_quotes() {
        for (name, text) in COLLECTIONS {
            let quotes = parse(text);
            assert!(!quotes.is_empty(), "{name}");
            for quote in &quotes {
                assert!(!quote.attribution.is_empty(), "{name}: {:?}", quote.lines);
                let chars: usize = quote.lines.iter().map(|l| l.chars().count()).sum();
                assert!(chars < MAX_CHARS && quote.lines.len() < MAX_LINES, "{name}");
                assert!(!quote.lines.iter().any(|l| l.ends_with(ELLIPSIS)), "{name}");
            }
            assert!(text.starts_with("# NIRU quotes: "), "{name}");
            assert!(text.len() as u64 <= MAX_FILE_BYTES);
        }
        let names: Vec<&str> = COLLECTIONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["havamal-en", "havamal-sv", "stoic-en", "stoic-sv"]);
        let sv = parse(COLLECTIONS[1].1);
        assert_eq!(sv.len(), 22);
        assert_eq!(sv[0].lines[0], "Alla dörrar,");
        assert!(sv
            .iter()
            .all(|q| q.attribution[0].ends_with(" (övers. Erik Brate, 1913)")));
    }

    #[test]
    fn glob_patterns_match_collection_names() {
        assert!(glob("*", "havamal-sv"));
        assert!(glob("*", ""));
        assert!(glob("havamal-*", "havamal-en"));
        assert!(!glob("havamal-*", "stoic-en"));
        assert!(glob("*-en", "stoic-en"));
        assert!(glob("stoic-??", "stoic-sv"));
        assert!(!glob("stoic-?", "stoic-sv"));
        assert!(glob("h*a*l-sv", "havamal-sv"));
        assert!(!glob("*a*", "stoic-sv"));
        assert!(glob("stoic-en", "stoic-en"));
        assert!(!glob("stoic-en", "stoic-env"));
        assert!(!glob("Stoic-en", "stoic-en"));
        assert!(glob("åäö*", "åäö-citat"));
        assert!(glob("**", "x"));
    }

    #[test]
    fn the_setting_is_a_pattern_or_an_array_of_them() {
        let v = |s: &str| s.parse::<toml::Value>().unwrap()["quotes"].clone();
        assert_eq!(
            patterns(&v("quotes = \"havamal-*\"")).unwrap(),
            ["havamal-*"]
        );
        assert_eq!(
            patterns(&v("quotes = [\"stoic-en\", \" havamal-sv \", \"\"]")).unwrap(),
            ["stoic-en", "havamal-sv"]
        );
        assert!(patterns(&v("quotes = []")).unwrap().is_empty());
        assert!(patterns(&v("quotes = 3")).is_err());
        assert!(patterns(&v("quotes = [\"a\", 3]")).is_err());
        assert_eq!(all(), ["*"]);
    }

    #[test]
    fn the_deck_shows_every_quote_once_and_never_twice_in_a_row() {
        for seed in 1..200u64 {
            let mut deck = Deck::new(seed);
            let mut last = None;
            for _ in 0..5 {
                let mut round: Vec<usize> = (0..7).map(|_| deck.draw(7).unwrap()).collect();
                assert_ne!(Some(round[0]), last, "seed {seed}");
                last = round.last().copied();
                round.sort();
                assert_eq!(round, (0..7).collect::<Vec<_>>(), "seed {seed}");
            }
        }
        let mut deck = Deck::new(9);
        assert_eq!(deck.draw(0), None);
        assert_eq!(deck.draw(1), Some(0));
        assert_eq!(deck.draw(1), Some(0));
        // A new pool size starts a new order at once.
        assert!(deck.draw(3).unwrap() < 3);
        deck.reset();
        assert!(deck.draw(2).unwrap() < 2);
    }

    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("niru-quotes-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn pool(base: &Path, patterns: &[&str]) -> Vec<Quote> {
        let patterns: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        load(&snapshot(base, &patterns), &patterns)
    }

    #[test]
    fn the_directory_is_created_once_and_never_written_into() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new("seed");
        let dir = temp.0.join("niru/quotes");
        assert!(ensure_defaults(&dir).unwrap());
        for (name, text) in COLLECTIONS {
            let path = dir.join(format!("{name}.txt"));
            assert_eq!(fs::read_to_string(&path).unwrap(), *text);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // A deleted collection stays deleted, an edited one stays edited.
        fs::remove_file(dir.join("stoic-sv.txt")).unwrap();
        fs::write(dir.join("havamal-en.txt"), "Mine\n").unwrap();
        assert!(!ensure_defaults(&dir).unwrap());
        assert!(!dir.join("stoic-sv.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.join("havamal-en.txt")).unwrap(),
            "Mine\n"
        );
        // An empty directory is the owner's too.
        fs::remove_dir_all(&dir).unwrap();
        fs::create_dir(&dir).unwrap();
        assert!(!ensure_defaults(&dir).unwrap());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        // No temporary directory is left behind.
        let names: Vec<_> = fs::read_dir(temp.0.join("niru")).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn patterns_select_collections_and_the_pool_joins_them() {
        let temp = Temp::new("select");
        let base = temp.0.join("niru");
        let dir = base.join("quotes");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a-en.txt"), "A1\n\nA2\n").unwrap();
        fs::write(dir.join("b-en.txt"), "B1\n").unwrap();
        fs::write(dir.join("c-sv.txt"), "C1\n").unwrap();
        fs::write(dir.join(".hidden.txt"), "H\n").unwrap();
        fs::write(dir.join("notes.md"), "M\n").unwrap();
        fs::write(dir.join(".txt"), "E\n").unwrap();
        fs::create_dir(dir.join("sub.txt")).unwrap();
        let first = |p: &[&str]| -> Vec<String> {
            pool(&base, p)
                .into_iter()
                .map(|q| q.lines[0].clone())
                .collect()
        };
        assert_eq!(first(&["*"]), ["A1", "A2", "B1", "C1"]);
        assert_eq!(first(&["*-en"]), ["A1", "A2", "B1"]);
        assert_eq!(first(&["c-sv", "b-*"]), ["B1", "C1"]);
        // Nothing selected, unknown names or empty collections: built-ins.
        let builtin_all = builtin(&[]).len();
        assert!(builtin_all > 22);
        assert_eq!(pool(&base, &[]).len(), builtin_all);
        assert_eq!(pool(&base, &["nope"]).len(), builtin_all);
        fs::write(dir.join("empty.txt"), "# only comments\n").unwrap();
        assert_eq!(pool(&base, &["empty"]).len(), builtin_all);
        // Patterns that also name a built-in collection pick it.
        assert_eq!(pool(&base, &["havamal-sv"]).len(), 22);
        assert_eq!(pool(&base, &["nope", "havamal-sv"]).len(), 22);
        // A missing directory is the same as an empty one.
        assert_eq!(pool(&temp.0.join("none"), &["*"]).len(), builtin_all);
    }

    #[test]
    fn limits_skip_large_files_and_extra_collections() {
        let temp = Temp::new("limits");
        let base = temp.0.join("niru");
        let dir = base.join("quotes");
        fs::create_dir_all(&dir).unwrap();
        let big = "x\n\n".repeat(MAX_FILE_BYTES as usize / 3 + 1);
        assert!(big.len() as u64 > MAX_FILE_BYTES);
        fs::write(dir.join("big.txt"), &big).unwrap();
        fs::write(dir.join("small.txt"), "S\n").unwrap();
        assert_eq!(pool(&base, &["*"]).len(), 1);
        for n in 0..MAX_FILES + 10 {
            fs::write(dir.join(format!("n{n:03}.txt")), format!("Q{n}\n")).unwrap();
        }
        assert_eq!(snapshot(&base, &all()).sources.len(), MAX_FILES);
        assert_eq!(pool(&base, &["*"]).len(), MAX_FILES);
    }

    #[test]
    fn the_legacy_file_counts_only_once_edited() {
        let temp = Temp::new("legacy");
        let base = temp.0.join("niru");
        fs::create_dir_all(base.join("quotes")).unwrap();
        fs::write(base.join("quotes/mine.txt"), "M\n").unwrap();
        // The text of the old default file reproduces its fingerprint.
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        let legacy = base.join("quotes.txt");
        fs::write(&legacy, "Edited\n— Me\n").unwrap();
        let first = |p: &[&str]| -> Vec<String> {
            pool(&base, p)
                .into_iter()
                .map(|q| q.lines[0].clone())
                .collect()
        };
        assert_eq!(first(&["*"]), ["M", "Edited"]);
        assert_eq!(first(&["quotes"]), ["Edited"]);
        assert_eq!(first(&["mine"]), ["M"]);
        // A file with the default's length and fingerprint is skipped.
        assert_eq!(read(&legacy).len(), 1);
        let mut probe = Vec::new();
        probe.resize(LEGACY_DEFAULT.0 as usize, b'x');
        assert_ne!(fnv(&probe), LEGACY_DEFAULT.1);
    }

    #[test]
    fn a_snapshot_changes_with_a_file_or_the_directory() {
        let temp = Temp::new("snap");
        let base = temp.0.join("niru");
        let dir = base.join("quotes");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        let one = snapshot(&base, &all());
        assert_eq!(one, snapshot(&base, &all()));
        fs::write(dir.join("a.txt"), "A longer\n").unwrap();
        let two = snapshot(&base, &all());
        assert_ne!(one, two);
        fs::write(dir.join("b.txt"), "B\n").unwrap();
        assert_ne!(two, snapshot(&base, &all()));
        // A file the patterns do not select does not count.
        let only_a = vec!["a".to_string()];
        let before = snapshot(&base, &only_a).sources;
        fs::write(dir.join("c.txt"), "C\n").unwrap();
        assert_eq!(before, snapshot(&base, &only_a).sources);
    }
}
