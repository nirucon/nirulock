#!/bin/sh
# Destructive fixture provisioning: only run inside a disposable test container.
set -eu
[ "${NIRU_PAM_TEST_CONTAINER:-}" = 1 ] || exit 2
case "$1" in
  arch) pacman -Syu --needed --noconfirm pam pambase shadow util-linux ;;
  debian) apt-get update; apt-get install -y libpam0g libpam-modules passwd util-linux ;;
  void)
    for directory in /etc/xbps.d /usr/share/xbps.d; do
      for config in "$directory"/*.conf; do
        [ ! -f "$config" ] || sed -i 's#alpha.de.repo.voidlinux.org#repo-default.voidlinux.org#g' "$config"
      done
    done
    xbps-install -Suy xbps
    xbps-install -Suy
    xbps-install -Sy pam pam-base shadow util-linux
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
printf 'PASS: real PAM accepts the correct password and rejects the wrong one\n'
