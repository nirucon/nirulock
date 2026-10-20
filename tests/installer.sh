#!/bin/sh
# Offline ownership regression; never writes PAM or the real home.
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
TEST=$(mktemp -d)
trap 'rm -rf "$TEST"' EXIT
export HOME=$TEST/home XDG_STATE_HOME=$TEST/state
prefix=$TEST/prefix
target=$prefix/share/nirux/current/bin/nirulock
mkdir -p "$prefix/bin" "$(dirname "$target")" "$XDG_STATE_HOME/nirulock"
printf 'suite binary\n' > "$target"
ln -s "$target" "$prefix/bin/nirulock"
for mode in --build --uninstall; do
  if sh "$SOURCE/install.sh" "$mode" --prefix "$prefix" > "$TEST/log" 2>&1; then
    printf 'FAIL: accepted NIRUX ownership (%s)\n' "$mode" >&2; exit 1
  fi
  grep -q 'managed by NIRUX' "$TEST/log"
  [ -L "$prefix/bin/nirulock" ]
  [ "$(cat "$target")" = 'suite binary' ]
done
rm "$prefix/bin/nirulock"
printf 'standalone binary\n' > "$prefix/bin/nirulock"
hash=$(sha256sum "$prefix/bin/nirulock" | cut -d ' ' -f1)
printf '%s\t%s\n' "$prefix/bin/nirulock" "$hash" > "$XDG_STATE_HOME/nirulock/manifest.tsv"
printf 'user replacement\n' > "$prefix/bin/nirulock"
sh "$SOURCE/install.sh" --uninstall --prefix "$prefix"
[ "$(cat "$prefix/bin/nirulock")" = 'user replacement' ]
printf '%s\t%s\n' "$prefix/bin/nirulock" "$(sha256sum "$prefix/bin/nirulock" | cut -d ' ' -f1)" > "$XDG_STATE_HOME/nirulock/manifest.tsv"
sh "$SOURCE/install.sh" --uninstall --prefix "$prefix"
[ ! -e "$prefix/bin/nirulock" ]
printf 'PASS: NIRUX ownership and modified-file preservation\n'
