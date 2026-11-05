//! Nirulock: the Niru Noir lock screen.
//!
//! Shows the same clock and hostname as NIRUSCREENSAVER, plus how long the
//! machine has been away and a prompt. The screen stays covered until the
//! password is accepted by PAM.
//!
//! Security properties, in order of importance:
//!   1. An opaque override-redirect surface follows root geometry, with stacking
//!      repairs for ordinary session windows. The prompt follows one output.
//!   2. Every key press and button press on the screen is grabbed, so focus
//!      cannot be moved to another window by clicking.
//!   3. PAM decides; a missing PAM, a failed conversation or an unexpected
//!      prompt all fail closed.

mod cava;
mod config;
mod core;
mod display;
mod keyboard;
mod palette;
mod pam;
mod secret;

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! from_display {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self { Error(e.to_string()) }
        }
    )*};
}
from_display!(
    String,
    std::io::Error,
    x11rb::errors::ConnectError,
    x11rb::errors::ConnectionError,
    x11rb::errors::ReplyOrIdError,
    x11rb::errors::ReplyError
);

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

use chrono::Local;
use std::{
    io::Write,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::mpsc,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{randr::ConnectionExt as _, xproto::*, Event},
    wrapper::ConnectionExt as _,
};

/// How long a failure is shown in the warning colour.
const WRONG_FEEDBACK: Duration = Duration::from_secs(2);

/// "1 failed attempt", "2 failed attempts".
fn failed_text(n: u32) -> String {
    if n == 1 {
        "1 failed attempt".to_string()
    } else {
        format!("{n} failed attempts")
    }
}

/// Width of the `pulse` echo.
const PULSE_CELLS: usize = 8;

/// The entry as shown on the prompt line.
///
/// `count` shows one `*` per character. `pulse` shows a fixed row of eight
/// cells with one lit `*` that moves to a different cell on every key, so
/// neither the width nor the position reveals the length; only an empty
/// entry is distinguishable (no row at all).
fn echo(mode: config::Echo, chars: usize, pulse: usize) -> String {
    match mode {
        config::Echo::Count => "*".repeat(chars),
        config::Echo::Pulse if chars == 0 => String::new(),
        config::Echo::Pulse => (0..PULSE_CELLS)
            .map(|cell| {
                if cell == pulse % PULSE_CELLS {
                    '*'
                } else {
                    '.'
                }
            })
            .collect(),
    }
}

/// A different, random cell. xorshift64: not cryptographic, only
/// unpredictable enough to look at.
fn next_pulse(pulse: usize, rng: &mut u64) -> usize {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    let step = 1 + (*rng % (PULSE_CELLS as u64 - 1)) as usize;
    (pulse + step) % PULSE_CELLS
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (nanos ^ (u64::from(std::process::id()) << 32)) | 1
}

/// Text shown while a password is being checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Prompt,
    Checking,
    Wrong,
}

struct Authentication {
    result: mpsc::Receiver<std::result::Result<pam::Outcome, String>>,
    wake: UnixStream,
    worker: std::thread::JoinHandle<()>,
}

impl Authentication {
    /// The password moves into the worker in its zeroizing buffer and is
    /// wiped when the worker drops it.
    fn start(user: String, password: zeroize::Zeroizing<String>) -> Result<Self> {
        let (wake, mut notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let (send, result) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("nirulock-auth".into())
            .spawn(move || {
                let outcome = pam::authenticate(&user, &password);
                drop(password);
                let _ = send.send(outcome);
                let _ = notify.write_all(&[1]);
            })?;
        Ok(Self {
            result,
            wake,
            worker,
        })
    }
}

struct Lock {
    surface: core::Surface,
    cfg: config::Config,
    renderer: core::TextRenderer,
    clock_renderer: core::TextRenderer,
    rune_renderer: Option<core::TextRenderer>,
    cava: Option<cava::Cava>,
    /// Display power, followed only while a spectrum is shown.
    power: Option<cava::DisplayPower>,
    state: State,
    input: zeroize::Zeroizing<String>,
    started: Instant,
    message: String,
    wm_notified: bool,
    authentication: Option<Authentication>,
    wrong_until: Option<Instant>,
    prompt_output: Option<u32>,
    prompt_area: display::Area,
    spectrum_rect: Option<(i16, i16, u16, u16)>,
    ready: bool,
    caps: bool,
    keyboard: keyboard::Keyboard,
    user: String,
    /// What PAM said during the last failed attempt (sanitized, never logged).
    pam_message: Option<String>,
    /// Failed attempts since locking.
    failed: u32,
    /// The `pulse` echo's lit cell and its random state.
    pulse: usize,
    rng: u64,
}

impl Drop for Lock {
    fn drop(&mut self) {
        if self.wm_notified {
            let _ = self.tell_wm_locked(false);
        }
        if self.ready {
            let _ = self.publish_ready(false);
        }
    }
}

impl Lock {
    fn new() -> Result<Self> {
        // Resolve the account before anything is grabbed or shown: a lock
        // that cannot name its owner must not start at all.
        let user = pam::current_user().map_err(|e| format!("refusing to lock: {e}"))?;
        let cfg = config::Config::load()?;
        let surface = core::Surface::new("NIRULOCK")?;
        let font = core::find_font(cfg.font_path.as_deref());
        if font.is_empty() {
            return Err("no monospace font found; set font_path in the config".into());
        }
        let setup = surface.c.setup().clone();
        let renderer = core::TextRenderer::new(&font, cfg.font_size, &setup)?;
        let clock_renderer = renderer.at_size((cfg.font_size * 2.4).max(cfg.font_size + 8.0));
        let cava = if cfg.layout == config::Layout::Cava && cfg.visualizer {
            Some(cava::Cava::new())
        } else {
            None
        };
        let power = cava.as_ref().map(|_| cava::DisplayPower::new(&surface.c));
        let rune_renderer = core::find_rune_font(cfg.rune_font_path.as_deref())
            .map(|font| core::TextRenderer::new(&font, cfg.font_size * 2.0, &setup))
            .transpose()?;
        let prompt_area = display::Area {
            x: 0,
            y: 0,
            width: surface.width,
            height: surface.height,
        };
        let caps = {
            let root = surface.c.setup().roots[surface.screen_num].root;
            surface
                .c
                .query_pointer(root)
                .ok()
                .and_then(|r| r.reply().ok())
                .map(|r| r.mask.contains(KeyButMask::LOCK))
                .unwrap_or(false)
        };
        let keyboard = keyboard::Keyboard::new(&surface.c);
        let mut lock = Self {
            surface,
            cfg,
            renderer,
            clock_renderer,
            rune_renderer,
            cava,
            power,
            state: State::Prompt,
            input: secret::new_entry(),
            started: Instant::now(),
            message: String::new(),
            wm_notified: false,
            authentication: None,
            wrong_until: None,
            prompt_output: None,
            prompt_area,
            spectrum_rect: None,
            ready: false,
            caps,
            keyboard,
            user,
            pam_message: None,
            failed: 0,
            pulse: 0,
            rng: seed(),
        };
        lock.trace("starting");
        lock.trace(&format!("keyboard: {}", lock.keyboard.backend()));
        if let Some(power) = lock.power.as_ref() {
            lock.trace(&format!("display power: {}", power.mode()));
            let on = power.on();
            if let Some(cava) = lock.cava.as_mut() {
                cava.set_display(on);
            }
        }
        // Ask the WM and suite overlays to yield their active grabs before the
        // lock takes over. The WM can release its own drag/keyboard grabs; the
        // launcher must close itself because its input grab belongs to another
        // X client and cannot be released by the locker.
        lock.tell_wm_locked(true)?;
        lock.wm_notified = true;
        lock.release_overlays()?;
        lock.grab_pointer()?;
        // Above everything from the first frame: the bar and any
        // override-redirect window are already on screen when a lock starts.
        lock.grab_input()?;
        lock.watch_stacking()?;
        lock.keep_on_top()?;
        lock.update_prompt_area();
        lock.draw()?;
        // Readiness is an explicit property, not a guess from a child pid.
        // `niructl` uses it before suspending or reporting a lock as active.
        lock.publish_ready(true)?;
        lock.ready = true;
        Ok(lock)
    }

    /// Publish or clear the lock-ready marker on the root window.
    ///
    /// The value is the lock window id. A caller must verify that the window
    /// still exists: a killed locker leaves the property behind when its X
    /// connection closes, but its window does not.
    fn publish_ready(&self, ready: bool) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let atom = self
            .surface
            .c
            .intern_atom(false, b"_NIRU_LOCK_READY")?
            .reply()?
            .atom;
        if ready {
            self.surface.c.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                root,
                atom,
                x11rb::protocol::xproto::AtomEnum::WINDOW,
                &[self.surface.win],
            )?;
        } else {
            let _ = self.surface.c.delete_property(root, atom);
        }
        self.surface.c.flush()?;
        Ok(())
    }

    /// Ask to be told when the stacking order changes.
    ///
    /// The lock window is raised above everything; if something raises itself
    /// over it, this brings the lock back. A lock screen that can be covered is
    /// not a lock screen, and the keyboard grab alone would hide the problem:
    /// the keys would still be caught while the desktop stayed readable.
    fn watch_stacking(&self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        self.surface.c.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_NOTIFY | EventMask::STRUCTURE_NOTIFY),
        )?;
        // Root ConfigureNotify remains the fallback on servers without RandR.
        let _ = self
            .surface
            .c
            .randr_select_input(
                root,
                x11rb::protocol::randr::NotifyMask::SCREEN_CHANGE
                    | x11rb::protocol::randr::NotifyMask::CRTC_CHANGE
                    | x11rb::protocol::randr::NotifyMask::OUTPUT_CHANGE
                    | x11rb::protocol::randr::NotifyMask::RESOURCE_CHANGE,
            )
            .ok()
            .and_then(|r| r.check().ok());
        self.surface.c.flush()?;
        self.trace("watching the stacking order");
        Ok(())
    }

    /// Grab the keyboard and pointer so input cannot reach other windows.
    ///
    /// `OwnerGrabButton` keeps the buttons grabbed after a key press; without
    /// it X releases the pointer grab and a click could focus another window.
    /// Tell NIRUWM whether a lock screen owns the keyboard.
    ///
    /// The message goes to the WM's check window, the same way `niructl`
    /// addresses commands. A missing WM is not an error: the grabs simply stay
    /// as they are.
    /// Write a line to the diagnostic log.
    ///
    /// A lock screen that misbehaves gives no other evidence: it covers the
    /// screen, so nobody can read a terminal. The log is the only way to see
    /// what happened afterwards.
    ///
    /// It is always on. The earlier version only logged when
    /// `NIRULOCK_TRACE=1` was set in the session that started the lock, which a
    /// key press started by the window manager never inherited, so failures
    /// left no trace at all. Lines are small and the file is trimmed.
    fn trace(&self, message: &str) {
        let dir = std::env::var("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(".local/state")
            })
            .join("nirulock");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("trace.log");
        // Keep the last few hundred lines: enough for a post mortem, small
        // enough to leave in place for months.
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 256 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
        // Older releases logged raw keyboard codes. Remove that legacy log
        // rather than append lifecycle diagnostics to password-derived data.
        if std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.lines().any(|l| l.contains("key code ")))
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{} {}", chrono::Local::now().format("%H:%M:%S"), message);
        }
    }

    fn tell_wm_locked(&self, locked: bool) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let atom = |n: &str| -> Result<x11rb::protocol::xproto::Atom> {
            Ok(self
                .surface
                .c
                .intern_atom(false, n.as_bytes())?
                .reply()?
                .atom)
        };
        let check = self
            .surface
            .c
            .get_property(
                false,
                root,
                atom("_NET_SUPPORTING_WM_CHECK")?,
                x11rb::protocol::xproto::AtomEnum::ANY,
                0,
                1,
            )?
            .reply()?
            .value32()
            .and_then(|mut v| v.next());
        let Some(wm) = check else {
            self.trace("no window manager to notify");
            return Ok(());
        };
        self.trace(&format!("telling wm {wm:#x} locked={locked}"));
        self.surface.c.send_event(
            false,
            wm,
            x11rb::protocol::xproto::EventMask::NO_EVENT,
            x11rb::protocol::xproto::ClientMessageEvent::new(
                32,
                wm,
                atom("_NIRUWM_LOCK")?,
                [u32::from(locked), 0, 0, 0, 0],
            ),
        )?;
        self.surface.c.sync()?;
        self.trace("wm notified");
        Ok(())
    }

    /// Ask suite overlays that own input grabs to close before locking.
    ///
    /// NIRULOCK cannot ungrab another client's active pointer grab. The launcher
    /// owns one while open, so it receives a bounded close request and the
    /// selection owner is watched until the overlay is gone.
    fn release_overlays(&self) -> Result<()> {
        let c = &self.surface.c;
        let selection = c
            .intern_atom(false, b"_NIRULANCHER_SELECTION")?
            .reply()?
            .atom;
        let request = c.intern_atom(false, b"_NIRULANCHER_LOCK")?.reply()?.atom;
        let owner = c.get_selection_owner(selection)?.reply()?.owner;
        if owner == x11rb::NONE {
            return Ok(());
        }
        self.trace(&format!("requesting launcher {owner:#x} to yield input"));
        c.send_event(
            false,
            owner,
            EventMask::NO_EVENT,
            x11rb::protocol::xproto::ClientMessageEvent::new(32, owner, request, [1, 0, 0, 0, 0]),
        )?;
        c.flush()?;
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if c.get_selection_owner(selection)?.reply()?.owner == x11rb::NONE {
                self.trace("launcher released its input grab");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err("the launcher did not release its input grab; lock was not established".into())
    }

    fn grab_pointer(&self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let deadline = Instant::now() + Duration::from_millis(800);
        loop {
            let reply = self
                .surface
                .c
                .grab_pointer(
                    true,
                    root,
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )?
                .reply()?;
            if reply.status == GrabStatus::SUCCESS {
                self.surface.c.flush()?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("cannot grab pointer: {:?}", reply.status).into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Acquire a verified active keyboard grab after notifying the WM.
    fn grab_input(&self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        // Give a WM handling the release request time to let go. A queued
        // request is not a successful grab: inspect the actual server reply.
        for _ in 0..100 {
            let reply = self
                .surface
                .c
                .grab_keyboard(
                    false,
                    root,
                    x11rb::CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )?
                .reply()?;
            if reply.status == GrabStatus::SUCCESS {
                self.trace("input grabbed");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.tell_wm_locked(false);
        Err("cannot acquire keyboard; lock was not established".into())
    }

    /// Raise the lock window over everything and make it full screen again.
    ///
    /// Also used once at start, so the window is certainly above the bar and
    /// any override-redirect window that was there before.
    ///
    /// Returns true when the surface was resized and must be redrawn. A
    /// restack needs no redraw of its own: whatever was covered arrives as an
    /// Expose event.
    fn keep_on_top(&mut self) -> Result<bool> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let geometry = self.surface.c.get_geometry(root)?.reply()?;
        let resized = self.surface.resize(geometry.width, geometry.height)?;
        self.surface.c.map_window(self.surface.win)?;
        // Only change the stack if necessary: reconfiguring in response to our
        // own ConfigureNotify otherwise creates a self-triggered event storm.
        let children = self.surface.c.query_tree(root)?.reply()?.children;
        if children.last() != Some(&self.surface.win) {
            self.surface.c.configure_window(
                self.surface.win,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
        }
        self.surface.c.flush()?;
        Ok(resized)
    }

    fn update_prompt_area(&mut self) {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let fallback = display::Area {
            x: 0,
            y: 0,
            width: self.surface.width,
            height: self.surface.height,
        };
        (self.prompt_output, self.prompt_area) =
            display::prompt_area(&self.surface.c, root, self.prompt_output, fallback);
    }

    fn draw(&mut self) -> Result<()> {
        let pal = self.cfg.palette.clone();
        self.surface.clear(pal.background)?;
        self.spectrum_rect = None;
        let spectrum = self.cava.as_mut().map(|cava| cava.values().to_vec());
        let area = self.prompt_area;
        let (centre, mid) = area.centre();
        let line = self.renderer.line_height();
        let hostname = core::hostname();
        let clock = core::clock_text(Local::now());
        let away = self.started.elapsed().as_secs();
        // The entry stays usable during checking and failure feedback; the
        // reason for a failure is shown on its own line below.
        let (label, colour) = match self.state {
            State::Prompt | State::Wrong => ("password: ", pal.foreground),
            State::Checking => ("checking... ", pal.muted),
        };
        let dots = echo(self.cfg.prompt_echo, self.input.chars().count(), self.pulse);
        let extras = self.status_lines(&pal);
        let step = line.saturating_mul(3) / 2;
        // The active layout next to the prompt when several are configured.
        let password = match (self.state, self.keyboard.indicator()) {
            (State::Checking, _) | (_, None) => format!("{label}{dots}"),
            (_, Some(layout)) => format!("[{layout}] {label}{dots}"),
        };
        let width = i32::from(area.width);
        let height = i32::from(area.height);
        let left = |offset: i32| {
            (i32::from(area.x) + offset).clamp(i16::MIN as i32, i16::MAX as i32) as i16
        };
        let top = |offset: i32| {
            (i32::from(area.y) + offset).clamp(i16::MIN as i32, i16::MAX as i32) as i16
        };
        let right = |offset: i32| left(width - offset);
        let draw = |renderer: &mut core::TextRenderer,
                    surface: &mut core::Surface,
                    baseline: i16,
                    text: &str,
                    fg: u32|
         -> Result<()> {
            renderer.draw_centred(
                &surface.c,
                surface.gc,
                surface.back,
                surface.depth,
                centre,
                baseline,
                text,
                fg,
                pal.background,
            )
        };
        match self.cfg.layout {
            config::Layout::Minimal => {
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid - line * 4,
                    &clock,
                    pal.foreground,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid - line * 2,
                    &hostname,
                    pal.muted,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid,
                    "locked",
                    pal.focus,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid + line * 2,
                    &core::away_text(away),
                    pal.border.max(pal.focus),
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid + line * 4,
                    &password,
                    colour,
                )?;
                for (index, (text, fg)) in extras.iter().enumerate() {
                    draw(
                        &mut self.renderer,
                        &mut self.surface,
                        (mid + line * 6).saturating_add(step.saturating_mul(index as i16)),
                        text,
                        *fg,
                    )?;
                }
            }
            config::Layout::Terminal => {
                let x = left((width / 10).max(24));
                let start = top((height / 3).max(i32::from(line) * 4));
                let line_at = |index: i32| start.saturating_add(line.saturating_mul(index as i16));
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    x,
                    line_at(0),
                    &format!("{hostname}:~$ lock"),
                    pal.muted,
                    pal.background,
                )?;
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    x,
                    line_at(2),
                    &clock,
                    pal.foreground,
                    pal.background,
                )?;
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    x,
                    line_at(3),
                    &core::away_text(away),
                    pal.border.max(pal.focus),
                    pal.background,
                )?;
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    x,
                    line_at(5),
                    "[ locked ]",
                    pal.focus,
                    pal.background,
                )?;
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    x,
                    line_at(7),
                    &password,
                    colour,
                    pal.background,
                )?;
                for (index, (text, fg)) in extras.iter().enumerate() {
                    self.renderer.draw_left(
                        &self.surface.c,
                        self.surface.gc,
                        self.surface.back,
                        self.surface.depth,
                        x,
                        line_at(9 + index as i32),
                        text,
                        *fg,
                        pal.background,
                    )?;
                }
            }
            config::Layout::Runic => {
                if let Some(rune) = self.rune_renderer.as_mut() {
                    rune.draw_centred(
                        &self.surface.c,
                        self.surface.gc,
                        self.surface.back,
                        self.surface.depth,
                        centre,
                        mid - line * 6,
                        "ᚠ  ᚢ  ᚦ  ᚨ  ᚱ  ᚲ  ᚷ  ᚹ",
                        pal.border.max(pal.focus),
                        pal.background,
                    )?;
                }
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid - line * 4,
                    &clock,
                    pal.foreground,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid - line * 2,
                    &hostname,
                    pal.muted,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid,
                    "locked",
                    pal.focus,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid + line * 2,
                    &core::away_text(away),
                    pal.border.max(pal.focus),
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid + line * 4,
                    &password,
                    colour,
                )?;
                for (index, (text, fg)) in extras.iter().enumerate() {
                    draw(
                        &mut self.renderer,
                        &mut self.surface,
                        (mid + line * 6).saturating_add(step.saturating_mul(index as i16)),
                        text,
                        *fg,
                    )?;
                }
            }
            config::Layout::Corner => {
                let margin = (width / 10).max(16);
                let left_x = left(margin);
                let right_x = right(margin);
                let top_y = top(margin + i32::from(line));
                let bottom_y = top(height - margin - i32::from(line));
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    left_x,
                    top_y,
                    &clock,
                    pal.foreground,
                    pal.background,
                )?;
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    left_x,
                    bottom_y.saturating_sub(line * 2),
                    &hostname,
                    pal.muted,
                    pal.background,
                )?;
                let away_text = core::away_text(away);
                let away_width = self.renderer.width(&away_text);
                self.renderer.draw_left(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    right_x.saturating_sub(away_width),
                    bottom_y,
                    &away_text,
                    pal.border.max(pal.focus),
                    pal.background,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid,
                    "locked",
                    pal.focus,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    mid + line * 2,
                    &password,
                    colour,
                )?;
                for (index, (text, fg)) in extras.iter().enumerate() {
                    draw(
                        &mut self.renderer,
                        &mut self.surface,
                        (mid + line * 4).saturating_add(step.saturating_mul(index as i16)),
                        text,
                        *fg,
                    )?;
                }
            }
            config::Layout::Cava => {
                let spec_w = ((width as f32 * 0.58) as i32).clamp(320, 1100) as u16;
                let spec_h = ((height as f32 * 0.16) as i32).clamp(96, 200) as u16;
                let spec_x = left((width - i32::from(spec_w)) / 2);
                let spec_y = top(((height - i32::from(spec_h)) / 2 - i32::from(line) * 2).max(0));
                let clock_baseline = spec_y.saturating_sub(line * 2);
                let host_baseline = spec_y.saturating_sub(line / 2);
                self.clock_renderer.draw_centred(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    centre,
                    clock_baseline,
                    &clock,
                    pal.foreground,
                    pal.background,
                )?;
                self.renderer.draw_centred(
                    &self.surface.c,
                    self.surface.gc,
                    self.surface.back,
                    self.surface.depth,
                    centre,
                    host_baseline,
                    &hostname,
                    pal.muted,
                    pal.background,
                )?;
                if let Some(values) = spectrum.as_deref() {
                    cava::paint(&self.surface, &pal, spec_x, spec_y, spec_w, spec_h, values)?;
                    self.spectrum_rect = Some((spec_x, spec_y, spec_w, spec_h));
                }
                let below = spec_y
                    .saturating_add(spec_h as i16)
                    .saturating_add(line * 2);
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    below,
                    "locked",
                    pal.focus,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    below + line * 2,
                    &password,
                    colour,
                )?;
                draw(
                    &mut self.renderer,
                    &mut self.surface,
                    below + line * 4,
                    &core::away_text(away),
                    pal.border.max(pal.focus),
                )?;
                for (index, (text, fg)) in extras.iter().enumerate() {
                    draw(
                        &mut self.renderer,
                        &mut self.surface,
                        (below + line * 6).saturating_add(step.saturating_mul(index as i16)),
                        text,
                        *fg,
                    )?;
                }
            }
        }
        self.surface.present()?;
        Ok(())
    }

    /// Lines under the prompt: why the last attempt failed, then the
    /// keyboard indicators.
    fn status_lines(&self, pal: &palette::Palette) -> Vec<(String, u32)> {
        let mut lines = Vec::new();
        match (self.state, self.pam_message.as_ref()) {
            // For two seconds after a failure: the reason, in the warning
            // colour. PAM's own words win over the generic message.
            (State::Wrong, Some(message)) => lines.push((message.clone(), self.cfg.warning)),
            (State::Wrong, None) => lines.push((self.message.clone(), self.cfg.warning)),
            // Afterwards PAM's message (a faillock lockout) stays readable.
            (State::Prompt, Some(message)) => lines.push((message.clone(), pal.muted)),
            _ => {}
        }
        if self.failed > 0 {
            lines.push((failed_text(self.failed), pal.muted));
        }
        if self.caps {
            lines.push(("Caps Lock".to_string(), pal.focus));
        }
        lines
    }

    /// Repaint only the CAVA panel between full frames. The clock, prompt and
    /// lock state are static until an event or the next minute, so clearing and
    /// uploading the whole screen at 30 fps would burn CPU for no visual gain.
    fn draw_cava_frame(&mut self) -> Result<()> {
        let Some((x, y, width, height)) = self.spectrum_rect else {
            return Ok(());
        };
        let Some(cava) = self.cava.as_mut() else {
            return Ok(());
        };
        let values = cava.values().to_vec();
        cava::paint(
            &self.surface,
            &self.cfg.palette,
            x,
            y,
            width,
            height,
            &values,
        )?;
        self.surface.present_rect(x, y, width, height)?;
        Ok(())
    }

    fn submit(&mut self) -> Result<()> {
        if self.authentication.is_some() {
            return Ok(());
        }
        self.wrong_until = None;
        let user = self.user.clone();
        let password = std::mem::replace(&mut self.input, secret::new_entry());
        match Authentication::start(user, password) {
            Ok(worker) => {
                self.authentication = Some(worker);
                self.state = State::Checking;
                self.pam_message = None;
            }
            Err(_) => {
                self.state = State::Wrong;
                self.message = "cannot start authentication".into();
                self.wrong_until = Some(Instant::now() + WRONG_FEEDBACK);
            }
        }
        self.draw()
    }

    fn check_authentication(&mut self) -> Result<bool> {
        let Some(authentication) = &self.authentication else {
            return Ok(false);
        };
        let outcome = match authentication.result.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return Ok(false),
            Err(mpsc::TryRecvError::Disconnected) => Err("authentication worker stopped".into()),
        };
        if let Some(authentication) = self.authentication.take() {
            // The worker's only remaining work is a nonblocking one-byte wakeup.
            let _ = authentication.worker.join();
        }
        match outcome {
            Ok(outcome) if outcome.accepted => {
                self.trace("password accepted");
                // Accepted: give the keyboard back, then release the grabs so
                // the session can continue.
                let _ = self.tell_wm_locked(false);
                let _ = self.publish_ready(false);
                self.ready = false;
                self.surface.release_input()?;
                // An active keyboard grab swallows synthetic key events, and
                // those are how the idle clock is told the user is here.
                self.surface.c.ungrab_keyboard(x11rb::CURRENT_TIME)?;
                self.surface.c.sync()?;
                self.surface.c.destroy_window(self.surface.win)?;
                self.surface.c.flush()?;
                self.touch_idle_clock();
                self.trace("password accepted, idle clock told");
                // process::exit skips destructors: stop CAVA and remove its
                // temporary config explicitly.
                self.cava = None;
                std::process::exit(0);
            }
            // The entry is not cleared: it was taken at submit, and anything
            // in it now was typed during the check.
            Ok(outcome) => {
                self.state = State::Wrong;
                self.message = "wrong password".to_string();
                // Shown under the prompt, for example pam_faillock's lockout.
                self.pam_message = outcome.message;
                self.failed = self.failed.saturating_add(1);
            }
            Err(e) => {
                self.state = State::Wrong;
                self.message = format!("cannot verify: {e}");
                self.pam_message = None;
            }
        }
        // Keep event handling and verified grabs active during failure feedback.
        self.wrong_until = Some(Instant::now() + WRONG_FEEDBACK);
        Ok(true)
    }

    /// Light another cell of the `pulse` echo, chosen at random so that its
    /// position says nothing about the length.
    fn move_pulse(&mut self) {
        self.pulse = next_pulse(self.pulse, &mut self.rng);
    }

    /// Reset X11's real idle clock; synthetic root key events do not do that.
    fn touch_idle_clock(&self) {
        let _ = self.surface.c.force_screen_saver(ScreenSaver::RESET);
        let _ = self.surface.c.flush();
    }

    fn handle_key(&mut self, e: KeyPressEvent) -> Result<bool> {
        self.caps = e.state.contains(KeyButMask::LOCK);
        // Never log keycodes, modifiers, characters or password lengths.
        // The server does not start the X11 idle clock for keys it delivered
        // through a grab. On a machine whose idle timer locks after a short
        // time, that means the screen locks again moments after the password
        // is accepted. Announcing each key the lock screen takes keeps the
        // clock honest the whole time, not only when it unlocks.
        self.touch_idle_clock();
        // Input stays enabled while a password is checked and while the
        // failure is shown; Enter during a check is ignored by submit().
        // Keysyms, XKB levels and compose come from the keyboard module.
        match self.keyboard.key(&e) {
            keyboard::Key::Submit => {
                self.submit()?;
                return Ok(true);
            }
            keyboard::Key::Erase => {
                secret::erase_last(&mut self.input);
                self.move_pulse();
            }
            // Escape clears the entry; it never dismisses the lock.
            keyboard::Key::Clear => secret::clear(&mut self.input),
            keyboard::Key::Text(text) => {
                secret::push(&mut self.input, text.as_str());
                self.move_pulse();
            }
            keyboard::Key::Nothing => {}
        }
        self.draw()?;
        Ok(true)
    }

    fn run(&mut self) -> Result<()> {
        let mut minute = String::new();
        let mut away_minute = u64::MAX;
        let mut repair_display = false;
        let mut pending = None;
        // The spectrum is drawn on a 30 fps schedule, not once per loop
        // iteration: other clients' ConfigureNotify events (several a second
        // on a busy session) must not add extra frames in between.
        let mut next_frame = Instant::now();
        loop {
            let mut repaint = self.check_authentication()?;
            let mut drained = false;
            // Bound each batch so a burst cannot starve authentication or repair.
            for _ in 0..256 {
                let event = match pending.take() {
                    Some(event) => Ok(Some(event)),
                    None => self.surface.c.poll_for_event(),
                };
                if let Ok(Some(event)) = &event {
                    if let Some(changed) = self.keyboard.handle_event(&self.surface.c, event) {
                        if let Some(caps) = self.keyboard.caps() {
                            self.caps = caps;
                        }
                        repaint |= changed;
                        continue;
                    }
                    if let Some(power) = self.power.as_mut() {
                        let changed = match event {
                            Event::KeyPress(_) | Event::ButtonPress(_) | Event::MotionNotify(_) => {
                                power.input(&self.surface.c, Instant::now())
                            }
                            event => power.event(event),
                        };
                        if let Some(on) = changed {
                            self.display_power(on);
                            repaint = true;
                        }
                    }
                }
                match event {
                    Ok(Some(Event::KeyPress(e))) => {
                        if let Err(e) = self.handle_key(e) {
                            eprintln!("nirulock: input handling: {e}");
                        }
                    }
                    Ok(Some(Event::Expose(_))) => repaint = true,
                    Ok(Some(Event::MapNotify(_)))
                    | Ok(Some(Event::CirculateNotify(_)))
                    | Ok(Some(Event::ConfigureNotify(_)))
                    | Ok(Some(Event::RandrScreenChangeNotify(_)))
                    | Ok(Some(Event::RandrNotify(_))) => repair_display = true,
                    Ok(Some(Event::Error(e))) => eprintln!("nirulock: X11 error: {e:?}"),
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        drained = true;
                        break;
                    }
                    Err(x11rb::errors::ConnectionError::IoError(e)) => {
                        return Err(format!("lost the X connection: {e}").into())
                    }
                    Err(e) => eprintln!("nirulock: {e}"),
                }
            }
            if repair_display {
                // Other clients configure their windows all the time (a
                // session on othala did so four times a second); only a
                // changed size or prompt output needs a full repaint.
                match self.keep_on_top() {
                    Ok(resized) => {
                        let before = (self.prompt_output, self.prompt_area);
                        self.update_prompt_area();
                        repair_display = false;
                        repaint |= resized || before != (self.prompt_output, self.prompt_area);
                    }
                    Err(e) => eprintln!("nirulock: coverage repair will retry: {e}"),
                }
            }
            let m = Local::now().format("%Y-%m-%d %H:%M").to_string();
            let away = self.started.elapsed().as_secs() / 60;
            // Nobody sees the clock with the display off; it is redrawn when
            // the display comes back on.
            let display_on = self.power.as_ref().map_or(true, cava::DisplayPower::on);
            if display_on && (m != minute || away != away_minute) {
                minute = m;
                away_minute = away;
                repaint = true;
            }
            if self
                .wrong_until
                .is_some_and(|until| Instant::now() >= until)
            {
                self.wrong_until = None;
                self.state = State::Prompt;
                repaint = true;
            }
            if let Some(on) = self
                .power
                .as_mut()
                .and_then(|power| power.poll(&self.surface.c, Instant::now()))
            {
                self.display_power(on);
                repaint = true;
            }
            let cava_active = self.cava.as_ref().is_some_and(cava::Cava::animating);
            if repaint {
                // Rendering/resource errors never dismiss an established lock.
                if let Err(e) = self.draw() {
                    eprintln!("nirulock: repaint: {e}");
                }
                next_frame = Instant::now() + CAVA_FRAME;
            } else if cava_active && Instant::now() >= next_frame {
                if let Err(e) = self.draw_cava_frame() {
                    eprintln!("nirulock: cava repaint: {e}");
                }
                // A steady cadence: the time spent drawing is not added to
                // the interval, and a late frame restarts it.
                next_frame += CAVA_FRAME;
                if next_frame <= Instant::now() {
                    next_frame = Instant::now() + CAVA_FRAME;
                }
            }
            self.surface.c.flush()?;
            if !drained {
                continue;
            }
            // Round trips above may have queued events in x11rb, away from the fd.
            pending = self.surface.c.poll_for_event()?;
            if pending.is_some() {
                continue;
            }
            let timeout = if cava_active {
                // At most one frame interval, so minute and DPMS deadlines
                // are met as before.
                let due = next_frame.saturating_duration_since(Instant::now());
                (due.as_micros().div_ceil(1000) as i32).clamp(1, 34)
            } else {
                self.idle_timeout()
            };
            let cava_wake = self.cava.as_ref().and_then(cava::Cava::wake_fd);
            let ready = wait_readable(
                [
                    Some(self.surface.c.stream().as_raw_fd()),
                    self.authentication
                        .as_ref()
                        .map(|auth| auth.wake.as_raw_fd()),
                    cava_wake,
                ],
                timeout,
            );
            if ready[2] {
                if let Some(cava) = self.cava.as_mut() {
                    cava.woke();
                }
                next_frame = Instant::now();
            }
        }
    }

    /// Follow a display power change: stop or restart the spectrum.
    fn display_power(&mut self, on: bool) {
        if let Some(cava) = self.cava.as_mut() {
            cava.set_display(on);
        }
        self.trace(if on { "display on" } else { "display off" });
    }

    /// Without animation the loop only has to wake for the next clock or
    /// away-time minute, the end of failure feedback and a due DPMS poll.
    fn idle_timeout(&self) -> i32 {
        use chrono::Timelike;
        let now = Local::now();
        let into_minute = u64::from(now.second()) * 1000 + u64::from(now.timestamp_subsec_millis());
        let mut ms = 60_000u64.saturating_sub(into_minute.min(59_999));
        let away = (self.started.elapsed().as_millis() % 60_000) as u64;
        ms = ms.min(60_000 - away);
        let instant = Instant::now();
        for deadline in [
            self.wrong_until,
            self.power.as_ref().and_then(cava::DisplayPower::deadline),
        ]
        .into_iter()
        .flatten()
        {
            ms = ms.min(deadline.saturating_duration_since(instant).as_millis() as u64);
        }
        // A few milliseconds late is better than waking just before a boundary.
        (ms + 5).clamp(1, 60_000) as i32
    }
}

/// The spectrum's frame interval: CAVA produces 30 frames per second.
const CAVA_FRAME: Duration = Duration::from_micros(33_333);

/// Wait on X, the bounded authentication worker and the CAVA silence wake-up
/// without periodic polling. Returns which descriptors became readable.
fn wait_readable(fds: [Option<std::os::fd::RawFd>; 3], timeout_ms: i32) -> [bool; 3] {
    let mut poll = fds.map(|fd| libc::pollfd {
        fd: fd.unwrap_or(-1),
        events: libc::POLLIN,
        revents: 0,
    });
    // SAFETY: three valid pollfd values; poll ignores a negative unused fd.
    let n = unsafe { libc::poll(poll.as_mut_ptr(), poll.len() as libc::nfds_t, timeout_ms) };
    if n <= 0 {
        return [false; 3];
    }
    poll.map(|p| p.fd >= 0 && p.revents & libc::POLLIN != 0)
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "--version" => println!("NIRULOCK {}", env!("CARGO_PKG_VERSION")),
        "--config" => match config::Config::load() {
            Ok(c) => println!("{}", c.describe()),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
        },
        "" | "--lock" => match Lock::new() {
            Ok(mut lock) => {
                if let Err(e) = lock.run() {
                    let _ = lock.tell_wm_locked(false);
                    eprintln!("nirulock: {e}");
                    drop(lock);
                    std::process::exit(1)
                }
            }
            Err(e) => {
                eprintln!("nirulock: {e}");
                std::process::exit(1)
            }
        },
        _ => {
            eprintln!("Usage: nirulock [--version|--config|--lock]");
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_echo_hides_the_length() {
        let short = echo(config::Echo::Pulse, 3, 5);
        let long = echo(config::Echo::Pulse, 40, 5);
        assert_eq!(short, long);
        assert_eq!(short.chars().count(), PULSE_CELLS);
        assert_eq!(short.matches('*').count(), 1);
        assert_eq!(echo(config::Echo::Pulse, 0, 5), "");
        assert_eq!(echo(config::Echo::Count, 3, 5), "***");
    }

    #[test]
    fn the_pulse_moves_on_every_key() {
        let mut pulse = 0;
        let mut rng = seed();
        let mut seen = [false; PULSE_CELLS];
        for _ in 0..200 {
            let next = next_pulse(pulse, &mut rng);
            assert_ne!(next, pulse);
            assert!(next < PULSE_CELLS);
            seen[next] = true;
            pulse = next;
        }
        assert!(seen.iter().all(|s| *s), "some cells are never lit");
    }

    #[test]
    fn failed_attempts_are_counted_in_words() {
        assert_eq!(failed_text(1), "1 failed attempt");
        assert_eq!(failed_text(2), "2 failed attempts");
        assert_eq!(WRONG_FEEDBACK, Duration::from_secs(2));
    }
}
