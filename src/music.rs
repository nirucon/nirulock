//! `[pause_music]`: music while the screen is locked.
//!
//! On by default, but only for the lock reasons in `on` (default: `manual`,
//! see [`crate::reason`]): a lock started by idle, suspend or the lid stays
//! silent, so a machine that locks itself in a meeting plays nothing. For
//! those reasons the lock waits [`GATE_WINDOW`] after it is
//! shown and starts the music only if the folder has tunes with a player and
//! no other audio played during that window ([`StartGate`]).
//!
//! The folder (default [`DEFAULT_FOLDER`], where NIRUX installs its own
//! chiptunes) may hold C64 SID tunes (`.sid`, played by `sidplayfp`) and
//! audio files (`.ogg`, `.oga`, `.opus`, `.flac`, `.wav`, played by the
//! first of `pw-play`, `ffplay` and `mpv`; see [`AudioPlayer`]), mixed. By
//! default (`shuffle = true`) they play in a shuffled order that starts with
//! a random tune and never plays the same tune twice in a row; with
//! `shuffle = false` in natural order. Either way they loop: a SID tune (its
//! default subtune) for its HVSC `Songlengths.md5` length or
//! `track_seconds`, an audio file to its end ([`AUDIO_LIMIT`] at most). A
//! tune without a player is skipped.
//!
//! The scan reads directory entries only, never a tune's contents (a tune is
//! read just before it plays), and stops after [`SCAN_BUDGET`] or
//! [`MAX_TUNES`]: a whole HVSC tree (about 60 000 files) is listed in a
//! fraction of a second on a warm cache, and a cold disk cannot make the
//! music miss its start window. It runs on the music thread, so it never
//! delays the lock.
//!
//! Isolation from the lock:
//!
//! - The player is an ordinary child process of the user (the lock has no
//!   privileges to drop), in its own session and so its own process group,
//!   without a controlling terminal, with stdin, stdout and stderr on
//!   `/dev/null`. sidplayfp opens `/dev/tty` for its keyboard controls; on a
//!   session started from a text console a background process group there
//!   is stopped by SIGTTOU/SIGTTIN, which left the lock showing a tune while
//!   nothing played. Its arguments are fixed options, a time, a subtune and
//!   a path; nothing typed on the lock screen ever reaches it.
//! - Everything that touches the player (scanning, spawning, waiting,
//!   killing) runs on the `nirulock-music` thread. The lock's event loop only
//!   sends a message and signals the process group, which never blocks, and
//!   reads the info line under a mutex.
//! - A player that crashes, hangs or never starts changes nothing for the
//!   lock: the thread moves on to the next tune or gives up quietly.
//! - The child gets `SIGKILL` when the thread that started it ends
//!   (`PR_SET_PDEATHSIG`), so the music never outlives the lock, even if the
//!   lock is killed.
//!
//! It stops (TERM, then KILL) on unlock, on Esc, before suspend (logind's
//! `PrepareForSleep`, watched with `dbus-monitor`) and, unless
//! `stop_on_display_off = false`, when the display goes off. Once stopped it
//! stays stopped for the rest of this lock.

use crate::audio::Heard;
use crate::reason::Reason;
use crate::sid;
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// The `[pause_music]` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    /// As written; `~/` is expanded by [`Settings::folder_path`].
    pub folder: String,
    pub track_seconds: u32,
    pub stop_on_display_off: bool,
    /// The lock reasons that may play; by default only a manual lock.
    pub on: Vec<Reason>,
    /// A shuffled order (the default) or natural order.
    pub shuffle: bool,
}

/// NIRUX installs its own music here; `folder = "~/Music/SID"` (or any
/// folder) plays another collection.
pub const DEFAULT_FOLDER: &str = "~/.local/share/nirux/music";
pub const DEFAULT_TRACK_SECONDS: u32 = 180;

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            folder: DEFAULT_FOLDER.to_string(),
            track_seconds: DEFAULT_TRACK_SECONDS,
            stop_on_display_off: true,
            on: vec![Reason::Manual],
            shuffle: true,
        }
    }
}

impl Settings {
    /// Read the `[pause_music]` table.
    pub fn from_value(table: &toml::Value) -> Result<Self, String> {
        let mut s = Self::default();
        let Some(table) = table.as_table() else {
            return Err("pause_music must be a table".into());
        };
        for (key, value) in table {
            match key.as_str() {
                "enabled" => {
                    s.enabled = value
                        .as_bool()
                        .ok_or("pause_music.enabled must be true or false")?
                }
                "folder" => {
                    s.folder = value
                        .as_str()
                        .filter(|f| !f.trim().is_empty())
                        .ok_or("pause_music.folder must be a path")?
                        .to_string()
                }
                "track_seconds" => {
                    s.track_seconds = value
                        .as_integer()
                        .filter(|n| (5..=3600).contains(n))
                        .ok_or("pause_music.track_seconds must be 5 to 3600")?
                        as u32
                }
                "stop_on_display_off" => {
                    s.stop_on_display_off = value
                        .as_bool()
                        .ok_or("pause_music.stop_on_display_off must be true or false")?
                }
                "on" => s.on = reasons(value)?,
                "shuffle" => {
                    s.shuffle = value
                        .as_bool()
                        .ok_or("pause_music.shuffle must be true or false")?
                }
                other => eprintln!("nirulock: pause_music: unknown setting {other:?} ignored"),
            }
        }
        Ok(s)
    }

    /// Whether a lock started for `reason` may play.
    pub fn plays_for(&self, reason: Reason) -> bool {
        self.enabled && self.on.contains(&reason)
    }

    pub fn folder_path(&self) -> PathBuf {
        expand_home(&self.folder, std::env::var_os("HOME").map(PathBuf::from))
    }

    pub fn describe(&self) -> String {
        if self.enabled {
            let on = if self.on.is_empty() {
                "no locks (pause_music.on is empty)".to_string()
            } else {
                let names: Vec<&str> = self.on.iter().map(|r| r.name()).collect();
                format!("{} locks", names.join(", "))
            };
            format!(
                "on for {} (folder {}, {}, {} s per SID tune without Songlengths.md5, {})",
                on,
                self.folder,
                if self.shuffle {
                    "shuffled"
                } else {
                    "natural order"
                },
                self.track_seconds,
                if self.stop_on_display_off {
                    "stops when the display goes off"
                } else {
                    "keeps playing with the display off"
                }
            )
        } else {
            "off".into()
        }
    }
}

/// `on = ["manual", ...]`: a list of lock reasons, duplicates ignored.
fn reasons(value: &toml::Value) -> Result<Vec<Reason>, String> {
    let wrong = || format!("pause_music.on must be a list of {}", Reason::names());
    let mut out = Vec::new();
    for item in value.as_array().ok_or_else(wrong)? {
        let reason = item.as_str().and_then(Reason::parse).ok_or_else(wrong)?;
        if !out.contains(&reason) {
            out.push(reason);
        }
    }
    Ok(out)
}

fn expand_home(folder: &str, home: Option<PathBuf>) -> PathBuf {
    match (folder.strip_prefix("~/"), folder == "~", home) {
        (Some(rest), _, Some(home)) => home.join(rest),
        (None, true, Some(home)) => home,
        _ => PathBuf::from(folder),
    }
}

// ---- the start decision -------------------------------------------------------

/// How long after locking other audio must stay away before the music starts.
pub const GATE_WINDOW: Duration = Duration::from_secs(2);
/// A stream that is present but not (yet) loud, such as an idle synthesizer,
/// delays the decision at most until here; then it counts as silence.
pub const GATE_MAX: Duration = Duration::from_secs(5);
/// How often the undecided gate is looked at.
pub const GATE_STEP: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Look again at [`StartGate::deadline`].
    Wait,
    /// Start the music now.
    Start,
    /// Not during this lock.
    Skip,
}

/// "No other audio is playing during the gate window after locking", free of
/// I/O so that it can be tested.
#[derive(Debug)]
pub struct StartGate {
    since: Instant,
    decided: Option<Decision>,
}

impl StartGate {
    pub fn new(now: Instant) -> Self {
        Self {
            since: now,
            decided: None,
        }
    }

    pub fn decided(&self) -> Option<Decision> {
        self.decided
    }

    /// One look. `other` is what is heard from other processes;
    /// `display_on` whether the display is on (an off display only holds
    /// the decision, so a display that wakes a moment late does not cost
    /// the music); `interrupted` is true when the machine slept or Esc was
    /// pressed since locking.
    pub fn step(
        &mut self,
        other: Heard,
        display_on: bool,
        interrupted: bool,
        now: Instant,
    ) -> Decision {
        if let Some(decided) = self.decided {
            return decided;
        }
        let elapsed = now.saturating_duration_since(self.since);
        let decision = if interrupted || other == Heard::Playing {
            Decision::Skip
        } else if !display_on {
            if elapsed >= GATE_MAX {
                Decision::Skip
            } else {
                Decision::Wait
            }
        } else if (other == Heard::Silent && elapsed >= GATE_WINDOW) || elapsed >= GATE_MAX {
            Decision::Start
        } else {
            Decision::Wait
        };
        if decision != Decision::Wait {
            self.decided = Some(decision);
        }
        decision
    }

    /// When to look again while undecided.
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        if self.decided.is_some() {
            return None;
        }
        let first = self.since + GATE_WINDOW;
        Some(if now < first { first } else { now + GATE_STEP })
    }
}

// ---- the player thread ---------------------------------------------------------

enum Request {
    Play(Option<crate::audio::Exclusions>),
    Stop,
}

struct Shared {
    /// The info line of the tune that plays and its "Artist - Title", if
    /// any.
    info: Mutex<Option<(String, String)>>,
    /// The playing child's process group, 0 when none.
    pgid: AtomicI32,
    /// The folder has tunes with a player.
    ready: AtomicBool,
    /// The scan is done (ready or not).
    scanned: AtomicBool,
    stopped: AtomicBool,
}

/// Handle to the music thread. Dropping it stops the music.
pub struct Music {
    shared: Arc<Shared>,
    send: mpsc::Sender<Request>,
    /// Readable when the info line changed; `None` once the thread is gone
    /// (a closed socket would read as ready forever).
    wake: Option<UnixStream>,
    gate: StartGate,
    _sleep: Option<SleepWatch>,
    slept: Arc<AtomicBool>,
    /// Boot and monotonic clocks when the lock started: a suspend shows up
    /// as a gap between them, even when no signal was seen.
    clocks: (Duration, Duration),
}

/// What [`Music::tick`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    Nothing,
    Started,
    Skipped(&'static str),
}

impl Music {
    /// Scan the folder and look for the players on the music thread; the
    /// caller decides when to [`Music::play`].
    pub fn start(settings: &Settings) -> Option<Self> {
        let (wake, notify) = UnixStream::pair().ok()?;
        wake.set_nonblocking(true).ok()?;
        notify.set_nonblocking(true).ok()?;
        let shared = Arc::new(Shared {
            info: Mutex::new(None),
            pgid: AtomicI32::new(0),
            ready: AtomicBool::new(false),
            scanned: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        let (send, receive) = mpsc::channel();
        let folder = settings.folder_path();
        let track_seconds = settings.track_seconds;
        let shuffle = settings.shuffle;
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("nirulock-music".into())
            .spawn(move || {
                player(
                    &thread_shared,
                    &folder,
                    track_seconds,
                    shuffle,
                    &receive,
                    notify,
                );
            })
            .ok()?;
        let slept = Arc::new(AtomicBool::new(false));
        let sleep = SleepWatch::start(Arc::clone(&slept), Arc::clone(&shared));
        Some(Self {
            shared,
            send,
            wake: Some(wake),
            gate: StartGate::new(Instant::now()),
            _sleep: sleep,
            slept,
            clocks: clocks(),
        })
    }

    /// Follow the gate. `other` is what is heard from other processes.
    pub fn tick(
        &mut self,
        other: Heard,
        display_on: bool,
        audio: Option<crate::audio::Exclusions>,
    ) -> Tick {
        if self.gate.decided().is_some() {
            return Tick::Nothing;
        }
        let interrupted = self.slept() || self.stopped();
        let now = Instant::now();
        // Without tunes or player there is nothing to wait for; the scan
        // runs while the window is open and is short.
        let decision = self.gate.step(other, display_on, interrupted, now);
        match decision {
            Decision::Wait => Tick::Nothing,
            Decision::Skip => {
                let _ = self.send.send(Request::Stop);
                Tick::Skipped(if interrupted {
                    "stopped before it started (sleep or the lock screen)"
                } else if display_on {
                    "other audio is playing"
                } else {
                    "the display stayed off"
                })
            }
            Decision::Start => {
                if !self.shared.scanned.load(Ordering::Acquire) {
                    // Still scanning (a very large folder): give up quietly
                    // rather than block or start late.
                    self.gate.decided = Some(Decision::Skip);
                    return Tick::Skipped("the folder scan did not finish in time");
                }
                if !self.shared.ready.load(Ordering::Acquire) {
                    return Tick::Skipped("no tunes, or no player for them");
                }
                let _ = self.send.send(Request::Play(audio));
                Tick::Started
            }
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.gate.deadline(Instant::now())
    }

    pub fn wake_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.wake.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Consume the wake-up bytes; forget the socket when the thread has
    /// closed its end.
    pub fn woke(&mut self) {
        let mut buf = [0u8; 64];
        let Some(wake) = self.wake.as_mut() else {
            return;
        };
        loop {
            match std::io::Read::read(wake, &mut buf) {
                Ok(0) => {
                    self.wake = None;
                    return;
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    /// The info line while a tune plays.
    pub fn info(&self) -> Option<String> {
        if self.stopped() {
            return None;
        }
        self.shared
            .info
            .lock()
            .ok()
            .and_then(|info| info.as_ref().map(|i| i.0.clone()))
    }

    /// The tune that plays as "Artist - Title" ([`sid::tune_name`]).
    pub fn tune(&self) -> Option<String> {
        if self.stopped() {
            return None;
        }
        self.shared
            .info
            .lock()
            .ok()
            .and_then(|info| info.as_ref().map(|i| i.1.clone()))
    }

    pub fn stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::Acquire)
    }

    fn slept(&self) -> bool {
        if self.slept.load(Ordering::Acquire) {
            return true;
        }
        let (boot, mono) = clocks();
        let gap = boot.saturating_sub(self.clocks.0);
        let ran = mono.saturating_sub(self.clocks.1);
        gap.saturating_sub(ran) > Duration::from_secs(2)
    }

    /// Stop for the rest of this lock. Never blocks: the process group gets
    /// TERM here, and the thread follows up with KILL and reaps it.
    pub fn stop(&mut self) {
        stop_now(&self.shared);
        self.gate.decided.get_or_insert(Decision::Skip);
        let _ = self.send.send(Request::Stop);
    }
}

impl Drop for Music {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stop_now(shared: &Shared) {
    shared.stopped.store(true, Ordering::Release);
    signal_group(shared.pgid.load(Ordering::Acquire), libc::SIGTERM);
    if let Ok(mut info) = shared.info.lock() {
        *info = None;
    }
}

fn signal_group(pgid: i32, signal: i32) {
    if pgid > 1 {
        // SAFETY: plain kill(2) on a process group the lock created.
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
}

/// CLOCK_BOOTTIME and CLOCK_MONOTONIC.
fn clocks() -> (Duration, Duration) {
    let read = |id| {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid timespec out-pointer.
        unsafe { libc::clock_gettime(id, &mut ts) };
        Duration::new(
            ts.tv_sec.max(0) as u64,
            ts.tv_nsec.clamp(0, 999_999_999) as u32,
        )
    };
    (read(libc::CLOCK_BOOTTIME), read(libc::CLOCK_MONOTONIC))
}

/// The first executable `name` on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// At most this many tunes and directory levels are looked at: room for
/// the whole HVSC (about 60 000 tunes) and then some.
pub const MAX_TUNES: usize = 150_000;
const MAX_DEPTH: usize = 8;
/// The scan stops after this long and plays what it found: the start gate
/// decides after [`GATE_WINDOW`], and a slow disk must not make it skip.
pub const SCAN_BUDGET: Duration = Duration::from_millis(1500);

/// How a tune is played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A C64 SID tune, played by `sidplayfp`.
    Sid,
    /// An audio file, played by an [`AudioPlayer`].
    Audio,
}

/// The audio file extensions, matched without case.
pub const AUDIO_EXTENSIONS: [&str; 5] = ["ogg", "oga", "opus", "flac", "wav"];

/// The kind of tune `path` is, by its extension; `None` for any other file.
pub fn kind(path: &Path) -> Option<Kind> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("sid") {
        Some(Kind::Sid)
    } else if AUDIO_EXTENSIONS
        .iter()
        .any(|e| extension.eq_ignore_ascii_case(e))
    {
        Some(Kind::Audio)
    } else {
        None
    }
}

/// The programs that play audio files, in order of preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioPlayer {
    /// PipeWire's own player.
    PwPlay,
    Ffplay,
    Mpv,
}

impl AudioPlayer {
    pub const ALL: [Self; 3] = [Self::PwPlay, Self::Ffplay, Self::Mpv];

    pub fn program(self) -> &'static str {
        match self {
            Self::PwPlay => "pw-play",
            Self::Ffplay => "ffplay",
            Self::Mpv => "mpv",
        }
    }
}

/// `pw-play` streams carry no process id of their own (only its PipeWire
/// client does, which the PulseAudio view does not show), so the stream
/// is given its player's pid: the shell `exec`s the player, which keeps
/// the shell's pid `$$`. `$0` is the player, `$1` the file.
const PW_PLAY_SCRIPT: &str = r#"exec "$0" -P "{ application.process.id = $$ }" "$1""#;

/// The program and arguments that play the audio `file` with `player`
/// (found at `program`). Every player plays the file once, to its end,
/// without a window, a terminal or output, and its stream carries the
/// player's process id as `application.process.id`, so that the lock's own
/// music is never "other audio" ([`crate::audio::Exclusions`]): ffplay is a
/// PulseAudio client, and mpv is asked to be one where it can.
pub fn audio_command(player: AudioPlayer, program: &Path, file: &Path) -> (PathBuf, Vec<OsString>) {
    // A relative path that starts with `-` must not read as an option.
    let file = if file.is_relative() {
        Path::new(".").join(file).into_os_string()
    } else {
        file.as_os_str().to_owned()
    };
    let args = |options: &[&str]| -> Vec<OsString> {
        options
            .iter()
            .map(OsString::from)
            .chain(std::iter::once(file.clone()))
            .collect()
    };
    match player {
        AudioPlayer::PwPlay => (
            PathBuf::from("/bin/sh"),
            vec![
                "-c".into(),
                PW_PLAY_SCRIPT.into(),
                program.as_os_str().to_owned(),
                file,
            ],
        ),
        AudioPlayer::Ffplay => (
            program.to_path_buf(),
            args(&["-nodisp", "-autoexit", "-loglevel", "quiet"]),
        ),
        AudioPlayer::Mpv => (
            program.to_path_buf(),
            // `pulse,`: PulseAudio (PipeWire's pulse server) first, as it
            // sends the pid, then mpv's own choice.
            args(&[
                "--no-video",
                "--really-quiet",
                "--no-terminal",
                "--ao=pulse,",
            ]),
        ),
    }
}

/// The players found on `PATH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Players {
    /// `sidplayfp`, for SID tunes.
    pub sid: Option<PathBuf>,
    /// Every audio player found, in order of preference; the first plays.
    pub audio: Vec<(AudioPlayer, PathBuf)>,
}

impl Players {
    pub fn find() -> Self {
        Self::find_with(which)
    }

    /// The players `lookup` finds (`which` outside the tests).
    pub fn find_with(mut lookup: impl FnMut(&str) -> Option<PathBuf>) -> Self {
        Self {
            sid: lookup("sidplayfp"),
            audio: AudioPlayer::ALL
                .into_iter()
                .filter_map(|player| lookup(player.program()).map(|path| (player, path)))
                .collect(),
        }
    }

    pub fn plays(&self, kind: Kind) -> bool {
        match kind {
            Kind::Sid => self.sid.is_some(),
            Kind::Audio => !self.audio.is_empty(),
        }
    }
}

/// The tunes under `folder`, relative to it, in natural order: `.sid` files
/// and audio files ([`AUDIO_EXTENSIONS`]), mixed.
#[cfg(test)]
pub fn scan(folder: &Path) -> Vec<PathBuf> {
    let mut tunes = scan_until(folder, None).0;
    sid::natural_sort(&mut tunes);
    tunes
}

/// The tunes under `folder` in directory order, without sorting, and
/// whether the scan saw everything (false when it stopped at `deadline` or
/// [`MAX_TUNES`]). Only directory entries are read: the file type comes
/// with the entry, and only a symbolic link costs a `stat`.
pub fn scan_until(folder: &Path, deadline: Option<Instant>) -> (Vec<PathBuf>, bool) {
    fn walk(
        base: &Path,
        dir: &Path,
        depth: usize,
        deadline: Option<Instant>,
        out: &mut Vec<PathBuf>,
    ) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return true;
        };
        for (n, entry) in entries.flatten().enumerate() {
            if out.len() >= MAX_TUNES {
                return false;
            }
            if n % 64 == 0 && deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            // Symlinked directories are not followed, so a loop cannot trap the scan.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !walk(base, &path, depth + 1, deadline, out) {
                    return false;
                }
            } else if kind(&path).is_some()
                && (file_type.is_file()
                    || (file_type.is_symlink()
                        && std::fs::metadata(&path).is_ok_and(|m| m.is_file())))
            {
                if let Ok(relative) = path.strip_prefix(base) {
                    out.push(relative.to_path_buf());
                }
            }
        }
        true
    }
    let mut tunes = Vec::new();
    let complete = walk(folder, folder, 0, deadline, &mut tunes);
    (tunes, complete)
}

/// The order the tunes play in: natural order, or shuffled ("a deck"): a
/// random first tune, every tune once per round, and a new round never
/// starts with the tune that ended the last one. Only indices are kept, so
/// a large collection costs four bytes per tune.
pub struct Order {
    len: usize,
    shuffle: bool,
    rng: u64,
    deck: Vec<u32>,
    pos: usize,
    last: Option<usize>,
}

impl Order {
    pub fn new(len: usize, shuffle: bool, seed: u64) -> Self {
        Self {
            len,
            shuffle,
            // xorshift must not start at zero.
            rng: seed | 1,
            deck: Vec::new(),
            pos: 0,
            last: None,
        }
    }

    /// xorshift64*: plenty for picking tunes, and no crate.
    fn random(&mut self) -> u64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.random() % n as u64) as usize
    }

    /// The index of the next tune; `None` without tunes.
    pub fn next(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let index = if !self.shuffle {
            self.last.map_or(0, |last| (last + 1) % self.len)
        } else {
            if self.pos >= self.deck.len() {
                self.deck = (0..self.len as u32).collect();
                for i in (1..self.len).rev() {
                    let j = self.below(i + 1);
                    self.deck.swap(i, j);
                }
                if self.len > 1 && self.last == Some(self.deck[0] as usize) {
                    let j = 1 + self.below(self.len - 1);
                    self.deck.swap(0, j);
                }
                self.pos = 0;
            }
            self.pos += 1;
            self.deck[self.pos - 1] as usize
        };
        self.last = Some(index);
        Some(index)
    }
}

/// A seed for the shuffle: the time and the process.
fn shuffle_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32) ^ 0x9e37_79b9_7f4a_7c15
}

/// The `tunes` that `players` can play, in their order, and the lines to
/// log about the rest. The music is ready when the list is not empty: a
/// folder of audio files needs no sidplayfp, a folder of SID tunes no
/// audio player.
pub fn playable(
    folder: &Path,
    tunes: Vec<PathBuf>,
    players: &Players,
) -> (Vec<PathBuf>, Vec<String>) {
    if tunes.is_empty() {
        return (
            tunes,
            vec![format!(
                "no tunes in {} (.sid, .ogg, .oga, .opus, .flac or .wav); no music",
                folder.display()
            )],
        );
    }
    let (play, skip): (Vec<PathBuf>, Vec<PathBuf>) = tunes
        .into_iter()
        .partition(|tune| kind(tune).is_some_and(|k| players.plays(k)));
    let sid = skip.iter().filter(|t| kind(t) == Some(Kind::Sid)).count();
    let audio = skip.len() - sid;
    let outcome = if play.is_empty() {
        "no music"
    } else {
        "they are skipped"
    };
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut notes = Vec::new();
    if sid > 0 {
        notes.push(format!(
            "no player for {} (sidplayfp is not installed); {outcome}",
            plural(sid, ".sid tune", ".sid tunes")
        ));
    }
    if audio > 0 {
        notes.push(format!(
            "no player for {} (none of pw-play, ffplay or mpv is installed); {outcome}",
            plural(audio, "audio file", "audio files")
        ));
    }
    (play, notes)
}

/// `Songlengths.md5` in the folder or its `DOCUMENTS` directory (the HVSC
/// layout), any case.
fn find_songlengths(folder: &Path) -> Option<sid::Songlengths> {
    for dir in [folder.to_path_buf(), folder.join("DOCUMENTS")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("songlengths.md5")
            {
                // The HVSC file is about 5 MB; refuse anything absurd.
                if entry.metadata().is_ok_and(|m| m.len() <= 64 << 20) {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        let db = sid::Songlengths::parse(&String::from_utf8_lossy(&bytes));
                        if !db.is_empty() {
                            return Some(db);
                        }
                    }
                }
            }
        }
    }
    None
}

/// A tune file larger than this is not a SID tune.
const MAX_TUNE_BYTES: u64 = 1 << 20;
/// An audio file plays to its end, but at most this long.
pub const AUDIO_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Extra time before a player that does not end on its own is stopped.
const GRACE: Duration = Duration::from_secs(3);
/// How long the player gets to exit after TERM before KILL.
const TERM_WAIT: Duration = Duration::from_millis(500);
/// How long the player may stay stopped (SIGSTOP, SIGTTOU, ...) before the
/// music gives up for this lock.
const STOPPED_GIVE_UP: Duration = Duration::from_secs(2);
/// After this many tunes in a row that end within a second, give up.
const MAX_QUICK_FAILURES: usize = 5;
/// After this many audio files in a row that end within a second, the next
/// audio player (if any) takes over: `pw-play` without a PipeWire server
/// fails at once, and ffplay or mpv may still play.
const AUDIO_PLAYER_FAILURES: usize = 2;

/// One tune, ready to start.
/// The Vorbis comments of an audio file, from its first
/// [`sid::TAG_BYTES`] bytes.
fn read_tags(file: &Path) -> sid::Tags {
    use std::io::Read;
    let mut data = Vec::new();
    match std::fs::File::open(file) {
        Ok(f) => {
            let _ = f.take(sid::TAG_BYTES as u64).read_to_end(&mut data);
            sid::vorbis_tags(&data)
        }
        Err(_) => sid::Tags::default(),
    }
}

struct Prepared {
    line: String,
    /// "Artist - Title".
    tune: String,
    /// The player's name for the log.
    name: &'static str,
    /// How long it may play before it is stopped (with [`GRACE`]).
    limit: Duration,
    kind: Kind,
}

fn player(
    shared: &Shared,
    folder: &Path,
    track_seconds: u32,
    shuffle: bool,
    receive: &mpsc::Receiver<Request>,
    mut notify: UnixStream,
) {
    let players = Players::find();
    let tunes = if folder.is_dir() {
        let begun = Instant::now();
        let (mut tunes, complete) = scan_until(folder, Some(begun + SCAN_BUDGET));
        if !complete {
            eprintln!(
                "nirulock: pause_music: the scan stopped after {} tunes ({:.1} s); playing those",
                tunes.len(),
                begun.elapsed().as_secs_f32()
            );
        }
        // The shuffled order needs no sorting; natural order does.
        if !shuffle {
            sid::natural_sort(&mut tunes);
        }
        tunes
    } else {
        Vec::new()
    };
    let (tunes, notes) = playable(folder, tunes, &players);
    for note in notes {
        eprintln!("nirulock: pause_music: {note}");
    }
    let ready = !tunes.is_empty();
    shared.ready.store(ready, Ordering::Release);
    shared.scanned.store(true, Ordering::Release);
    if !ready {
        return;
    }
    // Wait for the gate. Stop, or the lock going away, ends the thread.
    let exclusions = match receive.recv() {
        Ok(Request::Play(exclusions)) => exclusions,
        Ok(Request::Stop) | Err(_) => return,
    };
    if shared.stopped.load(Ordering::Acquire) {
        return;
    }
    let has_sid = tunes.iter().any(|t| kind(t) == Some(Kind::Sid));
    let lengths = has_sid.then(|| find_songlengths(folder)).flatten();
    // Asked once, when the first SID tune plays.
    let mut engine: Option<Option<&'static str>> = None;
    let mut audio_player = 0;
    let mut audio_failures = 0;
    let mut quick_failures = 0;
    let mut order = Order::new(tunes.len(), shuffle, shuffle_seed());
    loop {
        let Some(index) = order.next() else {
            return;
        };
        let file = folder.join(&tunes[index]);
        let spawned = match kind(&file) {
            Some(Kind::Audio) => {
                let (player, program) = &players.audio[audio_player];
                let (program, args) = audio_command(*player, program, &file);
                let tune = Prepared {
                    line: sid::audio_info_line(&file),
                    tune: sid::audio_tune_name(&file, &read_tags(&file)),
                    name: player.program(),
                    limit: AUDIO_LIMIT,
                    kind: Kind::Audio,
                };
                (tune, spawn_isolated(&program, &args))
            }
            _ => {
                let Some(program) = players.sid.as_deref() else {
                    continue;
                };
                let data = std::fs::metadata(&file)
                    .ok()
                    .filter(|m| m.len() <= MAX_TUNE_BYTES)
                    .and_then(|_| std::fs::read(&file).ok());
                let Some(data) = data else {
                    quick_failures += 1;
                    if quick_failures >= MAX_QUICK_FAILURES.max(tunes.len()) {
                        eprintln!("nirulock: pause_music: the tunes cannot be read; no music");
                        return;
                    }
                    continue;
                };
                let header = sid::parse_header(&data);
                let song = header.as_ref().map_or(1, |h| h.start_song);
                let file_name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let seconds = lengths
                    .as_ref()
                    .and_then(|db| db.length(&sid::md5_hex(&data), &file_name, song))
                    .unwrap_or(track_seconds);
                drop(data);
                let engine = *engine.get_or_insert_with(|| engine_flag(program));
                let tune = Prepared {
                    line: sid::info_line(header.as_ref(), &file),
                    tune: sid::tune_name(header.as_ref(), &file),
                    name: "sidplayfp",
                    limit: Duration::from_secs(u64::from(seconds)),
                    kind: Kind::Sid,
                };
                (tune, spawn(program, engine, &file, seconds, song))
            }
        };
        let (tune, child) = spawned;
        let started = Instant::now();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                eprintln!(
                    "nirulock: pause_music: cannot start {}: {e}; no music",
                    tune.name
                );
                return;
            }
        };
        let pid = child.id() as i32;
        shared.pgid.store(pid, Ordering::Release);
        if let Some(exclusions) = exclusions.as_ref() {
            exclusions.set(&[child.id()]);
        }
        // Stop may have come between the check and the spawn.
        if shared.stopped.load(Ordering::Acquire) {
            finish(shared, &mut child);
            return;
        }
        if let Ok(mut info) = shared.info.lock() {
            *info = Some((tune.line, tune.tune));
        }
        let _ = notify.write(&[1]);
        let deadline = started + tune.limit + GRACE;
        let mut last = clocks();
        let mut stopped_since: Option<Instant> = None;
        let stop = loop {
            match receive.recv_timeout(Duration::from_millis(200)) {
                Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break true,
                Ok(Request::Play(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            // A suspend that no PrepareForSleep announced: stop on resume.
            let now = clocks();
            if now.0.saturating_sub(last.0) > now.1.saturating_sub(last.1) + Duration::from_secs(2)
            {
                stop_now(shared);
            }
            last = now;
            if shared.stopped.load(Ordering::Acquire) {
                break true;
            }
            if !matches!(child.try_wait(), Ok(None)) || Instant::now() >= deadline {
                break false;
            }
            // A player that is stopped plays nothing; do not show its tune.
            if is_stopped(child.id()) {
                let since = *stopped_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= STOPPED_GIVE_UP {
                    eprintln!(
                        "nirulock: pause_music: {} was stopped by a signal; no music",
                        tune.name
                    );
                    finish(shared, &mut child);
                    if let Some(exclusions) = exclusions.as_ref() {
                        exclusions.set(&[]);
                    }
                    let _ = notify.write(&[1]);
                    return;
                }
            } else {
                stopped_since = None;
            }
        };
        finish(shared, &mut child);
        if stop {
            // The stopped player stays excluded: its stream may outlive it
            // by a moment in the sound server, and must not count as other
            // music then (the panel would flip back to the spectrum). The
            // music never starts again in this lock.
            let _ = notify.write(&[1]);
            return;
        }
        if started.elapsed() < Duration::from_secs(1) {
            quick_failures += 1;
            if tune.kind == Kind::Audio {
                audio_failures += 1;
                if audio_failures >= AUDIO_PLAYER_FAILURES && audio_player + 1 < players.audio.len()
                {
                    audio_player += 1;
                    audio_failures = 0;
                    quick_failures = 0;
                    eprintln!(
                        "nirulock: pause_music: {} keeps failing; trying {}",
                        tune.name,
                        players.audio[audio_player].0.program()
                    );
                }
            }
            if quick_failures >= MAX_QUICK_FAILURES {
                eprintln!(
                    "nirulock: pause_music: {} keeps failing; no music",
                    tune.name
                );
                if let Ok(mut info) = shared.info.lock() {
                    *info = None;
                }
                let _ = notify.write(&[1]);
                return;
            }
        } else {
            quick_failures = 0;
            if tune.kind == Kind::Audio {
                audio_failures = 0;
            }
        }
    }
}

/// The emulation option this sidplayfp needs, read from its `--help`.
///
/// sidplayfp lists only the emulations it was built with. Void's 3.2.3 has
/// sidlite alone but still asks for reSIDfp by default, so every tune fails
/// with "Requested SID emulation not built in"; it needs `--sidlite`. A build
/// with reSIDfp or reSID uses its own default.
///
/// The answer must come quickly: a player that does not know `--help` and
/// starts playing instead is stopped after a second, so no sound outlives
/// the question.
fn engine_flag(program: &Path) -> Option<&'static str> {
    // SAFETY: getpid is async-signal-safe and has no preconditions.
    let parent = unsafe { libc::getpid() };
    let mut command = Command::new(program);
    command
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: only async-signal-safe calls between fork and exec. The
    // question dies with the lock, like every player.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::getppid() != parent
            {
                libc::_exit(127);
            }
            Ok(())
        });
    }
    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(1) => {
                thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    engine_from_help(&text)
}

fn engine_from_help(help: &str) -> Option<&'static str> {
    let has = |flag: &str| {
        help.lines()
            .any(|line| line.split_whitespace().next() == Some(flag))
    };
    if has("--residfp") || has("--resid") {
        None
    } else if has("--sidlite") {
        Some("--sidlite")
    } else {
        None
    }
}

/// Start one tune: `sidplayfp [ENGINE] -q -os -o<song> -t<seconds> FILE`.
fn spawn(
    program: &Path,
    engine: Option<&str>,
    file: &Path,
    seconds: u32,
    song: u16,
) -> std::io::Result<Child> {
    let mut args: Vec<OsString> = engine.into_iter().map(OsString::from).collect();
    args.extend([
        "-q".into(),
        "-os".into(),
        format!("-o{song}").into(),
        format!("-t{seconds}").into(),
        file.as_os_str().to_owned(),
    ]);
    spawn_isolated(program, &args)
}

/// Start a player: in its own session, without a terminal or input, dying
/// with the music thread.
fn spawn_isolated(program: &Path, args: &[OsString]) -> std::io::Result<Child> {
    // SAFETY: getpid is async-signal-safe and has no preconditions.
    let parent = unsafe { libc::getpid() };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            // A new session: no controlling terminal, so the player's own
            // /dev/tty access cannot stop it, and its pid is its process
            // group for `signal_group`. Dies with the music thread, and so
            // with the lock.
            if libc::setsid() < 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::getppid() != parent
            {
                libc::_exit(127);
            }
            Ok(())
        });
    }
    command.spawn()
}

/// True while process `pid` is stopped (state `T` or `t` in /proc).
fn is_stopped(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| rest.trim_start().to_owned())
        })
        .is_some_and(|rest| rest.starts_with('T') || rest.starts_with('t'))
}

/// TERM, wait briefly, KILL, reap. Runs on the music thread only.
fn finish(shared: &Shared, child: &mut Child) {
    let pgid = child.id() as i32;
    if matches!(child.try_wait(), Ok(None)) {
        signal_group(pgid, libc::SIGTERM);
        let until = Instant::now() + TERM_WAIT;
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
        if matches!(child.try_wait(), Ok(None)) {
            signal_group(pgid, libc::SIGKILL);
            let _ = child.wait();
        }
    }
    // Anything else left in the group.
    signal_group(pgid, libc::SIGKILL);
    let _ = shared
        .pgid
        .compare_exchange(pgid, 0, Ordering::AcqRel, Ordering::Acquire);
    if let Ok(mut info) = shared.info.lock() {
        *info = None;
    }
}

// ---- before suspend ----------------------------------------------------------------

/// Watches logind's (or elogind's) `PrepareForSleep` with `dbus-monitor` and
/// stops the music on it. Either signal counts: `true` before suspend, and
/// `false` after a resume that the watcher was too late to see coming.
struct SleepWatch {
    child: Child,
}

impl SleepWatch {
    fn start(slept: Arc<AtomicBool>, shared: Arc<Shared>) -> Option<Self> {
        let Some(program) = which("dbus-monitor") else {
            eprintln!("nirulock: pause_music: dbus-monitor is missing; music stops on resume instead of before suspend");
            return None;
        };
        // SAFETY: getpid is async-signal-safe and has no preconditions.
        let parent = unsafe { libc::getpid() };
        let mut command = Command::new(program);
        command
            .args([
                "--system",
                "type='signal',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    || libc::getppid() != parent
                {
                    libc::_exit(127);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().ok()?;
        let stdout = child.stdout.take()?;
        thread::Builder::new()
            .name("nirulock-sleep".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if is_sleep_signal(&line) {
                        slept.store(true, Ordering::Release);
                        stop_now(&shared);
                    }
                }
            })
            .ok()?;
        Some(Self { child })
    }
}

/// dbus-monitor prints the signal's argument as `   boolean true`.
fn is_sleep_signal(line: &str) -> bool {
    let line = line.trim();
    line == "boolean true" || line == "boolean false"
}

impl Drop for SleepWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // Reaped right away: dbus-monitor exits on SIGKILL at once.
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn engine_follows_what_sidplayfp_was_built_with() {
        let debian = " --residfp    use reSIDfp emulation (default)\n --resid      use reSID emulation\n --exsid      enable exSID support\n";
        assert_eq!(engine_from_help(debian), None);
        let void = " -w[name]     create wav file\n --sidlite    use sidlite emulation\n";
        assert_eq!(engine_from_help(void), Some("--sidlite"));
        assert_eq!(engine_from_help("Options:\n -q quiet\n"), None);
        assert_eq!(
            engine_from_help(" --sidlite    use sidlite\n --residfp    use reSIDfp\n"),
            None
        );
    }

    use super::*;

    fn settings(text: &str) -> Result<Settings, String> {
        Settings::from_value(&text.parse::<toml::Value>().unwrap()["pause_music"])
    }

    #[test]
    fn only_a_manual_lock_plays_by_default() {
        // On without any setting at all, and the same when switched on.
        assert_eq!(
            Settings::default(),
            settings("[pause_music]\nenabled = true").unwrap()
        );
        let s = settings("[pause_music]\nenabled = true").unwrap();
        assert_eq!(s.on, vec![Reason::Manual]);
        assert!(s.plays_for(Reason::Manual));
        for r in [
            Reason::Idle,
            Reason::Suspend,
            Reason::Script,
            Reason::Session,
            Reason::Unspecified,
        ] {
            assert!(!s.plays_for(r), "{r} must not play by default");
        }
        assert!(s.describe().starts_with("on for manual locks"));
    }

    #[test]
    fn natural_order_loops_from_the_first_tune() {
        let mut order = Order::new(3, false, 42);
        let got: Vec<usize> = (0..7).filter_map(|_| order.next()).collect();
        assert_eq!(got, [0, 1, 2, 0, 1, 2, 0]);
        assert_eq!(Order::new(0, false, 1).next(), None);
        assert_eq!(Order::new(0, true, 1).next(), None);
    }

    #[test]
    fn shuffle_plays_every_tune_once_per_round_without_repeats() {
        for seed in 0..200u64 {
            let len = 1 + (seed as usize % 12);
            let mut order = Order::new(len, true, seed.wrapping_mul(0x9e37_79b9));
            let mut last = None;
            for _round in 0..4 {
                let mut seen = vec![false; len];
                for _ in 0..len {
                    let i = order.next().unwrap();
                    assert!(i < len);
                    assert!(!seen[i], "seed {seed}: {i} twice in a round");
                    seen[i] = true;
                    if len > 1 {
                        assert_ne!(Some(i), last, "seed {seed}: {i} twice in a row");
                    }
                    last = Some(i);
                }
                assert!(seen.iter().all(|s| *s));
            }
        }
    }

    #[test]
    fn shuffle_starts_with_a_random_tune() {
        // NIRUX's eleven tunes: the first one differs between locks.
        let firsts: std::collections::HashSet<usize> = (0..64u64)
            .map(|seed| Order::new(11, true, seed * 7919 + 3).next().unwrap())
            .collect();
        assert!(firsts.len() >= 6, "{firsts:?}");
        // A large collection is cheap: indices only, built on first use.
        let mut big = Order::new(60_000, true, shuffle_seed());
        assert!(big.deck.is_empty());
        let first = big.next().unwrap();
        assert!(first < 60_000);
        assert_eq!(big.deck.len(), 60_000);
    }

    #[test]
    fn a_large_tree_is_listed_quickly_and_the_budget_holds() {
        let dir = std::env::temp_dir().join(format!("lock-big-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in 0..40 {
            let sub = dir.join(format!(
                "MUSICIANS/{}/composer-{d}",
                (b'A' + d % 26) as char
            ));
            std::fs::create_dir_all(&sub).unwrap();
            for t in 0..50 {
                std::fs::write(sub.join(format!("tune_{t}.sid")), b"").unwrap();
            }
            std::fs::write(sub.join("readme.txt"), b"").unwrap();
        }
        let (tunes, complete) = scan_until(&dir, Some(Instant::now() + SCAN_BUDGET));
        assert!(complete);
        assert_eq!(tunes.len(), 2000);
        // A deadline that has passed stops the walk at once.
        let (_, complete) = scan_until(&dir, Some(Instant::now()));
        assert!(!complete);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_music_never_plays() {
        let s = settings("[pause_music]\nenabled = false\non = [\"manual\", \"idle\"]").unwrap();
        assert!(Reason::ALL.into_iter().all(|r| !s.plays_for(r)));
        assert_eq!(s.describe(), "off");
    }

    #[test]
    fn the_reasons_can_be_chosen() {
        let s = settings("[pause_music]\nenabled = true\non = [\"idle\", \"Manual\", \"idle\"]")
            .unwrap();
        assert_eq!(s.on, vec![Reason::Idle, Reason::Manual]);
        assert!(s.plays_for(Reason::Idle) && s.plays_for(Reason::Manual));
        assert!(!s.plays_for(Reason::Script) && !s.plays_for(Reason::Unspecified));
        let none = settings("[pause_music]\nenabled = true\non = []").unwrap();
        assert!(Reason::ALL.into_iter().all(|r| !none.plays_for(r)));
        assert!(none.describe().starts_with("on for no locks"));
    }

    #[test]
    fn a_wrong_reason_list_is_an_error() {
        assert!(settings("[pause_music]\non = \"manual\"").is_err());
        assert!(settings("[pause_music]\non = [\"party\"]").is_err());
        assert!(settings("[pause_music]\non = [1]").is_err());
    }

    /// Fields 6 and 7 of /proc/PID/stat: the session id and the controlling
    /// terminal (0 for none).
    fn session_and_tty(pid: u32) -> (i64, i64) {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let rest: Vec<i64> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .skip(1)
            .take(6)
            .map(|v| v.parse().unwrap())
            .collect();
        (rest[2], rest[3])
    }

    #[test]
    fn the_player_runs_in_its_own_session_without_a_terminal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nirulock-music-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let player = dir.join("player");
        std::fs::write(&player, "#!/bin/sh\nexec sleep 5\n").unwrap();
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = spawn(&player, None, Path::new("tune.sid"), 5, 1).unwrap();
        let pid = child.id();
        // Wait for the exec.
        let until = Instant::now() + Duration::from_secs(2);
        while std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map_or(true, |c| c.trim() != "sleep")
            && Instant::now() < until
        {
            thread::sleep(Duration::from_millis(10));
        }
        let (session, tty) = session_and_tty(pid);
        assert_eq!(session, i64::from(pid), "the player leads its own session");
        assert_eq!(tty, 0, "the player has no controlling terminal");
        assert!(!is_stopped(pid));
        // The pid is the process group, so the group signal reaches it.
        signal_group(pid as i32, libc::SIGSTOP);
        let until = Instant::now() + Duration::from_secs(2);
        while !is_stopped(pid) && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(is_stopped(pid));
        signal_group(pid as i32, libc::SIGKILL);
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn at(t: Instant, ms: u64) -> Instant {
        t + Duration::from_millis(ms)
    }

    #[test]
    fn silence_through_the_window_starts_the_music() {
        let t = Instant::now();
        let mut gate = StartGate::new(t);
        assert_eq!(
            gate.step(Heard::Listening, true, false, at(t, 0)),
            Decision::Wait
        );
        assert_eq!(
            gate.step(Heard::Silent, true, false, at(t, 1000)),
            Decision::Wait
        );
        assert_eq!(gate.deadline(at(t, 1000)), Some(t + GATE_WINDOW));
        assert_eq!(
            gate.step(Heard::Silent, true, false, at(t, 2000)),
            Decision::Start
        );
        assert_eq!(gate.decided(), Some(Decision::Start));
        assert_eq!(gate.deadline(at(t, 2000)), None);
        // Decided once: later audio does not change it.
        assert_eq!(
            gate.step(Heard::Playing, true, false, at(t, 3000)),
            Decision::Start
        );
    }

    #[test]
    fn other_audio_in_the_window_means_no_music() {
        let t = Instant::now();
        let mut gate = StartGate::new(t);
        assert_eq!(
            gate.step(Heard::Listening, true, false, at(t, 250)),
            Decision::Wait
        );
        assert_eq!(
            gate.step(Heard::Playing, true, false, at(t, 500)),
            Decision::Skip
        );
        assert_eq!(
            gate.step(Heard::Silent, true, false, at(t, 9000)),
            Decision::Skip
        );
    }

    #[test]
    fn a_quiet_stream_delays_the_decision_only_up_to_the_maximum() {
        // An idle synthesizer: always present, never loud.
        let t = Instant::now();
        let mut gate = StartGate::new(t);
        assert_eq!(
            gate.step(Heard::Listening, true, false, at(t, 2000)),
            Decision::Wait
        );
        assert_eq!(gate.deadline(at(t, 2000)), Some(at(t, 2000) + GATE_STEP));
        assert_eq!(
            gate.step(Heard::Listening, true, false, at(t, 4750)),
            Decision::Wait
        );
        assert_eq!(
            gate.step(Heard::Listening, true, false, at(t, 5000)),
            Decision::Start
        );
    }

    #[test]
    fn sleep_or_esc_in_the_window_means_no_music() {
        let t = Instant::now();
        let mut gate = StartGate::new(t);
        assert_eq!(
            gate.step(Heard::Silent, true, true, at(t, 100)),
            Decision::Skip
        );
        assert_eq!(
            gate.step(Heard::Silent, true, false, at(t, 3000)),
            Decision::Skip
        );
    }

    #[test]
    fn an_off_display_holds_the_decision() {
        let t = Instant::now();
        // Off at first, on a moment later: the music still starts.
        let mut gate = StartGate::new(t);
        assert_eq!(
            gate.step(Heard::Silent, false, false, at(t, 0)),
            Decision::Wait
        );
        assert_eq!(
            gate.step(Heard::Silent, false, false, at(t, 2500)),
            Decision::Wait
        );
        assert_eq!(
            gate.step(Heard::Silent, true, false, at(t, 2750)),
            Decision::Start
        );
        // Off for the whole window: no music.
        let mut gate = StartGate::new(t);
        assert_eq!(
            gate.step(Heard::Silent, false, false, at(t, 4750)),
            Decision::Wait
        );
        assert_eq!(
            gate.step(Heard::Silent, false, false, at(t, 5000)),
            Decision::Skip
        );
    }

    #[test]
    fn a_closed_wake_socket_is_forgotten() {
        let settings = Settings {
            enabled: true,
            folder: "/nonexistent/nirulock-test".into(),
            ..Settings::default()
        };
        let mut music = Music::start(&settings).expect("music thread");
        // The thread finds no tunes and ends, closing its end of the socket.
        for _ in 0..200 {
            music.woke();
            if music.wake_fd().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            music.wake_fd().is_none(),
            "a closed socket would spin the lock"
        );
    }

    #[test]
    fn settings_default_on_shuffled_and_validate() {
        let d = Settings::default();
        assert!(d.enabled);
        assert!(d.shuffle);
        assert_eq!(d.on, [Reason::Manual]);
        assert_eq!(d.folder, "~/.local/share/nirux/music");
        assert_eq!(d.folder, DEFAULT_FOLDER);
        assert_eq!(
            Settings {
                folder: "/home/n/.local/share/nirux/music".into(),
                ..d.clone()
            }
            .folder_path(),
            PathBuf::from("/home/n/.local/share/nirux/music")
        );
        assert_eq!(d.track_seconds, 180);
        assert!(d.stop_on_display_off);
        let parse = |text: &str| Settings::from_value(&text.parse::<toml::Value>().unwrap());
        let s = parse("enabled = true\nfolder = \"/srv/sid\"\ntrack_seconds = 60\nstop_on_display_off = false").unwrap();
        assert!(s.enabled && !s.stop_on_display_off);
        assert_eq!((s.folder.as_str(), s.track_seconds), ("/srv/sid", 60));
        assert!(parse("track_seconds = 1").is_err());
        assert!(parse("enabled = \"yes\"").is_err());
        assert!(parse("folder = \"\"").is_err());
        assert!(!parse("shuffle = false").unwrap().shuffle);
        assert!(parse("shuffle = true").unwrap().shuffle);
        assert!(parse("shuffle = \"random\"").is_err());
        assert!(parse("enabled = false").unwrap().describe() == "off");
        assert!(d.describe().contains("shuffled"));
        assert!(parse("shuffle = false")
            .unwrap()
            .describe()
            .contains("natural order"));
        assert_eq!(
            expand_home("~/Music/SID", Some(PathBuf::from("/home/n"))),
            PathBuf::from("/home/n/Music/SID")
        );
        assert_eq!(expand_home("/x", None), PathBuf::from("/x"));
    }

    #[test]
    fn sleep_signals_are_recognised() {
        assert!(is_sleep_signal("   boolean true"));
        assert!(is_sleep_signal("   boolean false"));
        assert!(!is_sleep_signal(
            "signal time=1 sender=:1.2 member=PrepareForSleep"
        ));
    }

    #[test]
    fn the_scan_finds_tunes_in_natural_order() {
        let dir = std::env::temp_dir().join(format!("lock-sid-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        for name in [
            "10-c.sid",
            "2-b.SID",
            "1-a.sid",
            "notes.txt",
            ".hidden/x.sid",
            "sub/3.sid",
        ] {
            std::fs::write(dir.join(name), b"PSID").unwrap();
        }
        let tunes: Vec<String> = scan(&dir)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(tunes, ["1-a.sid", "2-b.SID", "10-c.sid", "sub/3.sid"]);
        assert!(scan(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_scan_mixes_audio_files_and_sid_tunes_in_natural_order() {
        let dir = std::env::temp_dir().join(format!("lock-mixed-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("more")).unwrap();
        for name in [
            "nirux-10-ten.ogg",
            "nirux-2-two.OGG",
            "nirux-1-ginnungagap.ogg",
            "3-old.sid",
            "a.flac",
            "b.opus",
            "c.oga",
            "d.WAV",
            "cover.png",
            "e.mp3",
            "notes.txt",
            "more/x.sid",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let tunes: Vec<String> = scan(&dir)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            tunes,
            [
                "3-old.sid",
                "a.flac",
                "b.opus",
                "c.oga",
                "d.WAV",
                "more/x.sid",
                "nirux-1-ginnungagap.ogg",
                "nirux-2-two.OGG",
                "nirux-10-ten.ogg",
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_player_follows_the_extension() {
        assert_eq!(kind(Path::new("a/b.sid")), Some(Kind::Sid));
        assert_eq!(kind(Path::new("b.SID")), Some(Kind::Sid));
        for audio in ["x.ogg", "x.OGA", "x.opus", "x.Flac", "x.wav"] {
            assert_eq!(kind(Path::new(audio)), Some(Kind::Audio), "{audio}");
        }
        for other in ["x.mp3", "x.txt", "ogg", "x.sid.bak", "x"] {
            assert_eq!(kind(Path::new(other)), None, "{other}");
        }
        let file = Path::new("/m/nirux-1-ginnungagap.ogg");
        let strings = |(program, args): (PathBuf, Vec<OsString>)| {
            let mut all = vec![program.to_string_lossy().into_owned()];
            all.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
            all
        };
        assert_eq!(
            strings(audio_command(
                AudioPlayer::PwPlay,
                Path::new("/usr/bin/pw-play"),
                file
            )),
            [
                "/bin/sh",
                "-c",
                PW_PLAY_SCRIPT,
                "/usr/bin/pw-play",
                "/m/nirux-1-ginnungagap.ogg"
            ]
        );
        assert_eq!(
            strings(audio_command(
                AudioPlayer::Ffplay,
                Path::new("/usr/bin/ffplay"),
                file
            )),
            [
                "/usr/bin/ffplay",
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "quiet",
                "/m/nirux-1-ginnungagap.ogg"
            ]
        );
        assert_eq!(
            strings(audio_command(
                AudioPlayer::Mpv,
                Path::new("/usr/bin/mpv"),
                file
            )),
            [
                "/usr/bin/mpv",
                "--no-video",
                "--really-quiet",
                "--no-terminal",
                "--ao=pulse,",
                "/m/nirux-1-ginnungagap.ogg"
            ]
        );
        // A relative name is never an option.
        let relative = audio_command(
            AudioPlayer::Ffplay,
            Path::new("ffplay"),
            Path::new("-x.ogg"),
        );
        assert_eq!(relative.1.last().unwrap(), "./-x.ogg");
    }

    #[test]
    fn pw_play_is_preferred_then_ffplay_then_mpv() {
        let only = |names: &'static [&'static str]| {
            Players::find_with(|name| {
                names
                    .contains(&name)
                    .then(|| PathBuf::from(format!("/bin/{name}")))
            })
        };
        let all = only(&["sidplayfp", "pw-play", "ffplay", "mpv"]);
        assert_eq!(all.sid, Some(PathBuf::from("/bin/sidplayfp")));
        let order: Vec<AudioPlayer> = all.audio.iter().map(|(p, _)| *p).collect();
        assert_eq!(order, AudioPlayer::ALL);
        assert_eq!(only(&["mpv", "ffplay"]).audio[0].0, AudioPlayer::Ffplay);
        assert_eq!(only(&["mpv"]).audio[0].0, AudioPlayer::Mpv);
        let none = only(&[]);
        assert!(!none.plays(Kind::Sid) && !none.plays(Kind::Audio));
    }

    #[test]
    fn ready_means_a_tune_with_a_player() {
        let folder = Path::new("/m");
        let tunes = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let players = |sid: bool, audio: bool| Players {
            sid: sid.then(|| PathBuf::from("/bin/sidplayfp")),
            audio: if audio {
                vec![(AudioPlayer::PwPlay, PathBuf::from("/bin/pw-play"))]
            } else {
                Vec::new()
            },
        };
        // NIRUX's own music needs no sidplayfp.
        let (play, notes) = playable(
            folder,
            tunes(&["nirux-1-a.ogg", "nirux-2-b.ogg"]),
            &players(false, true),
        );
        assert_eq!(play, tunes(&["nirux-1-a.ogg", "nirux-2-b.ogg"]));
        assert!(notes.is_empty());
        // A SID collection needs sidplayfp, and no audio player.
        let (play, notes) = playable(folder, tunes(&["1.sid"]), &players(true, false));
        assert_eq!((play.len(), notes.len()), (1, 0));
        let (play, notes) = playable(folder, tunes(&["1.sid", "2.sid"]), &players(false, true));
        assert!(play.is_empty());
        assert_eq!(
            notes,
            ["no player for 2 .sid tunes (sidplayfp is not installed); no music"]
        );
        let (play, notes) = playable(folder, tunes(&["1.ogg"]), &players(true, false));
        assert!(play.is_empty());
        assert_eq!(
            notes,
            ["no player for 1 audio file (none of pw-play, ffplay or mpv is installed); no music"]
        );
        // Mixed: the tunes without a player are skipped, the order is kept.
        let (play, notes) = playable(
            folder,
            tunes(&["1.sid", "2.ogg", "3.sid"]),
            &players(false, true),
        );
        assert_eq!(play, tunes(&["2.ogg"]));
        assert_eq!(
            notes,
            ["no player for 2 .sid tunes (sidplayfp is not installed); they are skipped"]
        );
        let (play, notes) = playable(folder, tunes(&["1.sid", "2.ogg"]), &players(true, true));
        assert_eq!((play.len(), notes.len()), (2, 0));
        let (play, notes) = playable(folder, Vec::new(), &players(true, true));
        assert!(play.is_empty());
        assert_eq!(
            notes,
            ["no tunes in /m (.sid, .ogg, .oga, .opus, .flac or .wav); no music"]
        );
    }

    #[test]
    fn pw_play_gets_its_own_pid_as_the_stream_process_id() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("nirulock-pw-play-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("record");
        let player = dir.join("pw-play");
        std::fs::write(
            &player,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$@\" > {}.tmp\nmv {}.tmp {}\nexec sleep 5\n",
                record.display(),
                record.display(),
                record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (program, args) = audio_command(AudioPlayer::PwPlay, &player, Path::new("/m/a b.ogg"));
        let mut child = spawn_isolated(&program, &args).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while !record.exists() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        let text = std::fs::read_to_string(&record).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let pid = child.id();
        assert_eq!(
            lines,
            [
                pid.to_string(),
                "-P".to_string(),
                format!("{{ application.process.id = {pid} }}"),
                "/m/a b.ogg".to_string()
            ]
        );
        let (session, tty) = session_and_tty(pid);
        assert_eq!((session, tty), (i64::from(pid), 0));
        signal_group(pid as i32, libc::SIGKILL);
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
