#!/bin/sh
# Dead keys, compose and keypad digits through real key events and real PAM.
#
# Run as a disposable test user whose password is the fixture below, with
# the `nirulock` PAM service installed. CI creates that user; on a test host
# remove the user afterwards. The script starts its own Xvfb with the Swedish
# layout, so it never touches a real session.
#
# usage: sh tests/compose-unlock.sh NIRULOCK_BINARY [DISPLAY]
#
# Fixture password: "Aé´^~¨12", typed as
#   A                     Shift+a
#   é                     dead_acute, e
#   ´                     dead_acute, dead_acute
#   ^                     dead_circumflex, space
#   ~                     dead_tilde, space
#   ¨                     dead_diaeresis, dead_diaeresis
#   12                    the keypad with NumLock on (KP_End, KP_Down)
set -u
binary=$1
display=${2:-:94}
dir=$(mktemp -d)
export HOME="$dir" XDG_STATE_HOME="$dir/state" XDG_CONFIG_HOME="$dir/config" DISPLAY="$display"
export LANG="${LANG:-C.UTF-8}"
lock=
Xvfb "$display" -screen 0 1024x768x24 -nolisten tcp -noreset >/dev/null 2>&1 &
server=$!
cleanup() {
  [ -z "$lock" ] || kill "$lock" 2>/dev/null
  kill "$server" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$dir"
}
trap cleanup EXIT
fail() { printf 'FAIL %s\n' "$1"; [ ! -f "$dir/lock.log" ] || cat "$dir/lock.log"; exit 1; }
# Wait up to $1 tenths of a second for the lock to exit.
exited_within() {
  n=0
  while [ "$n" -lt "$1" ]; do
    kill -0 "$lock" 2>/dev/null || return 0
    sleep 0.1
    n=$((n + 1))
  done
  return 1
}
sleep 1.5
setxkbmap se || fail "setxkbmap se"
# The fixture must be typed on the Swedish layout, not on remapped keycodes.
setxkbmap -query | grep -q '^layout: *se$' || fail "the Swedish layout is not active"
"$binary" > "$dir/lock.log" 2>&1 &
lock=$!
sleep 2.5
kill -0 "$lock" 2>/dev/null || fail "lock exited at start"

type_fixture() {
  xdotool key --delay 40 A dead_acute e dead_acute dead_acute \
    dead_circumflex space dead_tilde space dead_diaeresis dead_diaeresis \
    Num_Lock KP_End KP_Down Num_Lock
}

# The composed characters without the keypad digits are wrong.
xdotool key --delay 40 A dead_acute e dead_acute dead_acute Return
exited_within 60 && fail "a wrong password unlocked"
printf 'OK   wrong composed password keeps it locked\n'
# A dead key that is cancelled by Escape must not leak into the next try.
xdotool key --delay 40 dead_acute Escape
type_fixture
xdotool key Return
exited_within 80 || fail "the composed password with keypad digits did not unlock"
wait "$lock"
status=$?
lock=
[ "$status" -eq 0 ] || fail "lock exited with status $status"
printf 'OK   dead keys, compose and keypad digits unlock through PAM (%s)\n' \
  "$(sed -n 's/.*keyboard: //p' "$dir/state/nirulock/trace.log" | tail -1)"
