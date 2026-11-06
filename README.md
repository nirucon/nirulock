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
sample, and quiet sounds just above the noise floor grow from zero. (With the
default `visual = "auto"`, below, CAVA only runs while music plays.) Bars follow the active
NIRUX theme's focus colour and lighter peak. Set `visualizer = false` to
keep the cava composition without the spectrum. If CAVA is not installed, the
lock still works and simply omits the bars; the password prompt never moves
behind or captures the visualizer.

### Music or quotes (`visual`)

`visual` decides what the `cava` layout shows where the spectrum is drawn:

- `auto` (the default): the CAVA spectrum only while music or radio plays, and
  otherwise one quote from the [quote collections](#quotes) below.
- `cava`: the spectrum alone, exactly as before 0.19 (CAVA runs whenever the
  display is on).
- `quotes`: quotes alone; CAVA never starts.
- `none`: neither; the panel stays empty. `visualizer = false` without a
  `visual` setting means `none`, as before.

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

### Quotes

The quotes live in `${XDG_CONFIG_HOME:-~/.config}/niru/quotes/`, shared by
NIRULOCK and NIRUSCREENSAVER. Every `NAME.txt` file there is one
*collection*, named `NAME`. When the directory is missing, either program
creates it (atomically; directory 0755, files 0644) with the built-in
collections:

| Collection | Contents |
|---|---|
| `havamal-en` | Hávamál in English |
| `havamal-sv` | Hávamál in Swedish |
| `stoic-en` | Stoic quotes in English |
| `stoic-sv` | Stoic quotes in Swedish |

After that the directory is yours: neither program ever writes into it
again, so an edited collection stays edited and a deleted one stays deleted.
To get the built-in collections back, remove (or rename) the directory; it
is created again the next time the lock or screensaver starts. Each file's
own header explains the format with an example.

#### The format

```text
# Lines starting with # are comments.
Quotes are separated by one or more blank lines.
A quote may span several lines;
its line breaks are kept.
— A line starting with "— " (em dash, space) or "-- " is the attribution

He who waits for something good
never waits too long.
-- Swedish proverb
```

- **Add a quote:** write it anywhere in a collection, for example at the end,
  with a blank line before and after it.
- **Remove a quote:** delete its lines, or put `#` in front of each of them.
- **Add a collection:** create another `.txt` file in the directory, for
  example `mine.txt` (the collection `mine`). Files whose names start with a
  dot, and files that are not `.txt`, are ignored.
- **Remove a collection:** delete its file, or leave it out of `quotes` below.

Files must be UTF-8. Very long quotes are shortened with an ellipsis so that
they fit. At most 64 collections are read (in name order), and a file larger
than 256 KiB is skipped. Changes are picked up the next time the lock or
screensaver starts and, while one is showing, at the next quote change. The
files are read on their own thread, never on the input or drawing path.

#### Choosing collections (`quotes`)

`quotes` in `~/.config/niruui/lock.toml` (NIRULOCK) and
`~/.config/niruui/screensaver.toml` (NIRUSCREENSAVER) selects collections by
name. `*` matches any run of characters and `?` any single character. One
pattern or an array of them:

```toml
quotes = ["*"]                       # the default: every collection
quotes = "havamal-sv"                # one collection
quotes = ["havamal-*", "stoic-en"]   # several, with a pattern
quotes = ["*-en", "mine"]            # English collections and your own
```

The quotes of all selected collections are put in one pool and shown in a
shuffled order: every quote once before any is repeated, and never the same
quote twice in a row, so a small collection is not shown more often than a
large one. The two programs can select different collections.

If nothing is selected, or the selected collections are missing, unreadable
or hold no quote, the built-in collections are shown instead (those the
patterns name, otherwise all of them), so the panel is never empty. To show
no quotes at all, set `visual = "cava"` or `visual = "none"`.

#### The old `quotes.txt`

NIRULOCK 0.19 and NIRUSCREENSAVER 0.11 used a single file,
`~/.config/niru/quotes.txt`. If you never edited it, it is ignored now (its
stanzas are the `havamal-sv` collection) and you can delete it. If you did
edit it, it still counts as the collection `quotes`; move your quotes into a
file in `quotes/` when convenient and delete the old file.

#### Sources

- `havamal-sv`: 22 stanzas of Hávamál ("Den höges sång") in Erik Brate's
  Swedish translation, *Eddan. De nordiska guda- och hjältesångerna* (1913),
  public domain (Brate died in 1924), verbatim from Project Runeberg,
  <https://runeberg.org/eddan/se-02.html>. These are the stanzas of the old
  default `quotes.txt`.
- `havamal-en`: the same 22 stanzas in Henry Adams Bellows' English
  translation, *The Poetic Edda* (1923), public domain (Bellows died in 1939),
  from Wikisource. Stanza numbers are Bellows'; four differ by one from
  Brate's.
- `stoic-en`: 32 passages on moderation, few words, friendship, fate,
  mortality and simple living: Marcus Aurelius and Epictetus' *Encheiridion*
  in George Long's translations (1862 and later; Long died in 1879), and
  Seneca's *Minor Dialogues* and *On Benefits* in Aubrey Stewart's
  translations (1887–1889; Stewart died in 1918), from Project Gutenberg.
- `stoic-sv`: the same 32 passages in Swedish. Three Seneca passages are
  verbatim from Johan Bergman's translation, *Om livets korthet* (2nd ed.
  1918), public domain (Bergman died in 1951), from Project Runeberg. The
  other 29 are **not** a published translation: they are Swedish renderings
  made for NIRUX from Long's and Stewart's English, and their attributions
  say "NIRUX-tolkning".

`…` marks an omission inside a passage. Every quote was checked word for
word against its source; the exact passages, URLs and editorial changes are
listed in [`quotes/SOURCES.md`](quotes/SOURCES.md).

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
