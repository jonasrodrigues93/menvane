#!/bin/sh
set -eu

usage() {
    printf '%s\n' "Usage: ./packaging/build-deb.sh VERSION TARGET MENvANE_BINARY SETUP_BINARY"
}

[ "$#" -eq 4 ] || { usage >&2; exit 2; }
version=$1
target=$2
binary=$3
setup_binary=$4

case "$version" in
    ''|*[!0-9A-Za-z.+:~-]*)
        printf '%s\n' "invalid Debian version" >&2
        exit 2
        ;;
esac
case "$target" in
    x86_64-unknown-linux-gnu) architecture=amd64 ;;
    aarch64-unknown-linux-gnu) architecture=arm64 ;;
    *) printf 'unsupported Debian target: %s\n' "$target" >&2; exit 2 ;;
esac
[ -f "$binary" ] || { printf 'binary not found: %s\n' "$binary" >&2; exit 1; }
[ -f "$setup_binary" ] || { printf 'setup binary not found: %s\n' "$setup_binary" >&2; exit 1; }

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output=${DEB_OUTPUT_DIR:-$root/dist}
package_root=$(mktemp -d "${TMPDIR:-/tmp}/menvane-deb.XXXXXX")
cleanup() { rm -r -- "$package_root"; }
trap cleanup EXIT HUP INT TERM

mkdir -p "$package_root/DEBIAN" "$package_root/usr/bin" "$package_root/usr/lib/systemd/user" \
    "$package_root/usr/share/applications" "$package_root/usr/share/doc/menvane"
install -m 755 "$binary" "$package_root/usr/bin/menvane"
install -m 755 "$setup_binary" "$package_root/usr/bin/menvane-setup"
install -m 644 "$root/packaging/debian/usr/lib/systemd/user/menvane.service" \
    "$package_root/usr/lib/systemd/user/menvane.service"
install -m 644 "$root/packaging/debian/usr/share/applications/menvane-setup.desktop" \
    "$package_root/usr/share/applications/menvane-setup.desktop"
install -m 644 "$root/packaging/debian/usr/share/doc/menvane/copyright" \
    "$package_root/usr/share/doc/menvane/copyright"
cat > "$package_root/DEBIAN/control" <<EOF
Package: menvane
Version: $version
Section: utils
Priority: optional
Architecture: $architecture
Maintainer: Jonas Rodrigues <jonasrodrigues93@users.noreply.github.com>
Depends: libxkbcommon0, libwayland-client0 | libx11-6
Description: local persistent memory for coding agents
 Menvane captures local session evidence and provides operational continuity
 between coding-agent sessions.
EOF
mkdir -p "$output"
dpkg-deb --build --root-owner-group "$package_root" "$output/menvane_${version}_${architecture}.deb" >/dev/null
