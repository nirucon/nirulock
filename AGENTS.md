# Agent assignment: NIRULOCK 0.18.0, a one-shot input, security and energy update

## 0. Quick start

1. Read this file, `README.md`, `src/main.rs`, `src/pam.rs`, `src/cava.rs` and
   `src/core.rs`. The audit baseline is `7d91d09` (0.17.3, pinned by NIRUX 0.12.199).
   Revalidate each item against current HEAD and skip items that are already fixed,
   noting that you did.
2. Implement **K1–K8, then U1–U3 in order**. Commit per item. Keep `cargo fmt --check`,
   `clippy -D warnings`, tests, the ignored x11 tests and `tests/installer.sh` green.
3. Pass the gates in §4, release in §5 and report in §6.

This is an **execution** assignment. Do not stop after a plan, a branch or a draft PR.
Make sensible decisions, document them, and continue without routine questions. Only ask
if you hit a genuine external blocker or a real risk of locking the owner out.

## 1. Security invariants (override everything else)

- **Only `PAM_SUCCESS` unlocks.** Every new path fails closed. No bypass key, no debug
  unlock, no timeout unlock, no environment override that skips PAM.
- Keep the verified grabs, `_NIRU_LOCK_READY` semantics, the cooperative overlay/WM handoff
  from F01 (0.17.2), coverage repair on RandR changes, the opaque window and the
  compositor opacity hint.
- Never log password characters, keycodes, keysyms, modifiers or lengths. That also applies
  to new code: the compose state, the keymap cache and PAM messages.
- Do not make the lock *harder* to unlock with the correct password. Every input change
  must accept every password that the owner can type in a normal X session with the same
  layout.
- Keep `rust-version = 1.75` and the dynamic glibc + libpam linkage (see `src/pam.rs`).
  Locked builds must work on rustc 1.75 and on Debian 13's 1.85.

## 2. Bugs (verified 2026-11-04 on niruxwork, in a separate Xvfb with `setxkbmap se`)

### K1: Dead keys are dropped, so some passwords cannot be typed (high)
`keystring()` (`src/main.rs` ~1006) maps only 0x20–0x7e and 0xa0–0xff. On the Swedish
layout, `´ ` ^ ~ ¨` are dead keys (`dead_acute` 0xfe51, `dead_grave`, `dead_circumflex`,
`dead_tilde`, `dead_diaeresis`). Live test: pressing `dead_acute` added **no**
character. A password that contains one of those characters, or a composed letter such as
`é`, can be set in a terminal and used in NIRULOGIN (winit/xkbcommon composes it), but it
**cannot unlock NIRULOCK**.
**Fix:** compose exactly like a normal X session:
- Preferred: use libxkbcommon (+ the x11 and compose modules) through a small binding.
  libxkbcommon is installed with Xorg on Debian, Arch and Void. Use the locale's compose
  table, the real XKB state (levels, Level5, NumLock and group switches) and
  `MappingNotify`/XKB state events. Check that the binding builds on rustc 1.75 with
  `--locked`.
- Fallback, if that is not viable: a built-in table that gives the same results as the
  default Latin-1 compose for these cases:
  - dead key + space gives the literal character,
  - dead key + the same dead key gives the literal character,
  - dead key + a vowel or `n`/`y` gives the Latin-1 composed letter, in both cases,
  - an invalid sequence follows libX11 compose behavior. Document the behavior.

**Done when:** unit tests cover the compose cases, and an Xvfb test with `setxkbmap se`
types `é´^~¨` with real key events and, through a test PAM service (§4), unlocks.

### K2: Keypad digits are lost
NumLock (Mod2) is ignored, so keypad keys resolve to `KP_End`/`KP_Down`/… (0xff9c…) and
are dropped. A numeric password typed on the keypad never unlocks. **Fix:** this is solved
by the xkbcommon state in K1. With the fallback, apply NumLock to keypad keysyms and map
`KP_0`–`KP_9`, `KP_Decimal`, `KP_Add`/`KP_Subtract`/`KP_Multiply`/`KP_Divide` and
`KP_Enter`.

### K3: The CAVA layout burns power while the machine is locked (high, battery)
`cava` is the **default** layout. Locked in a separate Xvfb with no audio playing,
NIRULOCK used about **2.6 % CPU** and the `cava` child about **1.5 %** of a core,
repainting at 30 fps. Nothing checks whether the display is off, so a laptop that stays
locked (lunch, overnight) keeps rendering and sampling audio into a dark panel.
**Fix:**
- Pause the spectrum whenever DPMS is not `On`. Enable the x11rb `dpms` feature, poll
  `DPMSInfo` at most every 2 s while locked, or better, react to screen-saver/DPMS
  events if available.
- While paused, **stop the `cava` child** (kill + reap) and draw one static frame. Restart
  it within 1 s of DPMS `On`.
- While the display is on but CAVA reports silence (all bars ≈ 0 for 2 s), stop the 30 fps
  repaint and draw one flat frame. Resume on the first non-zero sample. The loop then wakes
  for the minute clock only.

**Done when:** a locked machine with the display forced off (`xset dpms force off` on real
hardware) has 0 `cava` processes and ≤ 0.1 % CPU, and the bars return within 1 s after
wake. With the display on and no audio, CPU ≤ 0.3 %.

### K4: The account falls back to `$USER` and then to `root`
`pam::current_user()` trusts `$USER`, runs `id -un`, and finally returns `"root"`, so in a
broken environment the root password would unlock a user's screen. **Fix:** use
`getpwuid_r(getuid())`. If that fails, refuse to lock with a clear error, *before* any
grab. Never fall back to another account.

### K5: Password copies are not wiped
Several copies of the password live on: `self.input`, the `String` moved to the worker,
`PASSWORD`'s clone inside `conversation()` and the `CString`. **Fix:** use the `zeroize`
crate (check its MSRV against 1.75) for every owned copy. Use `Zeroizing<String>` for the
input and the worker, and zeroize the conversation clone and the `CString` buffer after
`strdup`. PAM frees its own copy; add a comment saying so.

### K6: PAM messages are hidden
`conversation()` ignores `PAM_ERROR_MSG` and `PAM_TEXT_INFO`. When `pam_faillock` has
locked the account after N failures (Arch's default stack), the user sees only
"wrong password", even with the correct password, for the whole lockout period.
**Fix:**
- Keep the last error/info text: sanitized, single line, at most 120 chars, never logged.
- Show it under the prompt after a failed attempt, for example "Account locked due to 3
  failed logins".
- Keep answering only `ECHO_OFF` and aborting on everything else.

**Done when:** verified on algiz with the system's own faillock (inspect
`/etc/security/faillock.conf` first, restore it if you change it, and reset the counter
with `faillock --reset` afterwards).

### K7: One X round trip per key press
`keystring()` calls `get_keyboard_mapping` for every key. **Fix:** cache the keymap (this
comes for free with K1's xkbcommon state) and refresh it on `MappingNotify`.

### K8: Code duplicated with NIRUSCREENSAVER
`core.rs`, `cava.rs`, `palette.rs` and most of `config.rs` are copies, and they have
already diverged: the opacity fix exists only in NIRULOCK. Do not build a shared crate in
this assignment. Keep the K3 CAVA pause logic self-contained in `cava.rs` so that the
NIRUSCREENSAVER assignment can copy it verbatim, and say so in the final report.

## 3. UX

### U1: A clearer Caps Lock and layout indicator
Keep the Caps Lock indicator. Also show the active XKB layout short name (`SE`/`US`) next
to the prompt when more than one group is configured, because a password typed in the
wrong group is the most common "wrong password".

### U2: A clearer failure state
After a wrong password, show the message in the warning color for 2 s (today it is
600 ms). Show the number of failed attempts since locking ("2 failed attempts"). Keep the
input enabled the whole time. The longer message must not delay retrying.

### U3: Option to hide the password length
Add a config option `prompt_echo = "count" | "pulse"`, default `"count"`, which is today's
behavior. `"pulse"` shows a fixed-width indicator that animates per key without revealing
the length. Document it in `config.example.toml` and the README.

Out of scope: fingerprint UI, multi-user switching, Wayland, and changing the visual
layouts.

## 4. Gates

Automated:
- `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo test --locked`, the ignored x11 tests under Xvfb, `sh tests/installer.sh`, a
  release build.
- An MSRV job in CI (`dtolnay/rust-toolchain@1.75`, `cargo build --locked`).
- A **test-only PAM path:** run the x11 tests against a dedicated PAM service file
  installed in the CI container (for example `pam_succeed_if`/`pam_unix` against a test
  user with a known password containing `é´^~¨` and keypad digits). Unit tests: the
  compose table, keypad mapping, `current_user` through getpwuid, PAM message capture and
  sanitizing, the CAVA pause state machine (DPMS on/off, silence), and zeroizing (test
  that the buffers are cleared where observable).
- A resource check under Xvfb, ported from niruscreensaver's `tests/resource-check.sh`:
  a locked instance with a silent fixture `cava` (a script printing zeros) must stop
  repainting at 30 fps after the silence window.

Native, using hosts the owner has authorized for full testing (othala/Debian, algiz/Arch,
void/Void), one host at a time, with a recovery SSH session open first:
- Lock and unlock with the owner's real password at least 5 times per host. The owner
  supplies credentials privately; never log or commit them.
- A test-user or test-PAM run proves the K1/K2 characters.
- K3 with `xset dpms force off` and on, checking `pgrep cava` and CPU.
- K6 with faillock on algiz.
- The lock-takeover smoke test with Quick Settings open (F01) still passes.
- On **niruxwork** (the owner's work laptop) use Xvfb only. Do not lock the owner's real
  session there unless the owner is present and agrees in the execution conversation.

## 5. Release

Bump to **0.18.0**. Update the README: input/compose support, the CAVA power behavior and
PAM messages. Tag `v0.18.0` through the normal workflow and verify the assets and
checksums. Then pin it in `nirucon/nirux` `manifest`:
- If `nirux/tools/suite-pin` exists, use it.
- Otherwise bump the manifest and suite version the usual way. Change only the nirulock
  line, and rebase if other pins land at the same time.

Install through `nirux update` on the test hosts and verify that the running binary comes
from the new generation after re-login. Do not move published tags or assets.

## 6. Final report

For each item K1–K8 and U1–U3: the change, the tests and the evidence (CPU and process
counts before and after, the composed characters tested, the faillock message seen).
Include release URLs, the installed and running version per host, and remaining limits,
for example input methods beyond compose. Mark anything unfinished as **PARTIAL** and
give the exact remaining steps.
