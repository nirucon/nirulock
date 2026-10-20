#!/bin/sh
# Offline ownership regression; never writes PAM or the real home.
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
TEST=$(mktemp -d)
trap 'rm -rf "$TEST"' EXIT
# Keep the real Rust toolchain available while the installer test isolates HOME.
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export HOME=$TEST/home XDG_STATE_HOME=$TEST/state
PAM_DIR=$TEST/pam.d
mkdir -p "$PAM_DIR"
export NIRULOCK_PAM_DIR=$PAM_DIR
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

# The released installer uses the selected release's binary and PAM service,
# not files from this checkout, and never touches the production PAM directory.
if [ -f /etc/void-release ] || command -v xbps-install >/dev/null 2>&1; then
  pam_distro=void
elif [ -f /etc/debian_version ]; then
  pam_distro=debian
elif [ -f /etc/arch-release ] || command -v pacman >/dev/null 2>&1; then
  pam_distro=arch
else
  printf 'FAIL: unsupported test distribution\n' >&2; exit 1
fi
[ -x "$SOURCE/target/release/nirulock" ] || (cd "$SOURCE" && cargo build --release --locked)
release=$TEST/release
mkdir -p "$release"
cp "$SOURCE/target/release/nirulock" "$release/nirulock"
cp "$SOURCE/pam/$pam_distro/nirulock" "$release/pam-$pam_distro"
printf '# release fixture marker\n' >> "$release/pam-$pam_distro"
(cd "$release" && sha256sum nirulock "pam-$pam_distro" > SHA256SUMS)
NIRULOCK_BASE_URL="file://$release" sh "$SOURCE/install.sh" --prefix "$TEST/prebuilt"
cmp "$release/pam-$pam_distro" "$PAM_DIR/nirulock" || { printf 'FAIL: release PAM was not installed\n' >&2; exit 1; }
"$TEST/prebuilt/bin/nirulock" --version >/dev/null

# A modified PAM file and a modified binary are both preserved on uninstall.
printf 'user PAM edit\n' > "$PAM_DIR/nirulock"
printf 'user binary replacement\n' > "$TEST/prebuilt/bin/nirulock"
sh "$SOURCE/install.sh" --uninstall --prefix "$TEST/prebuilt"
[ "$(cat "$PAM_DIR/nirulock")" = 'user PAM edit' ] || { printf 'FAIL: modified PAM was removed\n' >&2; exit 1; }
[ "$(cat "$TEST/prebuilt/bin/nirulock")" = 'user binary replacement' ] || { printf 'FAIL: modified binary was removed\n' >&2; exit 1; }

# Corrupt or incomplete release assets fail before replacing either file.
NIRULOCK_BASE_URL="file://$release" sh "$SOURCE/install.sh" --prefix "$TEST/prebuilt2"
binary_hash=$(sha256sum "$TEST/prebuilt2/bin/nirulock" | cut -d' ' -f1)
pam_hash=$(sha256sum "$PAM_DIR/nirulock" | cut -d' ' -f1)
cp "$release/nirulock" "$release/nirulock.good"
printf 'corrupt\n' >> "$release/nirulock"
if NIRULOCK_BASE_URL="file://$release" sh "$SOURCE/install.sh" --prefix "$TEST/prebuilt2" > "$TEST/corrupt.log" 2>&1; then
  printf 'FAIL: corrupt binary install succeeded\n' >&2; exit 1
fi
grep -q 'checksum mismatch' "$TEST/corrupt.log"
[ "$(sha256sum "$TEST/prebuilt2/bin/nirulock" | cut -d' ' -f1)" = "$binary_hash" ] || { printf 'FAIL: corrupt binary changed installed binary\n' >&2; exit 1; }
[ "$(sha256sum "$PAM_DIR/nirulock" | cut -d' ' -f1)" = "$pam_hash" ] || { printf 'FAIL: corrupt binary changed PAM\n' >&2; exit 1; }
mv "$release/nirulock.good" "$release/nirulock"
(cd "$release" && sha256sum nirulock "pam-$pam_distro" > SHA256SUMS)
rm "$release/pam-$pam_distro"
if NIRULOCK_BASE_URL="file://$release" sh "$SOURCE/install.sh" --prefix "$TEST/prebuilt2" > "$TEST/nopam.log" 2>&1; then
  printf 'FAIL: install without PAM asset succeeded\n' >&2; exit 1
fi
[ "$(sha256sum "$TEST/prebuilt2/bin/nirulock" | cut -d' ' -f1)" = "$binary_hash" ] || { printf 'FAIL: missing PAM changed installed binary\n' >&2; exit 1; }

# The source installer uses the checkout PAM service in the isolated directory.
if command -v cargo >/dev/null 2>&1 && { [ -f /usr/include/security/pam_appl.h ] || pkg-config --exists pam 2>/dev/null; }; then
  sh "$SOURCE/install.sh" --build --prefix "$TEST/source"
  cmp "$SOURCE/pam/$pam_distro/nirulock" "$PAM_DIR/nirulock" || { printf 'FAIL: source install PAM mismatch\n' >&2; exit 1; }
  "$TEST/source/bin/nirulock" --version >/dev/null
fi
printf 'PASS: NIRUX ownership, pinned release PAM, failed assets and modified-file preservation\n'
