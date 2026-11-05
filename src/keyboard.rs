//! Keyboard input for the password prompt: keysyms, XKB state and compose.
//!
//! A password must be typeable exactly as in a normal X session with the same
//! layout. That needs three things the core protocol does not give directly:
//!
//! * the real XKB level selection (Shift, AltGr/Level3, Level5, NumLock,
//!   group switches), which is what libxkbcommon's `xkb_state` computes;
//! * the locale's compose table, so dead keys (`´ ` ^ ~ ¨` on the Swedish
//!   layout) and the letters they compose (`é`) reach PAM as the same
//!   characters a terminal would have produced when the password was set;
//! * a cached keymap that is refreshed when the server announces a change,
//!   instead of one round trip per key press.
//!
//! libxkbcommon (with its X11 module) and libxcb are loaded at runtime with
//! `dlopen`. They ship with every Xorg installation the suite supports, but
//! a missing library must never make the lock impossible to start: then the
//! cached core keymap and a built-in Latin-1 compose table are used instead.
//!
//! Nothing in this module logs or stores key data beyond the composed text it
//! returns, and its buffers are wiped after use.

use std::ffi::{c_char, c_int, c_void};
use x11rb::{
    protocol::{
        xkb::{self, ConnectionExt as _},
        xproto::{ConnectionExt as _, KeyButMask, KeyPressEvent},
        Event,
    },
    rust_connection::RustConnection,
};
use zeroize::Zeroize;

/// A short UTF-8 string produced by one key press, wiped on drop.
pub struct Text {
    bytes: [u8; 64],
    len: usize,
}

impl Text {
    fn from_str(s: &str) -> Option<Self> {
        if s.is_empty() || s.len() > 64 || s.chars().any(char::is_control) {
            return None;
        }
        let mut bytes = [0u8; 64];
        bytes[..s.len()].copy_from_slice(s.as_bytes());
        Some(Self {
            bytes,
            len: s.len(),
        })
    }

    fn from_char(c: char) -> Option<Self> {
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        let text = Self::from_str(s);
        buf.zeroize();
        text
    }

    /// Copy a NUL-padded C buffer filled by libxkbcommon, then wipe it.
    fn from_c_buffer(buf: &mut [u8; 64], written: c_int) -> Option<Self> {
        let n = usize::try_from(written).ok().filter(|n| *n > 0 && *n < 64);
        let text = n.and_then(|n| std::str::from_utf8(&buf[..n]).ok().and_then(Self::from_str));
        buf.zeroize();
        text
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        self.bytes.zeroize();
        self.len = 0;
    }
}

/// What one key press does to the password entry.
pub enum Key {
    Text(Text),
    Submit,
    Erase,
    Clear,
    Nothing,
}

const XK_BACKSPACE: u32 = 0xff08;
const XK_TAB: u32 = 0xff09;
const XK_RETURN: u32 = 0xff0d;
const XK_ESCAPE: u32 = 0xff1b;
const XK_KP_ENTER: u32 = 0xff8d;
const XK_SPACE: u32 = 0x20;

fn is_modifier(sym: u32) -> bool {
    // Shift_L..Hyper_R, ISO_Lock..ISO_Last_Group_Lock (Level3/Level5 shifts,
    // latches and group switches), Mode_switch and Num_Lock.
    (0xffe1..=0xffee).contains(&sym)
        || (0xfe01..=0xfe13).contains(&sym)
        || sym == 0xff7e
        || sym == 0xff7f
}

fn is_dead(sym: u32) -> bool {
    (0xfe50..=0xfe8f).contains(&sym)
}

/// Keysym to character for the paths that do not use libxkbcommon's UTF-8.
pub fn keysym_char(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym),
        // Unicode keysyms: 0x01000000 + code point.
        0x0100_0100..=0x0110_ffff => char::from_u32(sym - 0x0100_0000),
        // Keypad symbols that produce text.
        0xff80 => Some(' '),
        0xffaa => Some('*'),
        0xffab => Some('+'),
        0xffac => Some(','),
        0xffad => Some('-'),
        0xffae => Some('.'),
        0xffaf => Some('/'),
        0xffb0..=0xffb9 => char::from_u32(sym - 0xffb0 + u32::from(b'0')),
        0xffbd => Some('='),
        _ => None,
    }
    .filter(|c| !c.is_control())
}

/// Result of feeding one keysym to a compose state.
#[derive(Debug, PartialEq, Eq)]
pub enum Compose {
    /// The keysym is not part of a sequence; handle it normally.
    Pass,
    /// The keysym started or continued a sequence; it produces nothing yet.
    Composing,
    /// A complete sequence produced this character.
    Composed(char),
    /// An invalid continuation: the sequence and the keysym are discarded,
    /// exactly as libX11 and libxkbcommon do.
    Cancelled,
}

/// The fallback compose table, used only when the locale's table cannot be
/// loaded through libxkbcommon. It gives the same results as libX11's
/// `en_US.UTF-8` Compose file for the five dead keys of the Swedish and other
/// Latin-1 layouts:
///
/// * dead key + space: the spacing character libX11 gives (`'` for acute and
///   `"` for diaeresis, the literal `` ` ^ ~ `` for the others);
/// * the same dead key twice: the literal accent (`´ ` ^ ~ ¨`);
/// * dead key + a vowel, `n` or `y`: the composed Latin-1 letter in either case;
/// * any other key cancels the sequence and is discarded; modifier keys are
///   ignored while a sequence is pending. Sequences of two different dead
///   keys (three-key sequences in libX11) are not supported and cancel.
#[derive(Default)]
pub struct BuiltinCompose {
    pending: Option<u32>,
}

impl BuiltinCompose {
    pub fn reset(&mut self) {
        self.pending = None;
    }

    pub fn composing(&self) -> bool {
        self.pending.is_some()
    }

    pub fn feed(&mut self, sym: u32) -> Compose {
        if is_modifier(sym) {
            return if self.pending.is_some() {
                Compose::Composing
            } else {
                Compose::Pass
            };
        }
        let Some(dead) = self.pending.take() else {
            if is_dead(sym) {
                self.pending = Some(sym);
                return Compose::Composing;
            }
            return Compose::Pass;
        };
        match builtin_sequence(dead, sym) {
            Some(c) => Compose::Composed(c),
            None => Compose::Cancelled,
        }
    }
}

const DEAD_GRAVE: u32 = 0xfe50;
const DEAD_ACUTE: u32 = 0xfe51;
const DEAD_CIRCUMFLEX: u32 = 0xfe52;
const DEAD_TILDE: u32 = 0xfe53;
const DEAD_DIAERESIS: u32 = 0xfe57;

fn builtin_sequence(dead: u32, sym: u32) -> Option<char> {
    if sym == XK_SPACE {
        return match dead {
            DEAD_GRAVE => Some('`'),
            DEAD_ACUTE => Some('\''),
            DEAD_CIRCUMFLEX => Some('^'),
            DEAD_TILDE => Some('~'),
            DEAD_DIAERESIS => Some('"'),
            _ => None,
        };
    }
    if sym == dead {
        return match dead {
            DEAD_GRAVE => Some('`'),
            DEAD_ACUTE => Some('\u{b4}'),
            DEAD_CIRCUMFLEX => Some('^'),
            DEAD_TILDE => Some('~'),
            DEAD_DIAERESIS => Some('\u{a8}'),
            _ => None,
        };
    }
    let base = char::from_u32(sym).filter(char::is_ascii_alphabetic)?;
    let upper = base.is_ascii_uppercase();
    let lower = base.to_ascii_lowercase();
    let composed = match (dead, lower) {
        (DEAD_GRAVE, 'a') => 'à',
        (DEAD_GRAVE, 'e') => 'è',
        (DEAD_GRAVE, 'i') => 'ì',
        (DEAD_GRAVE, 'o') => 'ò',
        (DEAD_GRAVE, 'u') => 'ù',
        (DEAD_ACUTE, 'a') => 'á',
        (DEAD_ACUTE, 'e') => 'é',
        (DEAD_ACUTE, 'i') => 'í',
        (DEAD_ACUTE, 'o') => 'ó',
        (DEAD_ACUTE, 'u') => 'ú',
        (DEAD_ACUTE, 'y') => 'ý',
        (DEAD_CIRCUMFLEX, 'a') => 'â',
        (DEAD_CIRCUMFLEX, 'e') => 'ê',
        (DEAD_CIRCUMFLEX, 'i') => 'î',
        (DEAD_CIRCUMFLEX, 'o') => 'ô',
        (DEAD_CIRCUMFLEX, 'u') => 'û',
        (DEAD_TILDE, 'a') => 'ã',
        (DEAD_TILDE, 'n') => 'ñ',
        (DEAD_TILDE, 'o') => 'õ',
        (DEAD_DIAERESIS, 'a') => 'ä',
        (DEAD_DIAERESIS, 'e') => 'ë',
        (DEAD_DIAERESIS, 'i') => 'ï',
        (DEAD_DIAERESIS, 'o') => 'ö',
        (DEAD_DIAERESIS, 'u') => 'ü',
        (DEAD_DIAERESIS, 'y') => 'ÿ',
        _ => return None,
    };
    if !upper {
        return Some(composed);
    }
    // ÿ has no Latin-1 capital; libX11 composes U+0178.
    if composed == 'ÿ' {
        return Some('\u{178}');
    }
    composed.to_uppercase().next()
}

// --- libxkbcommon through dlopen -------------------------------------------

type Ptr = *mut c_void;

struct Api {
    context_unref: unsafe extern "C" fn(Ptr),
    keymap_unref: unsafe extern "C" fn(Ptr),
    state_unref: unsafe extern "C" fn(Ptr),
    state_update_mask: unsafe extern "C" fn(Ptr, u32, u32, u32, u32, u32, u32) -> c_int,
    state_key_get_one_sym: unsafe extern "C" fn(Ptr, u32) -> u32,
    state_key_get_utf8: unsafe extern "C" fn(Ptr, u32, *mut c_char, usize) -> c_int,
    compose_table_unref: unsafe extern "C" fn(Ptr),
    compose_state_new: unsafe extern "C" fn(Ptr, c_int) -> Ptr,
    compose_state_unref: unsafe extern "C" fn(Ptr),
    compose_state_feed: unsafe extern "C" fn(Ptr, u32) -> c_int,
    compose_state_reset: unsafe extern "C" fn(Ptr),
    compose_state_get_status: unsafe extern "C" fn(Ptr) -> c_int,
    compose_state_get_utf8: unsafe extern "C" fn(Ptr, *mut c_char, usize) -> c_int,
    x11_keymap_new_from_device: unsafe extern "C" fn(Ptr, Ptr, i32, c_int) -> Ptr,
    x11_state_new_from_device: unsafe extern "C" fn(Ptr, Ptr, i32) -> Ptr,
    xcb_disconnect: unsafe extern "C" fn(Ptr),
}

const XKB_COMPOSE_FEED_ACCEPTED: c_int = 1;
const XKB_COMPOSE_COMPOSING: c_int = 1;
const XKB_COMPOSE_COMPOSED: c_int = 2;
const XKB_COMPOSE_CANCELLED: c_int = 3;

fn open(names: &[&[u8]]) -> Option<Ptr> {
    names.iter().find_map(|name| {
        // SAFETY: a NUL-terminated library name; the handle is never closed.
        let handle = unsafe {
            libc::dlopen(
                name.as_ptr() as *const c_char,
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        (!handle.is_null()).then_some(handle)
    })
}

fn symbol(handle: Ptr, name: &[u8]) -> Option<Ptr> {
    // SAFETY: a valid handle from dlopen and a NUL-terminated symbol name.
    let sym = unsafe { libc::dlsym(handle, name.as_ptr() as *const c_char) };
    (!sym.is_null()).then_some(sym)
}

/// Reinterpret a symbol address as the function pointer type `T`.
///
/// # Safety
/// `T` must be the C function pointer type of the symbol.
unsafe fn cast<T>(sym: Ptr) -> T {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<Ptr>());
    std::mem::transmute_copy(&sym)
}

macro_rules! load {
    ($handle:expr, $name:literal) => {{
        let sym = symbol($handle, $name)?;
        // SAFETY: the symbol has the C signature declared in `Api`, taken
        // from xkbcommon.h, xkbcommon-compose.h, xkbcommon-x11.h and xcb.h.
        unsafe { cast(sym) }
    }};
}

/// The XKB keymap, state and compose state for the core keyboard.
struct Xkb {
    api: Api,
    context: Ptr,
    xcb: Ptr,
    device: i32,
    keymap: Ptr,
    state: Ptr,
    compose_table: Ptr,
    compose: Ptr,
}

impl Xkb {
    fn new() -> Option<Self> {
        let common = open(&[b"libxkbcommon.so.0\0", b"libxkbcommon.so\0"])?;
        let x11 = open(&[b"libxkbcommon-x11.so.0\0", b"libxkbcommon-x11.so\0"])?;
        let xcb = open(&[b"libxcb.so.1\0", b"libxcb.so\0"])?;
        let context_new: unsafe extern "C" fn(c_int) -> Ptr = load!(common, b"xkb_context_new\0");
        let compose_table_new_from_locale: unsafe extern "C" fn(Ptr, *const c_char, c_int) -> Ptr =
            load!(common, b"xkb_compose_table_new_from_locale\0");
        let setup: unsafe extern "C" fn(
            Ptr,
            u16,
            u16,
            c_int,
            *mut u16,
            *mut u16,
            *mut u8,
            *mut u8,
        ) -> c_int = load!(x11, b"xkb_x11_setup_xkb_extension\0");
        let core_device: unsafe extern "C" fn(Ptr) -> i32 =
            load!(x11, b"xkb_x11_get_core_keyboard_device_id\0");
        let xcb_connect: unsafe extern "C" fn(*const c_char, *mut c_int) -> Ptr =
            load!(xcb, b"xcb_connect\0");
        let has_error: unsafe extern "C" fn(Ptr) -> c_int =
            load!(xcb, b"xcb_connection_has_error\0");
        let api = Api {
            context_unref: load!(common, b"xkb_context_unref\0"),
            keymap_unref: load!(common, b"xkb_keymap_unref\0"),
            state_unref: load!(common, b"xkb_state_unref\0"),
            state_update_mask: load!(common, b"xkb_state_update_mask\0"),
            state_key_get_one_sym: load!(common, b"xkb_state_key_get_one_sym\0"),
            state_key_get_utf8: load!(common, b"xkb_state_key_get_utf8\0"),
            compose_table_unref: load!(common, b"xkb_compose_table_unref\0"),
            compose_state_new: load!(common, b"xkb_compose_state_new\0"),
            compose_state_unref: load!(common, b"xkb_compose_state_unref\0"),
            compose_state_feed: load!(common, b"xkb_compose_state_feed\0"),
            compose_state_reset: load!(common, b"xkb_compose_state_reset\0"),
            compose_state_get_status: load!(common, b"xkb_compose_state_get_status\0"),
            compose_state_get_utf8: load!(common, b"xkb_compose_state_get_utf8\0"),
            x11_keymap_new_from_device: load!(x11, b"xkb_x11_keymap_new_from_device\0"),
            x11_state_new_from_device: load!(x11, b"xkb_x11_state_new_from_device\0"),
            xcb_disconnect: load!(xcb, b"xcb_disconnect\0"),
        };
        // SAFETY: plain C calls with valid arguments; every pointer returned
        // is checked before use and released in Drop.
        unsafe {
            // A second, private connection: libxkbcommon-x11 reads the keymap
            // through libxcb. It receives no events and is only used to
            // (re)load the keymap.
            let conn = xcb_connect(std::ptr::null(), std::ptr::null_mut());
            if conn.is_null() {
                return None;
            }
            if has_error(conn) != 0 {
                (api.xcb_disconnect)(conn);
                return None;
            }
            let (mut major, mut minor, mut event, mut error) = (0u16, 0u16, 0u8, 0u8);
            if setup(
                conn, 1, 0, 0, &mut major, &mut minor, &mut event, &mut error,
            ) != 1
            {
                (api.xcb_disconnect)(conn);
                return None;
            }
            let device = core_device(conn);
            let context = context_new(0);
            if device < 0 || context.is_null() {
                if !context.is_null() {
                    (api.context_unref)(context);
                }
                (api.xcb_disconnect)(conn);
                return None;
            }
            // The compose table follows the session's locale, as libX11 does.
            let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
                .unwrap_or_else(|| "C".into());
            let compose_table = std::ffi::CString::new(locale)
                .ok()
                .map(|locale| compose_table_new_from_locale(context, locale.as_ptr(), 0))
                .unwrap_or(std::ptr::null_mut());
            let compose = if compose_table.is_null() {
                std::ptr::null_mut()
            } else {
                (api.compose_state_new)(compose_table, 0)
            };
            let mut xkb = Self {
                api,
                context,
                xcb: conn,
                device,
                keymap: std::ptr::null_mut(),
                state: std::ptr::null_mut(),
                compose_table,
                compose,
            };
            xkb.reload().then_some(xkb)
        }
    }

    /// (Re)load the keymap and the current state from the server.
    fn reload(&mut self) -> bool {
        // SAFETY: context and connection are valid for the lifetime of self;
        // the new objects replace the old ones only when both were created.
        unsafe {
            let keymap =
                (self.api.x11_keymap_new_from_device)(self.context, self.xcb, self.device, 0);
            if keymap.is_null() {
                return false;
            }
            let state = (self.api.x11_state_new_from_device)(keymap, self.xcb, self.device);
            if state.is_null() {
                (self.api.keymap_unref)(keymap);
                return false;
            }
            if !self.state.is_null() {
                (self.api.state_unref)(self.state);
            }
            if !self.keymap.is_null() {
                (self.api.keymap_unref)(self.keymap);
            }
            self.keymap = keymap;
            self.state = state;
        }
        true
    }

    fn update(&mut self, e: &xkb::StateNotifyEvent) {
        // SAFETY: a valid state; plain integer arguments.
        unsafe {
            (self.api.state_update_mask)(
                self.state,
                u32::from(u16::from(e.base_mods)),
                u32::from(u16::from(e.latched_mods)),
                u32::from(u16::from(e.locked_mods)),
                e.base_group as i32 as u32,
                e.latched_group as i32 as u32,
                u32::from(u8::from(e.locked_group)),
            );
        }
    }

    fn reset_compose(&mut self) {
        if !self.compose.is_null() {
            // SAFETY: a valid compose state.
            unsafe { (self.api.compose_state_reset)(self.compose) }
        }
    }

    fn sym(&self, keycode: u32) -> u32 {
        // SAFETY: a valid state; any keycode is accepted.
        unsafe { (self.api.state_key_get_one_sym)(self.state, keycode) }
    }

    fn utf8(&self, keycode: u32) -> Option<Text> {
        let mut buf = [0u8; 64];
        // SAFETY: the buffer is 64 bytes and libxkbcommon NUL-terminates.
        let n = unsafe {
            (self.api.state_key_get_utf8)(
                self.state,
                keycode,
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
            )
        };
        Text::from_c_buffer(&mut buf, n)
    }

    /// Feed the compose state. `None` means there is no compose table.
    fn compose(&mut self, sym: u32) -> Option<std::result::Result<Compose, Text>> {
        if self.compose.is_null() {
            return None;
        }
        // SAFETY: a valid compose state and a 64-byte output buffer.
        unsafe {
            if (self.api.compose_state_feed)(self.compose, sym) != XKB_COMPOSE_FEED_ACCEPTED {
                return Some(Ok(Compose::Pass));
            }
            match (self.api.compose_state_get_status)(self.compose) {
                XKB_COMPOSE_COMPOSING => Some(Ok(Compose::Composing)),
                XKB_COMPOSE_CANCELLED => {
                    (self.api.compose_state_reset)(self.compose);
                    Some(Ok(Compose::Cancelled))
                }
                XKB_COMPOSE_COMPOSED => {
                    let mut buf = [0u8; 64];
                    let n = (self.api.compose_state_get_utf8)(
                        self.compose,
                        buf.as_mut_ptr() as *mut c_char,
                        buf.len(),
                    );
                    (self.api.compose_state_reset)(self.compose);
                    Some(match Text::from_c_buffer(&mut buf, n) {
                        Some(text) => Err(text),
                        // A sequence that produced only a keysym or nothing
                        // printable is handled as cancelled.
                        None => Ok(Compose::Cancelled),
                    })
                }
                _ => Some(Ok(Compose::Pass)),
            }
        }
    }

    fn composing(&self) -> bool {
        // SAFETY: a valid compose state.
        !self.compose.is_null()
            && unsafe { (self.api.compose_state_get_status)(self.compose) } == XKB_COMPOSE_COMPOSING
    }
}

impl Drop for Xkb {
    fn drop(&mut self) {
        // SAFETY: each pointer is owned by self and released exactly once.
        unsafe {
            if !self.compose.is_null() {
                (self.api.compose_state_unref)(self.compose);
            }
            if !self.compose_table.is_null() {
                (self.api.compose_table_unref)(self.compose_table);
            }
            if !self.state.is_null() {
                (self.api.state_unref)(self.state);
            }
            if !self.keymap.is_null() {
                (self.api.keymap_unref)(self.keymap);
            }
            (self.api.context_unref)(self.context);
            (self.api.xcb_disconnect)(self.xcb);
        }
    }
}

// --- the keyboard ------------------------------------------------------------

/// Translates key presses for the prompt.
pub struct Keyboard {
    xkb: Option<Xkb>,
    builtin: BuiltinCompose,
}

impl Keyboard {
    pub fn new(c: &RustConnection) -> Self {
        let events = select_xkb_events(c);
        let xkb = if events { Xkb::new() } else { None };
        Self {
            xkb,
            builtin: BuiltinCompose::default(),
        }
    }

    /// Whether libxkbcommon is in use; for the diagnostic log only.
    pub fn backend(&self) -> &'static str {
        match &self.xkb {
            Some(xkb) if !xkb.compose.is_null() => "xkbcommon with locale compose",
            Some(_) => "xkbcommon with built-in compose",
            None => "core keymap with built-in compose",
        }
    }

    /// Keep the keymap and modifier state current. Returns true when the
    /// event was a keyboard event.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        match event {
            Event::XkbStateNotify(e) => {
                if let Some(xkb) = self.xkb.as_mut() {
                    xkb.update(e);
                }
                true
            }
            Event::XkbMapNotify(_) | Event::XkbNewKeyboardNotify(_) => {
                if let Some(xkb) = self.xkb.as_mut() {
                    // A failed reload keeps the previous keymap.
                    xkb.reload();
                }
                true
            }
            Event::MappingNotify(_) => true,
            _ => false,
        }
    }

    pub fn reset(&mut self) {
        self.builtin.reset();
        if let Some(xkb) = self.xkb.as_mut() {
            xkb.reset_compose();
        }
    }

    fn composing(&self) -> bool {
        self.builtin.composing() || self.xkb.as_ref().is_some_and(Xkb::composing)
    }

    /// Translate one key press. Never logs anything about the key.
    pub fn key(&mut self, c: &RustConnection, e: &KeyPressEvent) -> Key {
        let keycode = u32::from(e.detail);
        let (sym, xkb_text) = match self.xkb.as_ref() {
            Some(xkb) => (xkb.sym(keycode), true),
            None => (core_sym(c, e).unwrap_or(0), false),
        };
        match sym {
            XK_RETURN | XK_KP_ENTER => {
                self.reset();
                return Key::Submit;
            }
            XK_ESCAPE => {
                self.reset();
                return Key::Clear;
            }
            XK_BACKSPACE => {
                // Backspace first cancels a pending dead key, like GTK and
                // Qt; only then does it erase a character.
                if self.composing() {
                    self.reset();
                    return Key::Nothing;
                }
                return Key::Erase;
            }
            XK_TAB | 0 => return Key::Nothing,
            _ => {}
        }
        if let Some(xkb) = self.xkb.as_mut() {
            match xkb.compose(sym) {
                Some(Err(text)) => return Key::Text(text),
                Some(Ok(Compose::Pass)) => return text_key(xkb.utf8(keycode)),
                Some(Ok(_)) => return Key::Nothing,
                None => {}
            }
        }
        match self.builtin.feed(sym) {
            Compose::Composed(c) => text_key(Text::from_char(c)),
            Compose::Composing | Compose::Cancelled => Key::Nothing,
            Compose::Pass if is_dead(sym) || is_modifier(sym) => Key::Nothing,
            Compose::Pass => match self.xkb.as_ref() {
                Some(xkb) if xkb_text => text_key(xkb.utf8(keycode)),
                _ => text_key(keysym_char(sym).and_then(Text::from_char)),
            },
        }
    }
}

fn text_key(text: Option<Text>) -> Key {
    text.map(Key::Text).unwrap_or(Key::Nothing)
}

/// Ask for XKB state and keymap change events on the lock's connection.
fn select_xkb_events(c: &RustConnection) -> bool {
    let supported = c
        .xkb_use_extension(1, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.supported);
    if !supported {
        return false;
    }
    let events = xkb::EventType::NEW_KEYBOARD_NOTIFY
        | xkb::EventType::MAP_NOTIFY
        | xkb::EventType::STATE_NOTIFY;
    let parts = xkb::MapPart::KEY_TYPES
        | xkb::MapPart::KEY_SYMS
        | xkb::MapPart::MODIFIER_MAP
        | xkb::MapPart::EXPLICIT_COMPONENTS
        | xkb::MapPart::KEY_ACTIONS
        | xkb::MapPart::VIRTUAL_MODS
        | xkb::MapPart::VIRTUAL_MOD_MAP;
    c.xkb_select_events(
        xkb::ID::USE_CORE_KBD.into(),
        xkb::EventType::from(0u16),
        events,
        parts,
        parts,
        &xkb::SelectEventsAux::new(),
    )
    .ok()
    .and_then(|cookie| cookie.check().ok())
    .is_some()
}

/// Core-protocol keysym lookup, used only without libxkbcommon.
///
/// A keycode has several keysym levels: unshifted, shifted, AltGr and so
/// on. Caps Lock is applied separately, as X does: it shifts letters but not
/// digits or punctuation.
fn core_sym(c: &RustConnection, e: &KeyPressEvent) -> Option<u32> {
    let map = c.get_keyboard_mapping(e.detail, 1).ok()?.reply().ok()?;
    Some(level_sym(&map.keysyms, e.state, KeyButMask::MOD2))
}

/// Keypad keysyms that NumLock selects: KP_Space..KP_9 and KP_Equal.
fn is_keypad(sym: u32) -> bool {
    (0xff80..=0xffbd).contains(&sym)
}

/// Pick the keysym for a modifier state from one keycode's core keysyms.
///
/// NumLock follows the core protocol rule: on a key whose second keysym is
/// a keypad keysym, NumLock selects the second keysym, and Shift reverses
/// that choice. Without it `KP_1` arrived as `KP_End` and keypad digits were
/// dropped.
fn level_sym(syms: &[u32], state: KeyButMask, numlock: KeyButMask) -> u32 {
    let shift = state.contains(KeyButMask::SHIFT);
    let lock = state.contains(KeyButMask::LOCK);
    let altgr = state.contains(KeyButMask::MOD5);
    let num = u16::from(numlock) != 0 && state.contains(numlock);
    if !altgr && num && syms.get(1).copied().is_some_and(is_keypad) {
        let index = usize::from(num != shift);
        return syms.get(index).copied().unwrap_or(0);
    }
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
    if lock {
        sym = caps(sym);
    }
    sym
}

/// Caps Lock swaps the case of alphabetic Latin-1 keysyms only; ÷ and × are
/// not letters.
fn caps(sym: u32) -> u32 {
    if (0x61..=0x7a).contains(&sym) || ((0xe0..=0xfe).contains(&sym) && sym != 0xf7) {
        sym - 0x20
    } else if (0x41..=0x5a).contains(&sym) || ((0xc0..=0xde).contains(&sym) && sym != 0xd7) {
        sym + 0x20
    } else {
        sym
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(syms: &[u32]) -> Vec<Compose> {
        let mut compose = BuiltinCompose::default();
        syms.iter().map(|sym| compose.feed(*sym)).collect()
    }

    fn composed(dead: u32, sym: u32) -> Option<char> {
        match feed_all(&[dead, sym]).pop() {
            Some(Compose::Composed(c)) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn dead_key_and_space_match_libx11() {
        assert_eq!(composed(DEAD_GRAVE, XK_SPACE), Some('`'));
        assert_eq!(composed(DEAD_ACUTE, XK_SPACE), Some('\''));
        assert_eq!(composed(DEAD_CIRCUMFLEX, XK_SPACE), Some('^'));
        assert_eq!(composed(DEAD_TILDE, XK_SPACE), Some('~'));
        assert_eq!(composed(DEAD_DIAERESIS, XK_SPACE), Some('"'));
    }

    #[test]
    fn doubled_dead_key_gives_the_accent() {
        assert_eq!(composed(DEAD_GRAVE, DEAD_GRAVE), Some('`'));
        assert_eq!(composed(DEAD_ACUTE, DEAD_ACUTE), Some('´'));
        assert_eq!(composed(DEAD_CIRCUMFLEX, DEAD_CIRCUMFLEX), Some('^'));
        assert_eq!(composed(DEAD_TILDE, DEAD_TILDE), Some('~'));
        assert_eq!(composed(DEAD_DIAERESIS, DEAD_DIAERESIS), Some('¨'));
    }

    #[test]
    fn dead_key_and_letter_compose_in_both_cases() {
        let e = u32::from(b'e');
        assert_eq!(composed(DEAD_ACUTE, e), Some('é'));
        assert_eq!(composed(DEAD_ACUTE, u32::from(b'E')), Some('É'));
        assert_eq!(composed(DEAD_GRAVE, u32::from(b'a')), Some('à'));
        assert_eq!(composed(DEAD_CIRCUMFLEX, u32::from(b'O')), Some('Ô'));
        assert_eq!(composed(DEAD_TILDE, u32::from(b'n')), Some('ñ'));
        assert_eq!(composed(DEAD_TILDE, u32::from(b'N')), Some('Ñ'));
        assert_eq!(composed(DEAD_DIAERESIS, u32::from(b'u')), Some('ü'));
        assert_eq!(composed(DEAD_DIAERESIS, u32::from(b'y')), Some('ÿ'));
        assert_eq!(composed(DEAD_DIAERESIS, u32::from(b'Y')), Some('Ÿ'));
        assert_eq!(composed(DEAD_ACUTE, u32::from(b'Y')), Some('Ý'));
    }

    #[test]
    fn invalid_sequence_is_discarded_like_libx11() {
        assert_eq!(
            feed_all(&[DEAD_ACUTE, u32::from(b'x')]),
            [Compose::Composing, Compose::Cancelled]
        );
        // Two different dead keys cancel in the fallback table.
        assert_eq!(
            feed_all(&[DEAD_ACUTE, DEAD_TILDE]),
            [Compose::Composing, Compose::Cancelled]
        );
        // After a cancel the next key is ordinary again.
        assert_eq!(
            feed_all(&[DEAD_ACUTE, u32::from(b'x'), u32::from(b'x')]),
            [Compose::Composing, Compose::Cancelled, Compose::Pass]
        );
    }

    #[test]
    fn modifiers_do_not_break_a_sequence() {
        // dead_acute, Shift_L, E gives É.
        assert_eq!(
            feed_all(&[DEAD_ACUTE, 0xffe1, u32::from(b'E')]),
            [
                Compose::Composing,
                Compose::Composing,
                Compose::Composed('É')
            ]
        );
        // AltGr (ISO_Level3_Shift) on its own passes through.
        assert_eq!(feed_all(&[0xfe03]), [Compose::Pass]);
    }

    #[test]
    fn ordinary_keys_pass_through() {
        assert_eq!(feed_all(&[u32::from(b'a')]), [Compose::Pass]);
        assert_eq!(keysym_char(u32::from(b'a')), Some('a'));
        assert_eq!(keysym_char(0xe5), Some('å'));
        assert_eq!(keysym_char(0x0100_20ac), Some('€'));
        assert_eq!(keysym_char(XK_RETURN), None);
    }

    #[test]
    fn text_buffers_reject_control_characters() {
        assert!(Text::from_str("\u{8}").is_none());
        assert!(Text::from_str("").is_none());
        assert_eq!(Text::from_str("é").unwrap().as_str(), "é");
        let mut buf = [0u8; 64];
        buf[..2].copy_from_slice("é".as_bytes());
        let text = Text::from_c_buffer(&mut buf, 2).unwrap();
        assert_eq!(text.as_str(), "é");
        assert!(buf.iter().all(|b| *b == 0), "the C buffer was not wiped");
    }

    #[test]
    fn numlock_selects_keypad_digits() {
        // Keycode 87 on a pc105 keymap: KP_End, KP_1.
        let kp1 = [0xff9c, 0xffb1];
        let num = KeyButMask::MOD2;
        assert_eq!(level_sym(&kp1, KeyButMask::from(0u16), num), 0xff9c);
        assert_eq!(level_sym(&kp1, num, num), 0xffb1);
        // Shift reverses NumLock, as in the core protocol.
        assert_eq!(level_sym(&kp1, num | KeyButMask::SHIFT, num), 0xff9c);
        assert_eq!(level_sym(&kp1, KeyButMask::SHIFT, num), 0xffb1);
        assert_eq!(keysym_char(0xffb1), Some('1'));
        assert_eq!(keysym_char(0xff9c), None);
        // NumLock never changes ordinary keys.
        let a = [u32::from(b'a'), u32::from(b'A')];
        assert_eq!(level_sym(&a, num, num), u32::from(b'a'));
    }

    #[test]
    fn keypad_operators_and_enter_map() {
        for (sym, c) in [
            (0xffb0, '0'),
            (0xffb9, '9'),
            (0xffae, '.'),
            (0xffac, ','),
            (0xffab, '+'),
            (0xffad, '-'),
            (0xffaa, '*'),
            (0xffaf, '/'),
        ] {
            assert_eq!(keysym_char(sym), Some(c));
        }
        assert_eq!(keysym_char(XK_KP_ENTER), None, "KP_Enter submits instead");
    }

    #[test]
    fn caps_lock_shifts_letters_only() {
        assert_eq!(caps(u32::from(b'a')), u32::from(b'A'));
        assert_eq!(caps(0xe5), 0xc5);
        assert_eq!(caps(0xf7), 0xf7);
        assert_eq!(caps(u32::from(b'1')), u32::from(b'1'));
    }
}
