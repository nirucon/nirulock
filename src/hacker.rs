//! The `hacker` layout: the NIRUX intrusion console, a hacker's terminal
//! of the films and the cyberpunk novels that has been left running.
//!
//! The lock screen is a text screen of 100x31 cells (8x16 pixels each, as
//! in [`crate::bbs`]). When the screen locks the console boots, fast: the
//! banner, `[ OK ]` lines for the ICE breaker, the keyring and the cloak,
//! a handshake with BIFROST/7, a deep trace, a burst of hex dump and two
//! progress bars. Then its panes open one by one and it settles:
//!
//! - MEMORY: a hex dump that scrolls a line at a time, a scan cursor on the
//!   newest line;
//! - TRACE: a wire-frame globe turning slowly, a mesh of the nine realms
//!   and the bridges between them (only Norse names, nothing real), and a
//!   trace that runs along the mesh hop by hop, with its hop table;
//! - PACKET SNIFFER: a log of made-up traffic between the realms;
//! - CLOCK: the time in dot-matrix digits, the date and how long the
//!   console has been sealed;
//! - INTERCEPT: the message of the day, a quote from the `quotes`
//!   collections;
//! - SIGNAL: a calm carrier wave, or the pause music's spectrum, which
//!   brightens on the beat, and `NOW PLAYING  ARTIST - TITLE`;
//! - AUTHENTICATION: `ENTER PASSPHRASE:` (honouring `prompt_echo`), the
//!   failed attempts, Caps Lock and PAM's messages.
//!
//! A failed attempt flashes ACCESS DENIED in the warning colour with a short
//! glitch. All colours are roles on the active NIRUX theme
//! ([`chunky::Palette`], through [`bbs::ANSI`]): the console is drawn in the
//! accent, so a green theme gives green phosphor, niru-satie sepia and
//! niru-noir grey. At two times scale and more every second screen line is
//! drawn a little darker, like a CRT's scanlines.
//!
//! Only what changed is sent: the cells that differ are drawn into the
//! canvas, the pixel panes are drawn over their blank cells, and the parts
//! of each cell row that differ from what the screen shows go out with
//! PutImage. At rest the console wakes four times a second. With `[motion] reduced` the console shows at once and
//! keeps still: no boot, no glitch, no scanlines, and only the clock
//! changes, once a minute. Nothing is drawn with the display off.

use crate::{
    bbs::{self, codes, Cell, Grid, Motd},
    chunky::{self, slot, Canvas, Palette, Screen},
    display::Area,
    layout, Lock, Result,
};
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder};

/// The text screen.
pub const COLS: usize = 100;
pub const ROWS: usize = 31;
const CW: i32 = bbs::CW;
const CH: i32 = bbs::CH;
pub const W: i32 = COLS as i32 * CW;
pub const H: i32 = ROWS as i32 * CH;

/// What the console draws with, as ANSI colours ([`bbs::ANSI`] maps them
/// onto the theme): the phosphor is the accent in three shades.
pub mod ink {
    use crate::bbs::ansi;
    pub const BLACK: u8 = ansi::BLACK;
    /// The accent, dark: frames, quiet text.
    pub const FRAME: u8 = ansi::BROWN;
    /// The accent: the console's text.
    pub const TEXT: u8 = ansi::LIGHT_BLUE;
    /// The accent, light: titles, what just arrived.
    pub const BRIGHT: u8 = ansi::YELLOW;
    /// The cool neighbour: addresses, labels.
    pub const AUX: u8 = ansi::CYAN;
    pub const AUX_BRIGHT: u8 = ansi::LIGHT_CYAN;
    pub const DIM: u8 = ansi::DARK_GREY;
    pub const PLAIN: u8 = ansi::GREY;
    pub const WHITE: u8 = ansi::WHITE;
    /// The warning colour and its flash.
    pub const ALERT: u8 = ansi::RED;
    pub const ALERT_BRIGHT: u8 = ansi::LIGHT_RED;
}

/// A framed pane: its outer cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pane {
    pub col: usize,
    pub row: usize,
    pub w: usize,
    pub h: usize,
}

impl Pane {
    /// The first inner row and column, and the last ones (inclusive).
    pub fn inner(&self) -> (usize, usize, usize, usize) {
        (
            self.col + 1,
            self.row + 1,
            self.col + self.w - 2,
            self.row + self.h - 2,
        )
    }
}

pub const MEMORY: Pane = Pane {
    col: 0,
    row: 1,
    w: 42,
    h: 12,
};
pub const TRACE: Pane = Pane {
    col: 42,
    row: 1,
    w: 58,
    h: 12,
};
pub const SNIFFER: Pane = Pane {
    col: 0,
    row: 13,
    w: 62,
    h: 7,
};
pub const CLOCK: Pane = Pane {
    col: 62,
    row: 13,
    w: 38,
    h: 7,
};
pub const INTERCEPT: Pane = Pane {
    col: 0,
    row: 20,
    w: 62,
    h: 6,
};
pub const SIGNAL: Pane = Pane {
    col: 62,
    row: 20,
    w: 38,
    h: 6,
};
pub const AUTH: Pane = Pane {
    col: 0,
    row: 26,
    w: 100,
    h: 4,
};
/// The panes in the order they open.
const PANES: [Pane; 7] = [MEMORY, TRACE, SNIFFER, CLOCK, INTERCEPT, SIGNAL, AUTH];
/// The status bar.
pub const BAR: usize = 30;

/// Inside the trace pane: the globe's cells (columns), the divider and the
/// hop table.
const GLOBE_COLS: (usize, usize) = (43, 75);
const DIVIDER: usize = 76;
const HOPS_COL: usize = 77;
/// The prompt's row and the line under it.
const ENTRY_ROW: usize = 27;
const STATUS_ROW: usize = 28;
/// The ACCESS DENIED box.
const DENIED: Pane = Pane {
    col: 28,
    row: 11,
    w: 44,
    h: 7,
};
const _: () = assert!(DENIED.col + DENIED.w <= COLS && DENIED.row + DENIED.h <= AUTH.row);
/// The message of the day: lines, width, and the longest attribution.
pub const MOTD_LINES: usize = 4;
pub const MOTD_WIDTH: usize = 58;
const BYLINE: usize = 40;

// ---- timing ------------------------------------------------------------------------

/// The boot ends and the console's panes start to open.
pub const BOOT_END: f32 = 5.3;
/// A pane opens this long after the one before.
const REVEAL: f32 = 0.09;
pub const REVEAL_END: f32 = REVEAL * PANES.len() as f32;
/// The hex dump scrolls a line this often.
const HEX_STEP: f32 = 1.0;
/// A packet may arrive in each slot this long (one in three does).
const SLOT: f32 = 0.5;
/// A trace hop takes this long; a trace (hops, then a rest) this long.
const HOP: f32 = 1.2;
const ROUTE: f32 = 12.0;
/// The globe turns this many degrees a second, a step every so often.
const SPIN: f32 = 4.0;
const SPIN_STEP: f32 = 1.0;
/// The glitch after a failed attempt.
const GLITCH: f32 = 0.45;
/// The cursor's half period.
const BLINK: f32 = 0.5;
/// Frame intervals: the boot and the glitch, the spectrum, rest.
const FRAME: Duration = Duration::from_millis(40);
const MUSIC: Duration = Duration::from_millis(80);
const TICK: f32 = 0.25;
/// TX stays lit this long after a key.
const SENT: Duration = Duration::from_millis(150);
/// A new message of the day this often, unless someone is typing.
const NEW_MOTD: f32 = 300.0;
const QUIET: Duration = Duration::from_secs(60);
/// The still console of reduced motion shows this moment.
pub const STILL: f32 = 9.0;
/// At most this many image bytes in one request.
const MAX_IMAGE_BYTES: usize = 200_000;

// ---- made-up data ------------------------------------------------------------------

/// The realms and bridges of the mesh: name, position from the globe's
/// centre in canvas pixels, and where the label goes from the node.
pub const NODES: [(&str, i32, i32, i32, i32); 11] = [
    ("YGGDRASIL", 0, -2, 7, -11),
    ("ASGARD", 4, -62, 7, -4),
    ("ALFHEIM", 54, -38, 7, -4),
    ("VANAHEIM", -46, -44, -71, -4),
    ("BIFROST", -18, -24, -63, -10),
    ("MIDGARD", -14, 26, -63, 2),
    ("JOTUNHEIM", 48, 12, 7, -4),
    ("NIDAVELLIR", -64, 4, -36, 8),
    ("NIFLHEIM", -42, 50, -71, -2),
    ("MUSPELHEIM", 34, 48, 7, -4),
    ("HEL", 4, 66, 7, -4),
];

/// The mesh's links.
pub const LINKS: [(usize, usize); 19] = [
    (0, 1),
    (0, 4),
    (0, 5),
    (0, 6),
    (0, 2),
    (1, 2),
    (1, 4),
    (4, 3),
    (3, 7),
    (2, 6),
    (6, 9),
    (9, 10),
    (10, 8),
    (8, 7),
    (5, 8),
    (5, 10),
    (5, 7),
    (6, 0),
    (4, 7),
];

/// Words that turn up in the memory.
const WORDS: [&str; 9] = [
    "NIRUX", "ODIN", "RUNE", "MIMIR", "HUGIN", "MUNIN", "FREYA", "LOKI", "SKALD",
];
const PORTS: [u16; 10] = [7, 22, 53, 80, 443, 1337, 6667, 8080, 9001, 31337];
const PROTOCOLS: [(&str, [&str; 4]); 5] = [
    ("TCP", ["SYN", "ACK", "PSH", "FIN"]),
    ("UDP", ["DGRAM", "DGRAM", "DGRAM", "DGRAM"]),
    ("TLS", ["HELLO", "DATA", "DATA", "DATA"]),
    ("DNS", ["QUERY", "ANSWR", "QUERY", "ANSWR"]),
    ("ICMP", ["ECHO", "REPLY", "ECHO", "REPLY"]),
];

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A number for item `k` of `stream`, the same for the same seed.
fn hash(seed: u64, stream: u64, k: i64) -> u64 {
    mix(seed ^ mix(stream.wrapping_mul(0x0100_0000_01b3) ^ k as u64))
}

/// Line `k` of the hex dump: an address, eight bytes and their text.
pub fn hex_bytes(seed: u64, k: u64) -> (u16, [u8; 8]) {
    let h = hash(seed, 1, k as i64);
    let mut bytes = h.to_le_bytes();
    if h % 4 == 0 {
        let word = WORDS[(h >> 8) as usize % WORDS.len()].as_bytes();
        let at = (h >> 16) as usize % (9 - word.len());
        bytes[at..at + word.len()].copy_from_slice(word);
    } else if h % 8 == 1 {
        // Some zeros, as real memory has.
        for b in bytes.iter_mut().skip(3 + (h >> 20) as usize % 4) {
            *b = 0;
        }
    }
    (
        (0x4000u64.wrapping_add(k.wrapping_mul(8)) & 0xffff) as u16,
        bytes,
    )
}

/// "A040  4E 49 52 55 58 00 13 37  NIRUX..7".
pub fn hex_line(seed: u64, k: u64) -> String {
    let (addr, bytes) = hex_bytes(seed, k);
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    let text: String = bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_uppercase() || b.is_ascii_digit() {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("{addr:04X}  {}  {text}", hex.join(" "))
}

/// A packet between two realms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub slot: i64,
    pub src: String,
    pub dst: String,
    pub proto: &'static str,
    pub len: u16,
    pub flags: &'static str,
}

/// The packet of slot `j`, if one arrived in it.
pub fn packet(seed: u64, j: i64) -> Option<Packet> {
    let h = hash(seed, 2, j);
    if h % 3 != 0 {
        return None;
    }
    let a = (h >> 4) as usize % NODES.len();
    let b = (a + 1 + (h >> 12) as usize % (NODES.len() - 1)) % NODES.len();
    let port = |shift: u32| PORTS[(h >> shift) as usize % PORTS.len()];
    let (proto, flags) = PROTOCOLS[(h >> 28) as usize % PROTOCOLS.len()];
    Some(Packet {
        slot: j,
        src: format!("{}:{}", NODES[a].0, port(20)),
        dst: format!("{}:{}", NODES[b].0, port(24)),
        proto,
        len: 40 + ((h >> 32) % 1461) as u16,
        flags: flags[(h >> 44) as usize % 4],
    })
}

/// "HH:MM:SS" for so many seconds into a day.
fn clock_of(seconds: i64) -> String {
    let s = seconds.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// The packet as a line of the sniffer, `day0` seconds into the day at
/// slot 0.
pub fn packet_line(p: &Packet, day0: u32) -> String {
    let time = clock_of(i64::from(day0) + (p.slot as f32 * SLOT).floor() as i64);
    format!(
        "{time} {:<16} > {:<16} {:<4} {:>4} {}",
        p.src, p.dst, p.proto, p.len, p.flags
    )
}

/// The newest `n` packets up to slot `j`, oldest first.
pub fn packets(seed: u64, j: i64, n: usize) -> Vec<Packet> {
    let mut out: Vec<Packet> = (0..n * 40)
        .filter_map(|back| packet(seed, j - back as i64))
        .take(n)
        .collect();
    out.reverse();
    out
}

fn linked(a: usize, b: usize) -> bool {
    LINKS
        .iter()
        .any(|&(x, y)| (x, y) == (a, b) || (y, x) == (a, b))
}

/// Trace `r`: four to six realms along the mesh, never one twice.
pub fn route(seed: u64, r: u64) -> Vec<usize> {
    let h = hash(seed, 3, r as i64);
    let want = 4 + (h % 3) as usize;
    let mut path = vec![(h >> 8) as usize % NODES.len()];
    let mut k = 0;
    while path.len() < want {
        let at = path[path.len() - 1];
        let next: Vec<usize> = (0..NODES.len())
            .filter(|&n| linked(at, n) && !path.contains(&n))
            .collect();
        if next.is_empty() {
            break;
        }
        k += 1;
        path.push(next[hash(seed, 4, (r * 16 + k) as i64) as usize % next.len()]);
    }
    path
}

/// Where trace `r` is `tau` seconds in: the hops done, and how far along
/// the next one (`None` once it is complete).
pub fn trace_at(path: &[usize], tau: f32) -> (usize, Option<f32>) {
    let hops = path.len().saturating_sub(1);
    if tau >= hops as f32 * HOP {
        return (hops, None);
    }
    let done = (tau / HOP) as usize;
    (done, Some((tau - done as f32 * HOP) / HOP))
}

/// The round trip to hop `i` of trace `r`, in milliseconds.
fn rtt(seed: u64, r: u64, i: usize) -> u32 {
    (0..=i)
        .map(|k| 2 + (hash(seed, 5, (r * 16 + k as u64) as i64) % 38) as u32)
        .sum()
}

// ---- the screen --------------------------------------------------------------------

/// Where the console is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// Booting, so many seconds in.
    Boot(f32),
    /// The console, so many seconds after it opened.
    Console(f32),
}

/// The boot's script: when, and what.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// A line that arrives at once.
    Say(&'static str),
    /// A command, typed.
    Type(&'static str),
    /// So many lines of hex dump.
    Dump(usize),
    /// A progress bar, filling in so many seconds.
    Bar(&'static str, f32),
}

const BOOT: [(f32, Step); 17] = [
    (0.0, Step::Say("NIRUX INTRUSION CONSOLE  V")),
    (0.12, Step::Say("(C) NIRUX. FOR AUTHORISED OPERATORS ONLY.")),
    (0.3, Step::Type("> BOOT --SILENT /DEV/ICE0")),
    (1.05, Step::Say("[ OK ] ICE BREAKER ............... LOADED")),
    (1.17, Step::Say("[ OK ] KEYRING ................... SEALED")),
    (
        1.29,
        Step::Say("[ OK ] CLOAK ..................... ENGAGED"),
    ),
    (1.45, Step::Type("> HANDSHAKE BIFROST/7")),
    (
        1.95,
        Step::Say("  SYN >>>   <<< SYN-ACK   ACK >>>   LINK UP"),
    ),
    (2.07, Step::Say("  CIPHER RUNE-512   KEY 7F:3A:9C:E1:04:B8")),
    (2.25, Step::Type("> TRACE YGGDRASIL --DEEP")),
    (2.75, Step::Dump(10)),
    (3.4, Step::Bar("DECRYPTING", 0.7)),
    (4.15, Step::Bar("UPLINK    ", 0.4)),
    (4.65, Step::Say("")),
    (
        4.7,
        Step::Say("LINK ESTABLISHED. CONSOLE SEALED, OPERATOR AWAY."),
    ),
    (4.9, Step::Say("PASSPHRASE REQUIRED.")),
    (5.0, Step::Say("")),
];
/// Seconds a typed character takes, and a dumped line.
const TYPE_PACE: f32 = 0.028;
const DUMP_PACE: f32 = 0.055;

/// Everything a frame shows besides the phase.
#[derive(Clone, Debug, Default)]
pub struct Facts<'a> {
    /// The lock's own random numbers for the made-up data.
    pub seed: u64,
    pub version: &'a str,
    /// "THU 26 NOV 2026", "14:32", and how long the screen has been locked,
    /// "00:12".
    pub date: &'a str,
    pub time: &'a str,
    pub locked: &'a str,
    /// Seconds into the day when the console opened.
    pub day0: u32,
    pub motd: Option<&'a Motd>,
    /// "Artist - Title".
    pub tune: Option<&'a str>,
    /// The entry as `prompt_echo` shows it.
    pub entry: &'a str,
    pub checking: bool,
    pub wrong: bool,
    /// Seconds since the attempt failed, while it is shown.
    pub denied: Option<f32>,
    /// Why, under ACCESS DENIED.
    pub why: &'a str,
    /// The line under the prompt, with ANSI colours.
    pub status: &'a [(String, u8)],
    pub failed: u32,
    pub keyboard: Option<&'a str>,
    pub caps: bool,
    pub reduced: bool,
    /// TX after a key; the beat of the music.
    pub sent: bool,
    pub beat: bool,
    /// The spectrum of the music, 0 to 1, while it plays.
    pub spectrum: Option<&'a [f32]>,
}

fn put(g: &mut Grid, col: usize, row: usize, text: &str, fg: u8) -> usize {
    g.put(col, row, text, fg)
}

/// Text on a background colour.
fn put_on(g: &mut Grid, col: usize, row: usize, text: &str, fg: u8, bg: u8) -> usize {
    let mut c = col;
    for ch in codes(text) {
        g.set(c, row, Cell { ch, fg, bg });
        c += 1;
    }
    c
}

/// Text cut to `max` columns with "..." when it is longer.
fn fit(text: &str, max: usize) -> String {
    let c = codes(text);
    if c.len() <= max {
        return String::from_utf8_lossy(&c).into_owned();
    }
    let cut = String::from_utf8_lossy(&c[..max.saturating_sub(3)]).into_owned();
    format!("{}...", cut.trim_end())
}

fn cell(ch: u8, fg: u8) -> Cell {
    Cell {
        ch,
        fg,
        bg: ink::BLACK,
    }
}

/// A single frame with its title on the top line.
fn frame(g: &mut Grid, p: Pane, title: &str, fg: u8, title_fg: u8) {
    let (right, bottom) = (p.col + p.w - 1, p.row + p.h - 1);
    for c in p.col + 1..right {
        g.set(c, p.row, cell(0xc4, fg));
        g.set(c, bottom, cell(0xc4, fg));
    }
    for r in p.row + 1..bottom {
        g.set(p.col, r, cell(0xb3, fg));
        g.set(right, r, cell(0xb3, fg));
    }
    g.set(p.col, p.row, cell(0xda, fg));
    g.set(right, p.row, cell(0xbf, fg));
    g.set(p.col, bottom, cell(0xc0, fg));
    g.set(right, bottom, cell(0xd9, fg));
    if !title.is_empty() {
        g.set(p.col + 2, p.row, cell(0xb4, fg));
        let next = put(g, p.col + 3, p.row, &format!(" {title} "), title_fg);
        g.set(next, p.row, cell(0xc3, fg));
    }
}

/// A double frame (the ACCESS DENIED box), its inside filled with `bg`.
fn double_frame(g: &mut Grid, p: Pane, fg: u8, bg: u8) {
    let (right, bottom) = (p.col + p.w - 1, p.row + p.h - 1);
    for r in p.row..=bottom {
        for c in p.col..=right {
            g.set(c, r, Cell { ch: b' ', fg, bg });
        }
    }
    let on = |ch: u8| Cell { ch, fg, bg };
    for c in p.col + 1..right {
        g.set(c, p.row, on(0xcd));
        g.set(c, bottom, on(0xcd));
    }
    for r in p.row + 1..bottom {
        g.set(p.col, r, on(0xba));
        g.set(right, r, on(0xba));
    }
    g.set(p.col, p.row, on(0xc9));
    g.set(right, p.row, on(0xbb));
    g.set(p.col, bottom, on(0xc8));
    g.set(right, bottom, on(0xbc));
}

/// The boot's lines at `t`, each with its colour, and whether the last one
/// is still being typed.
fn boot_lines(t: f32, f: &Facts) -> (Vec<(String, u8)>, bool) {
    let mut lines = Vec::new();
    let mut typing = false;
    for &(at, step) in &BOOT {
        if t < at {
            break;
        }
        match step {
            Step::Say(text) if text.ends_with(" V") => {
                lines.push((format!("{text}{}", f.version), ink::BRIGHT));
            }
            Step::Say(text) => {
                let fg = if text.starts_with("[ OK ]") || text.starts_with("  ") {
                    ink::TEXT
                } else {
                    ink::BRIGHT
                };
                lines.push((text.to_string(), fg));
            }
            Step::Type(text) => {
                let n = (((t - at) / TYPE_PACE) as usize + 1).min(text.len());
                typing = n < text.len();
                lines.push((text[..n].to_string(), ink::BRIGHT));
            }
            Step::Dump(count) => {
                let n = (((t - at) / DUMP_PACE) as usize + 1).min(count);
                for k in 0..n {
                    lines.push((
                        format!("  {}", hex_line(f.seed, 0x9000 + k as u64)),
                        ink::FRAME,
                    ));
                }
            }
            Step::Bar(label, length) => {
                let done = ((t - at) / length).clamp(0.0, 1.0);
                let width = 30;
                let lit = (done * width as f32).round() as usize;
                let bar: String = "█".repeat(lit) + &"░".repeat(width - lit);
                lines.push((
                    format!("{label}  [{bar}] {:>3}%", (done * 100.0) as u32),
                    ink::TEXT,
                ));
            }
        }
    }
    (lines, typing)
}

/// The screen for `phase`, as text cells; [`draw`] adds the pixel panes.
pub fn compose(phase: Phase, f: &Facts) -> Grid {
    let mut g = Grid::new(COLS, ROWS);
    match phase {
        Phase::Boot(t) => {
            let (lines, typing) = boot_lines(t, f);
            let room = ROWS - 2;
            let skip = lines.len().saturating_sub(room);
            let shown = &lines[skip..];
            for (i, (text, fg)) in shown.iter().enumerate() {
                put(&mut g, 2, 1 + i, text, *fg);
                if text.starts_with("[ OK ]") {
                    put(&mut g, 2, 1 + i, "[    ]", ink::FRAME);
                    put(&mut g, 4, 1 + i, "OK", ink::BRIGHT);
                }
            }
            if ((t / 0.12) as u64) % 2 == 0 || typing {
                let (col, row) = match shown.last() {
                    Some((text, _)) if typing => (2 + codes(text).len(), shown.len()),
                    _ => (2, shown.len() + 1),
                };
                g.set(col, row.min(ROWS - 1), cell(0xdb, ink::TEXT));
            }
        }
        Phase::Console(t) => console(&mut g, t, f),
    }
    g
}

/// The console `t` seconds after it opened.
fn console(g: &mut Grid, t: f32, f: &Facts) {
    let open = |i: usize| f.reduced || t >= i as f32 * REVEAL;
    let cursor = f.reduced || ((t / BLINK) as u64) % 2 == 0;
    title_bar(g, f);
    if open(0) {
        memory(g, t, f);
    }
    if open(1) {
        trace(g, t, f);
    }
    if open(2) {
        sniffer(g, t, f);
    }
    if open(3) {
        frame(g, CLOCK, "CLOCK // LOCAL", ink::FRAME, ink::BRIGHT);
        let (c0, _, c1, _) = CLOCK.inner();
        let centre = |g: &mut Grid, row: usize, text: &str, fg: u8| {
            let len = codes(text).len();
            put(g, c0 + (c1 + 1 - c0).saturating_sub(len) / 2, row, text, fg);
        };
        centre(g, CLOCK.row + 4, f.date, ink::TEXT);
        centre(
            g,
            CLOCK.row + 5,
            &format!("SEALED {}", f.locked),
            ink::FRAME,
        );
    }
    if open(4) {
        intercept(g, f);
    }
    if open(5) {
        signal(g, f);
    }
    if open(6) {
        auth(g, f, cursor);
    }
    status_bar(g, t, f);
    if let (true, Some(since)) = (f.wrong, f.denied) {
        if !f.reduced && since < GLITCH {
            glitch(g, f.seed, since);
        }
        denied_box(g, f, since);
    }
}

fn title_bar(g: &mut Grid, f: &Facts) {
    for c in 0..COLS {
        g.set(
            c,
            0,
            Cell {
                ch: b' ',
                fg: ink::BRIGHT,
                bg: ink::FRAME,
            },
        );
    }
    let mut col = put_on(g, 1, 0, "▓", ink::BRIGHT, ink::FRAME);
    col = put_on(
        g,
        col + 1,
        0,
        &format!("NIRUX INTRUSION CONSOLE {}", f.version),
        ink::WHITE,
        ink::FRAME,
    );
    col = put_on(g, col + 1, 0, "│", ink::BRIGHT, ink::FRAME);
    put_on(
        g,
        col + 1,
        0,
        "LINK BIFROST/7  RUNE-512",
        ink::BRIGHT,
        ink::FRAME,
    );
    let right = format!("{}  {}", f.date, f.time);
    let len = codes(&right).len();
    put_on(g, COLS - 1 - len, 0, &right, ink::WHITE, ink::FRAME);
}

fn memory(g: &mut Grid, t: f32, f: &Facts) {
    frame(g, MEMORY, "MEMORY // 0X7FFE", ink::FRAME, ink::BRIGHT);
    let (c0, r0, _, r1) = MEMORY.inner();
    let lines = r1 + 1 - r0;
    let step = (t / HEX_STEP).max(0.0) as u64;
    let scan = ((t / TICK).max(0.0) as usize) % 8;
    for i in 0..lines {
        let k = step + i as u64;
        let (addr, bytes) = hex_bytes(f.seed, k);
        let row = r0 + i;
        let newest = i + 1 == lines;
        let mut col = put(g, c0 + 1, row, &format!("{addr:04X}"), ink::AUX);
        col += 2;
        for (j, b) in bytes.iter().enumerate() {
            let fg = if newest {
                ink::BRIGHT
            } else if *b == 0 {
                ink::DIM
            } else {
                ink::TEXT
            };
            let text = format!("{b:02X}");
            if newest && j == scan && !f.reduced {
                put_on(g, col, row, &text, ink::BLACK, ink::TEXT);
            } else {
                put(g, col, row, &text, fg);
            }
            col += 3;
        }
        col += 1;
        let text: String = hex_line(f.seed, k).chars().skip(31).collect();
        put(
            g,
            col,
            row,
            &text,
            if newest { ink::TEXT } else { ink::FRAME },
        );
    }
}

/// The trace now: the route, the hops done and how far along the next.
pub fn trace_now(t: f32, seed: u64) -> (u64, Vec<usize>, usize, Option<f32>) {
    let r = (t / ROUTE).max(0.0) as u64;
    let path = route(seed, r);
    let (done, along) = trace_at(&path, t - r as f32 * ROUTE);
    (r, path, done, along)
}

fn trace(g: &mut Grid, t: f32, f: &Facts) {
    frame(
        g,
        TRACE,
        "TRACE // NINE REALMS MESH",
        ink::FRAME,
        ink::BRIGHT,
    );
    let (_, r0, c1, r1) = TRACE.inner();
    g.set(DIVIDER, TRACE.row, cell(0xc2, ink::FRAME));
    g.set(DIVIDER, TRACE.row + TRACE.h - 1, cell(0xc1, ink::FRAME));
    for r in r0..=r1 {
        g.set(DIVIDER, r, cell(0xb3, ink::FRAME));
    }
    let col = HOPS_COL + 1;
    put(g, col, r0, "HOP NODE        RTT", ink::AUX);
    let (r, path, done, along) = trace_now(t, f.seed);
    for (i, &node) in path.iter().enumerate().take(done + 1) {
        let row = r0 + 1 + i;
        let name = NODES[node].0;
        let current = along.is_some() && i == done;
        let fg = if current {
            ink::BRIGHT
        } else if i == path.len() - 1 {
            ink::WHITE
        } else {
            ink::TEXT
        };
        put(g, col, row, &format!("{:02}  {name:<11}", i + 1), fg);
        let ms = if current {
            "...".to_string()
        } else {
            format!("{:>3}MS", rtt(f.seed, r, i))
        };
        put(
            g,
            col + 15,
            row,
            &ms,
            if current { ink::FRAME } else { ink::AUX },
        );
    }
    let status = match along {
        Some(_) => {
            let dots = ".".repeat(1 + ((t / TICK) as usize) % 3);
            (format!("TRACING{dots}"), ink::BRIGHT)
        }
        None => ("TRACE COMPLETE".to_string(), ink::WHITE),
    };
    put(g, col, r1 - 1, &status.0, status.1);
    let id = (hash(f.seed, 6, r as i64) & 0xffff) as u16;
    let info = format!("ROUTE {id:04X}  {} HOPS", path.len().saturating_sub(1));
    put(g, col, r1, &fit(&info, c1 - col + 1), ink::FRAME);
}

fn sniffer(g: &mut Grid, t: f32, f: &Facts) {
    frame(
        g,
        SNIFFER,
        "PACKET SNIFFER // BIFROST0",
        ink::FRAME,
        ink::BRIGHT,
    );
    let (c0, r0, _, r1) = SNIFFER.inner();
    let lines = r1 + 1 - r0;
    let j = (t / SLOT).floor() as i64;
    let list = packets(f.seed, j, lines);
    for (i, p) in list.iter().enumerate() {
        let row = r0 + i;
        let fresh = !f.reduced && (t - p.slot as f32 * SLOT) < 1.0;
        let line = packet_line(p, f.day0);
        let fg = if fresh { ink::BRIGHT } else { ink::TEXT };
        put(g, c0, row, &line, fg);
        put(g, c0, row, &line[..8], ink::FRAME);
        if let Some(at) = line.find(" > ") {
            put(g, c0 + at + 1, row, ">", ink::AUX);
        }
    }
}

fn intercept(g: &mut Grid, f: &Facts) {
    frame(
        g,
        INTERCEPT,
        "INTERCEPT // MESSAGE OF THE DAY",
        ink::FRAME,
        ink::BRIGHT,
    );
    let (c0, r0, c1, _) = INTERCEPT.inner();
    let welcome = welcome();
    let motd = f.motd.unwrap_or(&welcome);
    for (i, text) in motd.lines.iter().take(MOTD_LINES).enumerate() {
        put(g, c0 + 1, r0 + i, text, ink::TEXT);
    }
    if !motd.who.is_empty() {
        let who = format!("- {}", bbs::byline(&motd.who, BYLINE));
        let width = codes(&who).len() + 4;
        let col = c1 + 1 - width;
        let bottom = INTERCEPT.row + INTERCEPT.h - 1;
        g.set(col, bottom, cell(0xb4, ink::FRAME));
        let next = put(g, col + 1, bottom, &format!(" {who} "), ink::AUX_BRIGHT);
        g.set(next, bottom, cell(0xc3, ink::FRAME));
    }
}

/// The message of the day without quotes.
pub fn welcome() -> Motd {
    Motd {
        lines: vec![
            "THE OPERATOR LEFT THE CONSOLE RUNNING.".into(),
            "ALL CHANNELS ARE SEALED UNTIL THE PASSPHRASE IS GIVEN.".into(),
        ],
        who: "NIRUX".into(),
    }
}

fn signal(g: &mut Grid, f: &Facts) {
    let tune = f.tune.map(str::trim).filter(|t| !t.is_empty());
    let (c0, _, c1, r1) = SIGNAL.inner();
    let width = c1 + 1 - c0 - 1;
    match tune {
        Some(tune) => {
            frame(g, SIGNAL, "SIGNAL // AUDIO", ink::FRAME, ink::BRIGHT);
            let line = format!("NOW PLAYING  {}", pixfont_plain(tune));
            if codes(&line).len() <= width {
                let next = put(g, c0 + 1, r1, "NOW PLAYING  ", ink::AUX);
                put(g, next, r1, &pixfont_plain(tune), ink::WHITE);
            } else {
                frame(g, SIGNAL, "NOW PLAYING", ink::FRAME, ink::BRIGHT);
                put(g, c0 + 1, r1, &fit(tune, width), ink::WHITE);
            }
        }
        None => {
            frame(g, SIGNAL, "SIGNAL // CARRIER", ink::FRAME, ink::BRIGHT);
            put(g, c0 + 1, r1, "NO AUDIO  //  CARRIER IDLE", ink::FRAME);
        }
    }
}

fn pixfont_plain(text: &str) -> String {
    crate::bbs::text_of(&codes(text))
}

fn auth(g: &mut Grid, f: &Facts, cursor: bool) {
    let fg = if f.wrong { ink::ALERT } else { ink::FRAME };
    frame(g, AUTH, "AUTHENTICATION // CONSOLE SEALED", fg, ink::BRIGHT);
    let (c0, _, c1, _) = AUTH.inner();
    let label = if f.wrong { ink::ALERT } else { ink::AUX_BRIGHT };
    let next = put(g, c0 + 1, ENTRY_ROW, "ENTER PASSPHRASE: ", label);
    let right = if f.failed > 0 {
        let s = if f.failed == 1 { "" } else { "S" };
        format!("{} FAILED ATTEMPT{s}", f.failed)
    } else {
        "0 FAILED ATTEMPTS".to_string()
    };
    let rlen = codes(&right).len();
    let rcol = c1 - rlen;
    put(
        g,
        rcol,
        ENTRY_ROW,
        &right,
        if f.failed > 0 { ink::ALERT } else { ink::FRAME },
    );
    let room = rcol.saturating_sub(next + 3);
    let entry: String = f.entry.chars().take(room).collect();
    let after = put(g, next, ENTRY_ROW, &entry, ink::BRIGHT);
    if cursor && !f.checking {
        g.set(after, ENTRY_ROW, cell(0xdb, ink::TEXT));
    }
    let mut parts: Vec<(String, u8)> = Vec::new();
    if f.checking {
        parts.push(("VERIFYING PASSPHRASE...".into(), ink::AUX_BRIGHT));
    }
    parts.extend(f.status.iter().cloned());
    if parts.is_empty() {
        parts.push((
            "AWAITING OPERATOR. EVERY CHANNEL IS ENCRYPTED.".into(),
            ink::FRAME,
        ));
    }
    let mut col = c0 + 1;
    for (i, (text, fg)) in parts.iter().enumerate() {
        if i > 0 {
            col = put(g, col, STATUS_ROW, "  ·  ", ink::FRAME);
        }
        let room = (c1 + 1).saturating_sub(col);
        if room < 4 {
            break;
        }
        col = put(g, col, STATUS_ROW, &fit(text, room), *fg);
    }
}

/// The bottom line: the console's state, the keyboard, Caps Lock, the
/// link, the time sealed and the TX and RX lights.
fn status_bar(g: &mut Grid, t: f32, f: &Facts) {
    let bg = ink::FRAME;
    for c in 0..COLS {
        g.set(
            c,
            BAR,
            Cell {
                ch: b' ',
                fg: ink::BRIGHT,
                bg,
            },
        );
    }
    let mut col = 1;
    let mut text = |g: &mut Grid, s: &str, fg: u8| {
        col = put_on(g, col, BAR, s, fg, bg);
    };
    let sep = |g: &mut Grid, text: &mut dyn FnMut(&mut Grid, &str, u8)| {
        text(g, " │ ", ink::BRIGHT);
    };
    text(g, "ICE ACTIVE", ink::WHITE);
    sep(g, &mut text);
    let keyboard = f.keyboard.map_or("KBD --".to_string(), |k| {
        format!("KBD {}", k.chars().take(4).collect::<String>())
    });
    text(g, &keyboard, ink::BRIGHT);
    sep(g, &mut text);
    text(
        g,
        "CAPS",
        if f.caps {
            ink::ALERT_BRIGHT
        } else {
            ink::BLACK
        },
    );
    sep(g, &mut text);
    text(g, "LINK BIFROST/7 ENCRYPTED", ink::BRIGHT);
    sep(g, &mut text);
    text(g, &format!("SEALED {}", f.locked), ink::BRIGHT);
    sep(g, &mut text);
    let rx = f.beat
        || (!f.reduced
            && f.spectrum.is_none()
            && packet(f.seed, (t / SLOT).floor() as i64).is_some()
            && (t / SLOT).fract() < 0.5);
    text(g, "TX ", ink::BRIGHT);
    text(g, "■", if f.sent { ink::WHITE } else { ink::BLACK });
    text(g, "  RX ", ink::BRIGHT);
    text(g, "■", if rx { ink::WHITE } else { ink::BLACK });
}

/// The glitch: a few rows slip sideways and some cells turn to noise in the
/// warning colour. It changes every 40 ms.
fn glitch(g: &mut Grid, seed: u64, since: f32) {
    let k = (since / 0.04) as i64;
    for i in 0..5 {
        let h = hash(seed, 7, k * 8 + i);
        let row = (h % ROWS as u64) as usize;
        let shift = (h >> 8) as i64 % 9 - 4;
        let old: Vec<Cell> = (0..COLS).map(|c| g.get(c, row)).collect();
        for c in 0..COLS {
            let from = c as i64 - shift;
            let cell = if (0..COLS as i64).contains(&from) {
                old[from as usize]
            } else {
                bbs::BLANK
            };
            g.set(c, row, cell);
        }
    }
    for i in 0..40 {
        let h = hash(seed, 8, k * 64 + i);
        let (col, row) = (
            (h % COLS as u64) as usize,
            ((h >> 8) % ROWS as u64) as usize,
        );
        let ch = [0xb0, 0xb1, 0xb2, 0xdb][(h >> 16) as usize % 4];
        g.set(col, row, cell(ch, ink::ALERT));
    }
}

/// The ACCESS DENIED box; it flashes inverted during the glitch.
fn denied_box(g: &mut Grid, f: &Facts, since: f32) {
    let flash = !f.reduced && since < GLITCH && ((since / 0.08) as u64) % 2 == 0;
    let bg = if flash { ink::ALERT } else { ink::BLACK };
    let fg = if flash { ink::BLACK } else { ink::ALERT };
    double_frame(g, DENIED, fg, bg);
    let why = fit(
        if f.why.is_empty() {
            "INVALID PASSPHRASE"
        } else {
            f.why
        },
        DENIED.w - 4,
    );
    let len = codes(&why).len();
    let col = DENIED.col + (DENIED.w - len) / 2;
    put_on(
        g,
        col,
        DENIED.row + 4,
        &why,
        if flash { ink::BLACK } else { ink::ALERT_BRIGHT },
        bg,
    );
    let attempt = format!("ATTEMPT {} LOGGED", f.failed.max(1));
    let len = codes(&attempt).len();
    let col = DENIED.col + (DENIED.w - len) / 2;
    put_on(g, col, DENIED.row + 5, &attempt, fg, bg);
}

/// The ACCESS DENIED box shows.
fn denied_shown(f: &Facts) -> bool {
    f.wrong && f.denied.is_some()
}

// ---- the pixel panes ---------------------------------------------------------------

/// A line, solid or every `dot`th pixel.
fn line(c: &mut Canvas, (x0, y0): (i32, i32), (x1, y1): (i32, i32), colour: u8, dot: i32) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    let mut n = 0;
    loop {
        if dot <= 1 || n % dot == 0 {
            c.set(x, y, colour);
        }
        n += 1;
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// A pane's inside in canvas pixels: x, y, width, height.
fn px(c0: usize, r0: usize, c1: usize, r1: usize) -> (i32, i32, i32, i32) {
    (
        c0 as i32 * CW,
        r0 as i32 * CH,
        (c1 + 1 - c0) as i32 * CW,
        (r1 + 1 - r0) as i32 * CH,
    )
}

fn globe_rect() -> (i32, i32, i32, i32) {
    let (_, r0, _, r1) = TRACE.inner();
    px(GLOBE_COLS.0, r0, GLOBE_COLS.1, r1)
}

/// The trace pane's picture: the globe, the mesh, the trace and its packet.
fn draw_globe(c: &mut Canvas, t: f32, f: &Facts) {
    let (x, y, w, h) = globe_rect();
    c.fill(x, y, w, h, slot::BLACK);
    let (cx, cy) = (x + w / 2, y + h / 2);
    let r = 72.0f32;
    // The globe turns in steps of half a second, so that the trace's
    // packet can move more often without redrawing the whole globe.
    let spin = (if f.reduced { STILL } else { t } / SPIN_STEP).floor() * SPIN_STEP * SPIN;
    let tilt = 22f32.to_radians();
    let (st, ct) = tilt.sin_cos();
    let dot = |c: &mut Canvas, lat: f32, lon: f32, front: u8, back: Option<u8>| {
        let (sl, cl) = lat.to_radians().sin_cos();
        let (so, co) = lon.to_radians().sin_cos();
        let (px_, py, pz) = (cl * so, sl, cl * co);
        let y2 = py * ct - pz * st;
        let z2 = py * st + pz * ct;
        let sx = cx + (px_ * r).round() as i32;
        let sy = cy - (y2 * r).round() as i32;
        if z2 >= 0.0 {
            c.set(sx, sy, front);
        } else if let Some(b) = back {
            c.set(sx, sy, b);
        }
    };
    // The far side faintly, then the near side.
    for lat in (-60..=60).step_by(30) {
        for k in 0..120 {
            let lon = k as f32 * 3.0;
            dot(
                c,
                lat as f32,
                lon,
                slot::ACCENT_DARK,
                (k % 2 == 0).then_some(slot::GREY_DARK),
            );
        }
    }
    for m in 0..12 {
        let lon = m as f32 * 30.0 + spin;
        for k in -30..=30 {
            dot(
                c,
                k as f32 * 3.0,
                lon,
                slot::ACCENT_DARK,
                (k % 2 == 0).then_some(slot::GREY_DARK),
            );
        }
    }
    // The rim.
    for k in 0..240 {
        let a = (k as f32 * 1.5).to_radians();
        c.set(
            cx + (a.cos() * (r + 1.0)).round() as i32,
            cy + (a.sin() * (r + 1.0)).round() as i32,
            slot::ACCENT_DARK,
        );
    }
    let at = |n: usize| (cx + NODES[n].1, cy + NODES[n].2);
    for &(a, b) in &LINKS {
        line(c, at(a), at(b), slot::GREY, 3);
    }
    let (_, path, done, along) = trace_now(if f.reduced { STILL } else { t }, f.seed);
    for pair in path.windows(2).take(done) {
        line(c, at(pair[0]), at(pair[1]), slot::ACCENT_LIGHT, 1);
    }
    let mut head = None;
    if let Some(k) = along {
        let (a, b) = (at(path[done]), at(path[done + 1]));
        let p = (
            a.0 + ((b.0 - a.0) as f32 * k).round() as i32,
            a.1 + ((b.1 - a.1) as f32 * k).round() as i32,
        );
        line(c, a, p, slot::ACCENT, 1);
        head = Some(p);
    }
    // The nodes and their names, the trace's on top.
    let blink = f.reduced || ((t / BLINK) as u64) % 2 == 0;
    for (n, &(name, _, _, lx, ly)) in NODES.iter().enumerate() {
        let (nx, ny) = at(n);
        let on = path.iter().take(done + 1).any(|&p| p == n);
        let last = along.is_none() && path.last() == Some(&n);
        let (size, colour) = if on {
            (2, slot::ACCENT_LIGHT)
        } else {
            (1, slot::ACCENT)
        };
        c.fill(nx - size, ny - size, 2 * size + 1, 2 * size + 1, colour);
        if last && blink {
            for k in 0..32 {
                let a = (k as f32 * 11.25).to_radians();
                c.set(
                    nx + (a.cos() * 6.0).round() as i32,
                    ny + (a.sin() * 6.0).round() as i32,
                    slot::WHITE,
                );
            }
        }
        let width = name.len() as i32 * 8;
        let (tx, ty) = (
            (nx + lx).clamp(x, x + w - width),
            (ny + ly).clamp(y, y + h - 8),
        );
        c.fill(tx - 1, ty - 1, width + 2, 10, slot::BLACK);
        let colour = if last {
            slot::WHITE
        } else if on {
            slot::ACCENT_LIGHT
        } else {
            slot::GREY
        };
        c.text(tx, ty, name, colour, 1);
    }
    if let Some((hx, hy)) = head {
        c.fill(hx - 2, hy - 2, 5, 5, slot::WHITE);
    }
}

/// The clock's dot-matrix digits: every pixel of the font a 3x3 dot, unlit
/// ones a faint point.
fn draw_clock(c: &mut Canvas, t: f32, f: &Facts) {
    let (c0, r0, c1, _) = CLOCK.inner();
    let (x, y, w, _) = px(c0, r0, c1, r0 + 1);
    c.fill(x, y, w, 2 * CH + 4, slot::BLACK);
    let colon = f.reduced || ((t / 1.0) as u64) % 2 == 0;
    let text: String = f.time.chars().take(5).collect();
    let pitch = 4;
    let width = text.chars().count() as i32 * 8 * pitch;
    let left = x + (w - width) / 2;
    let top = y + 2;
    for (i, ch) in text.chars().enumerate() {
        let glyph = crate::pixfont::glyph(if ch == ':' && !colon { ' ' } else { ch });
        for (gy, bits) in glyph.iter().enumerate() {
            for gx in 0..8 {
                let dx = left + (i as i32 * 8 + gx) * pitch;
                let dy = top + gy as i32 * pitch;
                if bits & (0x80 >> gx) != 0 {
                    c.fill(dx, dy, 3, 3, slot::ACCENT_LIGHT);
                } else {
                    c.set(dx + 1, dy + 1, slot::GREY_DARK);
                }
            }
        }
    }
}

/// The signal: a calm carrier, or the music's spectrum as a trace mirrored
/// around its middle line, brighter on the beat.
fn draw_signal(c: &mut Canvas, t: f32, f: &Facts) {
    let (c0, r0, c1, r1) = SIGNAL.inner();
    let (x, y, w, h) = px(c0, r0, c1, r1 - 1);
    c.fill(x, y, w, h, slot::BLACK);
    let mid = y + h / 2;
    for gx in (x + 4..x + w - 4).step_by(4) {
        c.set(gx, mid, slot::GREY_DARK);
    }
    for gx in (x + 4..x + w).step_by(32) {
        for gy in (y + 2..y + h - 2).step_by(4) {
            c.set(gx, gy, slot::GREY_DARK);
        }
    }
    let (x0, x1) = (x + 4, x + w - 5);
    let amp = (h / 2 - 3) as f32;
    match f.spectrum.filter(|s| !s.is_empty()) {
        Some(values) => {
            // An analyser: a bar of stacked segments for each band, its top
            // segment lit, all of it brighter on the beat.
            let (body, cap) = if f.beat {
                (slot::ACCENT, slot::WHITE)
            } else {
                (slot::ACCENT_DARK, slot::ACCENT_LIGHT)
            };
            let n = values.len();
            let bars = ((x1 - x0 + 2) / 6).max(1);
            let bottom = y + h - 2;
            let tall = h - 4;
            for b in 0..bars {
                let pos = b as f32 / (bars - 1).max(1) as f32 * (n - 1) as f32;
                let i = pos as usize;
                let k = pos - i as f32;
                let v = values[i] * (1.0 - k) + values[(i + 1).min(n - 1)] * k;
                let segments = (v.clamp(0.0, 1.0) * (tall / 3) as f32).round() as i32;
                let bx = x0 + b * 6;
                for s in 0..segments {
                    let colour = if s + 1 == segments { cap } else { body };
                    c.fill(bx, bottom - s * 3 - 2, 5, 2, colour);
                }
            }
        }
        None => {
            // In steps of the rest tick, whatever else wakes the console.
            let t = (if f.reduced { STILL } else { t } / TICK).floor() * TICK;
            let mut prev: Option<(i32, i32)> = None;
            for sx in x0..=x1 {
                let u = (sx - x0) as f32;
                let env = 0.55 + 0.45 * (u / 70.0 + t * 0.35).sin();
                let v = (u / 9.0 - t * 1.6).sin() * env * 0.6;
                let p = (sx, mid - (v * amp).round() as i32);
                if let Some(q) = prev {
                    line(c, q, p, slot::ACCENT, 1);
                }
                prev = Some(p);
            }
        }
    }
}

/// ACCESS DENIED in big letters in its box.
fn draw_denied(c: &mut Canvas, f: &Facts, since: f32) {
    let flash = !f.reduced && since < GLITCH && ((since / 0.08) as u64) % 2 == 0;
    let colour = if flash { slot::BLACK } else { slot::WARNING };
    let text = "ACCESS DENIED";
    let scale = 3;
    let width = text.len() as i32 * 8 * scale;
    let x = DENIED.col as i32 * CW + (DENIED.w as i32 * CW - width) / 2;
    let y = (DENIED.row as i32 + 1) * CH + 6;
    let jitter = if !f.reduced && since < GLITCH {
        (hash(f.seed, 9, (since / 0.04) as i64) % 5) as i32 - 2
    } else {
        0
    };
    if jitter != 0 {
        c.text(x - jitter, y, text, slot::FLASH, scale);
    }
    c.text(x + jitter, y, text, colour, scale);
}

/// What the pixel panes show, so that a pane is drawn again only when it
/// changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    globe: Option<u64>,
    clock: Option<u64>,
    signal: Option<u64>,
}

/// Draw the frame into the canvas: the cells that differ from `prev` (all
/// without it), then the pixel panes over their blank cells when they
/// changed (always without `prev`), then the ACCESS DENIED box again on
/// top.
pub fn draw(
    canvas: &mut Canvas,
    grid: &Grid,
    prev: Option<&Grid>,
    phase: Phase,
    f: &Facts,
    drawn: &mut Drawn,
) {
    bbs::render(canvas, grid, prev);
    if prev.is_none() {
        *drawn = Drawn::default();
    }
    let Phase::Console(t) = phase else {
        *drawn = Drawn::default();
        return;
    };
    let open = |i: usize| f.reduced || t >= i as f32 * REVEAL;
    let step = |every: f32| (if f.reduced { STILL } else { t } / every).floor() as u64;
    if open(1) {
        let (r, _, done, along) = trace_now(if f.reduced { STILL } else { t }, f.seed);
        let moment = if along.is_some() {
            step(TICK) << 1 | 1
        } else {
            step(SPIN_STEP) << 1
        };
        let key = mix(r << 40 ^ (done as u64) << 32 ^ moment);
        if drawn.globe != Some(key) {
            draw_globe(canvas, t, f);
            drawn.globe = Some(key);
        }
    }
    if open(3) {
        let colon = f.reduced || (t as u64) % 2 == 0;
        let key = f
            .time
            .bytes()
            .fold(u64::from(colon), |k, b| k.wrapping_mul(131) ^ u64::from(b));
        if drawn.clock != Some(key) {
            draw_clock(canvas, t, f);
            drawn.clock = Some(key);
        }
    }
    if open(5) {
        // The spectrum changes every frame; the carrier with the tick.
        let key = (f.spectrum.is_none() && !f.beat).then(|| step(TICK));
        if key.is_none() || drawn.signal != key {
            draw_signal(canvas, t, f);
            drawn.signal = key;
        }
    }
    if let (true, Some(since)) = (f.wrong, f.denied) {
        let p = DENIED;
        bbs::render_area(canvas, grid, p.col..p.col + p.w, p.row..p.row + p.h);
        draw_denied(canvas, f, since);
    }
}

// ---- sending it --------------------------------------------------------------------

/// A canvas rectangle: x0, y0, x1, y1 (exclusive).
pub type Rect = (i32, i32, i32, i32);

/// The parts of `cur` that differ from `prev`: in each band of a cell row,
/// the runs of cell columns with a pixel that differs (runs a few cells
/// apart are joined). A run that continues one of the band above with the
/// same columns is merged with it. Everything without `prev`.
pub fn dirty(cur: &Canvas, prev: Option<&Canvas>) -> Vec<Rect> {
    let Some(prev) = prev.filter(|p| p.w == cur.w && p.h == cur.h) else {
        return vec![(0, 0, cur.w, cur.h)];
    };
    const GAP: usize = 4;
    let w = cur.w as usize;
    let cells = w.div_ceil(CW as usize);
    let mut mask = vec![false; cells];
    let mut out: Vec<Rect> = Vec::new();
    let mut above = 0;
    let mut y = 0;
    while y < cur.h {
        let end = (y + CH).min(cur.h);
        mask.fill(false);
        let mut any = false;
        for row in y..end {
            let at = row as usize * w;
            let (a, b) = (&cur.px[at..at + w], &prev.px[at..at + w]);
            if a == b {
                continue;
            }
            any = true;
            for (i, (ca, cb)) in a.chunks(CW as usize).zip(b.chunks(CW as usize)).enumerate() {
                mask[i] |= ca != cb;
            }
        }
        let start = out.len();
        if any {
            let mut i = 0;
            while i < cells {
                if !mask[i] {
                    i += 1;
                    continue;
                }
                let first = i;
                let mut last = i;
                let mut j = i + 1;
                while j < cells && j <= last + GAP {
                    if mask[j] {
                        last = j;
                    }
                    j += 1;
                }
                let (x0, x1) = (first as i32 * CW, ((last + 1) as i32 * CW).min(cur.w));
                match out[above..start]
                    .iter_mut()
                    .find(|r| r.3 == y && r.0 == x0 && r.2 == x1)
                {
                    Some(r) => r.3 = end,
                    None => out.push((x0, y, x1, end)),
                }
                i = last + 1;
            }
        }
        above = start;
        y = end;
    }
    out
}

/// The palette for the darker scanlines.
pub fn scanline_palette(p: &Palette) -> [u32; 16] {
    p.0.map(|c| {
        let f = |shift: u32| ((((c >> shift) & 255) * 7 / 10) & 255) << shift;
        f(16) | f(8) | f(0)
    })
}

/// A canvas rectangle as a 32-bit image scaled by `place`; with `dim` the
/// last screen line of every canvas row is drawn in it (scanlines).
pub fn image(
    canvas: &Canvas,
    palette: &[u32; 16],
    dim: Option<&[u32; 16]>,
    place: &chunky::Placement,
    (x0, y0, x1, y1): Rect,
    big_endian: bool,
) -> (Vec<u8>, u16, u16) {
    let (x0, x1) = (x0.clamp(0, canvas.w), x1.clamp(0, canvas.w));
    let (y0, y1) = (y0.clamp(0, canvas.h), y1.clamp(0, canvas.h));
    let (sx, sy) = (place.sx.max(1) as usize, place.sy.max(1) as usize);
    let width = (x1 - x0).max(0) as usize * sx;
    let stride = width * 4;
    let rows = (y1 - y0).max(0) as usize;
    let mut out = vec![0u8; stride * rows * sy];
    let bytes = |c: u32| {
        if big_endian {
            c.to_be_bytes()
        } else {
            c.to_le_bytes()
        }
    };
    let light = palette.map(bytes);
    let dark = dim.filter(|_| sy >= 2).map(|d| d.map(bytes));
    for (k, y) in (y0..y1).enumerate() {
        let row = &canvas.px[(y * canvas.w) as usize..][x0 as usize..x1 as usize];
        let base = k * sy * stride;
        let (first, rest) = out[base..base + sy * stride].split_at_mut(stride);
        for (px, &p) in first.chunks_exact_mut(4 * sx).zip(row) {
            let c = &light[usize::from(p & 15)];
            for dst in px.chunks_exact_mut(4) {
                dst.copy_from_slice(c);
            }
        }
        for line in rest.chunks_exact_mut(stride) {
            line.copy_from_slice(first);
        }
        if let Some(dark) = &dark {
            let last = &mut out[base + (sy - 1) * stride..base + sy * stride];
            for (px, &p) in last.chunks_exact_mut(4 * sx).zip(row) {
                let c = &dark[usize::from(p & 15)];
                for dst in px.chunks_exact_mut(4) {
                    dst.copy_from_slice(c);
                }
            }
        }
    }
    let height = (rows * sy).min(usize::from(u16::MAX)) as u16;
    (out, width.min(usize::from(u16::MAX)) as u16, height)
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
    /// What is drawn, and what the screen shows.
    canvas: Option<Canvas>,
    sent: Option<Canvas>,
    shown: Option<Grid>,
    /// The ACCESS DENIED box was drawn last frame.
    boxed: bool,
    drawn: Drawn,
    /// The console was open last frame.
    opened: bool,
    seed: u64,
    /// When the lock started, and when the console opened.
    start: Option<Instant>,
    console: Option<Instant>,
    day0: u32,
    last_key: Option<Instant>,
    motd: Option<Motd>,
    quoted: bool,
    motd_round: u64,
    wrong: bool,
    denied: Option<Instant>,
    flash: f32,
    bass: f32,
    spectrum: Vec<f32>,
    last: Option<Instant>,
    next: Option<Instant>,
}

impl State {
    /// A key was pressed: the boot is skipped, and TX lights.
    pub fn key(&mut self, now: Instant) {
        self.last_key = Some(now);
        if self.console.is_none() {
            self.console = Some(now - Duration::from_secs_f32(REVEAL_END));
        }
    }
}

/// Seconds into the local day.
fn day_seconds() -> u32 {
    use chrono::Timelike;
    chrono::Local::now().num_seconds_from_midnight()
}

impl Lock {
    pub(crate) fn hacker_layout(&mut self, area: Area) -> bool {
        let screens: Vec<Screen> = if self.outputs.is_empty() {
            vec![area]
        } else {
            self.outputs.clone()
        }
        .into_iter()
        .map(arrange)
        .collect();
        let changed = screens != self.hacker.screens;
        self.hacker.screens = screens;
        changed
    }

    fn hacker_motd(&mut self, t: f32) {
        let round = (t / NEW_MOTD).max(0.0) as u64;
        let quiet = self.hacker.last_key.map_or(true, |k| k.elapsed() >= QUIET);
        if round != self.hacker.motd_round
            && quiet
            && self.input.is_empty()
            && !self.cfg.reduced_motion
        {
            self.hacker.motd_round = round;
            self.hacker.quoted = false;
        }
        if self.hacker.quoted {
            return;
        }
        if let Some(m) = self
            .visual
            .as_mut()
            .and_then(|v| bbs::pick_quote(v, MOTD_WIDTH, MOTD_LINES))
        {
            self.hacker.motd = Some(m);
            self.hacker.quoted = true;
        }
    }

    /// The line under the prompt: why the last attempt failed, PAM's
    /// message and Caps Lock.
    fn hacker_status(&self) -> (Vec<(String, u8)>, String) {
        let mut lines = Vec::new();
        let mut why = String::new();
        match (self.state, self.pam_message.as_ref()) {
            (crate::State::Wrong, Some(message)) => {
                lines.push((format!("ACCESS DENIED: {message}"), ink::ALERT));
                why = message.clone();
            }
            (crate::State::Wrong, None) => {
                why = if self.message == "wrong password" {
                    "INVALID PASSPHRASE".to_string()
                } else {
                    self.message.clone()
                };
                lines.push((format!("ACCESS DENIED: {why}"), ink::ALERT));
            }
            (crate::State::Prompt, Some(message)) => lines.push((message.clone(), ink::PLAIN)),
            _ => {}
        }
        if self.caps {
            lines.push(("CAPS LOCK IS ON".into(), ink::BRIGHT));
        }
        (lines, why)
    }

    /// Draw what changed into the backing pixmap (everything with `full`)
    /// and return the canvas rectangles sent.
    pub(crate) fn hacker_paint(&mut self, full: bool) -> Result<Vec<Rect>> {
        let now = Instant::now();
        let reduced = self.cfg.reduced_motion;
        let st = &mut self.hacker;
        let start = *st.start.get_or_insert(now);
        if st.seed == 0 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            st.seed = mix(nanos ^ u64::from(std::process::id())) | 1;
        }
        if st.console.is_none()
            && (reduced
                || !self.input.is_empty()
                || self.state != crate::State::Prompt
                || now.saturating_duration_since(start).as_secs_f32() >= BOOT_END)
        {
            st.console = Some(
                if reduced || now.saturating_duration_since(start).as_secs_f32() < BOOT_END {
                    now - Duration::from_secs_f32(REVEAL_END)
                } else {
                    start + Duration::from_secs_f32(BOOT_END)
                },
            );
        }
        let phase = match st.console {
            None => Phase::Boot(now.saturating_duration_since(start).as_secs_f32()),
            Some(_) if reduced => Phase::Console(STILL),
            Some(at) => Phase::Console(now.saturating_duration_since(at).as_secs_f32()),
        };
        if st.console.is_some() && st.day0 == 0 {
            st.day0 = day_seconds().max(1);
        }
        let wrong = self.state == crate::State::Wrong;
        if wrong && !st.wrong {
            st.denied = Some(now);
        }
        if !wrong {
            st.denied = None;
        }
        st.wrong = wrong;
        let t = match phase {
            Phase::Console(t) => t,
            Phase::Boot(_) => 0.0,
        };
        if st.console.is_some() {
            let real = st
                .console
                .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32());
            self.hacker_motd(real);
        }
        let st = &mut self.hacker;
        let dt = st.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        st.last = Some(now);
        let spectrum = if reduced {
            None
        } else {
            self.visual
                .as_mut()
                .and_then(crate::visual::Visual::spectrum)
                .map(<[f32]>::to_vec)
        };
        let following = spectrum.is_some();
        let bass = spectrum.as_ref().map_or(0.0, |values| {
            let low = &values[..values.len().min(6)];
            low.iter().sum::<f32>() / low.len().max(1) as f32
        });
        (st.flash, st.bass) = crate::demo::beat(st.flash, bass, st.bass, dt);
        st.spectrum = spectrum.unwrap_or_default();
        let local = chrono::Local::now();
        let date = local.format("%a %d %b %Y").to_string().to_uppercase();
        let time = local.format("%H:%M").to_string();
        let locked = bbs::hours_minutes(self.started.elapsed().as_secs());
        let chars = self.input.chars().count().min(crate::ENTRY_MAX_STARS);
        let entry = crate::echo(self.cfg.prompt_echo, chars, self.pulse);
        let (status, why) = self.hacker_status();
        let keyboard = self.keyboard.indicator().map(str::to_uppercase);
        let st = &mut self.hacker;
        let sent = st
            .last_key
            .is_some_and(|k| now.saturating_duration_since(k) < SENT);
        let version = env!("CARGO_PKG_VERSION");
        let facts = Facts {
            seed: st.seed,
            version,
            date: &date,
            time: &time,
            locked: &locked,
            day0: st.day0,
            motd: st.motd.as_ref(),
            tune: self.music_tune.as_deref(),
            entry: &entry,
            checking: self.state == crate::State::Checking,
            wrong,
            denied: st
                .denied
                .map(|d| now.saturating_duration_since(d).as_secs_f32()),
            why: &why,
            status: &status,
            failed: self.failed,
            keyboard: keyboard.as_deref(),
            caps: self.caps,
            reduced,
            sent,
            beat: st.flash > 0.5,
            spectrum: following.then_some(st.spectrum.as_slice()),
        };
        let grid = compose(phase, &facts);
        let boxed = denied_shown(&facts);
        let opened = matches!(phase, Phase::Console(_));
        let glitching = boxed && !reduced && facts.denied.is_some_and(|d| d < GLITCH);
        // The glitch moves cells over the pixel panes: all of it is drawn
        // again then.
        let all =
            full || glitching || boxed != st.boxed || opened != st.opened || st.shown.is_none();
        let canvas = st.canvas.get_or_insert_with(|| Canvas::new(W, H));
        draw(
            canvas,
            &grid,
            if all { None } else { st.shown.as_ref() },
            phase,
            &facts,
            &mut st.drawn,
        );
        st.shown = Some(grid);
        st.boxed = boxed;
        st.opened = opened;
        let rects = dirty(canvas, if full { None } else { st.sent.as_ref() });
        if let Some(sent) = st.sent.as_mut() {
            if !rects.is_empty() {
                sent.px.copy_from_slice(&canvas.px);
            }
        } else {
            st.sent = Some(canvas.clone());
        }
        let pal = &self.cfg.palette;
        let accent = crate::ascii::banner_colour(pal.focus, pal.foreground, pal.background);
        let palette = Palette::new(accent, pal.foreground, pal.muted, self.cfg.warning);
        let colours: [u32; 16] = palette.0;
        let dark = scanline_palette(&palette);
        let dim = (!reduced).then_some(&dark);
        let s = &self.surface;
        let big_endian = s.c.setup().image_byte_order == ImageOrder::MSB_FIRST;
        for &(x0, y0, x1, y1) in &rects {
            for screen in &st.screens {
                let Some(place) = screen.place else {
                    continue;
                };
                let line = ((x1 - x0) * place.sx * place.sy * 4).max(1) as usize;
                let step = (MAX_IMAGE_BYTES / line).max(1) as i32;
                let mut y = y0;
                while y < y1 {
                    let end = (y + step).min(y1);
                    let (bytes, w, h) =
                        image(canvas, &colours, dim, &place, (x0, y, x1, end), big_endian);
                    s.c.put_image(
                        ImageFormat::Z_PIXMAP,
                        s.back,
                        s.gc,
                        w,
                        h,
                        layout::clamp16(place.x + x0 * place.sx),
                        layout::clamp16(place.y + y * place.sy),
                        0,
                        s.depth,
                        &bytes,
                    )?;
                    y = end;
                }
            }
        }
        // When to look again.
        let mut wait: Option<Duration> = None;
        let mut sooner = |d: Duration| wait = Some(wait.map_or(d, |w| w.min(d)));
        let busy = match phase {
            Phase::Boot(_) => true,
            Phase::Console(t) => !reduced && t < REVEAL_END,
        };
        if busy || glitching {
            sooner(FRAME);
        }
        if following || st.flash > 0.01 {
            sooner(MUSIC);
        }
        if !reduced {
            sooner(Duration::from_secs_f32((TICK - t % TICK).max(0.01)));
        } else {
            // Only the clock changes: at the next minute.
            use chrono::Timelike;
            let into =
                f64::from(local.second()) + f64::from(local.nanosecond() % 1_000_000_000) / 1e9;
            sooner(Duration::from_secs_f64((60.0 - into).max(0.05)));
        }
        if let Some(k) = st.last_key {
            let lit = now.saturating_duration_since(k);
            if lit < SENT {
                sooner(SENT - lit);
            }
        }
        st.next = wait.map(|w| now + w);
        Ok(rects)
    }

    /// The whole screen, from `draw`.
    pub(crate) fn hacker_draw(&mut self) -> Result<()> {
        let area = self.prompt_area;
        let moved = self.hacker_layout(area);
        let full = moved || self.hacker.shown.is_none();
        if full {
            self.surface.clear(self.cfg.palette.background)?;
        }
        self.hacker_paint(full)?;
        self.surface.present()
    }

    pub(crate) fn hacker_tick(&mut self, display_on: bool) -> Result<()> {
        if !display_on || self.hacker.next.map_or(true, |due| Instant::now() < due) {
            return Ok(());
        }
        let rects = self.hacker_paint(false)?;
        for &(x0, y0, x1, y1) in &rects {
            for screen in &self.hacker.screens {
                let Some(place) = screen.place else {
                    continue;
                };
                // Copied to the window together, with one flush.
                let s = &self.surface;
                let (x, y) = (
                    layout::clamp16(place.x + x0 * place.sx),
                    layout::clamp16(place.y + y0 * place.sy),
                );
                s.c.copy_area(
                    s.back,
                    s.win,
                    s.gc,
                    x,
                    y,
                    x,
                    y,
                    ((x1 - x0) * place.sx).clamp(0, i32::from(u16::MAX)) as u16,
                    ((y1 - y0) * place.sy).clamp(0, i32::from(u16::MAX)) as u16,
                )?;
            }
        }
        self.surface.c.flush()?;
        Ok(())
    }

    pub(crate) fn hacker_timeout(&self, display_on: bool) -> Option<Duration> {
        if !display_on || self.cfg.layout != crate::config::Layout::Hacker {
            return None;
        }
        self.hacker
            .next
            .map(|due| due.saturating_duration_since(Instant::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts<'static> {
        Facts {
            seed: 0x5eed,
            version: "0.34.0",
            date: "THU 26 NOV 2026",
            time: "14:32",
            locked: "00:12",
            day0: 14 * 3600 + 32 * 60,
            ..Facts::default()
        }
    }

    fn text(g: &Grid) -> Vec<String> {
        (0..ROWS).map(|r| g.row_text(r)).collect()
    }

    #[test]
    fn the_boot_tells_its_story_in_order() {
        let at = |t: f32| text(&compose(Phase::Boot(t), &facts())).join("\n");
        assert!(at(0.0).contains("NIRUX INTRUSION CONSOLE  V0.34.0"));
        assert!(!at(0.0).contains("BOOT --SILENT"));
        assert!(at(0.5).contains("> BOOT"));
        assert!(at(1.2).contains("[ OK ] ICE BREAKER"));
        assert!(at(2.0).contains("SYN >>>"));
        let dump = at(3.3);
        assert!(dump.contains(&hex_line(0x5eed, 0x9000)));
        assert!(dump.contains(&hex_line(0x5eed, 0x9009)));
        let bar = at(3.75);
        assert!(bar.contains("DECRYPTING  [") && bar.contains("%"), "{bar}");
        assert!(!bar.contains("UPLINK"));
        let end = at(BOOT_END - 0.05);
        assert!(end.contains("UPLINK") && end.contains("100%"));
        assert!(end.contains("PASSPHRASE REQUIRED."));
        // Everything fits on the screen, the bars and all.
        let (lines, _) = boot_lines(BOOT_END, &facts());
        assert!(lines.len() <= ROWS - 2, "{}", lines.len());
        for (line, _) in &lines {
            assert!(codes(line).len() <= COLS - 4, "{line}");
        }
    }

    #[test]
    fn the_console_shows_the_facts_and_the_prompt() {
        let motd = Motd::quote_in(
            "The unwise man is awake all night, and ponders everything over",
            "Havamal",
            MOTD_WIDTH,
            MOTD_LINES,
        )
        .unwrap();
        let status = vec![("CAPS LOCK IS ON".to_string(), ink::BRIGHT)];
        let f = Facts {
            motd: Some(&motd),
            tune: Some("NIRUX - Gimle"),
            entry: "****",
            status: &status,
            caps: true,
            failed: 2,
            ..facts()
        };
        let g = compose(Phase::Console(14.0), &f);
        let rows = text(&g);
        assert!(rows[0].contains("NIRUX INTRUSION CONSOLE 0.34.0"));
        assert!(rows[0].trim_end().ends_with("THU 26 NOV 2026  14:32"));
        for title in [
            "MEMORY //",
            "TRACE //",
            "PACKET SNIFFER //",
            "CLOCK //",
            "INTERCEPT // MESSAGE OF THE DAY",
            "SIGNAL // AUDIO",
            "AUTHENTICATION //",
        ] {
            assert!(rows.iter().any(|r| r.contains(title)), "{title}");
        }
        assert!(rows[INTERCEPT.row + 1].contains("\"THE UNWISE MAN"));
        assert!(rows[INTERCEPT.row + INTERCEPT.h - 1].contains("- HAVAMAL"));
        assert!(rows[SIGNAL.row + SIGNAL.h - 2].contains("NOW PLAYING  NIRUX - GIMLE"));
        assert!(rows[CLOCK.row + 4].contains("THU 26 NOV 2026"));
        assert!(rows[CLOCK.row + 5].contains("SEALED 00:12"));
        let entry = &rows[ENTRY_ROW];
        assert!(entry.contains("ENTER PASSPHRASE: ****?"), "{entry}");
        assert!(entry.contains("2 FAILED ATTEMPTS"));
        assert!(rows[STATUS_ROW].contains("CAPS LOCK IS ON"));
        let bar = &rows[BAR];
        assert!(bar.contains("ICE ACTIVE") && bar.contains("SEALED 00:12"));
        let caps = bar.find("CAPS").unwrap();
        assert_eq!(g.get(caps, BAR).fg, ink::ALERT_BRIGHT);
        // A long tune moves NOW PLAYING to the title and is cut.
        let long = Facts {
            tune: Some("A Very Long Artist Name - An Even Longer Title Of A Tune"),
            ..facts()
        };
        let rows = text(&compose(Phase::Console(14.0), &long));
        assert!(rows[SIGNAL.row].contains("NOW PLAYING"));
        assert!(rows[SIGNAL.row + SIGNAL.h - 2].trim_end().ends_with("...?"));
        // Without music the carrier idles; checking hides the cursor.
        let f = Facts {
            entry: &"*".repeat(200),
            checking: true,
            ..facts()
        };
        let g = compose(Phase::Console(14.0), &f);
        let rows = text(&g);
        assert!(rows[SIGNAL.row + SIGNAL.h - 2].contains("NO AUDIO"));
        assert!(rows[STATUS_ROW].contains("VERIFYING PASSPHRASE..."));
        assert!(
            (0..COLS).all(|c| g.get(c, ENTRY_ROW).ch != 0xdb),
            "no cursor while checking"
        );
        assert!(
            rows[ENTRY_ROW].contains("0 FAILED ATTEMPTS"),
            "the entry stops short"
        );
    }

    #[test]
    fn a_failed_attempt_is_denied_with_a_glitch() {
        let status = vec![("ACCESS DENIED: INVALID PASSPHRASE".to_string(), ink::ALERT)];
        let wrong = |denied: f32, reduced: bool| Facts {
            wrong: true,
            denied: Some(denied),
            why: "INVALID PASSPHRASE",
            failed: 1,
            status: &status,
            reduced,
            ..facts()
        };
        let calm = compose(Phase::Console(14.0), &wrong(1.0, false));
        let rows = text(&calm);
        assert!(rows[DENIED.row + 4].contains("INVALID PASSPHRASE"));
        assert!(rows[DENIED.row + 5].contains("ATTEMPT 1 LOGGED"));
        assert!(rows[STATUS_ROW].contains("ACCESS DENIED: INVALID PASSPHRASE"));
        assert_eq!(calm.get(AUTH.col + 2, ENTRY_ROW).fg, ink::ALERT);
        assert_eq!(calm.get(DENIED.col, DENIED.row).ch, 0xc9);
        assert_eq!(calm.get(DENIED.col, DENIED.row).fg, ink::ALERT);
        // The glitch: noise in the warning colour, and the box flashes.
        let noise = |g: &Grid| {
            g.cells
                .iter()
                .filter(|c| [0xb0, 0xb1, 0xb2, 0xdb].contains(&c.ch) && c.fg == ink::ALERT)
                .count()
        };
        let glitch = compose(Phase::Console(14.0), &wrong(0.01, false));
        assert!(noise(&glitch) > 10, "{}", noise(&glitch));
        assert_eq!(glitch.get(DENIED.col + 1, DENIED.row + 1).bg, ink::ALERT);
        assert!(noise(&calm) < 3);
        // Not with reduced motion: the box only.
        let still = compose(Phase::Console(STILL), &wrong(0.01, true));
        assert!(noise(&still) < 3);
        assert_eq!(still.get(DENIED.col + 1, DENIED.row + 1).bg, ink::BLACK);
        // The pixel letters are drawn over the box, the panes under it.
        let mut c = Canvas::new(W, H);
        let f = wrong(1.0, false);
        let g = compose(Phase::Console(14.0), &f);
        draw(
            &mut c,
            &g,
            None,
            Phase::Console(14.0),
            &f,
            &mut Drawn::default(),
        );
        let inside = (DENIED.row as i32 + 1) * CH..(DENIED.row as i32 + 3) * CH;
        let lit = inside
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| c.get(x, y) == slot::WARNING)
            .count();
        assert!(lit > 500, "{lit}");
    }

    #[test]
    fn nothing_private_and_only_made_up_names() {
        let words: std::collections::HashSet<&str> = [
            "YGGDRASIL",
            "ASGARD",
            "ALFHEIM",
            "VANAHEIM",
            "BIFROST",
            "MIDGARD",
            "JOTUNHEIM",
            "NIDAVELLIR",
            "NIFLHEIM",
            "MUSPELHEIM",
            "HEL",
        ]
        .into();
        for (name, ..) in NODES {
            assert!(words.contains(name), "{name}");
        }
        let mut all = String::new();
        for t in [0.5, 1.5, 2.5, 3.5, 4.5] {
            all += &text(&compose(Phase::Boot(t), &facts())).join("\n");
        }
        for t in [3.0, 14.0, 40.0, 300.0, 4000.0] {
            all += &text(&compose(Phase::Console(t), &facts())).join("\n");
        }
        for j in -200..200 {
            if let Some(p) = packet(7, j) {
                all += &packet_line(&p, 0);
                all.push('\n');
            }
        }
        let user = std::env::var("USER").unwrap_or_default().to_uppercase();
        let host = std::fs::read_to_string("/etc/hostname")
            .unwrap_or_default()
            .trim()
            .to_uppercase();
        let tokens: Vec<&str> = all.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        for private in [user, host] {
            if !private.is_empty() && !words.contains(private.as_str()) {
                assert!(!tokens.contains(&private.as_str()), "{private}");
            }
        }
        // No address: never four numbers joined by dots.
        for line in all.lines() {
            let parts: Vec<&str> = line
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .collect();
            for p in parts {
                let n = p
                    .split('.')
                    .filter(|s| !s.is_empty() && s.len() <= 3)
                    .count();
                assert!(n < 4 || p.contains(".."), "{line}");
            }
        }
    }

    #[test]
    fn the_made_up_data_fits_its_panes() {
        let (c0, _, c1, _) = MEMORY.inner();
        for k in 0..500 {
            let line = hex_line(3, k);
            assert_eq!(line.len(), 39);
            assert!(line.len() < c1 + 1 - c0);
        }
        assert!((0..400).any(|k| hex_line(3, k).contains("NIRUX")));
        let (c0, _, c1, _) = SNIFFER.inner();
        let mut seen = 0;
        for j in -500..500 {
            if let Some(p) = packet(11, j) {
                seen += 1;
                let line = packet_line(&p, 86_399);
                assert!(line.len() <= c1 + 1 - c0, "{line}");
                assert!(!line.contains("TLS") || line.contains(" TLS "), "{line}");
                assert_ne!(p.src.split(':').next(), p.dst.split(':').next());
            }
        }
        assert!(
            (250..420).contains(&seen),
            "about one slot in three: {seen}"
        );
        let list = packets(11, 40, 5);
        assert_eq!(list.len(), 5);
        assert!(list.windows(2).all(|w| w[0].slot < w[1].slot));
        assert_eq!(clock_of(-1), "23:59:59");
        for r in 0..300 {
            let path = route(5, r);
            assert!((2..=6).contains(&path.len()), "{path:?}");
            for pair in path.windows(2) {
                assert!(linked(pair[0], pair[1]));
            }
            let mut sorted = path.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), path.len());
            assert!(path.len() as f32 * HOP < ROUTE - 4.0);
        }
        let path = [0, 1, 2];
        assert_eq!(trace_at(&path, 0.0), (0, Some(0.0)));
        assert_eq!(trace_at(&path, HOP * 1.5), (1, Some(0.5)));
        assert_eq!(trace_at(&path, HOP * 2.0), (2, None));
        // Every node is drawn inside the globe's part of the pane.
        let (x, y, w, h) = globe_rect();
        for (name, nx, ny, lx, ly) in NODES {
            let (px_, py) = (x + w / 2 + nx, y + h / 2 + ny);
            assert!(
                px_ > x + 2 && px_ < x + w - 2 && py > y + 2 && py < y + h - 2,
                "{name}"
            );
            let _ = (lx, ly);
        }
        let m = Motd::cut_in(&"word ".repeat(80), "x", MOTD_WIDTH, MOTD_LINES).unwrap();
        assert!(m.lines.len() <= MOTD_LINES);
        assert!(m.lines.iter().all(|l| l.len() <= MOTD_WIDTH));
    }

    #[test]
    fn the_panes_tile_the_screen() {
        let mut used = vec![false; COLS * ROWS];
        for p in PANES {
            assert!(p.col + p.w <= COLS && p.row + p.h <= BAR, "{p:?}");
            for r in p.row..p.row + p.h {
                for c in p.col..p.col + p.w {
                    assert!(!used[r * COLS + c], "{p:?} overlaps at {c},{r}");
                    used[r * COLS + c] = true;
                }
            }
        }
        // Rows 1 to 29 are all panes.
        assert!(used[COLS..BAR * COLS].iter().all(|&u| u));
    }

    #[test]
    fn the_console_takes_the_theme_colours() {
        // Forest Vigil's green accent gives green phosphor, Satie sepia,
        // and Noir grey.
        let rgb = |c: u32| ((c >> 16) & 255, (c >> 8) & 255, c & 255);
        let text = |accent: u32| {
            Palette::new(accent, 0xe0e0e0, 0x909090, crate::config::DEFAULT_WARNING).0
                [usize::from(bbs::ANSI[usize::from(ink::TEXT)])]
        };
        assert_eq!(text(0x8aa87e), 0x8aa87e);
        let (r, g, b) = rgb(text(0x8aa87e));
        assert!(g > r && g > b);
        let (r, g, b) = rgb(text(0xb08d7d));
        assert!(r > g && g > b);
        let (r, g, b) = rgb(text(0xe5e5e5));
        assert!(r == g && g == b);
        // The roles map to the theme's slots.
        assert_eq!(bbs::ANSI[usize::from(ink::FRAME)], slot::ACCENT_DARK);
        assert_eq!(bbs::ANSI[usize::from(ink::BRIGHT)], slot::ACCENT_LIGHT);
        assert_eq!(bbs::ANSI[usize::from(ink::ALERT)], slot::WARNING);
    }

    #[test]
    fn only_what_changed_is_sent() {
        let f = facts();
        let mut a = Canvas::new(W, H);
        let g = compose(Phase::Console(14.0), &f);
        draw(
            &mut a,
            &g,
            None,
            Phase::Console(14.0),
            &f,
            &mut Drawn::default(),
        );
        assert_eq!(dirty(&a, None), vec![(0, 0, W, H)]);
        assert!(dirty(&a, Some(&a)).is_empty());
        // The same moment drawn again over itself changes nothing.
        let mut b = a.clone();
        draw(
            &mut b,
            &g,
            Some(&g),
            Phase::Console(14.0),
            &f,
            &mut Drawn::default(),
        );
        assert!(dirty(&b, Some(&a)).is_empty());
        // One pixel: one band of one cell row, one pixel wide.
        let mut c = a.clone();
        c.set(100, 40, (a.get(100, 40) + 1) & 15);
        assert_eq!(dirty(&c, Some(&a)), vec![(96, 32, 104, 48)]);
        // A quarter of a second later only parts of the screen change.
        let t = 14.0 + TICK;
        let g2 = compose(Phase::Console(t), &f);
        let mut d = a.clone();
        draw(
            &mut d,
            &g2,
            Some(&g),
            Phase::Console(t),
            &f,
            &mut Drawn::default(),
        );
        let area: i32 = dirty(&d, Some(&a))
            .iter()
            .map(|r| (r.2 - r.0) * (r.3 - r.1))
            .sum();
        assert!(area > 0 && area < W * H / 3, "{area}");
        // Reduced motion: the same picture whenever it is drawn.
        let still = Facts {
            reduced: true,
            ..facts()
        };
        let one = compose(Phase::Console(STILL), &still);
        let mut x = Canvas::new(W, H);
        let mut y = Canvas::new(W, H);
        draw(
            &mut x,
            &one,
            None,
            Phase::Console(STILL),
            &still,
            &mut Drawn::default(),
        );
        draw(
            &mut y,
            &one,
            None,
            Phase::Console(STILL),
            &still,
            &mut Drawn::default(),
        );
        assert_eq!(x, y);
        assert!(!text(&one)[ENTRY_ROW].is_empty());
    }

    #[test]
    fn scanlines_darken_every_second_line() {
        let pal = Palette::new(0x8aa87e, 0xe0e0e0, 0x909090, 0xd7b36a);
        let dark = scanline_palette(&pal);
        assert_eq!(dark[0], 0);
        let mut c = Canvas::new(4, 2);
        c.clear(slot::ACCENT);
        let place = chunky::Placement {
            x: 0,
            y: 0,
            sx: 2,
            sy: 2,
        };
        let (bytes, w, h) = image(&c, &pal.0, Some(&dark), &place, (1, 0, 3, 2), false);
        assert_eq!((w, h), (4, 4));
        let px = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(px(0), pal.0[usize::from(slot::ACCENT)]);
        assert_eq!(px(4), dark[usize::from(slot::ACCENT)]);
        assert!(px(4) & 0xff00 < px(0) & 0xff00);
        // At scale 1, or without them, every line is the same.
        let one = chunky::Placement {
            sx: 1,
            sy: 1,
            ..place
        };
        let (bytes, ..) = image(&c, &pal.0, Some(&dark), &one, (0, 0, 4, 2), false);
        assert!(bytes.chunks(4).all(|p| p == pal.0[3].to_le_bytes()));
    }

    #[test]
    fn the_screen_fits_every_monitor() {
        for (w, h, scale) in [
            (1920, 1080, 2),
            (2560, 1440, 2),
            (3840, 2160, 4),
            (1366, 768, 1),
            (1920, 1200, 2),
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

    /// Pictures of the screen for looking at, at twice the size with the
    /// scanlines:
    /// `HACKER_PREVIEW=dir cargo test --release hacker::tests::preview -- --ignored`.
    #[test]
    #[ignore]
    fn preview() {
        let Some(dir) = std::env::var_os("HACKER_PREVIEW") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        // (name, foreground, muted, background, focus) as the suite themes.
        let themes = [
            ("forest", 0xd8ded2, 0x96a292, 0x2b362c, 0x8aa87e),
            ("nord", 0xeceff4, 0x9aa5b1, 0x3b4252, 0x88c0d0),
            ("satie", 0xdedad4, 0x8f8b86, 0x2a2724, 0xb08d7d),
            ("noir", 0xe5e5e5, 0xa8a8a8, 0x2a2a2d, 0x5a5a60),
        ];
        let motd = Motd::quote_in(
            "Cattle die, kinsmen die, you yourself will also die; but the fair fame never dies of the one who earns it",
            "Havamal 77 (tr. Henry Adams Bellows",
            MOTD_WIDTH,
            MOTD_LINES,
        )
        .unwrap();
        let status = vec![("ACCESS DENIED: INVALID PASSPHRASE".to_string(), ink::ALERT)];
        let spectrum: Vec<f32> = (0..40)
            .map(|i| (0.9 - i as f32 * 0.02) * (0.5 + 0.5 * (i as f32 * 0.7).sin().abs()))
            .collect();
        for (name, fg, muted, bg, focus) in themes {
            let accent = crate::ascii::banner_colour(focus, fg, bg);
            let pal = Palette::new(accent, fg, muted, crate::config::DEFAULT_WARNING);
            let dark = scanline_palette(&pal);
            let shots = [
                ("boot", Phase::Boot(2.4)),
                ("boot-late", Phase::Boot(4.95)),
                ("open", Phase::Console(0.3)),
                ("console", Phase::Console(14.0)),
                ("music", Phase::Console(21.5)),
                ("wrong", Phase::Console(30.0)),
                ("glitch", Phase::Console(30.0)),
            ];
            for (shot, phase) in shots {
                let f = Facts {
                    motd: Some(&motd),
                    tune: (shot == "music").then_some("Jeroen Tel - Cybernoid II"),
                    spectrum: (shot == "music").then_some(spectrum.as_slice()),
                    beat: shot == "music",
                    entry: "******",
                    wrong: shot == "wrong" || shot == "glitch",
                    denied: match shot {
                        "wrong" => Some(1.0),
                        "glitch" => Some(0.1),
                        _ => None,
                    },
                    failed: u32::from(shot == "wrong" || shot == "glitch"),
                    status: if shot == "wrong" { &status } else { &[] },
                    ..facts()
                };
                let mut c = Canvas::new(W, H);
                let g = compose(phase, &f);
                draw(&mut c, &g, None, phase, &f, &mut Drawn::default());
                let place = chunky::Placement {
                    x: 0,
                    y: 0,
                    sx: 2,
                    sy: 2,
                };
                let (bytes, w, h) = image(&c, &pal.0, Some(&dark), &place, (0, 0, W, H), true);
                let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
                for px in bytes.chunks(4) {
                    out.extend_from_slice(&px[1..4]);
                }
                std::fs::write(dir.join(format!("{name}-{shot}.ppm")), out).unwrap();
            }
        }
    }
}
