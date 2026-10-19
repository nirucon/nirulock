# Nirulock

Native X11/PAM lock screen with a clock, hostname and away time in the Niru Noir
style. Part of the [NIRUX desktop](https://github.com/nirucon/nirux); independently
runnable on **Void glibc, Arch and Debian**. Personal software for the author's
machines, public for others to try.

## Install

Use [NIRUX](https://github.com/nirucon/nirux#install) for the complete desktop.
Standalone x86_64/glibc releases require no Rust:

```sh
git clone https://github.com/nirucon/nirulock
cd nirulock
./install.sh
```

The installer requests sudo for `/etc/pam.d/nirulock`, which includes the system
authentication stack. Source builds: `./install.sh --build` (Rust 1.75+, C linker,
PAM development headers and a monospace font).

## Use and settings

```sh
nirulock                 # or Super+Escape in NIRUWM
nirulock --config
nirulock --version
```

Settings: `$XDG_CONFIG_HOME/niruui/lock.toml` (default `~/.config/niruui/lock.toml`).
Optional `font_path`, `font_size` and `[palette]` overrides are documented in
[config.example.toml](config.example.toml). Diagnostics:
`$XDG_STATE_HOME/nirulock/trace.log` (default `~/.local/state/nirulock/trace.log`).
Password characters, keycodes and modifiers are never logged.

## Current behaviour

**0.16.6:** verified keyboard/pointer grabs before displaying the prompt; continuous
input isolation after a wrong password; authentication on one worker while X11
remains responsive; restacking and opaque coverage follow display changes. The
prompt stays on its selected physical output and falls back if that output leaves.

Only successful PAM authentication unlocks. Escape clears input. Legacy timeout
settings are ignored; update niruidle and log out/in to remove old timeout wrappers.
Basic Swedish/US, Shift and AltGr input is supported; full compose/input-method
support remains planned. X11 does not isolate the lock from hostile same-server clients.

[Roadmap](https://github.com/nirucon/nirux/blob/main/ROADMAP.md) ·
[Audit](https://github.com/nirucon/nirux/blob/main/AUDIT.md) ·
[Tests](tests/x11.rs)

MIT. Copyright 2026 Nicklas Rudolfsson.
