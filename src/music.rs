//! `[pause_music]`: C64 SID tunes while the screen is locked.
//!
//! Off by default. When enabled, the lock waits [`GATE_WINDOW`] after it is
//! shown and starts `sidplayfp` only if the folder has `.sid` files and no
//! other audio played during that window ([`StartGate`]). The tunes play in
//! natural order and loop, each for its HVSC `Songlengths.md5` length or
//! `track_seconds`.
//!
//! Isolation from the lock:
//!
//! - The player is an ordinary child process of the user (the lock has no
//!   privileges to drop), in its own session and so its own process group,
//!   without a controlling terminal, with stdin, stdout and stderr on
//!   `/dev/null`. sidplayfp opens `/dev/tty` for its keyboard controls; on a
//!   session started from a text console a background process group there
//!   is stopped by SIGTTOU/SIGTTIN, which left the lock showing a tune while
//!   nothing played. Its arguments are a time, a subtune and a path;
//!   nothing typed on the lock screen ever reaches it.
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
use crate::sid;
use std::{
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
}

pub const DEFAULT_FOLDER: &str = "~/Music/SID";
pub const DEFAULT_TRACK_SECONDS: u32 = 180;

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            folder: DEFAULT_FOLDER.to_string(),
            track_seconds: DEFAULT_TRACK_SECONDS,
            stop_on_display_off: true,
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
                other => eprintln!("nirulock: pause_music: unknown setting {other:?} ignored"),
            }
        }
        Ok(s)
    }

    pub fn folder_path(&self) -> PathBuf {
        expand_home(&self.folder, std::env::var_os("HOME").map(PathBuf::from))
    }

    pub fn describe(&self) -> String {
        if self.enabled {
            format!(
                "on (folder {}, {} s per tune without Songlengths.md5, {})",
                self.folder,
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
    /// The info line of the tune that plays, if any.
    info: Mutex<Option<String>>,
    /// The playing child's process group, 0 when none.
    pgid: AtomicI32,
    /// The folder has tunes and sidplayfp exists.
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
    /// Scan the folder and look for sidplayfp on the music thread; the
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
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("nirulock-music".into())
            .spawn(move || {
                player(&thread_shared, &folder, track_seconds, &receive, notify);
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
                    return Tick::Skipped("no tunes or no sidplayfp");
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
        self.shared.info.lock().ok().and_then(|info| info.clone())
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

/// At most this many tunes and directory levels are looked at.
const MAX_TUNES: usize = 10_000;
const MAX_DEPTH: usize = 6;

/// The `.sid` files under `folder`, relative to it, in natural order.
pub fn scan(folder: &Path) -> Vec<PathBuf> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_TUNES {
                return;
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            // Symlinked directories are not followed, so a loop cannot trap the scan.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    walk(base, &path, depth + 1, out);
                }
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("sid"))
                && std::fs::metadata(&path).is_ok_and(|m| m.is_file())
            {
                if let Ok(relative) = path.strip_prefix(base) {
                    out.push(relative.to_path_buf());
                }
            }
        }
    }
    let mut tunes = Vec::new();
    walk(folder, folder, 0, &mut tunes);
    sid::natural_sort(&mut tunes);
    tunes
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
/// Extra time before a player that does not end on its own is stopped.
const GRACE: Duration = Duration::from_secs(3);
/// How long the player gets to exit after TERM before KILL.
const TERM_WAIT: Duration = Duration::from_millis(500);
/// How long the player may stay stopped (SIGSTOP, SIGTTOU, ...) before the
/// music gives up for this lock.
const STOPPED_GIVE_UP: Duration = Duration::from_secs(2);
/// After this many tunes in a row that end within a second, give up.
const MAX_QUICK_FAILURES: usize = 5;

fn player(
    shared: &Shared,
    folder: &Path,
    track_seconds: u32,
    receive: &mpsc::Receiver<Request>,
    mut notify: UnixStream,
) {
    let program = which("sidplayfp");
    let tunes = if folder.is_dir() {
        scan(folder)
    } else {
        Vec::new()
    };
    if program.is_none() {
        eprintln!("nirulock: pause_music: sidplayfp is not installed; no music");
    } else if tunes.is_empty() {
        eprintln!(
            "nirulock: pause_music: no .sid files in {}; no music",
            folder.display()
        );
    }
    let ready = program.is_some() && !tunes.is_empty();
    shared.ready.store(ready, Ordering::Release);
    shared.scanned.store(true, Ordering::Release);
    let (Some(program), true) = (program, ready) else {
        return;
    };
    // Wait for the gate. Stop, or the lock going away, ends the thread.
    let exclusions = match receive.recv() {
        Ok(Request::Play(exclusions)) => exclusions,
        Ok(Request::Stop) | Err(_) => return,
    };
    if shared.stopped.load(Ordering::Acquire) {
        return;
    }
    let lengths = find_songlengths(folder);
    let mut quick_failures = 0;
    let mut index = 0;
    loop {
        let file = folder.join(&tunes[index % tunes.len()]);
        index = (index + 1) % tunes.len();
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
        let line = sid::info_line(header.as_ref(), &file);
        let started = Instant::now();
        let mut child = match spawn(&program, &file, seconds, song) {
            Ok(child) => child,
            Err(e) => {
                eprintln!("nirulock: pause_music: cannot start sidplayfp: {e}; no music");
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
            *info = Some(line);
        }
        let _ = notify.write(&[1]);
        let deadline = started + Duration::from_secs(u64::from(seconds)) + GRACE;
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
                    eprintln!("nirulock: pause_music: sidplayfp was stopped by a signal; no music");
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
            if let Some(exclusions) = exclusions.as_ref() {
                exclusions.set(&[]);
            }
            let _ = notify.write(&[1]);
            return;
        }
        if started.elapsed() < Duration::from_secs(1) {
            quick_failures += 1;
            if quick_failures >= MAX_QUICK_FAILURES {
                eprintln!("nirulock: pause_music: sidplayfp keeps failing; no music");
                if let Ok(mut info) = shared.info.lock() {
                    *info = None;
                }
                let _ = notify.write(&[1]);
                return;
            }
        } else {
            quick_failures = 0;
        }
    }
}

/// Start one tune: `sidplayfp -q -os -o<song> -t<seconds> FILE`.
fn spawn(program: &Path, file: &Path, seconds: u32, song: u16) -> std::io::Result<Child> {
    // SAFETY: getpid is async-signal-safe and has no preconditions.
    let parent = unsafe { libc::getpid() };
    let mut command = Command::new(program);
    command
        .arg("-q")
        .arg("-os")
        .arg(format!("-o{song}"))
        .arg(format!("-t{seconds}"))
        .arg(file)
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
    use super::*;

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
        let mut child = spawn(&player, Path::new("tune.sid"), 5, 1).unwrap();
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
    fn settings_default_off_and_validate() {
        let d = Settings::default();
        assert!(!d.enabled);
        assert_eq!(d.folder, "~/Music/SID");
        assert_eq!(d.track_seconds, 180);
        assert!(d.stop_on_display_off);
        let parse = |text: &str| Settings::from_value(&text.parse::<toml::Value>().unwrap());
        let s = parse("enabled = true\nfolder = \"/srv/sid\"\ntrack_seconds = 60\nstop_on_display_off = false").unwrap();
        assert!(s.enabled && !s.stop_on_display_off);
        assert_eq!((s.folder.as_str(), s.track_seconds), ("/srv/sid", 60));
        assert!(parse("track_seconds = 1").is_err());
        assert!(parse("enabled = \"yes\"").is_err());
        assert!(parse("folder = \"\"").is_err());
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
}
