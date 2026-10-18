# Nirulock

A lock screen for X11 in the Niru Noir style, with the date, the time, the
hostname and how long the machine has been away.

Part of a small set of programs that share one look. Each works on its own:

- [niruwm](https://github.com/nirucon/niruwm), the window manager
- [nirubar](https://github.com/nirucon/nirubar), the status bar
- [niruscreensaver](https://github.com/nirucon/niruscreensaver), the idle screen

**A personal hobby project.** I build this for my own machines. It is public so
others can read it and try it, but there is no support promise. Early software,
used at your own risk.

## What it is

A lock screen that covers the screen and asks for your password. The password
is checked by PAM, the same system mechanism as `slock` and `sudo`, so account
rules already on the machine are respected.

It looks like [niruscreensaver](https://github.com/nirucon/niruscreensaver),
with one extra word: `locked`.

## How it works

The window is override redirect and covers the whole screen, so the window
manager cannot move or cover it. The keyboard and pointer are grabbed, so a
click cannot focus another window. Only a successful PAM authentication
releases the grabs and exits.

If PAM is missing, cannot start, or asks an unexpected question, the screen
stays locked. Failing closed is deliberate.

Unlike the other NIRU programs, Nirulock links against the system libpam. PAM
loads modules and reads `/etc/pam.d`; a static binary cannot do that correctly.

## Install

Releases contain a binary for x86_64 glibc systems. No Rust is needed.
`libpam` must be present, which it is on any system with a login program.

```sh
git clone https://github.com/nirucon/nirulock
cd nirulock
./install.sh
```

To build from source (Rust 1.75 or newer, plus the PAM development headers):

```sh
./install.sh --build
```

| Distro | Build dependency |
| --- | --- |
| Arch | `pam` |
| Debian | `libpam0g-dev` |
| Void | `linux-pam-devel` (in `nonfree`/`multilib` only; releases are prebuilt) |

### PAM service

The installer also writes `/etc/pam.d/nirulock` (sudo, once). It reuses the
system authentication stack only: `common-auth` on Debian and Arch,
`system-auth` on Void. Without it, PAM would fall back to `login`, which is the
*login* stack and carries rules meant for starting a session.

## Settings

`~/.config/niruui/lock.toml`. Everything is optional.

```toml
font_path = "/path/to/monospace.ttf"  # default: fontconfig lookup
font_size = 22.0

[palette]
background = "0f0f10"
foreground = "e5e5e5"
muted = "a8a8a8"
border = "2a2a2d"
focus = "5a5a60"
```

Use it from NIRUWM with `Super+Escape`, which picks `nirulock` when installed.
In a NIRUPI session, `slock` is used instead.

## Status

Version 0.5.0. Built and tested on Arch, Debian and Void.

Missing: fingerprint support, a configurable PAM service name, and a preview in
the settings file.

Known limits: Caps Lock and Shift are handled; dead keys (compose) are not.

## Build needs

Rust 1.75 or newer, a C linker, `pkg-config`, the PAM headers and a monospace
font.

## License

MIT. Copyright 2026 Nicklas Rudolfsson.
