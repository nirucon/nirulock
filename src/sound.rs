//! Lock-screen sounds (`auth-fail`, `lock`, `unlock`) through NIRUNOTIFY.
//!
//! NIRUNOTIFY owns the NIRUX system sounds and their settings in
//! `~/.config/nirux/sounds.toml`. Asking it is one line on its control
//! socket, sent from a short-lived thread so that the lock's event loop can
//! never wait for it. Nothing here touches authentication: the sound is
//! requested after PAM has answered, and its result is ignored.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
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
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
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

#[cfg(test)]
mod tests {
    use super::*;
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
