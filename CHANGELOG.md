# Changelog

Releases before 0.22.0 are described in their GitHub release notes and the
git history.

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
