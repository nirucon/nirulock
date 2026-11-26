# Changelog

Releases before 0.22.0 are described in their GitHub release notes and the
git history.

## 0.32.0 (2026-11-26)

### Added

- The info line under the clock names a tagged tune by its own title and
  artist when the artist is not NIRUX: "♪ Gymnopedie No. 1 · Erik Satie"
  instead of "♪ Gymnopedie · NIRUX 12".

- **`layout = "satie"`, a C64 tape loading screen of Erik Satie.** PRESS
  PLAY ON TAPE, SEARCHING, FOUND ERIK SATIE, and the portrait loads
  character cell by cell, first in greys while coloured loader stripes
  flicker in the wide border, then in colour, until READY. and the border
  settles; it loads again every five minutes. A text strip in NIRUSID's 8x8
  font shows ERIK SATIE 1866-1925, GYMNOPEDIES, the clock and the tune as
  `ARTIST - TITLE`. The clock, prompt, away time, keyboard layout, Caps Lock
  and failure messages stay on black below; a failed attempt turns the
  border to the warning colour; the settled border brightens on the beat.
- The picture is Henri Manuel's public domain photograph of Satie (about
  1920, Wikimedia Commons, `docs/CREDITS.md`), converted offline by the new
  `tools/satie-picture.py` to a 160x200 multicolour bitmap with six
  brightness levels and ordered dithering (16 KB in the binary), coloured
  from the active theme at run time.
- With `layout = "satie"`, NIRUX's Gymnopedie No. 1
  (`nirux-12-gymnopedie.ogg` in the NIRUX music folder) is the pause music,
  played on its own again and again, when it is installed; otherwise the
  usual pause music plays. The `[pause_music]` rules are unchanged.
- Frugal: only changed rows are drawn (PutImage); about 1 % of one core
  while loading and 0.1 % at rest in Xvfb at 1920x1080, nothing with the
  display off. `tests/resource-check.sh` and `tests/pause-music-reason.sh`
  cover it.

## 0.31.0 (2026-11-26)

### Added

- **`layout = "attract"`, an arcade cabinet in attract mode.** NIRUX RAID,
  an original C64 style vertical scrolling shoot 'em up made for NIRULOCK,
  plays itself: a rune craft flown by a small AI that looks a few moments
  ahead (dodging bullets and wisps, lining up under targets, picking up
  power runes for a wider shot or a shield) flies up a river through a
  Norse forest with runestones and the roots of Yggdrasil, against
  will-o'-wisp formations, iron drones, rune towers and longships, with
  particle explosions, a score, three lives and a high score. Like a C64
  game left alone it cycles title screen (raster bars behind the logo),
  HALL OF HEROES high score table, a game until GAME OVER, and round again
  with a new seed; a game that makes the table is added to it for the rest
  of the lock.
- A 320x200 picture with 16 colours derived from the active theme (the
  accent, a neighbour of it on the colour wheel for the river and one for
  the forest, from the warning colour when the accent is grey, the warning
  colour, the text colour and greys), in NIRUSID's 8x8 font, scaled by a
  whole number with sharp pixels inside a frame. The clock, prompt, away
  time, keyboard layout, Caps Lock and failure messages stay on plain black
  below it; a failed attempt turns the frame to the warning colour. Typing
  never reaches the game. While pause music plays, the bottom strip shows
  `NOW PLAYING  ARTIST - TITLE` and explosions flash on the beat. Every
  monitor shows the same game.
- Frugal: only the rows of the picture that changed are drawn, as merged
  rectangles, at 25 frames a second (fewer on very large screens), and
  nothing with the display off. In Xvfb at 1920x1080 the lock used about
  1 % of one core and 10 MB of memory.
- `tests/resource-check.sh` checks the attract layout's frame rate and CPU.

## 0.30.1 (2026-11-26)

### Fixed

- **The demo scroller names the tune by artist and title only.** It showed
  the whole info line (`NOW PLAYING: GIMLE * NIRUX 11 * ESC STOPS THE
  MUSIC`), and the quote that came right after it, separated like every
  other segment, read as a description of the song. The tune is now its
  own segment, `NOW PLAYING  NIRUX - GIMLE`, placed after the quote and
  before the password hint, and the quote is in quotation marks with its
  source. An Ogg Vorbis, Opus or FLAC file shows its `ARTIST` and `TITLE`
  tags (read from the first 64 KiB; a `COMMENT` or `DESCRIPTION` tag never
  shows), otherwise its file name (`nirux-11-gimle.ogg` is `NIRUX -
  GIMLE`); a SID tune shows `AUTHOR - TITLE` from its header. The
  info line of the other layouts is unchanged.

## 0.30.0 (2026-11-26)

### Changed

- **Pause music is on by default, for manual locks only.**
  `[pause_music] enabled` now defaults to `true`, and `on` still defaults
  to `["manual"]`: locking by hand plays NIRUX's own music, while a lock by
  idle, suspend, the lid or a script stays silent (a laptop that locks
  itself during a meeting plays nothing). Add reasons with, for example,
  `on = ["manual", "idle"]`; `enabled = false` turns the music off.
- **The tunes play shuffled.** A random first tune, then every tune once in
  a random order per round, never the same tune twice in a row. The new
  `shuffle` key (default `true`) brings back the natural order with
  `shuffle = false`. A SID tune plays its default subtune.
- The folder scan reads directory entries only (no `stat` per file, no
  sorting when shuffling), stops after 1.5 seconds and plays what it found,
  and allows up to 150,000 tunes and eight levels: a whole HVSC
  (`folder = "~/Music/SID"`, about 60,000 tunes) is listed in about 50 ms on
  a warm cache, in the background, so it never delays the lock or makes the
  music miss its start.

### Added

- **`layout = "demo"`, a C64 demo lock screen.** A big chunky NIRUX logo
  built from NIRUSID's own 8x8 pixel font (not the Commodore character set)
  wobbles over four moving raster bars and a parallax starfield; a sine
  scroller along the bottom carries the date and time, the away time, the
  hostname (only with `show_hostname = true`), the tune and a quote, between
  copper lines. Colours come from the active NIRUX theme (accent, two
  neighbouring hues, warning and text colour) on black. While music plays
  the bars and the logo flash on the beat, from the CAVA spectrum. The
  clock, prompt, away time, keyboard layout, Caps Lock and failure messages
  stay in the theme font in a calm black middle; a failed attempt also turns
  the bars to the warning colour. Every monitor shows the logo and the
  scroller. 25 frames a second (down to 15 on very large screens), only the
  animated bands are drawn, and nothing at all with the display off.
  `ascii-cava` stays the default.
- `docs/screenshots/demo.png`, unit tests for the shuffle, the scan budget,
  the demo scene (inside its bands on every monitor geometry), its colours,
  beat flash and frame rate, the pixel font and `config.example.toml`.

### Fixed

- `config.example.toml` had a stray `layout = "cava".` line that made the
  file invalid TOML when copied as it was.
- `tests/resource-check.sh` runs with a private `HOME` and XDG tree and a
  free display, takes screenshots with ffmpeg when `xwd` is missing, and
  checks the demo layout's CPU use.

## 0.29.0 (2026-11-21)

### Changed

- **NIRUX's own music is the default pause music.** `[pause_music]` now
  plays from `~/.local/share/nirux/music`, where NIRUX installs eleven
  original SID-style chiptunes (`nirux-1-ginnungagap.ogg` ...), instead of
  `~/Music/SID`. Your own SID collection still plays with
  `folder = "~/Music/SID"`. `enabled` stays `false` by default.

### Added

- The folder may hold audio files (`.ogg`, `.oga`, `.opus`, `.flac`, `.wav`,
  any case) as well as `.sid` tunes, mixed, in natural order. Audio files
  play to their end (30 minutes at most) with the first of `pw-play`
  (PipeWire), `ffplay` and `mpv`; two quick failures in a row hand them to
  the next player. `track_seconds` and `Songlengths.md5` apply to SID tunes
  only. The player is isolated exactly like sidplayfp: its own session, no
  terminal or input, `PR_SET_PDEATHSIG`, TERM then KILL on Esc, unlock,
  suspend and display off.
- The lock's own audio file stream carries the player's pid, so it never
  counts as other audio: pw-play gets `application.process.id` as a stream
  property (it sends none by itself), and mpv prefers its PulseAudio output.
- The info line of an audio file: `♪ Ginnungagap  ·  NIRUX 1  ·  Esc stops
  the music` for NIRUX's music, otherwise the file name.
- Ready means at least one tune with its player: an `.ogg` folder needs no
  sidplayfp. Tunes without a player are skipped with one log line, and the
  messages name what is missing ("no tunes in FOLDER", "no player for 3
  .sid tunes (sidplayfp is not installed)").

## 0.28.0 (2026-11-19)

### Added

- **`ascii-cava`, the new default layout.** NIRUX ascii with the CAVA
  spectrum under it: the banner and "A MINIMAL X11 DESKTOP" on every
  monitor, and on the prompt monitor the spectrum, exactly as wide as the
  banner, while music plays, then the clock, the password prompt and the
  away time. With `visual = "auto"` (the default) the panel stays empty
  when nothing plays: no quote competes with the banner. `layout = "cava"`
  brings back the previous default.
- `visual = "music"`: the spectrum while music plays, otherwise nothing, in
  `cava` too.

### Changed

- **Both ascii layouts are typed slowly and look like an old terminal.**
  The banner is typed one `#` at a time at NIRUSCREENSAVER's pace (about
  seven seconds with the line), the character just typed glows white and
  settles into the accent colour, a large banner has a dark scanline
  through every third pixel row, and a block cursor blinks after "A MINIMAL
  X11 DESKTOP" once the text is done (it stops while the display is off).
  The prompt works from the first moment. Only what changes is drawn and
  copied: one character while typing, the cursor's cell when it blinks.

## 0.27.0 (2026-11-19)

### Added

- A new layout, `ascii` ("NIRUX ascii"), the lock screen's twin of
  NIRUSCREENSAVER's `ascii` scene. When the screen locks, the large NIRUX
  banner is typed out behind a block cursor on every monitor, centred and
  sized for each one, with "A MINIMAL X11 DESKTOP" under it. The prompt
  monitor has the clock, the password entry and the away time below. The
  typing takes about two seconds and never delays the prompt: you can type
  the password at once. Afterwards the screen is still and the loop wakes
  once a minute, as before. The banner takes the theme's accent colour when
  it reads well on the background (WCAG contrast 4.5), otherwise the text
  colour. Choose it with `layout = "ascii"` in `~/.config/niruui/lock.toml`;
  `layout = "cava"` (the default) goes back.

## 0.26.0 (2026-11-12)

### Added

- A typewriter click while you type the password: each key that changes the
  entry (a character or Backspace) asks NIRUNOTIFY 0.6.0 or later for the
  new NIRUX sound `lock-typing` (category `system`, on by default; off with
  `nirux sounds` or `lock-typing = false` under `[sounds.system]` in
  `~/.config/nirux/sounds.toml`). Enter, Esc, modifiers, arrows and
  Backspace on an empty entry stay silent. A held key's auto-repeat does not
  click again (the same key within 100 ms), and there is at most one click
  per 35 ms. The request goes to one sender thread through a small queue
  that drops a click rather than wait, so input and PAM are never delayed;
  without NIRUNOTIFY nothing happens. Nothing about the keys is logged.

### Tests

- `tests/typing-sound.sh` (in CI): in its own Xvfb with a fake NIRUNOTIFY
  socket, typed characters and Backspace click, Esc, Enter, modifiers and
  Backspace on an empty entry do not.

## 0.25.0 (2026-11-09)

There is no 0.24.0 release: its tag was pushed by mistake on the 0.23.1
commit and its release build was cancelled. 0.25.0 is the first release with
these changes.

### Changed

- The pause music plays only when you lock by hand. The lock now knows why
  it was started: `nirulock --reason manual|idle|suspend|script|session`, or
  the `NIRULOCK_REASON` environment variable for callers that must also work
  with older releases (the argument wins). `[pause_music]` starts only for
  the reasons in the new `on` setting, by default `["manual"]`. An idle lock,
  the lock before suspend, a script's or SSH lock (`niructl lock --silent`)
  and an older caller that gives no reason stay quiet; after a rollout locked
  five machines at once, all five had started playing. Existing configs work
  unchanged and get the new default. A missing or unknown reason never stops
  the lock from starting; it only means no music. Esc still stops the music
  and returns to the quote. Authentication, the grabs and PAM are unchanged.
- `nirulock --help` describes the options. `--config` names the reasons the
  music plays for.

### Tests

- `tests/pause-music-reason.sh` (in CI): in its own Xvfb with isolated XDG
  directories, no sound server and a fake `sidplayfp`, the music starts for
  `manual` (argument or environment) and the configured reasons only, never
  without a reason, and Esc still stops it.

## 0.23.1 (2026-11-08)

### Fixed

- Esc during the pause music now returns the panel to a quote promptly, as
  when no music plays. The audio detector still heard "playing" for its
  5 s silence window, so the spectrum lingered, flat, for about six seconds
  (measured on othala) before the quote faded in. With no other stream the
  quote now fades in at once. With other streams (an idle synthesizer, or
  real music) the detector is rearmed once the player's last sound has
  left, and the spectrum is held for under a second while it decides:
  music that really plays keeps the spectrum without a flicker, and anything
  else fades to the quote within about 1.3 s. A stopped player stays
  excluded from "other audio", so its lingering stream cannot flip the
  panel back. The info line already disappeared at once, and its row stays
  reserved, so nothing moves.

## 0.23.0 (2026-11-08)

### Changed

- The lock screen's layout is cleaner and properly centred. Same elements,
  same colours and fonts:
  - Every centred line is centred by its ink, measured from the glyphs. The
    pen now follows the font's exact advances (and kerning) instead of
    rounding each advance up, which pushed long lines right of their
    measured centre (the clock by 5–8 px, quote lines by up to 4 px), and a
    trailing space no longer pulls "password:" 3 px left.
  - The `cava` layout is one column with an even vertical rhythm: clock,
    hostname, panel, pause-music line, `locked`, entry, away time. The block
    sits at the optical centre of the prompt output (47 % of its height).
  - Rows that come and go are reserved: a failure or PAM message, the
    failed-attempt count, Caps Lock and the pause-music line never move the
    entry or anything above it. Typing never slides the entry either: the
    label stays put and the echo grows to the right (`count` shows at most
    32 stars).
  - Long PAM messages wrap to two balanced lines in the text column and are
    cut with an ellipsis after that; the music line and the hostname are
    cut to the panel's width. Quote lines wrap evenly (no lone last word).
  - The spectrum is centred on the text column and as wide as its bars, so
    its guides end where the outer bars do.
- The runic layout leaves its ornament out when no installed font has the
  runes, instead of drawing missing-glyph boxes.

CAVA pacing is unchanged (each frame painted as it arrives, `draw_cava_frame`
with `present_rect`); input, PAM, grabs, Esc and pause music are untouched.

## 0.22.2 (2026-11-08)

### Fixed

- Pause music was silent on sessions started from a text console (void's
  `startx` login) although the lock showed the tune. sidplayfp opens
  `/dev/tty` for its keyboard controls, and as a background process group on
  the session's controlling terminal it was stopped by SIGTTOU/SIGTTIN. The
  player now starts in its own session without a controlling terminal
  (`setsid`); its pid is still its process group, so stopping it is
  unchanged. A player that stays stopped for 2 s is ended and the music gives
  up for this lock instead of showing a tune that does not play.

## 0.22.1 (2026-11-08)

### Fixed

- The CAVA spectrum no longer stutters. It was repainted on a 30 fps timer of
  its own, which drifted against CAVA's clock (29.8 frames a second measured):
  every second or so a frame was shown twice and another skipped, and each
  frame was shown up to 34 ms late. Each CAVA frame is now painted as it
  arrives, like NIRUAMP: no repeated or skipped frames, 0.1 ms from arrival
  to paint. A 100 ms fallback keeps the silence pause and a dead child
  noticed; silence, display-off and pause-music behaviour are unchanged.
- `NIRU_CAVA_TRACE` lines carry the CAVA frame's sequence number and age.

## 0.22.0 (2026-11-08)

### Added

- `[pause_music]`, off by default: Commodore 64 SID tunes from `~/Music/SID`
  (setting `folder`) through `sidplayfp` while the screen is locked.
  - Starts only when no other audio plays in the first 2 s after locking (5 s
    with a quiet stream present). The lock's own player stream never counts as
    other audio, but CAVA visualises it.
  - Natural order (`1-`, `2-`, `10-`), looping. Each tune plays for its length
    from the HVSC `Songlengths.md5` in the folder, otherwise for
    `track_seconds` (default 180).
  - A subtle info line from the PSID/RSID header: `♪ Title — Author · Year  ·
    Esc stops the music`.
  - Stops on unlock, on Esc, before suspend (logind/elogind
    `PrepareForSleep`) and, with `stop_on_display_off = true` (the default),
    when the display goes off. Once stopped it stays stopped for that lock.
  - The player is an unprivileged child in its own process group with no
    input, killed with TERM and then KILL, and it dies with the lock. The lock
    never waits for it, and a crashing player does not affect it.
  - Without `sidplayfp`, one line is logged and the lock stays silent.
- `tests/fixtures/make-test-sid.py` writes a small original test tune.

### Changed

- Escape still clears the password entry and now also stops the pause music.
  It never dismisses the lock.

Authentication, PAM and the grabs are unchanged.
