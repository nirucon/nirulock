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

On by default since 0.30.0 (`enabled = true`), for manual locks only. When
it is enabled and the lock's reason (`--reason` or `NIRULOCK_REASON`; see
the README's Usage) is in `on` (default `["manual"]`), a `nirulock-music`
thread scans
`folder` (default `~/.local/share/nirux/music`, where NIRUX installs its own
music; recursively, at most eight levels and 150,000 tunes, hidden entries
and symlinked directories skipped; directory entries only, the file type
comes with the entry and only a symbolic link costs a `stat`; the scan stops
after 1.5 s and plays what it found, which a 60,000-tune HVSC tree on a warm
cache needs about 50 ms for) for `.sid` tunes and audio files (`.ogg`,
`.oga`, `.opus`, `.flac`, `.wav`, any case) and looks for their players on
`PATH` (`sidplayfp`; the first of `pw-play`, `ffplay` and `mpv`), while the
lock is already shown. The lock itself only
reads atomics and a mutex-guarded info line; it never waits for the thread or
the player.

**Why the lock was started.** For any other reason (idle, suspend, script,
session, or a caller that gave none) the music thread is not started at all:
no folder scan, no player, no audio detector for the gate. The trace log
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
the decision; if it stays off for 5 s, there is no music. The music is ready
when at least one tune has its player: a folder of audio files needs no
sidplayfp, a folder of SID tunes no audio player. Tunes without a player are
skipped with one log line (`no player for 3 .sid tunes (sidplayfp is not
installed); they are skipped`). Without any tune it can play (`no tunes in
FOLDER`, `...; no music`) the lock stays silent.
When `visual` is not `auto`, the music runs its own detector for the gate
and drops it once the decision is made.

**Playing.** With `shuffle = true` (the default) the tunes play from a
shuffled deck: a Fisher-Yates permutation of their indices (four bytes a
tune, made when the first tune plays) from an xorshift generator seeded with
the time and the process id, so the first tune is random; every tune plays
once per round, and a new round never starts with the tune that ended the
last one. With `shuffle = false` they play in natural order of their path
(digit runs compare as numbers, letters without case). Either way SID tunes
and audio files are mixed, looping.
A SID tune runs as `sidplayfp -q -os -o<start song> -t<seconds> FILE`, where
the length comes from the HVSC `Songlengths.md5` (by the MD5 of the whole
file, or by an unambiguous file name) for the header's start song, otherwise
`track_seconds`. An audio file plays to its natural end, 30 minutes at most,
with the first player found:

- `pw-play FILE` (PipeWire), started as `sh -c 'exec "$0" -P "{
  application.process.id = $$ }" "$1"' pw-play FILE`: a pw-play stream
  carries no process id of its own in the PulseAudio view, so it is given
  the player's pid, which the shell keeps through `exec`;
- `ffplay -nodisp -autoexit -loglevel quiet FILE` (a PulseAudio client, which
  sends its pid);
- `mpv --no-video --really-quiet --no-terminal --ao=pulse, FILE` (PulseAudio
  first, for the pid, then mpv's own choice).

A player that does not end on its own is stopped 3 s after its time. A player
that crashes just moves on to the next tune; five tunes in a row that end
within a second end the music for this lock. Two audio files in a row that
end within a second (pw-play without a PipeWire server) hand the audio files
to the next player found. The info line under the clock or panel comes from
the PSID/RSID header for a SID tune (Latin-1, control characters removed):
`♪ Title — Author · Year  ·  Esc stops the music`, with the file name when the
title is unknown. For an audio file it comes from the file name:
`nirux-1-ginnungagap.ogg` is `♪ Ginnungagap  ·  NIRUX 1  ·  Esc stops the
music`, any other file its name with `-` and `_` as spaces.
When the theme font has no `♪`, it is drawn from DejaVu Sans, or left out.

**Isolation.** The player is a child of the lock owned by the same user (the
lock has no privileges), in its own session and process group without a
controlling terminal (so its `/dev/tty` keyboard handling cannot stop it with
SIGTTOU on a console-started session), with stdin, stdout and
stderr on `/dev/null`, and `PR_SET_PDEATHSIG` set to `SIGKILL`, so it dies
with the music thread even if the lock itself is killed. Its arguments are
only fixed options, a time, a subtune and a path: nothing typed on the lock screen reaches it
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

## The demo layout (`layout = "demo"`)

Each monitor gets two animated bands: the logo band at the top (stars, four
raster bars, the NIRUX logo with an outline and a drop shadow) and the
scroller band at the bottom (stars, copper lines, the sine scroller). The
middle stays black; on the prompt monitor it holds the clock, the entry,
the away time and the status lines in the theme font. When the prompt lines
need more room (a small screen, a large font), the logo band gives way.

A frame is a list of solid rectangles per colour, computed without I/O
(`demo::scene`) and sent as a few dozen `PolyFillRectangle` requests into the
backing pixmap; black is filled only where no bar covers the band, and only
the two bands are copied to the screen. Nothing else on the screen is
redrawn between frames. The frame interval is 40 ms (25 frames a second)
and grows to at most 66 ms when the bands of all monitors together exceed
two full HD screens' worth of pixels, keeping the cadence (a late frame does
not shift the following ones). With the display off (DPMS) the scene is not
drawn and the loop sleeps until the next clock minute or a DPMS change.

The scroller's text is built when the previous text has scrolled out: the
date and time, the away time, the hostname with `show_hostname = true`, the
next quote in quotation marks with its source, and the tune as its own
segment, `NOW PLAYING  ARTIST - TITLE` (Vorbis comment `ARTIST` and `TITLE`
tags, read from the first 64 KiB of an Ogg Vorbis, Opus or FLAC file, else
the file name; a SID tune's author and title from its header), followed by
the password hint, so the quote never reads as the tune's description. It
is shown in NIRUSID's 8x8 font,
upper case, with accents and Swedish letters reduced to their plain letters
and characters the font lacks left out. With `visual = "auto"` the panel's
audio detector starts CAVA while music plays; the demo reads the spectrum in
its own frames and flashes when the bass jumps above its running average,
fading in about a third of a second.

## The attract layout (`layout = "attract"`)

The game (`arcade.rs`) runs in fixed ticks, 25 a second, on a 320x200
canvas of 16 palette indices (`chunky.rs`). It is fully determined by its
seed: the lock takes a new seed from the clock at start, and every new game
derives its own from it. The loop is title (8 s), high scores (7 s), the
game until the last of three lives is lost, GAME OVER (4 s). The AI picks,
every tick, one of fifteen moves by looking twelve ticks ahead at the
bullets and the flying enemies (the wisps along their wave), plus a pull
towards the lowest target or a power rune; now and then it has a short
lapse and flies straight on, more often as the game gets harder (one level
every 40 seconds), so every game ends. The high score table lives as long as
the lock; a game that beats an entry is added as `NRX`.

Each frame the canvas is compared row by row with the previous one; the rows
that changed become runs of one colour, merged down the rows while they
repeat, and are sent as at most 16 `PolyFillRectangle` requests (one per
colour) per monitor into the backing pixmap, scaled by the whole number
that fits (the playfield and the prompt lines together on the prompt
monitor, the playfield alone, centred, on the others). Only the rows that
changed are copied to the screen; the frame around the playfield is drawn
when its colour changes (the warning colour after a failed attempt). The
frame interval follows the demo layout's rule. The game time is the time
between frames, at most 0.2 s at once, so a slow frame never makes the game
jump, and with the display off nothing is drawn and the game waits. A typed
key repaints the screen with the game as it is at that moment; it never
changes the game. The music's beat (the same spectrum path as the demo)
only changes how bright the explosions and the muzzle flash are drawn.

## The satie layout (`layout = "satie"`)

A load follows a fixed timeline (`satie::Stage`): PRESS PLAY ON TAPE (1.5
s), SEARCHING (2 s), FOUND ERIK SATIE (1.5 s), the bitmap (13 s) and the
colours (6 s), each 1,000 character cells of 4 by 8 wide pixels, left to
right and top to bottom, then READY. A new load starts five minutes after
the last one began. The canvas is 320x224: the 160x200 picture with every
pixel doubled across, and a two-line text strip.

The picture (`assets/satie.bin`, 16,000 bytes, see `docs/CREDITS.md`) holds
six brightness levels; while the bitmap loads they are drawn in greys, and
the colour pass maps them to black, the muted colour darkened, and the
accent dark, as it is and light, and white. Each frame the canvas rows that
changed since the last one are sent with PutImage, scaled by the whole
number that fits (at most 200 KB a request): during the load that is one
band of cells, at rest nothing (the clock in the strip changes once a
minute). The border is drawn as rectangles around the picture: random
stripes in five theme colours every frame while loading, then one settled
colour that brightens a little on the beat while music plays, the warning
colour after a failed attempt. Loading and following the beat run at the
demo layout's frame rate; at rest the layout looks four times a second.
With the display off nothing is drawn.

With `layout = "satie"` and `~/.local/share/nirux/music/nirux-12-gymnopedie.ogg`
present and playable, the music thread plays that one file again and again
instead of scanning the folder; the start gate, the lock reasons, Esc and
the display-off stop work as for any pause music. Without it the folder
plays as usual.

## The bbs layout (`layout = "bbs"`)

The screen is 80 columns by 30 rows of 8x16 character cells (a 640x480
canvas), shown at the largest whole scale that fits each monitor, centred,
on every monitor; the prompt is part of it. A call follows a timeline
(`bbs::Phase`). The first call types `ATZ` and `AT&F1 M1 L1` (each answered
`OK`) in its first 2.2 seconds, then waits, at most until the pause music's
start gate decides (two to five seconds after locking), so that the dialing
and the handshake sound begin together. From the dial: `ATDT 555-64789`
(typed at once), `RINGING` at 2 s, `CARRIER 14400`, `PROTOCOL: LAP-M`,
`COMPRESSION: V.42BIS` and `CONNECT 14400/ARQ/V42BIS` at 5.75 s, and from
6.45 s the board arrives row by row at 1,440 characters a second (14400
bps, ten bits a character), about 1.7 seconds for the whole screen. A key
that reaches the lock (anything typed, Backspace, Escape) skips to the
board, and so does a check or a failed attempt.

Five minutes after a call began a new one starts (`NO CARRIER`, then the
dial again, without the reset and without sound) if the entry is empty, no
key came in the last minute and no attempt has failed in this lock; the
message of the day changes with it. With `[motion] reduced = true` in
`~/.config/niruwm/appearance.toml` (the appearance contract, schema 1) the
board shows at once, the cursor and the lights stay still and the line
never drops.

The board's static part is `assets/nirux-bbs.ans`, read once: CP437 bytes,
CR and LF, and SGR colour codes (bold for a bright foreground, blink for a
bright background as with iCE colours), up to the end-of-file mark before
the SAUCE record; a line wraps after the 80th column. The values, the
message of the day, the tune, the prompt, the lines under it and the status
bar are written into a copy every frame. Block elements, the three shades,
the single and double box lines and a few symbols (`·`, `■`, the notes) are
drawn from fixed pixel patterns; printable ASCII uses NIRUSID's 8x8 font
with every row doubled; any other code is a small block, so nothing ever
depends on a font.

The ANSI colours map to the theme palette of the chunky layouts (accent,
its cool and warm neighbours, warning and greys, each from the active
theme): 0 black, 1 red the warning colour, 2 green the warm neighbour,
3 brown the accent dark, 4 blue the cool neighbour dark, 5 magenta the warm
neighbour dark, 6 cyan the cool neighbour, 7 light grey, 8 dark grey,
9 light red the warning light, 10 light green the warm neighbour light,
11 yellow the accent light, 12 light blue the accent, 13 light magenta the
muted grey, 14 light cyan the cool neighbour light, 15 white.

Each frame the cells that differ from the last frame are drawn into the
canvas, and the canvas rows they cover are sent with PutImage (at most 200
KB a request) and copied to the screen. While a call runs the layout looks
every 40 ms; online it wakes for the cursor's blink (every 0.53 s), while
pause music plays every 80 ms for the RD light (the bass of the CAVA
spectrum, as in the demo), and for the SD light going out 150 ms after a
key. With the display off nothing is drawn.

### The modem handshake

With `layout = "bbs"`, `modem::wav` makes a 5.9 second mono WAV file
(22,050 Hz, 16 bit): 0.3 s of silence, a dial tone (350 and 440 Hz), the
touch tones of 555-64789, one ring back (440 and 480 Hz), the 2100 Hz
answer tone with a phase reversal, two short tones, and the training hiss
from a 23-bit noise register like the SID's, over a faint carrier. Square
and triangle voices with 8 ms attack and release go through a one-pole
low-pass at 3.5 kHz; the peak is 0.16 of full scale. The music thread
writes it, when the start gate has opened, to
`$XDG_RUNTIME_DIR/nirulock-intro-PID.wav` (a new file, mode 0600, never
following a link; the temporary directory without a runtime directory),
plays it once with the first audio player found, as an isolated child like
every tune (its own session, no terminal, killed with the lock, excluded
from "other audio"), removes it, and goes on with the pause music. Esc,
suspend and the display going off stop it like the music. Without an audio
player, or when the file cannot be written, the music starts without it.
