# NIRULOCK

Native X11/PAM lock screen for the NIRU desktop, with clock, hostname and away
time in the active theme.

Part of the [NIRUX suite](https://github.com/nirucon/nirux): [NIRUWM](https://github.com/nirucon/niruwm) · [NIRUBAR](https://github.com/nirucon/nirubar) · [NIRUSCREENSAVER](https://github.com/nirucon/niruscreensaver) · [NIRULOCK](https://github.com/nirucon/nirulock) · [NIRULAUNCHER](https://github.com/nirucon/nirulauncher) · [NIRUNOTIFY](https://github.com/nirucon/nirunotify) · [NIRUSHOT](https://github.com/nirucon/nirushot).

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

Super+Escape locks in NIRUWM. The lock shows a Caps Lock indicator when active;
AltGr and Latin-1 case conversion are supported for password input.

Settings live in `~/.config/niruui/lock.toml` (XDG respected). Optional
`font_path`, `font_size` and `[palette]` overrides are shown in
[config.example.toml](config.example.toml). Diagnostics are written to
`~/.local/state/nirulock/trace.log`; password characters, keycodes and modifiers
are never logged.

## Behaviour

Only successful PAM authentication unlocks. NIRULOCK publishes
`_NIRU_LOCK_READY` after verified keyboard/pointer grabs and mapping, and clears
it on unlock; `niructl suspend` requires that readiness. Content follows the
selected suite theme on an opaque black background. The prompt stays on its
selected output and falls back if that output disappears. Authentication runs on
one worker while X11 remains responsive.

Basic Swedish/US, Shift and AltGr input is supported. Full compose/input-method
support is not implemented. X11 cannot isolate the lock from hostile clients on
the same display server.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
sh tests/installer.sh
```

MIT. Copyright 2026 Nicklas Rudolfsson.
