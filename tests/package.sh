#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
command -v dpkg-deb >/dev/null 2>&1 || exit 0

binary=$script_dir/target/release/menvane
setup_binary=$script_dir/target/release/menvane-setup
[ -x "$binary" ] || { printf '%s\n' "release binary is required" >&2; exit 1; }
[ -x "$setup_binary" ] || { printf '%s\n' "setup binary is required" >&2; exit 1; }

test_root=$(mktemp -d)
cleanup() { rm -r -- "$test_root"; }
trap cleanup EXIT HUP INT TERM
DEB_OUTPUT_DIR=$test_root/dist "$script_dir/packaging/build-deb.sh" 0.1.0 x86_64-unknown-linux-gnu "$binary" "$setup_binary"
package=$test_root/dist/menvane_0.1.0_amd64.deb
dpkg-deb --info "$package" >/dev/null
contents=$(dpkg-deb --contents "$package")
printf '%s\n' "$contents" | grep -F '/usr/bin/menvane ' >/dev/null
printf '%s\n' "$contents" | grep -F '/usr/bin/menvane-setup ' >/dev/null
printf '%s\n' "$contents" | grep -F '/usr/lib/systemd/user/menvane.service ' >/dev/null
printf '%s\n' "$contents" | grep -F '/usr/share/applications/menvane-setup.desktop ' >/dev/null
