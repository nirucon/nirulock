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

mod config;
mod core;
mod display;
mod palette;
mod pam;

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

/// Text shown while a password is being checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Prompt,
    Checking,
    Wrong,
}

struct Authentication {
    result: mpsc::Receiver<std::result::Result<bool, String>>,
    wake: UnixStream,
    worker: std::thread::JoinHandle<()>,
}

impl Authentication {
    fn start(user: String, password: String) -> Result<Self> {
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
    state: State,
    input: String,
    started: Instant,
    message: String,
    wm_notified: bool,
    authentication: Option<Authentication>,
    wrong_until: Option<Instant>,
    prompt_output: Option<u32>,
    prompt_area: display::Area,
    ready: bool,
    caps: bool,
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
        let cfg = config::Config::load()?;
        let surface = core::Surface::new("NIRULOCK")?;
        let font = core::find_font(cfg.font_path.as_deref());
        if font.is_empty() {
            return Err("no monospace font found; set font_path in the config".into());
        }
        let setup = surface.c.setup().clone();
        let renderer = core::TextRenderer::new(&font, cfg.font_size, &setup)?;
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
        let mut lock = Self {
            surface,
            cfg,
            renderer,
            state: State::Prompt,
            input: String::new(),
            started: Instant::now(),
            message: String::new(),
            wm_notified: false,
            authentication: None,
            wrong_until: None,
            prompt_output: None,
            prompt_area,
            ready: false,
            caps,
        };
        lock.trace("starting");
        // Grab the pointer first, then ask the window manager to let go of its
        // key grabs, then take the keyboard. The order is the fix: a keyboard
        // grab does not displace key grabs another client already holds, so
        // taking the keyboard first meant the window manager's grabs stayed
        // on top until each of its keys was pressed -- which is why Super had
        // to be held to type a password.
        lock.grab_pointer()?;
        // Notify before acquiring input. The X server round trip does not prove
        // that the WM processed the message; verified active grabs are required.
        lock.tell_wm_locked(true)?;
        lock.wm_notified = true;
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

    fn grab_pointer(&self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
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
        if reply.status != GrabStatus::SUCCESS {
            return Err(format!("cannot grab pointer: {:?}", reply.status).into());
        }
        self.surface.c.flush()?;
        Ok(())
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
    fn keep_on_top(&mut self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let geometry = self.surface.c.get_geometry(root)?.reply()?;
        self.surface.resize(geometry.width, geometry.height)?;
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
        Ok(())
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
        let (centre, mid) = self.prompt_area.centre();
        let line = self.renderer.line_height();

        // Clock, then hostname, then the lock caption.
        let clock = core::clock_text(Local::now());
        self.renderer.draw_centred(
            &self.surface.c,
            self.surface.gc,
            self.surface.back,
            self.surface.depth,
            centre,
            mid - line * 4,
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
            mid - line * 2,
            &core::hostname(),
            pal.muted,
            pal.background,
        )?;
        self.renderer.draw_centred(
            &self.surface.c,
            self.surface.gc,
            self.surface.back,
            self.surface.depth,
            centre,
            mid,
            "locked",
            pal.focus,
            pal.background,
        )?;

        let away = self.started.elapsed().as_secs();
        self.renderer.draw_centred(
            &self.surface.c,
            self.surface.gc,
            self.surface.back,
            self.surface.depth,
            centre,
            mid + line * 2,
            &core::away_text(away),
            pal.border.max(pal.focus),
            pal.background,
        )?;

        // Password line: dots for typed characters, never the characters.
        let (label, colour) = match self.state {
            State::Prompt => ("password: ".to_string(), pal.foreground),
            State::Checking => ("checking...".to_string(), pal.muted),
            State::Wrong => (self.message.clone(), pal.focus),
        };
        let dots = "*".repeat(self.input.chars().count());
        let text = format!("{label}{dots}");
        self.renderer.draw_centred(
            &self.surface.c,
            self.surface.gc,
            self.surface.back,
            self.surface.depth,
            centre,
            mid + line * 4,
            &text,
            colour,
            pal.background,
        )?;
        if self.caps {
            self.renderer.draw_centred(
                &self.surface.c,
                self.surface.gc,
                self.surface.back,
                self.surface.depth,
                centre,
                mid + line * 6,
                "Caps Lock",
                pal.focus,
                pal.background,
            )?;
        }
        self.surface.present()?;
        Ok(())
    }

    fn submit(&mut self) -> Result<()> {
        if self.authentication.is_some() {
            return Ok(());
        }
        let user = pam::current_user();
        let password = std::mem::take(&mut self.input);
        match Authentication::start(user, password) {
            Ok(worker) => {
                self.authentication = Some(worker);
                self.state = State::Checking;
            }
            Err(_) => {
                self.state = State::Wrong;
                self.message = "cannot start authentication".into();
                self.wrong_until = Some(Instant::now() + Duration::from_millis(600));
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
            Ok(true) => {
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
                std::process::exit(0);
            }
            Ok(false) => {
                self.state = State::Wrong;
                self.message = "wrong password".to_string();
                self.input.clear();
            }
            Err(e) => {
                self.state = State::Wrong;
                self.message = format!("cannot verify: {e}");
                self.input.clear();
            }
        }
        // Keep event handling and verified grabs active during failure feedback.
        self.wrong_until = Some(Instant::now() + Duration::from_millis(600));
        Ok(true)
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
        if self.state == State::Checking {
            return Ok(true);
        }
        if self.state == State::Wrong {
            self.state = State::Prompt;
            self.wrong_until = None;
        }
        // Map the keycode to a printable character through the keyboard map.
        let sym = self.keystring(e)?;
        match sym.as_str() {
            "\r" | "\n" => {
                self.submit()?;
                return Ok(true);
            }
            "\u{8}" => {
                self.input.pop();
            }
            "\u{1b}" => {
                // Escape clears the entry; it never dismisses the lock.
                self.input.clear();
            }
            s if s.chars().count() == 1 => {
                let c = s.chars().next().unwrap();
                if !c.is_control() && self.input.len() + c.len_utf8() <= 4096 {
                    self.input.push(c);
                }
            }
            _ => {}
        }
        self.draw()?;
        Ok(true)
    }

    /// Translate a key press into the character it produces.
    ///
    /// A keycode has several keysym levels: unshifted, shifted, AltGr and so
    /// on. Reading only the first level made every capital letter and every
    /// shifted symbol (on a Swedish layout `@`, `!`, `?`, `#`) impossible to
    /// type, so a password containing one could never unlock the screen.
    ///
    /// Caps Lock is applied separately, as X does: it shifts letters but not
    /// digits or punctuation.
    fn keystring(&self, e: KeyPressEvent) -> Result<String> {
        let map = self.surface.c.get_keyboard_mapping(e.detail, 1)?.reply()?;
        let syms = &map.keysyms;
        let shift = e.state.contains(KeyButMask::SHIFT);
        let lock = e.state.contains(KeyButMask::LOCK);
        let altgr = e.state.contains(KeyButMask::MOD5);
        let level = match (shift, altgr) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        };
        let mut sym = syms
            .get(level)
            .or_else(|| syms.get(level % 2))
            .or_else(|| syms.first())
            .copied()
            .unwrap_or(0);
        // XK_ISO_Level3_Shift as a leftover: fall back to the unshifted level.
        if sym == 0 || sym == 0xfe03 {
            sym = syms.first().copied().unwrap_or(0);
        }
        // Caps Lock swaps the case of alphabetic keysyms only. Latin-1
        // covers å ä ö and the other Swedish letters; ÷ and × are not letters.
        if lock {
            if (0x61..=0x7a).contains(&sym) || ((0xe0..=0xfe).contains(&sym) && sym != 0xf7) {
                sym -= 0x20;
            } else if (0x41..=0x5a).contains(&sym) || ((0xc0..=0xde).contains(&sym) && sym != 0xd7)
            {
                sym += 0x20;
            }
        }
        Ok(match sym {
            0xff0d | 0xff8d => "\r".to_string(),
            0xff08 => "\u{8}".to_string(),
            0xff1b => "\u{1b}".to_string(),
            0xff09 => "\t".to_string(),
            s if (0x20..0x7f).contains(&s) => char::from_u32(s).unwrap_or(' ').to_string(),
            // Latin-1 keysyms map directly to their code point, which covers
            // å ä ö and the other Swedish letters.
            s if (0xa0..0x100).contains(&s) => char::from_u32(s).unwrap_or(' ').to_string(),
            _ => String::new(),
        })
    }

    fn run(&mut self) -> Result<()> {
        let mut minute = String::new();
        let mut away_minute = u64::MAX;
        let mut repair_display = false;
        let mut pending = None;
        loop {
            let mut repaint = self.check_authentication()?;
            let mut drained = false;
            // Bound each batch so a burst cannot starve authentication or repair.
            for _ in 0..256 {
                let event = match pending.take() {
                    Some(event) => Ok(Some(event)),
                    None => self.surface.c.poll_for_event(),
                };
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
                match self.keep_on_top() {
                    Ok(()) => {
                        self.update_prompt_area();
                        repair_display = false;
                        repaint = true;
                    }
                    Err(e) => eprintln!("nirulock: coverage repair will retry: {e}"),
                }
            }
            let m = Local::now().format("%Y-%m-%d %H:%M").to_string();
            let away = self.started.elapsed().as_secs() / 60;
            if m != minute || away != away_minute {
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
            if repaint {
                // Rendering/resource errors never dismiss an established lock.
                if let Err(e) = self.draw() {
                    eprintln!("nirulock: repaint: {e}");
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
            let timeout = self
                .wrong_until
                .map(|until| {
                    until
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .clamp(1, 1000) as i32
                })
                .unwrap_or(1000);
            wait_readable(
                self.surface.c.stream().as_raw_fd(),
                self.authentication
                    .as_ref()
                    .map(|auth| auth.wake.as_raw_fd()),
                timeout,
            );
        }
    }
}

/// Wait on X and the bounded authentication worker without periodic auth polling.
fn wait_readable(fd: std::os::fd::RawFd, auth_fd: Option<std::os::fd::RawFd>, timeout_ms: i32) {
    let mut fds = [
        libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: auth_fd.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: two valid pollfd values; poll ignores a negative unused fd.
    unsafe {
        libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms);
    }
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
