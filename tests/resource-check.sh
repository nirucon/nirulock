#!/bin/sh
# A locked CAVA layout must stop its 30 fps repaint during silence and resume
# on sound. Ported from NIRUSCREENSAVER's resource check: an isolated Xvfb,
# a fixture `cava` on PATH, and the lock's own CPU time from /proc.
#
# usage: sh tests/resource-check.sh NIRULOCK_BINARY [check|baseline]
# `baseline` prints the numbers without enforcing them (for older releases).
set -eu
BIN=${1:?path to nirulock}
MODE=${2:-check}
TEST=$(mktemp -d)
LOCK=; XVFB=
cleanup() {
  if [ -n "$LOCK" ]; then kill "$LOCK" 2>/dev/null || :; wait "$LOCK" 2>/dev/null || :; fi
  pkill -f "$TEST/bin/cava" 2>/dev/null || :
  if [ -n "$XVFB" ]; then kill "$XVFB" 2>/dev/null || :; wait "$XVFB" 2>/dev/null || :; fi
  rm -rf "$TEST"
}
trap cleanup EXIT INT TERM
fail() { printf 'FAIL %s\n' "$1"; [ "$MODE" = baseline ] || exit 1; }
mkdir -p "$TEST/bin" "$TEST/config" "$TEST/state"
head -c 48 /dev/zero > "$TEST/zero"
i=0; while [ "$i" -lt 48 ]; do printf '\200'; i=$((i + 1)); done > "$TEST/loud-a"
i=0; while [ "$i" -lt 48 ]; do printf '\040'; i=$((i + 1)); done > "$TEST/loud-b"
# Fixture CAVA: raw 8-bit frames at about 30 fps; silence unless $TEST/sound
# exists, and then alternating levels so the bars keep moving.
cat > "$TEST/bin/cava" <<HELPER
#!/bin/sh
n=0
while :; do
  if [ -e "$TEST/sound" ]; then
    n=\$((1 - n)); if [ \$n -eq 1 ]; then cat "$TEST/loud-a" || exit; else cat "$TEST/loud-b" || exit; fi
  else
    cat "$TEST/zero" || exit
  fi
  sleep 0.033
done
HELPER
chmod +x "$TEST/bin/cava"
export PATH="$TEST/bin:$PATH" DISPLAY=:96 XDG_CONFIG_HOME="$TEST/config" XDG_STATE_HOME="$TEST/state"
Xvfb :96 -screen 0 1024x768x24 -nolisten tcp > "$TEST/xvfb.log" 2>&1 & XVFB=$!
sleep 1
tck=$(getconf CLK_TCK)
ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
# CPU percent of one process over $2 seconds.
cpu() {
  a=$(ticks "$1"); sleep "$2"; b=$(ticks "$1")
  awk -v a="$a" -v b="$b" -v t="$tck" -v s="$2" 'BEGIN { printf "%.2f", (b - a) * 100 / t / s }'
}
# Distinct root images among five shots 200 ms apart.
frames() {
  for _ in 1 2 3 4 5; do xwd -root -silent | md5sum; sleep 0.2; done | sort -u | wc -l
}
touch "$TEST/sound"
"$BIN" > "$TEST/lock.log" 2>&1 & LOCK=$!
sleep 2
kill -0 "$LOCK" || { cat "$TEST/lock.log"; exit 1; }
live_cpu=$(cpu "$LOCK" 3)
live_frames=$(frames)
printf 'sound:   nirulock %s%% CPU, %s distinct frames of 5\n' "$live_cpu" "$live_frames"
[ "$live_frames" -gt 1 ] || fail "the spectrum did not animate with sound"

rm -f "$TEST/sound"
sleep 3
silent_cpu=$(cpu "$LOCK" 6)
silent_frames=$(frames)
printf 'silence: nirulock %s%% CPU, %s distinct frames of 5\n' "$silent_cpu" "$silent_frames"
[ "$silent_frames" -eq 1 ] || fail "the 30 fps repaint continued during silence"
awk -v c="$silent_cpu" 'BEGIN { exit !(c <= 0.3) }' || fail "silent CPU above 0.3%"
pgrep -f "$TEST/bin/cava" > /dev/null || fail "the CAVA child stopped while the display was on"

touch "$TEST/sound"
sleep 1
resumed_frames=$(frames)
printf 'resumed: %s distinct frames of 5 within 1 s of sound\n' "$resumed_frames"
[ "$resumed_frames" -gt 1 ] || fail "the spectrum did not resume on sound"
kill -0 "$LOCK" || fail "the lock exited"
printf 'PASS: silence stops the repaint (%s%% CPU), sound resumes it\n' "$silent_cpu"
