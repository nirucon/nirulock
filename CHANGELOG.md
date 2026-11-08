# Changelog

Releases before 0.22.0 are described in their GitHub release notes and the
git history.

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
