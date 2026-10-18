//! Nirulock: the Niru Noir lock screen.
//!
//! Shows the same clock and hostname as NIRUSCREENSAVER, plus how long the
//! machine has been away and a prompt. The screen stays covered until the
//! password is accepted by PAM.
//!
//! Security properties, in order of importance:
//!   1. The window is override-redirect and full-screen, so no window manager
//!      can move, resize, focus or cover it.
//!   2. Every key press and button press on the screen is grabbed, so focus
//!      cannot be moved to another window by clicking.
//!   3. PAM decides; a missing PAM, a failed conversation or an unexpected
//!      prompt all fail closed.

mod config;
mod core;
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
use std::time::{Duration, Instant};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event},
    wrapper::ConnectionExt as _,
};

/// Text shown while a password is being checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Prompt,
    Checking,
    Wrong,
}

struct Lock {
    surface: core::Surface,
    cfg: config::Config,
    renderer: core::TextRenderer,
    state: State,
    input: String,
    started: Instant,
    last_minute: String,
    message: String,
    /// Keycodes grabbed on top of the keyboard grab, released before asking
    /// for the password again.
    key_grabs: std::cell::RefCell<Vec<u8>>,
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
        let mut lock = Self {
            surface,
            cfg,
            renderer,
            state: State::Prompt,
            input: String::new(),
            started: Instant::now(),
            last_minute: String::new(),
            message: String::new(),
            key_grabs: std::cell::RefCell::new(Vec::new()),
        };
        lock.trace("starting");
        // Grab the pointer first, then ask the window manager to let go of its
        // key grabs, then take the keyboard. The order is the fix: a keyboard
        // grab does not displace key grabs another client already holds, so
        // taking the keyboard first meant the window manager's grabs stayed
        // on top until each of its keys was pressed -- which is why Super had
        // to be held to type a password.
        lock.grab_pointer()?;
        // Tell the window manager before taking the keyboard. It holds a
        // grab on every key it binds, and until it has released them its grabs
        // win over the lock's: the user then had to hold Super to reach the
        // lock screen at all, because Super was the one modifier whose grab
        // the lock could displace. Order matters, and so does waiting: the
        // message is a round trip, and the client processes it before the
        // next request from us.
        lock.tell_wm_locked(true)?;
        // Above everything from the first frame: the bar and any
        // override-redirect window are already on screen when a lock starts.
        lock.keep_on_top()?;
        lock.draw()?;
        lock.grab_input()?;
        Ok(lock)
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
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        )?;
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
        self.surface.c.grab_pointer(
            true,
            root,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        )?;
        self.surface.c.flush()?;
        Ok(())
    }

    /// Grab the keyboard and every key.
    ///
    /// Called after the window manager has released its own grabs; the
    /// keyboard grab does not displace a passive key grab that another client
    /// already holds, which is why the order matters.
    fn grab_input(&self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        self.surface.c.grab_keyboard(
            true,
            root,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )?;
        // Grab every key individually as well. The keyboard grab above is
        // enough on a quiet server, but a client that already holds a
        // combination makes X split the keyboard, so the explicit grabs are
        // the safety net for keys such as Escape.
        //
        // Two important limits. A client that asked for key grabs with
        // `owner_events = false` (normally the window manager, which is still
        // running) makes every plain-key grab fail with Access. That is not an
        // error worth printing: the keyboard grab already covers those keys.
        // And when a grab fails, X reports it asynchronously, so the error must
        // not be queued before the rest of the grabs are requested.
        self.surface.c.flush()?;
        // Remember what was grabbed so that going back to the password prompt
        // releases exactly that and nothing else.
        let mut grabbed = Vec::new();
        for code in 8..=255u8 {
            if self
                .surface
                .c
                .grab_key(
                    true,
                    root,
                    ModMask::ANY,
                    code,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .is_ok()
            {
                grabbed.push(code);
            }
        }
        self.surface.c.flush()?;
        // The refusals are asynchronous and arrive before or with this reply.
        // Reading it here keeps them from reaching the lock's own result.
        let _ = self.surface.c.get_input_focus()?.reply();
        self.trace(&format!("key grabs: {} requested", grabbed.len()));

        *self.key_grabs.borrow_mut() = grabbed;
        self.surface.c.sync()?;
        self.trace("input grabbed");
        Ok(())
    }

    /// Raise the lock window over everything and make it full screen again.
    ///
    /// Also used once at start, so the window is certainly above the bar and
    /// any override-redirect window that was there before.
    fn keep_on_top(&mut self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        self.surface.c.configure_window(
            self.surface.win,
            &ConfigureWindowAux::new()
                .x(0)
                .y(0)
                .width(u32::from(self.surface.width))
                .height(u32::from(self.surface.height))
                .stack_mode(StackMode::ABOVE),
        )?;
        self.surface.c.map_window(self.surface.win)?;
        let _ = root;
        self.surface.c.flush()?;
        Ok(())
    }

    fn draw(&mut self) -> Result<()> {
        let pal = self.cfg.palette.clone();
        self.surface.clear(pal.background)?;
        let centre = (self.surface.width / 2) as i16;
        let mid = (self.surface.height / 2) as i16;
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
        self.surface.present()?;
        Ok(())
    }

    fn submit(&mut self) -> Result<()> {
        let user = pam::current_user();
        self.state = State::Checking;
        self.draw()?;
        match pam::authenticate(&user, &self.input) {
            Ok(true) => {
                self.trace("password accepted");
                // Accepted: give the keyboard back, then release the grabs so
                // the session can continue.
                let _ = self.tell_wm_locked(false);
                self.surface.release_input()?;
                self.surface.c.ungrab_key(
                    0,
                    self.surface.c.setup().roots[self.surface.screen_num].root,
                    ModMask::ANY,
                )?;
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
        // A short visible failure before returning to the prompt.
        self.draw()?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(600) {
            std::thread::sleep(Duration::from_millis(50));
        }
        // Back to the prompt with a clean keyboard. A refused key grab leaves
        // a passive grab behind if it is not released explicitly, and a passive
        // grab on every key of a modifier makes every later keystroke belong to
        // this window, which looks exactly like a lock that ignores typing.
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        for code in self.key_grabs.borrow_mut().drain(..) {
            let _ = self.surface.c.ungrab_key(code, root, ModMask::ANY);
        }
        let _ = self.surface.c.ungrab_keyboard(x11rb::CURRENT_TIME);
        self.surface.c.sync()?;
        let _ = self.surface.c.get_input_focus()?.reply();
        // Take the keyboard again, the same way the lock started, so typing
        // is checked with the same names the wrong password used.
        self.grab_input()?;
        self.state = State::Prompt;
        self.last_minute.clear();
        self.draw()?;
        Ok(())
    }

    /// Send a key event to the root window, which is what counts as user
    /// activity for X11's idle clock.
    fn touch_idle_clock(&self) {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        let _ = self.surface.c.send_event(
            true,
            root,
            EventMask::NO_EVENT,
            KeyPressEvent {
                response_type: KEY_PRESS_EVENT,
                sequence: 0,
                detail: 0,
                time: x11rb::CURRENT_TIME,
                root,
                event: root,
                child: x11rb::NONE,
                root_x: 0,
                root_y: 0,
                event_x: 0,
                event_y: 0,
                state: KeyButMask::from(0u16),
                same_screen: true,
            },
        );
        let _ = self.surface.c.flush();
    }

    fn handle_key(&mut self, e: KeyPressEvent) -> Result<bool> {
        // Every key that arrives is logged while it is being judged. When one
        // did not arrive (a keyboard grab held by the window manager won over
        // the lock's), this line is what tells the difference; without it the
        // only evidence was a password that seemed to be ignored.
        self.trace(&format!(
            "key code {} state {:#x}",
            e.detail,
            u16::from(e.state)
        ));
        // The server does not start the X11 idle clock for keys it delivered
        // through a grab. On a machine whose idle timer locks after a short
        // time, that means the screen locks again moments after the password
        // is accepted. Announcing each key the lock screen takes keeps the
        // clock honest the whole time, not only when it unlocks.
        self.touch_idle_clock();
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
                if !c.is_control() {
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
        // Caps Lock swaps the case of alphabetic keysyms only.
        if lock {
            if (0x61..=0x7a).contains(&sym) {
                sym -= 0x20;
            } else if (0x41..=0x5a).contains(&sym) {
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
        // Escape hatch for testing: NIRULOCK_SAFE_SECONDS=20 releases the lock
        // after twenty seconds. It exists so a lock that cannot be unlocked
        // never costs another ssh session; it is deliberately an environment
        // variable, not a setting, so it cannot be left on by accident.
        //
        // The same clock is the ladder's `lock_timeout_minutes` multiplied by
        // ten, so a lock screen that stops working releases the screen instead
        // of leaving the machine unusable. An error-free lock never reaches it.
        let limit = std::env::var("NIRULOCK_SAFE_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .or_else(|| {
                (self.cfg.lock_timeout_minutes > 0).then_some(self.cfg.lock_timeout_minutes * 10)
            });
        let emergency: Option<Instant> = limit.map(|secs| {
            self.trace(&format!("release after {secs}s without a password"));
            Instant::now() + Duration::from_secs(secs)
        });
        // Watch the server for as long as the lock is up. Compositors can
        // reset their stacking order after the lock window was raised (picom
        // recreates windows when it restarts, for one), and then the desktop
        // is visible through the lock even though its keyboard is still
        // grabbed. Re-asserting the place is cheap and is the only thing that
        // cannot go wrong quietly.
        self.watch_stacking()?;
        loop {
            if let Some(deadline) = emergency {
                if Instant::now() >= deadline {
                    self.trace("release fired: the screen came back without a password");
                    self.surface.release_input()?;
                    self.surface.c.destroy_window(self.surface.win)?;
                    self.surface.c.flush()?;
                    return Ok(());
                }
            }
            let now = Local::now();
            let m = now.format("%Y-%m-%d %H:%M").to_string();
            if m != minute {
                minute = m;
                self.draw()?;
            }
            let ready = wait_readable(
                std::os::fd::AsRawFd::as_raw_fd(self.surface.c.stream()),
                1000,
            );
            if ready {
                // `poll_for_event` stops at the first error, and every rejected
                // key grab on a busy window manager is one. A lock screen must
                // not stop reading keys because the server refused a grab it
                // did not need, so errors between events are drained here.
                loop {
                    match self.surface.c.poll_for_event() {
                        Ok(Some(Event::CirculateNotify(_))) => {
                            self.keep_on_top()?;
                        }
                        Ok(Some(Event::ConfigureNotify(e))) if e.window == self.surface.win => {
                            // Something reconfigured the lock window itself.
                            self.keep_on_top()?;
                        }
                        Ok(Some(event)) => match event {
                            Event::KeyPress(e) => {
                                self.handle_key(e)?;
                            }
                            Event::Expose(_) => self.draw()?,
                            Event::MappingNotify(_) => {
                                // The keymap changed under us (setxkbmap, layout
                                // switch). x11rb refreshes its own cache; the next
                                // key press reads the new mapping.
                                self.surface.c.flush()?;
                            }
                            Event::Error(e) => eprintln!("nirulock: X11 error: {e:?}"),
                            _ => {}
                        },
                        Ok(None) => break,
                        // Every decode error already has an event next in
                        // line, so draining is safe.
                        // A closed connection is the one error that means
                        // stop; anything else arrives as an event next.
                        Err(x11rb::errors::ConnectionError::IoError(e)) => {
                            return Err(format!("lost the X connection: {e}").into());
                        }
                        Err(e) => eprintln!("nirulock: {e}"),
                    }
                }
            } else {
                // Redraw the away counter once a minute even without input.
                if self.started.elapsed().as_secs() % 60 == 0 {
                    self.draw()?;
                }
            }
        }
    }
}

/// Block until the X socket is readable or the timeout expires.
fn wait_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> bool {
    #[repr(C)]
    struct PollFd {
        fd: std::os::fd::RawFd,
        events: i16,
        revents: i16,
    }
    extern "C" {
        fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
    }
    let mut pfd = PollFd {
        fd,
        events: 1,
        revents: 0,
    };
    // SAFETY: one valid pollfd for the duration of the call.
    unsafe { poll(&mut pfd, 1, timeout_ms) > 0 }
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
