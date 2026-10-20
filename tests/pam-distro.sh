#!/bin/sh
# Destructive fixture provisioning: only run inside a disposable test container.
set -eu
[ "${NIRU_PAM_TEST_CONTAINER:-}" = 1 ] || exit 2
case "$1" in
  arch) pacman -Syu --needed --noconfirm pam pambase shadow util-linux curl ;;
  debian) apt-get update; apt-get install -y libpam0g libpam-modules passwd util-linux curl ;;
  void)
    for directory in /etc/xbps.d /usr/share/xbps.d; do
      for config in "$directory"/*.conf; do
        [ ! -f "$config" ] || sed -i 's#alpha.de.repo.voidlinux.org#repo-default.voidlinux.org#g' "$config"
      done
    done
    xbps-install -Suy xbps
    xbps-install -Suy
    xbps-install -Sy pam pam-base shadow util-linux curl
    ;;
  *) exit 2 ;;
esac
install -m 644 "/src/pam/$1/nirulock" /etc/pam.d/nirulock
useradd -m -s /bin/sh tester
printf 'tester:fixture-password\n' | chpasswd -c SHA512
su -s /bin/sh tester -c '/src/target/release/pamcheck tester fixture-password'
if su -s /bin/sh tester -c '/src/target/release/pamcheck tester wrong-password'; then
  printf 'FAIL: wrong password authenticated\n' >&2
  exit 1
fi

# The prebuilt standalone installer, using the selected release's pinned PAM,
# must work without touching the container's production PAM configuration.
fixture=$(mktemp -d)
mkdir -p "$fixture/pam.d"
test_binary="${NIRULOCK_TEST_BINARY:-/src/target/release/nirulock}"
cp "$test_binary" "$fixture/nirulock"
cp "/src/pam/$1/nirulock" "$fixture/pam-$1"
(cd "$fixture" && sha256sum nirulock "pam-$1" > SHA256SUMS)
(
  export HOME="$fixture/home" XDG_STATE_HOME="$fixture/state"
  export NIRULOCK_PAM_DIR="$fixture/pam.d" NIRULOCK_BASE_URL="file://$fixture"
  sh /src/install.sh --prefix "$fixture/prefix" > "$fixture/install.log" 2>&1
  [ "$(sha256sum "/src/pam/$1/nirulock" | cut -d' ' -f1)" = \
    "$(sha256sum "$fixture/pam.d/nirulock" | cut -d' ' -f1)" ]
  "$fixture/prefix/bin/nirulock" --version >/dev/null
  sh /src/install.sh --check --prefix "$fixture/prefix" > "$fixture/check.log" 2>&1
  printf 'user PAM edit\n' > "$fixture/pam.d/nirulock"
  sh /src/install.sh --uninstall --prefix "$fixture/prefix" > "$fixture/uninstall.log" 2>&1
  [ "$(cat "$fixture/pam.d/nirulock")" = 'user PAM edit' ]
)
rm -rf "$fixture"
printf 'PASS: real PAM accepts the correct password and rejects the wrong one; pinned standalone install works\n'
