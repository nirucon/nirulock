# How NIRULOCK behaves

Technical details behind the [README](../README.md): authentication, input,
the CAVA spectrum and the music/quote switch.

## Authentication

Only successful PAM authentication unlocks. The account is the owner of the
process, from the password database; if it cannot be found the lock refuses to
start rather than fall back to `$USER` or `root`. Messages from PAM modules are
shown under the prompt after a failed attempt, for example `pam_faillock`'s
"The account is locked due to 3 failed logins", so a lockout no longer looks
like a wrong password. A failure is shown for two seconds in the theme's
warning colour together with the number of failed attempts since locking; the
entry stays usable the whole time. With NIRUNOTIFY running, a rejected password
also plays the soft NIRUX `auth-fail` sound, and locking and unlocking can play
a near-silent chime (`lock`/`unlock`, off by default in
`~/.config/nirux/sounds.toml`). The request is one line on NIRUNOTIFY's socket,
sent after PAM has answered; it never affects authentication. Each key that
changes the password entry (a character or Backspace, not Enter, Esc,
modifiers or arrows) asks for a quiet typewriter click, `lock-typing` (category
`system`, on by default; turn it off with `nirux sounds` or
`lock-typing = false` under `[sounds.system]`). The request goes to one sender
thread through a small queue that drops rather than waits, so typing and PAM
never wait for it. A held key's auto-repeat stays silent (the same key again
within 100 ms) and clicks are at most one per 35 ms. Every copy of the password (the entry, the
authentication worker and the PAM response) is wiped after use. NIRULOCK publishes
`_NIRU_LOCK_READY` after verified keyboard/pointer grabs and mapping, and clears
it on unlock; `niructl suspend` requires that readiness. Content follows the
selected suite theme on an opaque black background. The prompt stays on its
selected output and falls back if that output disappears. Authentication runs on
one worker while X11 remains responsive.

## Input

Keys are translated like in a normal X session: through libxkbcommon's XKB
state (Shift, AltGr/Level3, Level5, NumLock and layout groups) and the locale's
compose table, so dead keys (`´ ` ^ ~ ¨` on the Swedish layout), composed
letters such as `é` and keypad digits all reach PAM as the same characters a
terminal produces. libxkbcommon, libxkbcommon-x11 and libxcb are loaded at
runtime; without them the lock still starts and uses the core keymap with a
built-in compose table that matches libX11 for the Latin-1 dead keys. Escape
clears the entry, Backspace first cancels a pending dead key. Input methods
(IBus, Fcitx) are not used: the lock holds the keyboard grab itself. X11
cannot isolate the lock from hostile clients on the same display server.


## The CAVA spectrum

The `cava` layout samples system audio through [CAVA](https://github.com/karlstav/cava)
at 30 frames per second with the same smoothing as NIRUAMP. Like NIRUAMP it
paints each CAVA frame the moment it arrives, so every frame is shown exactly
once and without delay (a separate 30 fps timer drifted against CAVA's clock
and repeated or skipped a frame every second or so). Between full lock
frames only the spectrum rectangle is repainted, and CopyArea exposures are
disabled. The spectrum pauses whenever it cannot be seen or has nothing to
show: with the display off (DPMS standby, suspend or off) CAVA is stopped and
restarted as soon as the display is on, and after two seconds of silence the
repaint stops on a flat frame until the first sound. A locked machine with no
audio then only wakes for the minute clock. When sound returns, the bars rise
from the flat frame with the normal attack instead of jumping to the first
sample, and quiet sounds just above the noise floor grow from zero. (With the
default `visual = "auto"`, CAVA only runs while music plays.) Bars follow the active
NIRUX theme's focus colour and lighter peak. Set `visualizer = false` to
keep the cava composition without the spectrum. If CAVA is not installed, the
lock still works and simply omits the bars; the password prompt never moves
behind or captures the visualizer.

## Music detection (`visual = "auto"`)

With `auto`, a small PulseAudio client (PipeWire serves the same API through
`pipewire-pulse`; libpulse is loaded at run time, so it is not a build or run
dependency) watches the playback streams through subscription events. Only
while a stream is playing (not corked) does it open a peak monitor on the
default sink's monitor, at ten peaks a second delivered five times a second;
with no stream nothing is polled at all. CAVA is started when the peak has
stayed above about -54 dBFS for 300 ms, so an always-present but silent stream
(an idle MIDI synthesizer) never starts it, and it is stopped and reaped, not
just paused, after 5 s of silence. The change fades the quote out and the
spectrum in; the bars still rise from flat with the usual attack. Without a
sound server the panel shows quotes and the client retries in the background
(every 2 s, backing off to once a minute). With the display off nothing runs:
no CAVA, no peak monitor, no quote changes.

A quote changes to another, chosen at random, every minute with a soft fade;
between changes nothing is redrawn. It is drawn in the NIRUX theme font and
colours, centred where the spectrum would be, wrapped to about 64 characters,
with a smaller size (down to half, at least 11 px) when it is long and an
ellipsis when it still does not fit. Attributions are muted.

The quote collections themselves are described in [QUOTES.md](QUOTES.md).

## Pause music (`[pause_music]`)

Off by default. When `enabled = true`, and the lock's reason (`--reason` or
`NIRULOCK_REASON`; see the README's Usage) is in `on` (default `["manual"]`),
a `nirulock-music` thread scans
`folder` (default `~/Music/SID`, recursively, at most six levels and 10,000
files, hidden entries and symlinked directories skipped) and looks for
`sidplayfp` on `PATH`, while the lock is already shown. The lock itself only
reads atomics and a mutex-guarded info line; it never waits for the thread or
the player.

**Why the lock was started.** For any other reason (idle, suspend, script,
session, or a caller that gave none) the music thread is not started at all:
no folder scan, no `sidplayfp`, no audio detector for the gate. The trace log
says `pause music not started: lock reason idle`. The reason is read once at
start, the variable is removed from the lock's environment so CAVA and the
player do not inherit it, and an unknown or missing value is
`unspecified`: the lock starts exactly as before, only without music. The
reason never changes authentication, the grabs or how the lock is dismissed.

**Starting.** The start gate looks at what the audio detector hears from
streams *other than the lock's own player*: sink inputs are matched to the
player by `application.process.id`, so CAVA may visualise the tune while it
never counts as other audio. Because the detector measures peaks on the sink's
mix, other audio means "another stream is playing and the mix is loud"; with
no other stream there is certainly none. The music starts when nothing else
played during the first 2 s after locking, or after 5 s when a quiet stream
(an idle synthesizer) is present but never loud. Any other music in that
window means no pause music for this lock. A display that is off only holds
the decision; if it stays off for 5 s, there is no music. If the folder has no
tunes or `sidplayfp` is missing, the lock logs one line and stays silent.
When `visual` is not `auto`, the music runs its own detector for the gate
and drops it once the decision is made.

**Playing.** The tunes play in natural order of their path (digit runs compare
as numbers, letters without case), looping. Each runs as
`sidplayfp -q -os -o<start song> -t<seconds> FILE`, where the length comes from
the HVSC `Songlengths.md5` (by the MD5 of the whole file, or by an unambiguous
file name) for the header's start song, otherwise `track_seconds`. A player
that does not end on its own is stopped 3 s after that. A player that crashes
just moves on to the next tune; five tunes in a row that end within a second
end the music for this lock. The info line under the clock or panel comes from
the PSID/RSID header (Latin-1, control characters removed): `♪ Title — Author ·
Year  ·  Esc stops the music`, with the file name when the title is unknown.
When the theme font has no `♪`, it is drawn from DejaVu Sans, or left out.

**Isolation.** The player is a child of the lock owned by the same user (the
lock has no privileges), in its own session and process group without a
controlling terminal (so its `/dev/tty` keyboard handling cannot stop it with
SIGTTOU on a console-started session), with stdin, stdout and
stderr on `/dev/null`, and `PR_SET_PDEATHSIG` set to `SIGKILL`, so it dies
with the music thread even if the lock itself is killed. Its arguments are
only a time, a subtune and a path: nothing typed on the lock screen reaches it
or any log. Authentication, the grabs and the PAM service are unchanged; the
music adds no way to dismiss the lock.

**Stopping.** Unlock, Esc, suspend and (with `stop_on_display_off = true`, the
default) the display going off send `SIGTERM` to the player's process group
from the lock's event loop, which never blocks; the music thread follows with
`SIGKILL` after 0.5 s and reaps the player. Suspend is seen through
`dbus-monitor --system` watching logind's (or elogind's) `PrepareForSleep`;
either value stops the music, and a gap between the boot and monotonic clocks
catches a sleep that was not announced. Once stopped, the music stays stopped
until the next lock.
