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
# 48 bars of 16 bits each.
head -c 96 /dev/zero > "$TEST/zero"
# Fixture CAVA: raw 16-bit frames at about 30 fps; silence unless $TEST/sound
# exists, and then random levels so the bars keep moving. (Two alternating
# levels would alias with the 200 ms screenshots: every frame is painted as it
# arrives, so shots six frames apart showed the same image.)
cat > "$TEST/bin/cava" <<HELPER
#!/bin/sh
while :; do
  if [ -e "$TEST/sound" ]; then
    head -c 96 /dev/urandom || exit
  else
    cat "$TEST/zero" || exit
  fi
  sleep 0.033
done
HELPER
chmod +x "$TEST/bin/cava"
# A free display (never one this script did not start) and a private HOME
# and XDG tree: nothing of the user's is read, and no pause music is found.
n=96
while [ -e "/tmp/.X11-unix/X$n" ] || [ -e "/tmp/.X$n-lock" ]; do n=$((n + 1)); done
export PATH="$TEST/bin:$PATH" DISPLAY=":$n" HOME="$TEST/home"
export XDG_CONFIG_HOME="$TEST/config" XDG_STATE_HOME="$TEST/state" XDG_DATA_HOME="$TEST/data"
export XDG_CACHE_HOME="$TEST/cache" XDG_RUNTIME_DIR="$TEST/run"
mkdir -p "$HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
mkdir -m 700 "$XDG_RUNTIME_DIR"
Xvfb "$DISPLAY" -screen 0 1024x768x24 -nolisten tcp -noreset > "$TEST/xvfb.log" 2>&1 & XVFB=$!
sleep 1
tck=$(getconf CLK_TCK)
ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
# CPU percent of one process over $2 seconds.
cpu() {
  a=$(ticks "$1"); sleep "$2"; b=$(ticks "$1")
  awk -v a="$a" -v b="$b" -v t="$tck" -v s="$2" 'BEGIN { printf "%.2f", (b - a) * 100 / t / s }'
}
# Distinct root images among five shots 200 ms apart. Only the pixels are
# compared: some xwd builds leave uninitialized bytes in the header.
pixels() {
  python3 -I -c 'import hashlib, sys
d = sys.stdin.buffer.read()
header = int.from_bytes(d[0:4], "big")
colours = int.from_bytes(d[76:80], "big")
print(hashlib.md5(d[header + 12 * colours:]).hexdigest())'
}
# Without xwd, ffmpeg's x11grab takes the shot (raw pixels, no header).
shot() {
  if command -v xwd > /dev/null; then
    xwd -root -silent | pixels
  else
    ffmpeg -loglevel error -f x11grab -video_size 1024x768 -i "$DISPLAY" \
      -frames:v 1 -f rawvideo - | md5sum | cut -d ' ' -f1
  fi
}
frames() {
  for _ in 1 2 3 4 5; do shot; sleep 0.2; done | sort -u | wc -l
}
# The spectrum alone, as before 0.19: CAVA runs whenever the display is on.
mkdir -p "$TEST/config/niruui"
printf 'layout = "cava"\nvisual = "cava"\n' > "$TEST/config/niruui/lock.toml"
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

# The cava layout with visual = "auto" (the default) without a sound server:
# quotes, never CAVA, even while the fixture would play, and a static screen
# between changes.
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=
printf 'layout = "cava"\n' > "$TEST/config/niruui/lock.toml"
touch "$TEST/sound"
PULSE_SERVER=unix:$TEST/no-server "$BIN" > "$TEST/lock-auto.log" 2>&1 & LOCK=$!
sleep 3
kill -0 "$LOCK" || { cat "$TEST/lock-auto.log"; exit 1; }
quotes=$TEST/config/niru/quotes
for name in havamal-en havamal-sv stoic-en stoic-sv; do
  [ -f "$quotes/$name.txt" ] || fail "the collection $name was not written"
  [ "$(stat -c %a "$quotes/$name.txt")" = 644 ] || fail "$name.txt is not 0644"
done
grep -q 'runeberg.org/eddan' "$quotes/havamal-sv.txt" || fail "havamal-sv lacks its source"
pgrep -f "$TEST/bin/cava" > /dev/null && fail "CAVA started without music"
auto_cpu=$(cpu "$LOCK" 6)
auto_frames=$(frames)
printf 'quotes:  nirulock %s%% CPU, %s distinct frames of 5, no CAVA\n' "$auto_cpu" "$auto_frames"
[ "$auto_frames" -eq 1 ] || fail "the quote view repainted between changes"
awk -v c="$auto_cpu" 'BEGIN { exit !(c <= 0.3) }' || fail "quote view CPU above 0.3%"
# An existing directory is never written into: an edited collection stays
# edited and a deleted one stays deleted.
printf 'My own quote\n' > "$quotes/havamal-en.txt"
rm "$quotes/stoic-sv.txt"
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=
PULSE_SERVER=unix:$TEST/no-server "$BIN" > "$TEST/lock-auto.log" 2>&1 & LOCK=$!
sleep 2
[ "$(cat "$quotes/havamal-en.txt")" = "My own quote" ] || fail "a collection was overwritten"
[ ! -e "$quotes/stoic-sv.txt" ] || fail "a deleted collection was recreated"
printf 'PASS: without music the panel shows quotes and CAVA never starts\n'

# The default layout, ascii-cava, without a sound server: no CAVA and no
# quote collections, the banner typed and then only the cursor blinking
# (at most two distinct frames), at idle CPU.
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=
rm -f "$TEST/config/niruui/lock.toml"
PULSE_SERVER=unix:$TEST/no-server "$BIN" > "$TEST/lock-ascii.log" 2>&1 & LOCK=$!
sleep 9
kill -0 "$LOCK" || { cat "$TEST/lock-ascii.log"; exit 1; }
pgrep -f "$TEST/bin/cava" > /dev/null && fail "CAVA started in ascii-cava without music"
ascii_cpu=$(cpu "$LOCK" 6)
ascii_frames=$(frames)
printf 'ascii:   nirulock %s%% CPU, %s distinct frames of 5 (the cursor blinks), no CAVA\n' "$ascii_cpu" "$ascii_frames"
[ "$ascii_frames" -le 2 ] || fail "ascii-cava repainted more than the cursor"
awk -v c="$ascii_cpu" 'BEGIN { exit !(c <= 0.5) }' || fail "ascii-cava CPU above 0.5%"
printf 'PASS: ascii-cava stays still but for the cursor, without CAVA\n'

# The demo layout without a sound server: it animates (raster bars, logo,
# scroller) at about 25 frames a second, frugally, and with the display off
# it stops drawing altogether.
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=
printf 'layout = "demo"\n' > "$TEST/config/niruui/lock.toml"
PULSE_SERVER=unix:$TEST/no-server "$BIN" > "$TEST/lock-demo.log" 2>&1 & LOCK=$!
sleep 3
kill -0 "$LOCK" || { cat "$TEST/lock-demo.log"; exit 1; }
pgrep -f "$TEST/bin/cava" > /dev/null && fail "CAVA started in demo without music"
demo_cpu=$(cpu "$LOCK" 6)
demo_frames=$(frames)
printf 'demo:    nirulock %s%% CPU, %s distinct frames of 5\n' "$demo_cpu" "$demo_frames"
[ "$demo_frames" -ge 4 ] || fail "the demo did not animate"
awk -v c="$demo_cpu" 'BEGIN { exit !(c <= 5.0) }' || fail "demo CPU above 5%"
# Only where the server has DPMS (Xvfb usually has not).
if xset q 2>/dev/null | grep -q 'DPMS is' && xset dpms force off 2>/dev/null; then
  sleep 3
  off_cpu=$(cpu "$LOCK" 5)
  off_frames=$(frames)
  printf 'demo off: nirulock %s%% CPU, %s distinct frames of 5 with the display off\n' "$off_cpu" "$off_frames"
  [ "$off_frames" -eq 1 ] || fail "the demo drew with the display off"
  awk -v c="$off_cpu" 'BEGIN { exit !(c <= 0.3) }' || fail "demo CPU above 0.3% with the display off"
  xset dpms force on 2>/dev/null || :
  sleep 1.5
  on_frames=$(frames)
  [ "$on_frames" -ge 4 ] || fail "the demo did not resume with the display on"
  printf 'PASS: the demo rests with the display off and resumes\n'
else
  printf 'demo off: skipped, this X server has no DPMS\n'
fi
kill -0 "$LOCK" || fail "the lock exited"
printf 'PASS: the demo animates frugally\n'

# The attract layout without a sound server: the game plays itself at about
# 25 frames a second, frugally, and with the display off it stops drawing.
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=
printf 'layout = "attract"\n' > "$TEST/config/niruui/lock.toml"
PULSE_SERVER=unix:$TEST/no-server "$BIN" > "$TEST/lock-attract.log" 2>&1 & LOCK=$!
sleep 3
kill -0 "$LOCK" || { cat "$TEST/lock-attract.log"; exit 1; }
attract_cpu=$(cpu "$LOCK" 6)
attract_frames=$(frames)
printf 'attract: nirulock %s%% CPU, %s distinct frames of 5\n' "$attract_cpu" "$attract_frames"
# The title screen moves slowly at times: three of five is enough.
[ "$attract_frames" -ge 3 ] || fail "the attract mode did not animate"
awk -v c="$attract_cpu" 'BEGIN { exit !(c <= 5.0) }' || fail "attract CPU above 5%"
if xset q 2>/dev/null | grep -q 'DPMS is' && xset dpms force off 2>/dev/null; then
  sleep 3
  off_cpu=$(cpu "$LOCK" 5)
  off_frames=$(frames)
  printf 'attract off: nirulock %s%% CPU, %s distinct frames of 5 with the display off\n' "$off_cpu" "$off_frames"
  [ "$off_frames" -eq 1 ] || fail "the attract mode drew with the display off"
  awk -v c="$off_cpu" 'BEGIN { exit !(c <= 0.3) }' || fail "attract CPU above 0.3% with the display off"
  xset dpms force on 2>/dev/null || :
  sleep 1.5
  on_frames=$(frames)
  [ "$on_frames" -ge 4 ] || fail "the attract mode did not resume with the display on"
  printf 'PASS: the attract mode rests with the display off and resumes\n'
else
  printf 'attract off: skipped, this X server has no DPMS\n'
fi
kill -0 "$LOCK" || fail "the lock exited"
printf 'PASS: the attract mode plays frugally\n'
