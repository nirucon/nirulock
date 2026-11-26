//! The `bbs` layout: "NIRUX BBS", a dial-up bulletin board of the modem
//! era.
//!
//! The lock screen is a terminal program on an 80x30 text screen (8x16
//! character cells, like VGA text mode). When the screen locks, the modem is
//! reset and dials (`ATZ`, `ATDT 555-64789`, `RINGING`, `CARRIER 14400`,
//! `CONNECT 14400/ARQ/V42BIS`), and then the board's ANSI screen comes in at
//! the speed of the line, cell by cell: the Yggdrasil picture in its frame,
//! the NIRUX logo, the system box (node, connection, date, time, time online
//! and status), the message of the day (a quote from the `quotes`
//! collections), the tune that plays (artist and title only) and the
//! `LOGIN:` and `PASSWORD:` prompt. A status bar at the bottom shows the
//! terminal's settings, the time online, Caps Lock and the modem's lights.
//! Every [`RELOAD`] the line drops (`NO CARRIER`) and the board is called
//! again, unless someone is typing.
//!
//! The static part of the board is a real ANSI file,
//! `assets/nirux-bbs.ans` (CP437 characters, ANSI colours, a SAUCE record),
//! drawn by `tools/bbs-art.py`; [`parse`] reads it. The block, shade and box
//! characters are drawn here as exact pixel patterns, so they need no font
//! and always join up; letters and digits use NIRUSID's 8x8 font
//! ([`crate::pixfont`]) at double height, as on a CGA screen. Anything else
//! becomes its plain letter or is left out.
//!
//! The 16 ANSI colours are roles mapped onto the active NIRUX theme
//! ([`ANSI`]): the accent in three shades (brown, light blue, yellow), its
//! cool neighbour (blue, cyan, light cyan), its warm neighbour (the greens
//! and magenta), the warning colour (the reds) and greys from the theme's
//! muted and text colours, on black.
//!
//! Typing skips the call and shows the board at once; the password entry
//! honours `prompt_echo`, and a failed attempt says ACCESS DENIED in the
//! warning colour. With `[motion] reduced` in the appearance contract the
//! board shows at once, the cursor does not blink and the line never drops.
//!
//! Only the character cells that changed are drawn (PutImage of their
//! rows): at rest that is the cursor's blink twice a second, and nothing at
//! all with the display off.

use crate::{
    chunky::{self, slot::*, Canvas, Palette, Screen},
    display::Area,
    layout, modem, pixfont, Lock, Result,
};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder};

/// The text screen.
pub const COLS: usize = 80;
pub const ROWS: usize = 30;
/// A character cell in canvas pixels.
pub const CW: i32 = 8;
pub const CH: i32 = 16;
pub const W: i32 = COLS as i32 * CW;
pub const H: i32 = ROWS as i32 * CH;
/// The board's ANSI screen (all rows but the status bar).
const SCREEN: &[u8] = include_bytes!("../assets/nirux-bbs.ans");

/// The ANSI colours by name.
#[allow(dead_code)]
pub mod ansi {
    pub const BLACK: u8 = 0;
    pub const RED: u8 = 1;
    pub const GREEN: u8 = 2;
    pub const BROWN: u8 = 3;
    pub const BLUE: u8 = 4;
    pub const MAGENTA: u8 = 5;
    pub const CYAN: u8 = 6;
    pub const GREY: u8 = 7;
    pub const DARK_GREY: u8 = 8;
    pub const LIGHT_RED: u8 = 9;
    pub const LIGHT_GREEN: u8 = 10;
    pub const YELLOW: u8 = 11;
    pub const LIGHT_BLUE: u8 = 12;
    pub const LIGHT_MAGENTA: u8 = 13;
    pub const LIGHT_CYAN: u8 = 14;
    pub const WHITE: u8 = 15;
}

/// The ANSI colour roles on the theme's palette ([`chunky::Palette`]).
pub const ANSI: [u8; 16] = [
    BLACK,
    WARNING,
    WARM,
    ACCENT_DARK,
    COOL_DARK,
    WARM_DARK,
    COOL,
    GREY_LIGHT,
    GREY_DARK,
    FLASH,
    WARM_LIGHT,
    ACCENT_LIGHT,
    ACCENT,
    GREY,
    COOL_LIGHT,
    WHITE,
];

/// The line drops and the board is called again this long after a call
/// began.
pub const RELOAD: f32 = 300.0;
/// No new call while someone typed in the last this many seconds.
const QUIET: Duration = Duration::from_secs(60);
/// Characters a second over the line: 14400 bps, ten bits a character.
pub const CPS: f32 = 1440.0;
/// The cursor's half period.
const BLINK: f32 = 0.53;
/// Frame intervals: while the call runs, while the lights follow the
/// music.
const FRAME: Duration = Duration::from_millis(40);
const LIGHTS: Duration = Duration::from_millis(80);
/// The SD light stays on this long after a key.
const SENT: Duration = Duration::from_millis(150);
/// At most this many image bytes in one request.
const MAX_IMAGE_BYTES: usize = 200_000;

/// The board's rows and columns for what is drawn at run time.
mod at {
    /// The system box's values.
    pub const VALUE_COL: usize = 52;
    pub const NODE: usize = 9;
    /// The message of the day: its first line, how many, how wide.
    pub const MOTD_COL: usize = 38;
    pub const MOTD_ROW: usize = 17;
    pub const MOTD_LINES: usize = 4;
    pub const MOTD_WIDTH: usize = 40;
    /// The bottom line of its box, for the attribution.
    pub const MOTD_BOTTOM: usize = 21;
    pub const MOTD_RIGHT: usize = 77;
    /// The longest attribution on that line.
    pub const BYLINE: usize = 34;
    pub const TUNE: usize = 22;
    pub const LOGIN: usize = 24;
    pub const PASSWORD: usize = 25;
    pub const STATUS: usize = 26;
    pub const STATUS_ROWS: usize = 3;
    pub const PROMPT_COL: usize = 37;
    pub const BAR: usize = 29;
}

// ---- the text screen ---------------------------------------------------------------

/// One character cell: a CP437 code and two ANSI colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: u8,
    pub fg: u8,
    pub bg: u8,
}

pub const BLANK: Cell = Cell {
    ch: b' ',
    fg: ansi::GREY,
    bg: ansi::BLACK,
};

/// A text screen's cells, row by row (the board's 80x30 by default; the
/// `hacker` layout's console is wider).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Cell>,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new(COLS, ROWS)
    }
}

impl Grid {
    /// A blank screen of `cols` by `rows` cells.
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![BLANK; cols * rows],
        }
    }

    pub fn get(&self, col: usize, row: usize) -> Cell {
        if col < self.cols && row < self.rows {
            self.cells[row * self.cols + col]
        } else {
            BLANK
        }
    }

    pub fn set(&mut self, col: usize, row: usize, cell: Cell) {
        if col < self.cols && row < self.rows {
            self.cells[row * self.cols + col] = cell;
        }
    }

    /// Text at (col, row) in `fg` on black, cut at the right edge. The text
    /// is shown in capitals; letters the font lacks become their plain
    /// letter ([`pixfont::plain`]) and a few symbols their CP437 code.
    /// Returns the column after it.
    pub fn put(&mut self, col: usize, row: usize, text: &str, fg: u8) -> usize {
        let mut c = col;
        for code in codes(text) {
            self.set(
                c,
                row,
                Cell {
                    ch: code,
                    fg,
                    bg: ansi::BLACK,
                },
            );
            c += 1;
        }
        c
    }

    /// The cells as text (CP437 codes as `char`s below 0x80, `?` else), for
    /// tests.
    #[cfg(test)]
    pub fn row_text(&self, row: usize) -> String {
        (0..self.cols)
            .map(|c| {
                let ch = self.get(c, row).ch;
                if (0x20..0x7f).contains(&ch) {
                    ch as char
                } else {
                    '?'
                }
            })
            .collect()
    }
}

/// CP437 codes for `text`, in capitals.
pub fn codes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        // The common case without allocating: printable ASCII.
        if (' '..='~').contains(&c) {
            out.push(c.to_ascii_uppercase() as u8);
            continue;
        }
        let code = match c {
            '♪' => Some(0x0d),
            '♫' => Some(0x0e),
            '·' => Some(0xfa),
            '■' => Some(0xfe),
            '░' => Some(0xb0),
            '▒' => Some(0xb1),
            '▓' => Some(0xb2),
            '█' => Some(0xdb),
            '│' => Some(0xb3),
            '┤' => Some(0xb4),
            '├' => Some(0xc3),
            '─' => Some(0xc4),
            _ => None,
        };
        match code {
            Some(code) => out.push(code),
            None => out.extend(
                pixfont::plain(&c.to_string())
                    .to_uppercase()
                    .bytes()
                    .filter(|b| (0x20..0x7f).contains(b)),
            ),
        }
    }
    out
}

/// Read an ANSI file onto a blank screen: CP437 text, CR and LF, SGR
/// colours (bold for a bright foreground, blink for a bright background as
/// with iCE colours), up to the end-of-file mark before a SAUCE record.
/// Other escape sequences are skipped; a line wraps after the 80th column.
pub fn parse(data: &[u8]) -> Grid {
    let mut grid = Grid::default();
    let (mut col, mut row) = (0usize, 0usize);
    let (mut fg, mut bg, mut bold, mut blink) = (7u8, 0u8, false, false);
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            0x1a => break,
            b'\r' => col = 0,
            b'\n' => row += 1,
            0x1b if data.get(i) == Some(&b'[') => {
                i += 1;
                let start = i;
                while i < data.len() && !(0x40..=0x7e).contains(&data[i]) {
                    i += 1;
                }
                let params = &data[start..i.min(data.len())];
                let end = data.get(i).copied();
                i += 1;
                if end != Some(b'm') {
                    continue;
                }
                let text = std::str::from_utf8(params).unwrap_or("");
                for p in text.split(';') {
                    match p.parse::<u8>().unwrap_or(0) {
                        0 => (fg, bg, bold, blink) = (7, 0, false, false),
                        1 => bold = true,
                        5 => blink = true,
                        22 => bold = false,
                        25 => blink = false,
                        n @ 30..=37 => fg = n - 30,
                        39 => fg = 7,
                        n @ 40..=47 => bg = n - 40,
                        49 => bg = 0,
                        _ => {}
                    }
                }
            }
            _ => {
                if col >= COLS {
                    col = 0;
                    row += 1;
                }
                grid.set(
                    col,
                    row,
                    Cell {
                        ch: b,
                        fg: fg + if bold { 8 } else { 0 },
                        bg: bg + if blink { 8 } else { 0 },
                    },
                );
                col += 1;
            }
        }
    }
    grid
}

/// The board as the file draws it.
fn board() -> &'static Grid {
    static B: OnceLock<Grid> = OnceLock::new();
    B.get_or_init(|| parse(SCREEN))
}

// ---- the characters ----------------------------------------------------------------

/// Rows of a CP437 character, 8 pixels wide (bit 7 left), 16 tall.
pub type Glyph = [u8; 16];

pub fn glyphs() -> &'static [Glyph; 256] {
    static G: OnceLock<[Glyph; 256]> = OnceLock::new();
    G.get_or_init(|| {
        let mut out = [[0u8; 16]; 256];
        for (code, g) in out.iter_mut().enumerate() {
            *g = make_glyph(code as u8);
        }
        out
    })
}

/// A rectangle of a box character: x0..x1, y0..y1.
type Bar = (u8, u8, u8, u8);

fn mirror_x(bars: &[Bar]) -> Vec<Bar> {
    bars.iter()
        .map(|&(x0, x1, y0, y1)| (8 - x1, 8 - x0, y0, y1))
        .collect()
}

fn mirror_y(bars: &[Bar]) -> Vec<Bar> {
    bars.iter()
        .map(|&(x0, x1, y0, y1)| (x0, x1, 16 - y1, 16 - y0))
        .collect()
}

/// The box drawing characters as bars: single lines two pixels thick
/// through the middle, double lines as two of them.
fn box_bars(code: u8) -> Option<Vec<Bar>> {
    const H1: Bar = (0, 8, 7, 9);
    const V1: Bar = (3, 5, 0, 16);
    const HD: [Bar; 2] = [(0, 8, 5, 7), (0, 8, 9, 11)];
    const VD: [Bar; 2] = [(1, 3, 0, 16), (5, 7, 0, 16)];
    let single_corner = [(3, 8, 7, 9), (3, 5, 7, 16)];
    let double_corner = [(1, 3, 5, 16), (1, 8, 5, 7), (5, 7, 9, 16), (5, 8, 9, 11)];
    let left_double = [V1, (0, 3, 5, 7), (0, 3, 9, 11)];
    let right_single = [VD[0], VD[1], (5, 8, 7, 9)];
    let down_single = [HD[0], HD[1], (3, 5, 11, 16)];
    Some(match code {
        0xc4 => vec![H1],
        0xb3 => vec![V1],
        0xda => single_corner.to_vec(),
        0xbf => mirror_x(&single_corner),
        0xc0 => mirror_y(&single_corner),
        0xd9 => mirror_x(&mirror_y(&single_corner)),
        0xc3 => vec![V1, (3, 8, 7, 9)],
        0xb4 => vec![V1, (0, 5, 7, 9)],
        0xc2 => vec![H1, (3, 5, 7, 16)],
        0xc1 => vec![H1, (3, 5, 0, 9)],
        0xc5 => vec![H1, V1],
        0xcd => HD.to_vec(),
        0xba => VD.to_vec(),
        0xc9 => double_corner.to_vec(),
        0xbb => mirror_x(&double_corner),
        0xc8 => mirror_y(&double_corner),
        0xbc => mirror_x(&mirror_y(&double_corner)),
        0xb5 => left_double.to_vec(),
        0xc6 => mirror_x(&left_double),
        0xc7 => right_single.to_vec(),
        0xb6 => mirror_x(&right_single),
        0xd1 => down_single.to_vec(),
        0xcf => mirror_y(&down_single),
        _ => return None,
    })
}

fn make_glyph(code: u8) -> Glyph {
    let mut g = [0u8; 16];
    let rows = |g: &mut Glyph, y0: usize, y1: usize, bits: u8| {
        for row in g.iter_mut().take(y1).skip(y0) {
            *row |= bits;
        }
    };
    match code {
        0x20..=0x7e => {
            let small = pixfont::glyph(code as char);
            for (y, row) in g.iter_mut().enumerate() {
                *row = small[y / 2];
            }
        }
        // Light, medium and dark shade: a quarter, half and three quarters
        // of the pixels, in VGA's patterns.
        0xb0 => {
            for (y, row) in g.iter_mut().enumerate() {
                *row = if y % 2 == 0 { 0x88 } else { 0x22 };
            }
        }
        0xb1 => {
            for (y, row) in g.iter_mut().enumerate() {
                *row = if y % 2 == 0 { 0xaa } else { 0x55 };
            }
        }
        0xb2 => {
            for (y, row) in g.iter_mut().enumerate() {
                *row = if y % 2 == 0 { 0x77 } else { 0xdd };
            }
        }
        0xdb => rows(&mut g, 0, 16, 0xff),
        0xdc => rows(&mut g, 8, 16, 0xff),
        0xdf => rows(&mut g, 0, 8, 0xff),
        0xdd => rows(&mut g, 0, 16, 0xf0),
        0xde => rows(&mut g, 0, 16, 0x0f),
        0xfe => rows(&mut g, 5, 11, 0x7e),
        0xfa => rows(&mut g, 7, 9, 0x18),
        0xf9 => rows(&mut g, 6, 10, 0x3c),
        // The notes.
        0x0d => {
            g = [
                0, 0, 0x0c, 0x0e, 0x0b, 0x09, 0x08, 0x08, 0x08, 0x38, 0x78, 0x70, 0, 0, 0, 0,
            ]
        }
        0x0e => {
            g = [
                0, 0, 0x1f, 0x1f, 0x11, 0x11, 0x11, 0x11, 0x11, 0x77, 0xff, 0xee, 0, 0, 0, 0,
            ]
        }
        _ => match box_bars(code) {
            Some(bars) => {
                for (x0, x1, y0, y1) in bars {
                    let bits = (x0..x1).fold(0u8, |b, x| b | (0x80 >> x));
                    rows(&mut g, usize::from(y0), usize::from(y1), bits);
                }
            }
            // Anything else: a small block, never a missing glyph.
            None => rows(&mut g, 4, 12, 0x3c),
        },
    }
    g
}

/// Draw one cell into the canvas, its colours mapped by [`ANSI`].
fn draw_cell(canvas: &mut Canvas, col: usize, row: usize, cell: Cell) {
    let glyph = &glyphs()[usize::from(cell.ch)];
    let (fg, bg) = (
        ANSI[usize::from(cell.fg & 15)],
        ANSI[usize::from(cell.bg & 15)],
    );
    let x0 = col * CW as usize;
    for (dy, bits) in glyph.iter().enumerate() {
        let y = row as i32 * CH + dy as i32;
        let line = &mut canvas.row_mut(y)[x0..x0 + CW as usize];
        for (dx, px) in line.iter_mut().enumerate() {
            *px = if bits & (0x80 >> dx) != 0 { fg } else { bg };
        }
    }
}

/// Draw the cells of `grid` that differ from `prev` (all without it) and
/// return the canvas rows drawn.
pub fn render(canvas: &mut Canvas, grid: &Grid, prev: Option<&Grid>) -> Option<(i32, i32)> {
    let mut rows: Option<(usize, usize)> = None;
    let prev = prev.filter(|p| p.cols == grid.cols && p.rows == grid.rows);
    for row in 0..grid.rows.min((canvas.h / CH).max(0) as usize) {
        for col in 0..grid.cols.min((canvas.w / CW).max(0) as usize) {
            let cell = grid.get(col, row);
            if prev.is_some_and(|p| p.get(col, row) == cell) {
                continue;
            }
            draw_cell(canvas, col, row, cell);
            rows = Some(rows.map_or((row, row + 1), |(a, b)| (a.min(row), b.max(row + 1))));
        }
    }
    rows.map(|(a, b)| (a as i32 * CH, b as i32 * CH))
}

/// Draw the cells of `grid` in `cols` and `rows` again, whatever the
/// canvas shows there now.
pub fn render_area(
    canvas: &mut Canvas,
    grid: &Grid,
    cols: std::ops::Range<usize>,
    rows: std::ops::Range<usize>,
) {
    let max_rows = (canvas.h / CH).max(0) as usize;
    let max_cols = (canvas.w / CW).max(0) as usize;
    for row in rows.filter(|&r| r < grid.rows.min(max_rows)) {
        for col in cols.clone().filter(|&c| c < grid.cols.min(max_cols)) {
            draw_cell(canvas, col, row, grid.get(col, row));
        }
    }
}

// ---- the call ----------------------------------------------------------------------

/// Which call: the first one resets the modem, a later one follows a
/// dropped line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cycle {
    First,
    Redial,
}

/// A line the modem or the terminal shows: when (seconds), what, and
/// whether it is typed out (a command) or arrives at once (a result).
type Line = (f32, &'static str, bool);

const INIT: [Line; 4] = [
    (0.15, "ATZ", true),
    (0.75, "OK", false),
    (1.05, "AT&F1 M1 L1", true),
    (1.85, "OK", false),
];
/// The first call dials no earlier than this.
pub const INIT_END: f32 = 2.2;
const REDIAL: [Line; 1] = [(0.0, "NO CARRIER", false)];
pub const REDIAL_END: f32 = 1.6;
/// The dialing, from the moment `ATDT` is typed (and the handshake sound
/// starts).
const DIAL: [Line; 6] = [
    (0.0, "ATDT 555-64789", true),
    (modem::RINGING, "RINGING", false),
    (modem::CONNECT - 0.45, "CARRIER 14400", false),
    (modem::CONNECT - 0.3, "PROTOCOL: LAP-M", false),
    (modem::CONNECT - 0.15, "COMPRESSION: V.42BIS", false),
    (modem::CONNECT, "CONNECT 14400/ARQ/V42BIS", false),
];
/// Seconds a typed character takes: the reset slowly by hand, the number
/// from the dialing directory.
const TYPE_BY_HAND: f32 = 0.07;
const TYPE_DIAL: f32 = 0.02;
/// The board starts to arrive this long after the dial.
pub const STREAM_AT: f32 = modem::CONNECT + 0.7;

/// Where a call is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// The terminal's text: `t` seconds into the call, dialled at `dial`.
    Modem {
        cycle: Cycle,
        t: f32,
        dial: Option<f32>,
    },
    /// So many cells of the board have arrived.
    Stream(usize),
    Online,
}

impl Phase {
    /// The phase `t` seconds into a call dialled `dial` seconds in.
    pub fn at(cycle: Cycle, t: f32, dial: Option<f32>) -> Self {
        let Some(d) = dial.filter(|&d| t >= d + STREAM_AT) else {
            return Self::Modem { cycle, t, dial };
        };
        let n = ((t - d - STREAM_AT) * CPS) as usize;
        if n >= COLS * ROWS {
            Self::Online
        } else {
            Self::Stream(n)
        }
    }

    pub fn online(self) -> bool {
        self == Self::Online
    }
}

/// The terminal's lines at this moment, and whether a command is being
/// typed or a result just arrived.
fn terminal(cycle: Cycle, t: f32, dial: Option<f32>) -> (Vec<String>, bool, bool) {
    let mut lines = Vec::new();
    let (mut typing, mut arrived) = (false, false);
    let pre: &[Line] = match cycle {
        Cycle::First => &INIT,
        Cycle::Redial => &REDIAL,
    };
    let mut show = |at: f32, text: &str, typed: bool, pace: f32| {
        if t < at {
            return;
        }
        if typed {
            let n = (((t - at) / pace) as usize + 1).min(text.len());
            typing |= n < text.len();
            lines.push(text[..n].to_string());
        } else {
            arrived |= t - at < 0.2;
            lines.push(text.to_string());
        }
    };
    for &(at, text, typed) in pre {
        show(at, text, typed, TYPE_BY_HAND);
    }
    if let Some(d) = dial {
        for &(at, text, typed) in &DIAL {
            show(d + at, text, typed, TYPE_DIAL);
        }
    }
    (lines, typing, arrived)
}

// ---- what is drawn -----------------------------------------------------------------

/// The message of the day: up to four lines and who said it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Motd {
    pub lines: Vec<String>,
    pub who: String,
}

impl Motd {
    /// A quote in quotation marks over at most four lines, or `None` when
    /// it does not fit.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn quote(words: &str, who: &str) -> Option<Self> {
        Self::quote_in(words, who, at::MOTD_WIDTH, at::MOTD_LINES)
    }

    /// [`Motd::quote`] for a box `width` columns wide and `max` lines tall.
    pub fn quote_in(words: &str, who: &str, width: usize, max: usize) -> Option<Self> {
        let words = words.trim().trim_matches(['"', '\u{201c}', '\u{201d}']);
        let text = format!(
            "\"{}\"",
            words.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        let lines = wrap(&text, width);
        (lines.len() <= max && !words.is_empty()).then(|| Self {
            lines,
            who: who
                .trim()
                .trim_start_matches(['-', '\u{2014}', '\u{2013}', ' '])
                .to_string(),
        })
    }

    /// A quote cut to four lines.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cut(words: &str, who: &str) -> Self {
        Self::cut_in(words, who, at::MOTD_WIDTH, at::MOTD_LINES).unwrap_or_else(Self::welcome)
    }

    /// [`Motd::cut`] for a box `width` columns wide and `max` lines tall.
    pub fn cut_in(words: &str, who: &str, width: usize, max: usize) -> Option<Self> {
        let long = format!("{} ", words.trim());
        let mut text = String::new();
        for word in long.split_whitespace() {
            let next = format!("{text} {word}");
            if wrap(&format!("\"{}...\"", next.trim()), width).len() > max {
                break;
            }
            text = next;
        }
        Self::quote_in(&format!("{}...", text.trim()), who, width, max)
    }

    /// Without quotes.
    pub fn welcome() -> Self {
        Self {
            lines: vec![
                "WELCOME TO NIRUX BBS.".into(),
                "THE SYSOP IS AWAY FROM THE KEYBOARD.".into(),
                "TYPE THE PASSWORD TO COME BACK.".into(),
            ],
            who: "SYSOP".into(),
        }
    }
}

/// Words wrapped to `width` columns (as the font shows them); a word longer
/// than a line is broken.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word: String = String::from_utf8_lossy(&codes(word)).into_owned();
        let mut word = word.as_str();
        loop {
            let len = line.chars().count();
            let need = word.chars().count() + usize::from(len > 0);
            if len + need <= width {
                if len > 0 {
                    line.push(' ');
                }
                line.push_str(word);
                break;
            }
            if len > 0 {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            let cut: String = word.chars().take(width).collect();
            word = &word[cut.len()..];
            lines.push(cut);
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Everything a frame shows besides the phase.
#[derive(Clone, Debug, Default)]
pub struct Facts<'a> {
    /// "THU 26 NOV 2026", "14:32", "00:12".
    pub date: &'a str,
    pub time: &'a str,
    pub online: &'a str,
    pub motd: Option<&'a Motd>,
    /// "Artist - Title".
    pub tune: Option<&'a str>,
    /// The entry as `prompt_echo` shows it.
    pub entry: &'a str,
    pub checking: bool,
    pub wrong: bool,
    /// The lines under the prompt, with ANSI colours.
    pub status: &'a [(String, u8)],
    pub keyboard: Option<&'a str>,
    pub caps: bool,
    /// The cursor is lit.
    pub cursor: bool,
    /// The RD light flashes on the beat; SD lights after a key.
    pub beat: bool,
    pub sent: bool,
}

/// The screen for `phase`.
pub fn compose(phase: Phase, f: &Facts) -> Grid {
    use ansi::*;
    let mut g = Grid::default();
    let (line, rd, sd, oh, cd) = match phase {
        Phase::Modem { cycle, t, dial } => {
            let (lines, typing, arrived) = terminal(cycle, t, dial);
            for (row, text) in lines.iter().enumerate() {
                g.put(0, row, text, GREY);
            }
            if f.cursor {
                let row = lines.len().saturating_sub(1);
                let col = lines.last().map_or(0, String::len);
                let (col, row) = if typing || lines.is_empty() {
                    (col, row)
                } else {
                    (0, lines.len())
                };
                g.put(col, row, "_", GREY);
            }
            let oh = dial.is_some_and(|d| t >= d);
            let cd = dial.is_some_and(|d| t >= d + modem::CONNECT);
            let line = if cd {
                " 14400 8N1 "
            } else if oh {
                " DIALING   "
            } else {
                " OFFLINE   "
            };
            (line, arrived, f.sent || typing, oh, cd)
        }
        Phase::Stream(n) => {
            let full = online(f);
            g.cells[..n].copy_from_slice(&full.cells[..n]);
            if n < COLS * ROWS {
                g.set(n % COLS, n / COLS, Cell { ch: b'_', ..BLANK });
            }
            (" 14400 8N1 ", true, f.sent, true, true)
        }
        Phase::Online => {
            g = online(f);
            (" 14400 8N1 ", f.beat, f.sent, true, true)
        }
    };
    status_bar(&mut g, f, line, [cd, false, cd, oh, rd, sd, true, true]);
    g
}

/// The terminal's status bar on the last row: its name, the keyboard, the
/// line, the time online, Caps Lock and the modem's lights (HS AA CD OH RD
/// SD TR MR).
fn status_bar(g: &mut Grid, f: &Facts, line: &str, lights: [bool; 8]) {
    use ansi::*;
    let row = at::BAR;
    let bar = |ch: u8, fg: u8| Cell { ch, fg, bg: BLUE };
    for col in 0..COLS {
        g.set(col, row, bar(b' ', GREY));
    }
    let mut col = 0;
    let mut text = |g: &mut Grid, s: &str, fg: u8| {
        for code in codes(s) {
            g.set(col, row, bar(code, fg));
            col += 1;
        }
    };
    let keyboard = f.keyboard.map_or("ANSI".to_string(), |k| {
        format!("KBD {}", k.chars().take(4).collect::<String>())
    });
    let caps = if f.caps { WHITE } else { DARK_GREY };
    text(g, " NIRUTERM ", WHITE);
    text(g, "│", CYAN);
    text(g, &format!(" {keyboard:<8} "), GREY);
    text(g, "│", CYAN);
    text(g, line, GREY);
    text(g, "│", CYAN);
    text(g, &format!(" ONLINE {:>5} ", f.online), GREY);
    text(g, "│", CYAN);
    text(g, " CAPS ", caps);
    text(g, "│", CYAN);
    text(g, " ", GREY);
    for (name, lit) in ["HS", "AA", "CD", "OH", "RD", "SD", "TR", "MR"]
        .iter()
        .zip(lights)
    {
        text(g, name, if lit { YELLOW } else { DARK_GREY });
        text(g, " ", GREY);
    }
}

/// The board with everything that changes drawn in.
fn online(f: &Facts) -> Grid {
    use ansi::*;
    let mut g = board().clone();
    let values = [
        ("1 OF 4", WHITE),
        ("14400/ARQ/V42BIS", WHITE),
        (f.date, WHITE),
        (f.time, WHITE),
        (f.online, WHITE),
        (
            if f.wrong {
                "ACCESS DENIED"
            } else {
                "LOCKED, SYSOP AWAY"
            },
            if f.wrong { RED } else { YELLOW },
        ),
    ];
    for (i, (text, fg)) in values.iter().enumerate() {
        let text: String = text.chars().take(26).collect();
        g.put(at::VALUE_COL, at::NODE + i, &text, *fg);
    }
    let welcome = Motd::welcome();
    let motd = f.motd.unwrap_or(&welcome);
    for (i, text) in motd.lines.iter().take(at::MOTD_LINES).enumerate() {
        g.put(at::MOTD_COL, at::MOTD_ROW + i, text, GREY);
    }
    if !motd.who.is_empty() {
        let who = format!("- {}", byline(&motd.who, at::BYLINE));
        let width = codes(&who).len() + 4;
        let col = at::MOTD_RIGHT + 1 - width;
        let next = g.put(col, at::MOTD_BOTTOM, "┤ ", BLUE);
        let next = g.put(next, at::MOTD_BOTTOM, &who, LIGHT_CYAN);
        g.put(next, at::MOTD_BOTTOM, " ├", BLUE);
    }
    let width = COLS - at::PROMPT_COL;
    if let Some(tune) = f.tune.map(str::trim).filter(|t| !t.is_empty()) {
        let next = g.put(at::PROMPT_COL, at::TUNE, "♫ NOW PLAYING  ", CYAN);
        let room = COLS - next;
        let tune: Vec<u8> = codes(tune).into_iter().take(room).collect();
        g.put(next, at::TUNE, &String::from_utf8_lossy(&tune), WHITE);
    }
    let next = g.put(at::PROMPT_COL, at::LOGIN, "LOGIN: ", CYAN);
    g.put(next, at::LOGIN, "SYSOP", WHITE);
    let label = if f.wrong { RED } else { CYAN };
    let next = g.put(at::PROMPT_COL, at::PASSWORD, "PASSWORD: ", label);
    let room = COLS - next - 1;
    let entry: String = f.entry.chars().take(room).collect();
    let next = g.put(next, at::PASSWORD, &entry, YELLOW);
    if f.cursor && !f.checking {
        g.put(next, at::PASSWORD, "_", GREY);
    }
    let mut row = at::STATUS;
    let mut lines: Vec<(String, u8)> = Vec::new();
    if f.checking {
        lines.push(("CHECKING PASSWORD...".into(), GREY));
    }
    lines.extend(f.status.iter().cloned());
    for (text, fg) in lines {
        for part in wrap(&text, width) {
            if row >= at::STATUS + at::STATUS_ROWS {
                break;
            }
            g.put(at::PROMPT_COL, row, &part, fg);
            row += 1;
        }
    }
    g
}

/// Who said it, in at most `max` columns: a long name loses its
/// parenthesis (the translator), then its end.
pub fn byline(who: &str, max: usize) -> String {
    let fits = |s: &str| codes(s).len() <= max;
    let who = who.trim();
    // The quotes reader may already have cut a long source mid-parenthesis
    // ("Havamal 78 (tr. Henry Adams"): drop the open part.
    let who = match who.rfind(" (") {
        Some(at) if !who[at..].contains(')') => who[..at].trim_end().trim_end_matches(','),
        _ => who,
    };
    if fits(who) {
        return who.to_string();
    }
    let short = match who.find(" (") {
        Some(at) => who[..at].trim_end().trim_end_matches(','),
        None => who,
    };
    if fits(short) {
        return short.to_string();
    }
    let cut: String = String::from_utf8_lossy(&codes(short)[..max.saturating_sub(3)]).into_owned();
    format!("{}...", cut.trim_end())
}

/// A message of the day from the quotes for a box `width` columns wide and
/// `max` lines tall: the first of up to eight quotes that fits, otherwise
/// the first one cut to fit. `None` without quotes.
pub(crate) fn pick_quote(
    visual: &mut crate::visual::Visual,
    width: usize,
    max: usize,
) -> Option<Motd> {
    let mut first = None;
    for _ in 0..8 {
        let Some(q) = visual.next_quote() else {
            break;
        };
        let words = q.lines.join(" ");
        let who = q.attribution.join(", ");
        if let Some(m) = Motd::quote_in(&words, &who, width, max) {
            return Some(m);
        }
        first.get_or_insert((words, who));
    }
    let (words, who) = first?;
    Motd::cut_in(&words, &who, width, max)
}

/// "00:12" for so many seconds.
pub fn hours_minutes(seconds: u64) -> String {
    format!("{:02}:{:02}", (seconds / 3600).min(99), seconds / 60 % 60)
}

// ---- the layout --------------------------------------------------------------------

/// Lay out one monitor: the text screen as large as it fits, centred.
pub fn arrange(area: Area) -> Screen {
    chunky::arrange(area, 0, W, H, |_| 0)
}

/// The layout's state between frames.
#[derive(Default)]
pub struct State {
    screens: Vec<Screen>,
    canvas: Option<Canvas>,
    /// What the canvas shows.
    shown: Option<Grid>,
    /// The call: when it began and which.
    call: Option<(Instant, Cycle)>,
    /// Seconds into the call when it dialled.
    dial: Option<f32>,
    /// Shown at once: a key was pressed, or reduced motion.
    skipped: bool,
    last_key: Option<Instant>,
    motd: Option<Motd>,
    /// The message of the day came from the quotes.
    quoted: bool,
    flash: f32,
    bass: f32,
    last: Option<Instant>,
    next: Option<Instant>,
}

impl State {
    /// A key was pressed: the board shows at once, and SD lights.
    pub fn key(&mut self, now: Instant) {
        self.last_key = Some(now);
        self.skipped = true;
    }
}

impl Lock {
    pub(crate) fn bbs_layout(&mut self, area: Area) -> bool {
        let screens: Vec<Screen> = if self.outputs.is_empty() {
            vec![area]
        } else {
            self.outputs.clone()
        }
        .into_iter()
        .map(arrange)
        .collect();
        let changed = screens != self.bbs.screens;
        self.bbs.screens = screens;
        changed
    }

    /// The message of the day: a quote that fits, otherwise the welcome
    /// until the quotes are read.
    fn bbs_motd(&mut self) {
        if self.bbs.quoted {
            return;
        }
        let picked = self
            .visual
            .as_mut()
            .and_then(|v| pick_quote(v, at::MOTD_WIDTH, at::MOTD_LINES));
        match picked {
            Some(m) => {
                self.bbs.motd = Some(m);
                self.bbs.quoted = true;
            }
            None => {
                self.bbs.motd.get_or_insert_with(Motd::welcome);
            }
        }
    }

    /// Draw what changed into the backing pixmap (everything with `full`)
    /// and return the canvas rows drawn.
    pub(crate) fn bbs_paint(&mut self, full: bool) -> Result<Option<(i32, i32)>> {
        let now = Instant::now();
        let reduced = self.cfg.reduced_motion;
        if self.bbs.call.is_none() {
            self.bbs.call = Some((now, Cycle::First));
        }
        if reduced || !self.input.is_empty() || self.state != crate::State::Prompt {
            self.bbs.skipped = true;
        }
        let (start, cycle) = self.bbs.call.expect("a call");
        let mut t = now.saturating_duration_since(start).as_secs_f32();
        let deciding = self
            .music
            .as_ref()
            .is_some_and(crate::music::Music::deciding);
        if self.bbs.dial.is_none() && cycle == Cycle::First && t >= INIT_END && !deciding {
            self.bbs.dial = Some(t);
        }
        let mut phase = if self.bbs.skipped {
            Phase::Online
        } else {
            Phase::at(cycle, t, self.bbs.dial)
        };
        let quiet = self
            .bbs
            .last_key
            .map_or(true, |k| now.saturating_duration_since(k) >= QUIET);
        if phase.online()
            && t >= RELOAD
            && !reduced
            && quiet
            && self.input.is_empty()
            && self.state == crate::State::Prompt
            && self.failed == 0
        {
            // The line drops; call again, with a new message of the day.
            self.bbs.call = Some((now, Cycle::Redial));
            self.bbs.dial = Some(REDIAL_END);
            self.bbs.skipped = false;
            self.bbs.quoted = false;
            t = 0.0;
            phase = Phase::at(Cycle::Redial, t, self.bbs.dial);
        }
        self.bbs_motd();
        let dt = self.bbs.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.bbs.last = Some(now);
        let spectrum = if reduced {
            None
        } else {
            self.visual
                .as_mut()
                .and_then(crate::visual::Visual::spectrum)
                .map(|values| {
                    let low = &values[..values.len().min(6)];
                    low.iter().sum::<f32>() / low.len().max(1) as f32
                })
        };
        let following = spectrum.is_some();
        (self.bbs.flash, self.bbs.bass) =
            crate::demo::beat(self.bbs.flash, spectrum.unwrap_or(0.0), self.bbs.bass, dt);
        let local = chrono::Local::now();
        let date = local.format("%a %d %b %Y").to_string().to_uppercase();
        let time = local.format("%H:%M").to_string();
        let away = hours_minutes(self.started.elapsed().as_secs());
        let chars = self.input.chars().count().min(crate::ENTRY_MAX_STARS);
        let entry = crate::echo(self.cfg.prompt_echo, chars, self.pulse);
        let status = self.bbs_status();
        let cursor = reduced || ((t / BLINK) as u64) % 2 == 0;
        let sent = self
            .bbs
            .last_key
            .is_some_and(|k| now.saturating_duration_since(k) < SENT);
        let keyboard = self.keyboard.indicator().map(str::to_uppercase);
        let grid = compose(
            phase,
            &Facts {
                date: &date,
                time: &time,
                online: &away,
                motd: self.bbs.motd.as_ref(),
                tune: self.music_tune.as_deref(),
                entry: &entry,
                checking: self.state == crate::State::Checking,
                wrong: self.state == crate::State::Wrong,
                status: &status,
                keyboard: keyboard.as_deref(),
                caps: self.caps,
                cursor,
                beat: self.bbs.flash > 0.5,
                sent,
            },
        );
        let st = &mut self.bbs;
        let canvas = st.canvas.get_or_insert_with(|| Canvas::new(W, H));
        let rows = render(canvas, &grid, if full { None } else { st.shown.as_ref() });
        st.shown = Some(grid);
        let pal = &self.cfg.palette;
        let accent = crate::ascii::banner_colour(pal.focus, pal.foreground, pal.background);
        let palette = Palette::new(accent, pal.foreground, pal.muted, self.cfg.warning);
        let s = &self.surface;
        let big_endian = s.c.setup().image_byte_order == ImageOrder::MSB_FIRST;
        if let Some((y0, y1)) = rows {
            for screen in &st.screens {
                let Some(place) = screen.place else {
                    continue;
                };
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
        }
        // When to look again: every frame while the call runs or the
        // lights follow the music, otherwise at the cursor's next blink,
        // the SD light going out, or the next call.
        let mut wait: Option<Duration> = None;
        let mut sooner = |d: Duration| wait = Some(wait.map_or(d, |w| w.min(d)));
        if !phase.online() {
            sooner(FRAME);
        }
        if following || st.flash > 0.01 {
            sooner(LIGHTS);
        }
        if !reduced {
            let into = t % BLINK;
            sooner(Duration::from_secs_f32((BLINK - into).max(0.01)));
            if t < RELOAD {
                sooner(Duration::from_secs_f32(RELOAD - t + 0.01));
            }
        }
        if let Some(k) = st.last_key {
            let lit = now.saturating_duration_since(k);
            if lit < SENT {
                sooner(SENT - lit);
            }
        }
        st.next = wait.map(|w| now + w);
        Ok(rows)
    }

    /// The lines under the prompt: why the last attempt failed, the failed
    /// attempts and Caps Lock, in BBS words.
    fn bbs_status(&self) -> Vec<(String, u8)> {
        use ansi::*;
        let mut lines = Vec::new();
        match (self.state, self.pam_message.as_ref()) {
            (crate::State::Wrong, Some(message)) => {
                lines.push((format!("ACCESS DENIED: {message}"), RED));
            }
            (crate::State::Wrong, None) => {
                let why = if self.message == "wrong password" {
                    "INVALID PASSWORD".to_string()
                } else {
                    self.message.clone()
                };
                lines.push((format!("ACCESS DENIED: {why}"), RED));
            }
            (crate::State::Prompt, Some(message)) => lines.push((message.clone(), GREY)),
            _ => {}
        }
        if self.failed > 0 {
            let n = self.failed;
            let s = if n == 1 { "" } else { "S" };
            lines.push((format!("{n} FAILED LOGIN ATTEMPT{s}"), GREY));
        }
        if self.caps {
            lines.push(("CAPS LOCK IS ON".into(), YELLOW));
        }
        lines
    }

    /// The whole screen, from `draw`.
    pub(crate) fn bbs_draw(&mut self) -> Result<()> {
        let area = self.prompt_area;
        let moved = self.bbs_layout(area);
        let full = moved || self.bbs.shown.is_none();
        if full {
            self.surface.clear(self.cfg.palette.background)?;
        }
        self.bbs_paint(full)?;
        self.surface.present()
    }

    pub(crate) fn bbs_tick(&mut self, display_on: bool) -> Result<()> {
        if !display_on || self.bbs.next.map_or(true, |due| Instant::now() < due) {
            return Ok(());
        }
        let Some((y0, y1)) = self.bbs_paint(false)? else {
            return Ok(());
        };
        for screen in &self.bbs.screens {
            let Some(place) = screen.place else {
                continue;
            };
            self.surface.present_rect(
                layout::clamp16(place.x),
                layout::clamp16(place.y + y0 * place.sy),
                (W * place.sx).clamp(0, i32::from(u16::MAX)) as u16,
                ((y1 - y0) * place.sy).clamp(0, i32::from(u16::MAX)) as u16,
            )?;
        }
        Ok(())
    }

    pub(crate) fn bbs_timeout(&self, display_on: bool) -> Option<Duration> {
        if !display_on || self.cfg.layout != crate::config::Layout::Bbs {
            return None;
        }
        self.bbs
            .next
            .map(|due| due.saturating_duration_since(Instant::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts<'static> {
        Facts {
            date: "THU 26 NOV 2026",
            time: "14:32",
            online: "00:12",
            cursor: true,
            ..Facts::default()
        }
    }

    #[test]
    fn the_board_is_a_real_ansi_file() {
        assert!(SCREEN.starts_with(b"\x1b["));
        // The SAUCE record: 128 bytes at the end, after the EOF mark.
        let sauce = &SCREEN[SCREEN.len() - 128..];
        assert_eq!(&sauce[..7], b"SAUCE00");
        assert_eq!(&sauce[7..16], b"NIRUX BBS");
        assert_eq!(SCREEN[SCREEN.len() - 129], 0x1a);
        assert_eq!(u16::from_le_bytes([sauce[96], sauce[97]]), COLS as u16);
        let b = board();
        // The frame of the tree and the boxes, where the code puts things.
        assert_eq!(b.get(0, 0).ch, 0xc9);
        assert_eq!(b.get(34, 28).ch, 0xbc);
        assert_eq!(b.get(36, 8).ch, 0xda);
        assert_eq!(b.get(79, 15).ch, 0xd9);
        assert!(b.row_text(at::NODE).contains("NODE ...."));
        assert!(b.row_text(at::NODE + 5).contains("STATUS"));
        let blank = |c: Cell| c.ch == b' ' && c.bg == ansi::BLACK;
        assert!(blank(b.get(at::VALUE_COL, at::NODE)));
        assert!(b.row_text(16).contains("MESSAGE OF THE DAY"));
        assert_eq!(b.get(at::MOTD_RIGHT + 2, at::MOTD_BOTTOM).ch, 0xd9);
        // Nothing is drawn where the prompt goes or on the status bar.
        for row in at::TUNE..ROWS {
            for col in at::PROMPT_COL..COLS {
                assert!(blank(b.get(col, row)), "{col},{row}");
            }
        }
        assert!((0..COLS).all(|c| blank(b.get(c, at::BAR))));
        // The picture uses blocks and shades and the theme's greens.
        let tree: Vec<Cell> = (1..28)
            .flat_map(|r| (1..34).map(move |c| (c, r)))
            .map(|(c, r)| b.get(c, r))
            .collect();
        for code in [0xdf, 0xb0, 0xb1] {
            assert!(tree.iter().any(|c| c.ch == code), "{code:x}");
        }
        assert!(tree.iter().filter(|c| c.fg == ansi::GREEN).count() > 30);
        assert!(tree.iter().filter(|c| c.fg == ansi::BROWN).count() > 10);
    }

    #[test]
    fn ansi_colours_and_wrapping_are_read() {
        let g = parse(b"\x1b[0;1;33;44mA\x1b[0;5;31;42mB\x1b[0mC\r\nD\x1a\x1b[31mE");
        assert_eq!(
            g.get(0, 0),
            Cell {
                ch: b'A',
                fg: 11,
                bg: 4
            }
        );
        assert_eq!(
            g.get(1, 0),
            Cell {
                ch: b'B',
                fg: 1,
                bg: 10
            }
        );
        assert_eq!(
            g.get(2, 0),
            Cell {
                ch: b'C',
                fg: 7,
                bg: 0
            }
        );
        assert_eq!(g.get(0, 1).ch, b'D');
        assert_eq!(g.get(1, 1), BLANK, "nothing after the EOF mark");
        // 80 characters and CR LF make one line, not two.
        let mut line = vec![b'x'; 80];
        line.extend_from_slice(b"\r\nY");
        let g = parse(&line);
        assert_eq!(g.get(0, 1).ch, b'Y');
        let mut line = vec![b'x'; 81];
        line.push(b'Z');
        assert_eq!(parse(&line).get(1, 1).ch, b'Z');
    }

    #[test]
    fn every_character_used_has_a_drawn_glyph() {
        let fallback = make_glyph(0x01);
        let mut used: Vec<u8> = board().cells.iter().map(|c| c.ch).collect();
        used.extend(codes("♫ ·│┤├─░▒▓█"));
        used.sort_unstable();
        used.dedup();
        for code in used {
            let g = glyphs()[usize::from(code)];
            if code != b' ' {
                assert_ne!(g, [0; 16], "{code:#x} is empty");
            }
            assert!(code == 0x01 || g != fallback, "{code:#x} has no glyph");
        }
        // Blocks fill exactly their part; box lines join at the cell edges.
        assert_eq!(glyphs()[0xdb], [0xff; 16]);
        assert_eq!(glyphs()[0xdf][7], 0xff);
        assert_eq!(glyphs()[0xdf][8], 0);
        let ones = |g: &Glyph| g.iter().map(|r| r.count_ones()).sum::<u32>();
        assert_eq!(ones(&glyphs()[0xb0]), 32);
        assert_eq!(ones(&glyphs()[0xb1]), 64);
        assert_eq!(ones(&glyphs()[0xb2]), 96);
        let h = glyphs()[0xcd];
        let v = glyphs()[0xba];
        let corner = glyphs()[0xc9];
        assert_eq!(h[5] & 1, 1, "═ reaches the right edge");
        assert_eq!(corner[15] & 0x60, 0x60, "╔ goes down on both lines");
        assert_eq!(corner[5] & 1, 1, "╔ goes right");
        assert_eq!(v[0], v[15]);
        // Text is the pixel font at double height.
        let a = pixfont::glyph('A');
        assert_eq!(glyphs()[usize::from(b'A')][2], a[1]);
        assert_eq!(glyphs()[usize::from(b'A')][3], a[1]);
        assert_eq!(codes("é·♫x"), vec![b'E', 0xfa, 0x0e, b'X']);
    }

    #[test]
    fn the_call_tells_its_story_in_order() {
        // The first call: the modem is reset, then held until the dial.
        let p = |t: f32, dial: Option<f32>| Phase::at(Cycle::First, t, dial);
        let text = |t: f32, dial: Option<f32>| {
            let g = compose(p(t, dial), &facts());
            (0..ROWS - 1)
                .map(|r| g.row_text(r).trim_end().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(text(0.0, None)[0], "_");
        assert_eq!(&text(2.0, None)[..4], ["ATZ", "OK", "AT&F1 M1 L1", "OK"]);
        assert_eq!(text(60.0, None)[4], "_", "held while the music decides");
        let d = Some(3.0);
        assert!(text(3.1, d)[4].starts_with("ATDT"));
        assert_eq!(text(3.0 + modem::RINGING + 0.1, d)[5], "RINGING");
        let connected = text(3.0 + modem::CONNECT + 0.1, d);
        assert_eq!(connected[9], "CONNECT 14400/ARQ/V42BIS");
        assert_eq!(connected[6], "CARRIER 14400");
        // Then the board arrives, a line at a time, and is online.
        match p(3.0 + STREAM_AT + 0.5, d) {
            Phase::Stream(n) => assert!((600..=800).contains(&n), "{n}"),
            other => panic!("{other:?}"),
        }
        let done = 3.0 + STREAM_AT + (COLS * ROWS) as f32 / CPS + 0.1;
        assert!(p(done, d).online());
        // A later call starts with the dropped line.
        let r = compose(Phase::at(Cycle::Redial, 0.5, Some(REDIAL_END)), &facts());
        assert_eq!(r.row_text(0).trim_end(), "NO CARRIER");
        let r = compose(
            Phase::at(Cycle::Redial, REDIAL_END + 0.5, Some(REDIAL_END)),
            &facts(),
        );
        assert!(r.row_text(1).starts_with("ATDT 555-64789"));
    }

    #[test]
    fn the_board_shows_the_facts_and_the_prompt() {
        let motd = Motd::quote(
            "The unwise man is awake all night, and ponders everything over",
            "Havamal",
        )
        .unwrap();
        let status = vec![("ACCESS DENIED: INVALID PASSWORD".to_string(), ansi::RED)];
        let f = Facts {
            motd: Some(&motd),
            tune: Some("NIRUX - Gimle"),
            entry: "****",
            wrong: true,
            status: &status,
            caps: true,
            ..facts()
        };
        let g = compose(Phase::Online, &f);
        assert!(g.row_text(at::NODE + 2).contains("THU 26 NOV 2026"));
        assert!(g.row_text(at::NODE + 3).contains("14:32"));
        assert!(g.row_text(at::NODE + 5).contains("ACCESS DENIED"));
        assert!(g.row_text(at::MOTD_ROW).contains("\"THE UNWISE MAN"));
        assert!(g.row_text(at::MOTD_BOTTOM).contains("- HAVAMAL"));
        assert!(g.row_text(at::TUNE).contains("NOW PLAYING  NIRUX - GIMLE"));
        assert!(g.row_text(at::LOGIN).contains("LOGIN: SYSOP"));
        assert!(g.row_text(at::PASSWORD).contains("PASSWORD: ****_"));
        assert_eq!(g.get(at::PROMPT_COL, at::PASSWORD).fg, ansi::RED);
        assert!(g
            .row_text(at::STATUS)
            .contains("ACCESS DENIED: INVALID PASSWORD"));
        let bar = g.row_text(at::BAR);
        assert!(bar.starts_with(" NIRUTERM "), "{bar}");
        assert!(bar.contains("14400 8N1") && bar.contains("ONLINE 00:12"));
        assert!(bar.contains("HS AA CD OH RD SD TR MR"));
        let caps = bar.find("CAPS").unwrap();
        assert_eq!(g.get(caps, at::BAR).fg, ansi::WHITE);
        // Nothing private: no user name, host or address on the board.
        let all: String = (0..ROWS).map(|r| g.row_text(r)).collect();
        let user = std::env::var("USER").unwrap_or_default().to_uppercase();
        let words: Vec<&str> = all.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        assert!(user.is_empty() || !words.contains(&user.as_str()), "{user}");
        // The entry never runs past the screen, and checking hides the
        // cursor.
        let long = "*".repeat(60);
        let g = compose(
            Phase::Online,
            &Facts {
                entry: &long,
                checking: true,
                ..facts()
            },
        );
        assert_eq!(g.get(COLS - 1, at::PASSWORD).ch, b' ');
        assert!(g.row_text(at::STATUS).contains("CHECKING"));
    }

    #[test]
    fn messages_of_the_day_fit_their_box() {
        let m = Motd::quote("short", "\u{2014} Someone").unwrap();
        assert_eq!(m.lines, vec!["\"SHORT\""]);
        assert_eq!(m.who, "Someone");
        let long = "word ".repeat(60);
        assert!(Motd::quote(&long, "x").is_none());
        let cut = Motd::cut(&long, "x");
        assert!(cut.lines.len() <= at::MOTD_LINES);
        assert!(cut.lines.last().unwrap().ends_with("...\""));
        for line in &cut.lines {
            assert!(line.len() <= at::MOTD_WIDTH);
        }
        assert_eq!(wrap("abcdefghij klm", 4), vec!["ABCD", "EFGH", "IJ", "KLM"]);
        assert_eq!(byline("Seneca", 34), "Seneca");
        assert_eq!(byline("Havamal 78 (tr. Henry Adams", 34), "Havamal 78");
        assert_eq!(
            byline("Epictetus (Enchiridion 1)", 34),
            "Epictetus (Enchiridion 1)"
        );
        assert_eq!(
            byline("Havamal 78 (tr. Henry Adams Bellows, 1923)", 34),
            "Havamal 78"
        );
        let long = byline(&"Marcus Aurelius ".repeat(4), 34);
        assert!(long.len() <= 34 && long.ends_with("..."), "{long}");
    }

    #[test]
    fn only_changed_cells_are_drawn() {
        let mut c = Canvas::new(W, H);
        let a = compose(Phase::Online, &facts());
        assert_eq!(render(&mut c, &a, None), Some((0, H)));
        assert_eq!(render(&mut c, &a, Some(&a)), None);
        // The cursor's blink is one row of cells.
        let b = compose(
            Phase::Online,
            &Facts {
                cursor: false,
                ..facts()
            },
        );
        let rows = render(&mut c, &b, Some(&a)).unwrap();
        assert_eq!(
            rows,
            (at::PASSWORD as i32 * CH, (at::PASSWORD as i32 + 1) * CH)
        );
        assert_eq!(hours_minutes(12 * 60 + 5), "00:12");
        assert_eq!(hours_minutes(3 * 3600 + 7 * 60), "03:07");
    }

    #[test]
    fn the_screen_fits_every_monitor() {
        for (w, h, scale) in [
            (1920, 1080, 2),
            (2560, 1440, 2),
            (3840, 2160, 4),
            (1366, 768, 1),
        ] {
            let s = arrange(Area {
                x: 0,
                y: 0,
                width: w,
                height: h,
            });
            let p = s.place.expect("fits");
            assert_eq!(p.sx, scale, "{w}x{h}");
            let (x, y, pw, ph) = p.rect(W, H);
            assert!(x >= 0 && y >= 0 && x + pw <= i32::from(w) && y + ph <= i32::from(h));
        }
    }

    /// Pictures of the screen for looking at:
    /// `BBS_PREVIEW=dir cargo test --release bbs::tests::preview -- --ignored`.
    #[test]
    #[ignore]
    fn preview() {
        let Some(dir) = std::env::var_os("BBS_PREVIEW") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let themes = [
            ("noir", 0x5a5a60, 0xe5e5e5, 0xa8a8a8),
            ("nord", 0x88c0d0, 0xeceff4, 0x9aa5b1),
            ("satie", 0xb08d7d, 0xdedad4, 0x8f8b86),
        ];
        let motd = Motd::quote(
            "Cattle die, kinsmen die, you yourself will also die; but the fair fame never dies of the one who earns it",
            "Havamal 77",
        )
        .unwrap_or_else(|| Motd::cut("Cattle die, kinsmen die, you yourself will also die; but the fair fame never dies of the one who earns it", "Havamal 77"));
        let status = vec![
            ("ACCESS DENIED: INVALID PASSWORD".to_string(), ansi::RED),
            ("1 FAILED LOGIN ATTEMPT".to_string(), ansi::DARK_GREY),
        ];
        for (name, focus, fg, muted) in themes {
            let accent = crate::ascii::banner_colour(focus, fg, 0);
            let pal = Palette::new(accent, fg, muted, crate::config::DEFAULT_WARNING);
            let shots = [
                (
                    "dial",
                    Phase::at(Cycle::First, 3.0 + modem::CONNECT + 0.3, Some(3.0)),
                ),
                ("stream", Phase::Stream(1300)),
                ("online", Phase::Online),
                ("wrong", Phase::Online),
            ];
            for (shot, phase) in shots {
                let f = Facts {
                    motd: Some(&motd),
                    tune: Some("NIRUX - Gimle"),
                    entry: "******",
                    wrong: shot == "wrong",
                    status: if shot == "wrong" { &status } else { &[] },
                    beat: true,
                    ..facts()
                };
                let mut c = Canvas::new(W, H);
                render(&mut c, &compose(phase, &f), None);
                let mut out = format!("P6\n{W} {H}\n255\n").into_bytes();
                for p in &c.px {
                    let rgb = pal.0[usize::from(*p)];
                    out.extend_from_slice(&[(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]);
                }
                std::fs::write(dir.join(format!("{name}-{shot}.ppm")), out).unwrap();
            }
        }
    }
}
