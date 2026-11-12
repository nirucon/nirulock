//! Lock-screen sounds (`auth-fail`, `lock`, `unlock`, `lock-typing`) through
//! NIRUNOTIFY.
//!
//! NIRUNOTIFY owns the NIRUX system sounds and their settings in
//! `~/.config/nirux/sounds.toml`. Asking it is one line on its control
//! socket, sent from a short-lived thread so that the lock's event loop can
//! never wait for it. Nothing here touches authentication: the sound is
//! requested after PAM has answered, and its result is ignored.
//!
//! `lock-typing` is a typewriter click for each key that changes the password
//! entry. Keys hand it to one long-lived sender thread through a small queue
//! that drops rather than waits, so typing is never slowed down; a held key's
//! auto-repeat and anything faster than typing are left silent.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc::{self, SyncSender},
    time::Duration,
};

fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(format!("nirunotify-{}.sock", unsafe { libc::getuid() }))
}

/// The line sent for EVENT.
pub fn request(event: &str) -> String {
    format!("sound {event}\n")
}

fn send(event: &str) {
    send_to(&socket_path(), event);
}

fn send_to(socket: &std::path::Path, event: &str) {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return;
    };
    let limit = Some(Duration::from_millis(300));
    let _ = stream.set_write_timeout(limit);
    let _ = stream.set_read_timeout(limit);
    if stream.write_all(request(event).as_bytes()).is_ok() {
        let mut reply = [0u8; 64];
        let _ = stream.read(&mut reply);
    }
}

/// Ask for EVENT without waiting.
pub fn play(event: &'static str) {
    let _ = std::thread::Builder::new()
        .name("nirulock-sound".into())
        .spawn(move || send(event));
}

/// Ask for EVENT and wait up to LIMIT for the request to be delivered, for
/// the moment just before the process exits.
pub fn play_before_exit(event: &'static str, limit: Duration) {
    let (done, wait) = mpsc::channel();
    if std::thread::Builder::new()
        .name("nirulock-sound".into())
        .spawn(move || {
            send(event);
            let _ = done.send(());
        })
        .is_ok()
    {
        let _ = wait.recv_timeout(limit);
    }
}

/// The same key again within this many milliseconds is a held key's
/// auto-repeat (X repeats at 25 to 40 keys a second), not typing.
const REPEAT_MS: u32 = 100;
/// At most one click per this many milliseconds, whatever the keys.
const GAP_MS: u32 = 35;

/// Typing clicks. Times are the X server's key event timestamps.
#[derive(Default)]
pub struct Typing {
    /// The previous key press (keycode and time), clicked or not. Kept in
    /// memory only, never logged.
    last_key: Option<(u8, u32)>,
    last_click: Option<u32>,
    sender: Option<SyncSender<()>>,
}

impl Typing {
    /// Whether a key press that changed the entry should click.
    fn allow(&mut self, keycode: u8, time: u32) -> bool {
        let repeat = matches!(self.last_key,
            Some((key, at)) if key == keycode && time.wrapping_sub(at) < REPEAT_MS);
        self.last_key = Some((keycode, time));
        if repeat {
            return false;
        }
        if self
            .last_click
            .is_some_and(|at| time.wrapping_sub(at) < GAP_MS)
        {
            return false;
        }
        self.last_click = Some(time);
        true
    }

    /// A key press that did not change the entry still counts for repeats.
    pub fn other_key(&mut self, keycode: u8, time: u32) {
        self.last_key = Some((keycode, time));
    }

    /// A key press that changed the entry: ask for a click without waiting.
    pub fn key(&mut self, keycode: u8, time: u32) {
        if !self.allow(keycode, time) {
            return;
        }
        if self.sender.is_none() {
            let (tx, rx) = mpsc::sync_channel::<()>(4);
            if std::thread::Builder::new()
                .name("nirulock-typing".into())
                .spawn(move || {
                    while rx.recv().is_ok() {
                        send("lock-typing");
                    }
                })
                .is_ok()
            {
                self.sender = Some(tx);
            }
        }
        if let Some(sender) = &self.sender {
            // A full queue (a daemon that does not answer) drops the click.
            let _ = sender.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_clicks_but_held_keys_and_floods_do_not() {
        let mut typing = Typing::default();
        // Normal typing, about 15 keys a second, with a double letter.
        let mut clicks = 0;
        for (n, key) in [38u8, 39, 40, 40, 41, 22, 42].iter().enumerate() {
            clicks += u32::from(typing.allow(*key, 1000 + 130 * n as u32));
        }
        assert_eq!(clicks, 7);
        // Holding a key: the press, the first repeat after X's delay (it looks
        // like a second press), then silence through the auto-repeat at 30 a
        // second.
        let mut clicks = 0;
        for n in 0..40u32 {
            let at = 5000 + if n == 0 { 0 } else { 500 + 33 * n };
            clicks += u32::from(typing.allow(55, at));
        }
        assert_eq!(clicks, 2);
        // Different keys faster than anyone types are capped.
        let mut clicks = 0;
        for n in 0..20u32 {
            clicks += u32::from(typing.allow(60 + n as u8, 9000 + 10 * n));
        }
        assert!((5..=7).contains(&clicks), "{clicks}");
        // The server clock wraps after 49 days.
        let mut typing = Typing::default();
        assert!(typing.allow(38, u32::MAX - 10));
        assert!(typing.allow(39, 200));
        // Enter, arrows and the like keep a held key from clicking again.
        typing.other_key(39, 300);
        assert!(!typing.allow(39, 350));
    }

    #[test]
    fn a_typing_click_reaches_the_daemon_without_waiting() {
        let dir = std::env::temp_dir().join(format!("nirulock-typing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join(format!("nirunotify-{}.sock", unsafe { libc::getuid() }));
        let listener = UnixListener::bind(&socket).unwrap();
        // Our own sender thread, pointed at the test socket.
        let (tx, rx) = mpsc::sync_channel::<()>(4);
        let target = socket.clone();
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                send_to(&target, "lock-typing");
            }
        });
        let mut typing = Typing {
            sender: Some(tx),
            ..Typing::default()
        };
        let start = std::time::Instant::now();
        typing.key(38, 1000);
        assert!(
            start.elapsed() < Duration::from_millis(20),
            "the key waited"
        );
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = [0u8; 64];
        let n = stream.read(&mut line).unwrap();
        stream.write_all(b"ok\n").unwrap();
        assert_eq!(&line[..n], b"sound lock-typing\n");
        let _ = std::fs::remove_dir_all(dir);
    }
    use std::os::unix::net::UnixListener;

    #[test]
    fn the_request_reaches_a_listening_daemon() {
        let dir = std::env::temp_dir().join(format!("nirulock-sound-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let listener = UnixListener::bind(socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = [0u8; 64];
            let n = stream.read(&mut line).unwrap();
            stream.write_all(b"ok\n").unwrap();
            String::from_utf8_lossy(&line[..n]).to_string()
        });
        play_before_exit("auth-fail", Duration::from_secs(2));
        assert_eq!(server.join().unwrap(), "sound auth-fail\n");
        // Without a daemon nothing waits longer than the limit.
        std::fs::remove_file(socket_path()).unwrap();
        let start = std::time::Instant::now();
        play_before_exit("unlock", Duration::from_millis(200));
        assert!(start.elapsed() < Duration::from_millis(400));
        let _ = std::fs::remove_dir_all(dir);
    }
}
