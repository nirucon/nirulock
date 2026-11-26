#!/bin/sh
# [pause_music] plays only for the lock reasons in pause_music.on (by
# default only a manual lock; it is on by default), and Esc still stops it.
#
#     sh tests/pause-music-reason.sh target/release/nirulock [:DISPLAY]
#
# Runs its own Xvfb (default :180; the next free display is used when that
# one is taken) with isolated HOME and XDG directories, no sound server and a
# fake `sidplayfp` on PATH that only records that it was started. The SID
# tune is in the default folder, where NIRUX installs its music. A last check
# plays a folder of .ogg files only, with a fake `pw-play`. It plays no audio
# and never touches another X session. Needs Xvfb, xdotool, python3.
set -eu

BIN=$(cd "$(dirname "${1:?path to nirulock}")" && pwd)/$(basename "$1")
DISPLAY_NUM=${2:-:180}
HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/nirulock-reason.XXXXXX")
XVFB=""
LOCK=""
cleanup() {
  [ -n "$LOCK" ] && kill "$LOCK" 2>/dev/null || :
  if [ -s "$T/players" ]; then
    while read -r pid; do kill "$pid" 2>/dev/null || :; done < "$T/players"
  fi
  [ -n "$XVFB" ] && kill "$XVFB" 2>/dev/null || :
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

# A free display: never reuse a server this script did not start.
n=${DISPLAY_NUM#:}
while [ -e "/tmp/.X11-unix/X$n" ] || [ -e "/tmp/.X$n-lock" ]; do n=$((n + 1)); done
export DISPLAY=":$n"
Xvfb "$DISPLAY" -screen 0 1024x768x24 -nolisten tcp -noreset >/dev/null 2>&1 &
XVFB=$!
i=0
until xdpyinfo >/dev/null 2>&1 || xdotool getmouselocation >/dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -lt 50 ] || { echo "FAIL: Xvfb $DISPLAY did not start"; exit 1; }
  sleep 0.1
done

export HOME="$T/home" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state"
export XDG_CACHE_HOME="$T/cache" XDG_DATA_HOME="$T/data" XDG_RUNTIME_DIR="$T/run"
# No sound server at all: nothing can be played, and nothing counts as
# other audio for the start gate.
export PULSE_SERVER="unix:$T/no-such-server" PIPEWIRE_REMOTE="$T/no-such-server"
export NIRULOCK_TRACE=1
unset NIRULOCK_REASON
MUSIC="$HOME/.local/share/nirux/music"
mkdir -p "$MUSIC" "$XDG_CONFIG_HOME/niruui" "$XDG_STATE_HOME" "$T/bin" "$T/ogg"
mkdir -m 700 "$XDG_RUNTIME_DIR"
python3 "$HERE/fixtures/make-test-sid.py" "$MUSIC/1-test.sid" >/dev/null

# The fake player: record the start and the pid, then wait to be stopped.
cat > "$T/bin/sidplayfp" <<EOF
#!/bin/sh
echo "\$\$" >> "$T/players"
echo started >> "$T/started"
exec sleep 120
EOF
chmod 755 "$T/bin/sidplayfp"
export PATH="$T/bin:$PATH"

config() {
  {
    echo 'visual = "quotes"'
    echo '[pause_music]'
    echo 'enabled = true'
    [ -z "$1" ] || echo "on = $1"
  } > "$XDG_CONFIG_HOME/niruui/lock.toml"
}

player_alive() {
  [ -s "$T/players" ] || return 1
  while read -r pid; do kill -0 "$pid" 2>/dev/null && return 0; done < "$T/players"
  return 1
}

# lock EXPECT ENV-REASON ARGS...: EXPECT is "music" or "silent".
fails=0
lock() {
  expect=$1; env_reason=$2; shift 2
  rm -f "$T/started" "$T/players" "$XDG_STATE_HOME/nirulock/trace.log"
  if [ -n "$env_reason" ]; then
    NIRULOCK_REASON=$env_reason "$BIN" "$@" > "$T/lock.log" 2>&1 &
  else
    "$BIN" "$@" > "$T/lock.log" 2>&1 &
  fi
  LOCK=$!
  label="env=${env_reason:--} args=${*:--}"
  # The gate decides 2 s after the lock is shown, 5 s at the latest.
  i=0
  while [ "$i" -lt 80 ] && [ ! -s "$T/started" ]; do sleep 0.1; i=$((i + 1)); done
  if ! kill -0 "$LOCK" 2>/dev/null; then
    echo "FAIL $label: the lock exited"; cat "$T/lock.log"; fails=$((fails + 1)); LOCK=""; return
  fi
  if [ "$expect" = music ]; then
    if [ -s "$T/started" ]; then
      echo "OK   $label: pause music started"
    else
      echo "FAIL $label: no pause music"; fails=$((fails + 1))
    fi
  else
    if [ -e "$T/started" ]; then
      echo "FAIL $label: pause music started"; fails=$((fails + 1))
    else
      echo "OK   $label: no pause music"
    fi
  fi
  kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=""
  i=0
  while player_alive && [ "$i" -lt 30 ]; do sleep 0.1; i=$((i + 1)); done
  if player_alive; then echo "FAIL $label: the player outlived the lock"; fails=$((fails + 1)); fi
}

config ""
lock silent ""                         # an older caller: no reason
lock silent "" --lock                  # the same, explicit
lock music  "" --reason manual
lock music  manual
lock music  "" --lock --reason=manual
lock silent "" --reason idle
lock silent "" --reason suspend
lock silent "" --reason script
lock silent "" --reason session
lock silent manual --reason script     # the argument wins
lock silent "" --reason party          # unknown: locks, no music
lock silent "" --reason                # no value: locks, no music

config '["idle", "suspend"]'
lock music  "" --reason idle
lock music  suspend
lock silent "" --reason manual

config '[]'
lock silent "" --reason manual

# Since 0.30.0 the music is on by default: no [pause_music] table at all
# plays for a manual lock and stays silent for the others; enabled = false
# turns it off.
echo 'visual = "quotes"' > "$XDG_CONFIG_HOME/niruui/lock.toml"
lock music  "" --reason manual
lock silent "" --reason idle
lock silent "" --reason suspend
printf 'visual = "quotes"\n[pause_music]\nenabled = false\n' > "$XDG_CONFIG_HOME/niruui/lock.toml"
lock silent "" --reason manual

# Esc still stops the music for the rest of the lock and keeps it locked.
config ""
rm -f "$T/started" "$T/players" "$XDG_STATE_HOME/nirulock/trace.log"
"$BIN" --reason manual > "$T/lock.log" 2>&1 &
LOCK=$!
i=0
while [ "$i" -lt 80 ] && [ ! -s "$T/started" ]; do sleep 0.1; i=$((i + 1)); done
if player_alive; then
  xdotool key Escape
  i=0
  while player_alive && [ "$i" -lt 30 ]; do sleep 0.1; i=$((i + 1)); done
  if player_alive; then
    echo "FAIL Esc did not stop the pause music"; fails=$((fails + 1))
  elif ! kill -0 "$LOCK" 2>/dev/null; then
    echo "FAIL Esc unlocked"; fails=$((fails + 1))
  else
    echo "OK   Esc stops the pause music and the screen stays locked"
  fi
  sleep 3
  if [ "$(wc -l < "$T/started")" -ne 1 ]; then
    echo "FAIL the music came back after Esc"; fails=$((fails + 1))
  fi
else
  echo "FAIL Esc case: no pause music to stop"; fails=$((fails + 1))
fi
grep -q 'reason: manual' "$XDG_STATE_HOME/nirulock/trace.log" ||
  { echo "FAIL the trace does not name the reason"; fails=$((fails + 1)); }
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=""

# Audio files only (NIRUX's own music is Ogg Vorbis): a fake pw-play plays
# them, and its stream is given the player's pid.
: > "$T/ogg/nirux-1-test-tune.ogg"
cat > "$T/bin/pw-play" <<EOF
#!/bin/sh
echo "\$\$" >> "$T/players"
printf '%s|' "\$\$" "\$@" > "$T/pw-args"
echo started >> "$T/started"
exec sleep 120
EOF
chmod 755 "$T/bin/pw-play"
{
  echo 'visual = "quotes"'
  echo '[pause_music]'
  echo 'enabled = true'
  echo "folder = \"$T/ogg\""
} > "$XDG_CONFIG_HOME/niruui/lock.toml"
rm -f "$T/started" "$T/players" "$T/pw-args"
"$BIN" --reason manual > "$T/lock.log" 2>&1 &
LOCK=$!
i=0
while [ "$i" -lt 80 ] && [ ! -s "$T/pw-args" ]; do sleep 0.1; i=$((i + 1)); done
if [ -s "$T/pw-args" ]; then
  pid=$(cut -d'|' -f1 "$T/pw-args")
  if [ "$(cat "$T/pw-args")" = "$pid|-P|{ application.process.id = $pid }|$T/ogg/nirux-1-test-tune.ogg|" ]; then
    echo "OK   an .ogg folder plays with pw-play, and the stream carries the player's pid"
  else
    echo "FAIL pw-play arguments: $(cat "$T/pw-args")"; fails=$((fails + 1))
  fi
else
  echo "FAIL an .ogg folder did not start pw-play"; cat "$T/lock.log"; fails=$((fails + 1))
fi
kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=""
i=0
while player_alive && [ "$i" -lt 30 ]; do sleep 0.1; i=$((i + 1)); done
if player_alive; then echo "FAIL pw-play outlived the lock"; fails=$((fails + 1)); fi

# The satie layout plays NIRUX's Gymnopedie on its own when it is installed
# in the NIRUX music folder, whatever else is there; without it the usual
# pause music plays.
satie() {
  rm -f "$T/started" "$T/players" "$T/pw-args"
  "$BIN" --reason manual > "$T/lock.log" 2>&1 &
  LOCK=$!
  i=0
  while [ "$i" -lt 80 ] && [ ! -s "$T/pw-args" ]; do sleep 0.1; i=$((i + 1)); done
  played=$(cut -d'|' -f4 "$T/pw-args" 2>/dev/null || :)
  kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=""
  i=0
  while player_alive && [ "$i" -lt 30 ]; do sleep 0.1; i=$((i + 1)); done
}
rm -f "$MUSIC"/*
: > "$MUSIC/nirux-1-test-tune.ogg"
: > "$MUSIC/nirux-2-other.ogg"
: > "$MUSIC/nirux-12-gymnopedie.ogg"
printf 'layout = "satie"\n' > "$XDG_CONFIG_HOME/niruui/lock.toml"
for round in 1 2 3; do
  satie
  if [ "$played" = "$MUSIC/nirux-12-gymnopedie.ogg" ]; then
    echo "OK   satie plays the Gymnopedie (round $round)"
  else
    echo "FAIL satie played '$played'"; fails=$((fails + 1))
  fi
done
rm "$MUSIC/nirux-12-gymnopedie.ogg"
satie
case $played in
  "$MUSIC"/nirux-[12]-*.ogg) echo "OK   satie without the Gymnopedie plays the folder" ;;
  *) echo "FAIL satie without the Gymnopedie played '$played'"; fails=$((fails + 1)) ;;
esac

# The bbs layout plays the modem handshake (a WAV file NIRULOCK writes in
# the runtime directory) once before the pause music, under the same rules:
# a manual lock plays both, an idle lock neither, and the file is removed.
cat > "$T/bin/pw-play" <<EOF
#!/bin/sh
echo "\$\$" >> "$T/players"
for f; do :; done
echo "\$f" >> "$T/pw-files"
case "\$f" in
  *.wav) cp "\$f" "$T/intro.wav"; exec sleep 1 ;;
esac
exec sleep 120
EOF
chmod 755 "$T/bin/pw-play"
for old in "${MUSIC:?}"/*; do [ -e "$old" ] && rm -f "$old"; done
: > "$MUSIC/nirux-1-test-tune.ogg"
printf 'layout = "bbs"\n' > "$XDG_CONFIG_HOME/niruui/lock.toml"
bbs() {
  rm -f "$T/players" "$T/pw-files" "$T/intro.wav"
  "$BIN" --reason "$1" > "$T/lock.log" 2>&1 &
  LOCK=$!
  i=0
  while [ "$i" -lt 90 ] && [ "$(cat "$T/pw-files" 2>/dev/null | wc -l)" -lt 2 ]; do
    sleep 0.1; i=$((i + 1))
  done
  kill "$LOCK"; wait "$LOCK" 2>/dev/null || :; LOCK=""
  i=0
  while player_alive && [ "$i" -lt 30 ]; do sleep 0.1; i=$((i + 1)); done
}
bbs manual
first=$(sed -n 1p "$T/pw-files" 2>/dev/null || :)
second=$(sed -n 2p "$T/pw-files" 2>/dev/null || :)
case $first in
  "$XDG_RUNTIME_DIR"/nirulock-intro-*.wav)
    if [ "$(head -c 4 "$T/intro.wav")" = RIFF ] && [ ! -e "$first" ]; then
      echo "OK   bbs plays the modem handshake first, and removes its file"
    else
      echo "FAIL the handshake file was not a WAV or was left behind"; fails=$((fails + 1))
    fi ;;
  *) echo "FAIL bbs did not play the handshake first: '$first'"; fails=$((fails + 1)) ;;
esac
if [ "$second" = "$MUSIC/nirux-1-test-tune.ogg" ]; then
  echo "OK   bbs plays the pause music after the handshake"
else
  echo "FAIL bbs played '$second' after the handshake"; fails=$((fails + 1))
fi
bbs idle
if [ -s "$T/pw-files" ]; then
  echo "FAIL bbs played for an idle lock: $(cat "$T/pw-files")"; fails=$((fails + 1))
else
  echo "OK   bbs stays silent for an idle lock"
fi

if [ "$fails" -ne 0 ]; then
  echo "FAIL: $fails pause music reason checks failed"
  exit 1
fi
echo "PASS: pause music only for the configured lock reasons"
