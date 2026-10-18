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
        };
        lock.grab_input()?;
        lock.draw()?;
        Ok(lock)
    }

    /// Grab the keyboard and pointer so input cannot reach other windows.
    ///
    /// `OwnerGrabButton` keeps the buttons grabbed after a key press; without
    /// it X releases the pointer grab and a click could focus another window.
    fn grab_input(&self) -> Result<()> {
        let root = self.surface.c.setup().roots[self.surface.screen_num].root;
        self.surface.c.grab_keyboard(
            true,
            root,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )?;
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
        // Also grab the modifier keys individually: if the keyboard grab is
        // broken by another client, Escape and the modifiers must still land
        // on the lock window instead of a shell.
        for code in 8..=255u8 {
            let _ = self.surface.c.grab_key(
                true,
                root,
                ModMask::ANY,
                code,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            );
        }
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
                // Accepted: release the grabs so the session can continue.
                self.surface.release_input()?;
                self.surface.c.ungrab_key(
                    0,
                    self.surface.c.setup().roots[self.surface.screen_num].root,
                    ModMask::ANY,
                )?;
                self.surface.c.destroy_window(self.surface.win)?;
                self.surface.c.flush()?;
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
        self.state = State::Prompt;
        self.last_minute.clear();
        self.draw()?;
        Ok(())
    }

    fn handle_key(&mut self, e: KeyPressEvent) -> Result<bool> {
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
        loop {
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
                while let Some(event) = self.surface.c.poll_for_event()? {
                    match event {
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
