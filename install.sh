#!/bin/sh
# Niruscreensaver installer.
#
#   ./install.sh          install the released static binary
#   ./install.sh --build  build from this checkout (needs Rust)
#   ./install.sh --check  verify what is installed
#   ./install.sh --uninstall
#
# Installs to ~/.local/bin unless --prefix is given. Never touches other
# programs.
set -eu

VERSION="0.1.0"
REPO="${NIRULOCK_REPO:-nirucon/nirulock}"
BIN="nirulock"
PREFIX="${NIRULOCK_PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
CONFDIR="${XDG_CONFIG_HOME:-$HOME/.config}/niruui"
STATEDIR="${XDG_STATE_HOME:-$HOME/.local/state}/nirulock"
MANIFEST="$STATEDIR/manifest.tsv"

die() { printf 'nirulock install: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() {
  cat <<'EOF'
Usage: ./install.sh [options]
  (no options)  install the released binary
  --build       build from this checkout and install the result
  --check       verify the installation; make no changes
  --uninstall   remove the installed binary
  --prefix DIR  install under DIR (default ~/.local)
EOF
}

record() {
  mkdir -p "$STATEDIR"
  entry_path="$1"
  entry_hash="$2"
  manifest_tmp="$STATEDIR/.manifest.$$"
  if [ -f "$MANIFEST" ]; then
    awk -F '\t' -v p="$entry_path" '$1 != p' "$MANIFEST" > "$manifest_tmp" || true
  fi
  printf '%s\t%s\n' "$entry_path" "$entry_hash" >> "$manifest_tmp"
  mv -f "$manifest_tmp" "$MANIFEST"
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

put() {
  src="$1"; dest="$2"; mode="${3:-0755}"
  mkdir -p "$(dirname "$dest")"
  install -m "$mode" "$src" "$dest.new"
  mv -f "$dest.new" "$dest"
  record "$dest" "$(sha256 "$dest")"
}

fetch() {
  url="$1"; out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$out" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$out" "$url"
  else
    die "curl or wget is required"
  fi
}

install_release() {
  tag="${NIRULOCK_VERSION:-$VERSION}"
  base="${NIRULOCK_BASE_URL:-https://github.com/$REPO/releases/download/v$tag}"
  dl="$(mktemp -d)"
  trap 'rm -rf "$dl"' EXIT HUP INT TERM
  say "Downloading $BIN $tag ..."
  fetch "$base/$BIN" "$dl/$BIN" || die "cannot download $base/$BIN"
  fetch "$base/SHA256SUMS" "$dl/SHA256SUMS" || die "cannot download checksums"
  expected="$(awk -v n="$BIN" '$2 == n {print $1}' "$dl/SHA256SUMS")"
  [ -n "$expected" ] || die "no checksum for $BIN"
  actual="$(sha256 "$dl/$BIN")"
  [ "$expected" = "$actual" ] || die "checksum mismatch for $BIN"
  chmod 0755 "$dl/$BIN"
  put "$dl/$BIN" "$BINDIR/$BIN"
  install_pam_service
  say "Installed $BIN $("$BINDIR/$BIN" --version | awk '{print $2}') in $BINDIR"
  say "Settings: $CONFDIR/lock.toml (created when you first edit it)"
  if ! ldconfig -p 2>/dev/null | grep -q libpam; then
    say "Warning: libpam was not found on this system. Nirulock needs it to check passwords."
  fi
}

# PAM headers differ per distribution; name the package so the error is useful.
pam_hint() {
  case "$(uname -s)" in *) : ;; esac
  if [ -f /etc/arch-release ] || command -v pacman >/dev/null 2>&1; then
    echo "install pam (pacman -S pam)"
  elif [ -f /etc/debian_version ]; then
    echo "install libpam0g-dev (apt install libpam0g-dev)"
  elif [ -f /etc/void-release ] || command -v xbps-install >/dev/null 2>&1; then
    echo "install linux-pam-devel (xbps-install -S linux-pam-devel)"
  else
    echo "install the PAM development headers for your distribution"
  fi
}

build_local() {
  command -v cargo >/dev/null 2>&1 || die "cargo is required for --build"
  if [ ! -f /usr/include/security/pam_appl.h ] && ! pkg-config --exists pam 2>/dev/null; then
    die "PAM headers not found: $(pam_hint)"
  fi
  say "Building from source ..."
  ( cd "$(dirname "$0")" && cargo build --release --locked )
  put "target/release/$BIN" "$BINDIR/$BIN"
  install_pam_service
  say "Installed $BIN $("$BINDIR/$BIN" --version | awk '{print $2}') in $BINDIR"
}

# Install the PAM service file. Without it, PAM falls back to `login`, which on
# Void requires a shell in /etc/shells and refuses while /etc/nologin exists:
# a locked screen that cannot be unlocked. This is the one system file Nirulock
# needs, so it is installed with sudo and reported clearly.
distro() {
  if [ -f /etc/void-release ] || command -v xbps-install >/dev/null 2>&1; then
    echo void
  elif [ -f /etc/debian_version ]; then
    echo debian
  elif [ -f /etc/arch-release ] || command -v pacman >/dev/null 2>&1; then
    echo arch
  elif [ -r /etc/os-release ] && grep -qi '^ID_LIKE=.*\(debian\|arch\|void\)' /etc/os-release 2>/dev/null; then
    # Derivatives answer to the family they build on.
    if grep -qi void /etc/os-release; then echo void
    elif grep -qi debian /etc/os-release; then echo debian
    else echo arch; fi
  else
    echo unknown
  fi
}

install_pam_service() {
  d="$(distro)"
  src="$(dirname "$0")/pam/$d/nirulock"
  if [ ! -f "$src" ]; then
    say "Note: no PAM service file for this distribution ($d)."
    say "Nirulock will fall back to the 'login' service."
    return 0
  fi
  dest=/etc/pam.d/nirulock
  if [ -f "$dest" ] && cmp -s "$src" "$dest"; then
    say "PAM service already installed: $dest"
    return 0
  fi
  if [ -w /etc/pam.d ] 2>/dev/null; then
    install -m 0644 "$src" "$dest"
  elif command -v sudo >/dev/null 2>&1; then
    say "Installing the PAM service to $dest (sudo):"
    sudo install -m 0644 "$src" "$dest" || die "could not install $dest"
  else
    say "Warning: cannot write $dest and sudo is missing."
    say "Install it yourself: copy pam/$d/nirulock to $dest"
    return 0
  fi
  say "Installed the PAM service: $dest"
}

check() {
  ok=0
  if [ -f /etc/pam.d/nirulock ]; then
    say "OK   /etc/pam.d/nirulock"
  else
    say "WARN no /etc/pam.d/nirulock (falls back to the login service)"
  fi
  if [ -x "$BINDIR/$BIN" ]; then say "OK   $BINDIR/$BIN"; else say "FAIL missing $BINDIR/$BIN"; ok=1; fi
  "$BINDIR/$BIN" --version || ok=1
  return "$ok"
}

uninstall() {
  if [ -f /etc/pam.d/nirulock ]; then
    if [ -w /etc/pam.d ]; then rm -f /etc/pam.d/nirulock
    elif command -v sudo >/dev/null 2>&1; then sudo rm -f /etc/pam.d/nirulock; fi
    say "Removed /etc/pam.d/nirulock"
  fi
  [ -f "$MANIFEST" ] || { say "Nothing to remove."; return 0; }
  while IFS="$(printf '\t')" read -r path _hash; do
    [ -n "$path" ] || continue
    case "$path" in
      */nirulock) [ -e "$path" ] && rm -f "$path" && say "Removed $path" ;;
      *) say "Skipping unexpected entry: $path" ;;
    esac
  done < "$MANIFEST"
  rm -f "$MANIFEST"
  say "Settings kept in $CONFDIR."
}

MODE=install
while [ $# -gt 0 ]; do
  case "$1" in
    --build) MODE=build ;;
    --check) MODE=check ;;
    --uninstall) MODE=uninstall ;;
    --prefix) shift; PREFIX="${1:?--prefix needs a directory}"; BINDIR="$PREFIX/bin" ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  shift
done

cd "$(dirname "$0")"
case "$MODE" in
  install) install_release ;;
  build) build_local ;;
  check) check; exit $? ;;
  uninstall) uninstall ;;
esac
say "Done."
