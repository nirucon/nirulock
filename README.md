# NIRULOCK

**Part of [NIRUX - A minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

**Early beta.** Native X11/PAM lock screen for the NIRU desktop, with clock, hostname,
away time and an optional CAVA spectrum in the active theme.


## Install

Use [NIRUX](https://github.com/nirucon/nirux#install) for the complete desktop.
Standalone x86_64/glibc releases need no Rust:

```sh
git clone https://github.com/nirucon/nirulock
cd nirulock
./install.sh
```

The installer requests sudo to write `/etc/pam.d/nirulock`, which includes the
system authentication stack. Source builds use `./install.sh --build` and need
Rust 1.75+, a C linker, PAM development headers and a monospace font.

## Use

```sh
nirulock
nirulock --config
nirulock --version
```

Super+Escape locks in NIRUWM. The lock shows a Caps Lock indicator when active
and, when more than one keyboard layout is configured, the active one next to
the prompt (`[SE] password:`), because a password typed in the wrong layout is
the most common wrong password.

Settings live in `~/.config/niruui/lock.toml` (XDG respected). Optional
`font_path`, `rune_font_path`, `font_size`, `prompt_echo` and `[palette]`
overrides are shown in [config.example.toml](config.example.toml).

`prompt_echo = "count"` (the default) shows one `*` per typed character.
`prompt_echo = "pulse"` shows a fixed row of eight cells with one lit `*` that
jumps to a random cell on every key, so the password length is never shown.

### Layouts

Choose one with `layout = "minimal" | "terminal" | "runic" | "corner" | "cava"`:

- `cava` — the default: large clock and hostname above a live CAVA spectrum, with
  the lock state, away time and password prompt kept below the bars.
- `minimal` — the original centred clock, hostname, lock state, away time and prompt.
- `terminal` — left-aligned monospace lock screen with a `hostname:~$ lock` line.
- `runic` — the minimal composition with a discreet rune row when a runic font is
  available. Runes are ornamental, never a second prompt or a hidden unlock path.
- `corner` — clock top-left, hostname and away time at the bottom, prompt centred.

The `cava` layout samples system audio through [CAVA](https://github.com/karlstav/cava)
at 30 frames per second with the same smoothing as NIRUAMP. Between full lock
frames only the spectrum rectangle is repainted, and CopyArea exposures are
disabled. The spectrum pauses whenever it cannot be seen or has nothing to
show: with the display off (DPMS standby, suspend or off) CAVA is stopped and
restarted as soon as the display is on, and after two seconds of silence the
repaint stops on a flat frame until the first sound. A locked machine with no
audio then only wakes for the minute clock. When sound returns, the bars rise
from the flat frame with the normal attack instead of jumping to the first
sample, and quiet sounds just above the noise floor grow from zero. Bars follow the active
NIRUX theme's focus colour and lighter peak. Set `visualizer = false` to
keep the cava composition without the spectrum. If CAVA is not installed, the
lock still works and simply omits the bars; the password prompt never moves
behind or captures the visualizer.

The NIRUX theme controls colours; the layout only controls composition. The lock
always stays opaque black. `nirulock --config` prints the selected layout. Diagnostics are written to
`~/.local/state/nirulock/trace.log`; password characters, keycodes and modifiers
are never logged.

## Behaviour

Only successful PAM authentication unlocks. The account is the owner of the
process, from the password database; if it cannot be found the lock refuses to
start rather than fall back to `$USER` or `root`. Messages from PAM modules are
shown under the prompt after a failed attempt, for example `pam_faillock`'s
"The account is locked due to 3 failed logins", so a lockout no longer looks
like a wrong password. A failure is shown for two seconds in the theme's
warning colour together with the number of failed attempts since locking; the
entry stays usable the whole time. Every copy of the password (the entry, the
authentication worker and the PAM response) is wiped after use. NIRULOCK publishes
`_NIRU_LOCK_READY` after verified keyboard/pointer grabs and mapping, and clears
it on unlock; `niructl suspend` requires that readiness. Content follows the
selected suite theme on an opaque black background. The prompt stays on its
selected output and falls back if that output disappears. Authentication runs on
one worker while X11 remains responsive.

### Input

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

## NIRUX suite

[NIRUX](https://github.com/nirucon/nirux) · [NIRUWM](https://github.com/nirucon/niruwm) · [NIRUBAR](https://github.com/nirucon/nirubar) · [NIRUSCREENSAVER](https://github.com/nirucon/niruscreensaver) · [NIRULOCK](https://github.com/nirucon/nirulock) · [NIRULAUNCHER](https://github.com/nirucon/nirulauncher) · [NIRUNOTIFY](https://github.com/nirucon/nirunotify) · [NIRUSHOT](https://github.com/nirucon/nirushot)

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
sh tests/installer.sh
sh tests/resource-check.sh target/release/nirulock
```

`NIRU_CAVA_TRACE=/path/to/file` appends one line per spectrum frame (the
milliseconds since start, `L`ive, `S`ilent or display `O`ff, and the 48 bar
heights) for measuring motion and frame pacing; it records nothing else.

`tests/compose-unlock.sh` types a password with dead keys and keypad digits in
its own Xvfb; run it as a disposable test account whose password is the
fixture described in the script.

MIT. Copyright 2026 Nicklas Rudolfsson.
