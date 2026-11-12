#!/bin/sh
# The typewriter click: every key that changes the password entry asks
# NIRUNOTIFY for `sound lock-typing`; keys that change nothing do not.
#
#     sh tests/typing-sound.sh target/release/nirulock [:DISPLAY]
#
# Runs its own Xvfb (default :182, or the next free one) with isolated HOME
# and XDG directories. A small Python server stands in for NIRUNOTIFY's
# control socket and records the requests, so nothing is played. Needs Xvfb,
# xdotool and python3.
set -eu

BIN=$(cd "$(dirname "${1:?path to nirulock}")" && pwd)/$(basename "$1")
DISPLAY_NUM=${2:-:182}
T=$(mktemp -d "${TMPDIR:-/tmp}/nirulock-typing.XXXXXX")
XVFB=""
LOCK=""
SERVER=""
cleanup() {
  [ -n "$LOCK" ] && kill "$LOCK" 2>/dev/null || :
  [ -n "$SERVER" ] && kill "$SERVER" 2>/dev/null || :
  [ -n "$XVFB" ] && kill "$XVFB" 2>/dev/null || :
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

n=${DISPLAY_NUM#:}
while [ -e "/tmp/.X11-unix/X$n" ] || [ -e "/tmp/.X$n-lock" ]; do n=$((n + 1)); done
export DISPLAY=":$n"
Xvfb "$DISPLAY" -screen 0 1024x768x24 -nolisten tcp -noreset >/dev/null 2>&1 &
XVFB=$!
i=0
until xdotool getmouselocation >/dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -lt 50 ] || { echo "FAIL: Xvfb $DISPLAY did not start"; exit 1; }
  sleep 0.1
done

export HOME="$T/home" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state"
export XDG_CACHE_HOME="$T/cache" XDG_DATA_HOME="$T/data" XDG_RUNTIME_DIR="$T/run"
export PULSE_SERVER="unix:$T/no-such-server" PIPEWIRE_REMOTE="$T/no-such-server"
unset NIRULOCK_REASON
mkdir -p "$HOME" "$XDG_CONFIG_HOME/niruui" "$XDG_STATE_HOME"
mkdir -m 700 "$XDG_RUNTIME_DIR"
echo 'visual = "quotes"' > "$XDG_CONFIG_HOME/niruui/lock.toml"

SOCKET="$XDG_RUNTIME_DIR/nirunotify-$(id -u).sock"
python3 - "$SOCKET" "$T/requests" <<'PY' &
import socket, sys
path, log = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(path)
s.listen(16)
while True:
    c, _ = s.accept()
    data = b""
    while not data.endswith(b"\n"):
        chunk = c.recv(64)
        if not chunk:
            break
        data += chunk
    c.sendall(b"ok\n")
    c.close()
    with open(log, "a") as f:
        f.write(data.decode().strip() + "\n")
PY
SERVER=$!
i=0
while [ ! -S "$SOCKET" ]; do
  i=$((i + 1)); [ "$i" -lt 50 ] || { echo "FAIL: fake NIRUNOTIFY did not start"; exit 1; }
  sleep 0.1
done

"$BIN" > "$T/lock.log" 2>&1 &
LOCK=$!
i=0
until xprop -root _NIRU_LOCK_READY 2>/dev/null | grep -q 'window id'; do
  i=$((i + 1))
  if [ "$i" -gt 100 ] || ! kill -0 "$LOCK" 2>/dev/null; then
    echo "FAIL: the lock did not become ready"; cat "$T/lock.log"; exit 1
  fi
  sleep 0.1
done
sleep 0.5
: > "$T/requests"

count() { c=$(grep -c '^sound lock-typing$' "$T/requests" 2>/dev/null) || :; echo "${c:-0}"; }
settle() { sleep 0.6; }
fails=0
check() { # LABEL WANT
  got=$(count)
  if [ "$got" -eq "$2" ]; then echo "OK   $1 ($got)"; else echo "FAIL $1: $got clicks, want $2"; fails=$((fails + 1)); fi
}

xdotool type --delay 90 'wxyzwq'
settle; check 'six typed characters click six times' 6
xdotool key --delay 120 BackSpace BackSpace
settle; check 'two erased characters click twice more' 8
xdotool key Escape
sleep 0.2
xdotool key --delay 150 BackSpace Shift_L Control_L Left
settle; check 'Esc, Backspace on an empty entry and modifiers stay silent' 8
xdotool type --delay 90 'ab'
xdotool key Return
settle; check 'Enter does not click' 10
if ! kill -0 "$LOCK" 2>/dev/null; then echo "FAIL: the lock exited"; fails=$((fails + 1)); fi
if grep -Ev '^sound (lock-typing|auth-fail|lock)$' "$T/requests" | grep -q .; then
  echo "FAIL: unexpected requests: $(grep -v '^sound lock-typing$' "$T/requests")"; fails=$((fails + 1))
fi
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=""

if [ "$fails" -ne 0 ]; then
  echo "FAIL: $fails typing sound checks failed"
  exit 1
fi
echo "PASS: typing clicks follow the password entry"
