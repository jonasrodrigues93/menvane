#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$script_dir/target/release/menvane
test_root=$(mktemp -d)
fake_bin=$test_root/bin
mkdir -p "$fake_bin"

cleanup() {
    rm -r -- "$test_root"
}
trap cleanup EXIT HUP INT TERM

[ -x "$binary" ] || {
    printf '%s\n' "release binary is required before testing the installer" >&2
    exit 1
}

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) printf '%s\n' "${MENVANE_TEST_PLATFORM:?MENVANE_TEST_PLATFORM is not set}" ;;
    -m) printf '%s\n' "${MENVANE_TEST_ARCHITECTURE:?MENVANE_TEST_ARCHITECTURE is not set}" ;;
    *) exit 1 ;;
esac
EOF
chmod 755 "$fake_bin/uname"

cat > "$fake_bin/systemctl" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$fake_bin/systemctl"

cat > "$fake_bin/launchctl" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$fake_bin/launchctl"

cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            output=$2
            shift 2
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done
[ -n "$output" ]
if [ "${MENVANE_TEST_DOWNLOAD_FAILURE:-0}" = 1 ]; then
    exit 1
fi
case "$url" in
    *SHA256SUMS)
        if [ "${MENVANE_TEST_BAD_CHECKSUM:-0}" = 1 ]; then
            printf '%s  %s\n' "0000000000000000000000000000000000000000000000000000000000000000" "$(basename "$MENVANE_TEST_FIXTURE_ARCHIVE")" > "$output"
        else
            cp "$MENVANE_TEST_FIXTURE_CHECKSUMS" "$output"
        fi
        ;;
    *) cp "$MENVANE_TEST_FIXTURE_ARCHIVE" "$output" ;;
esac
EOF
chmod 755 "$fake_bin/curl"

source_bin=$test_root/source-bin
mkdir -p "$source_bin"
cat > "$source_bin/cargo" <<'EOF'
#!/bin/sh
set -eu
: > "${MENVANE_TEST_CARGO_MARKER:?MENVANE_TEST_CARGO_MARKER is not set}"
EOF
chmod 755 "$source_bin/cargo"

run_install() {
    test_home=$1
    platform=$2
    architecture=$3
    mkdir -p "$test_home"
    HOME="$test_home" \
    XDG_CONFIG_HOME="$test_home/.config" \
    MENVANE_TEST_PLATFORM="$platform" \
    MENVANE_TEST_ARCHITECTURE="$architecture" \
    PATH="$fake_bin:$PATH" \
        "$script_dir/install.sh" --binary "$binary"
}

fixture_dir=$test_root/release
mkdir -p "$fixture_dir"
fixture_archive=$fixture_dir/menvane-x86_64-unknown-linux-gnu.tar.gz
tar -czf "$fixture_archive" -C "$(dirname "$binary")" "$(basename "$binary")"
if command -v sha256sum >/dev/null 2>&1; then
    (CDPATH= cd -- "$fixture_dir" && sha256sum "$(basename "$fixture_archive")" > SHA256SUMS)
else
    (CDPATH= cd -- "$fixture_dir" && shasum -a 256 "$(basename "$fixture_archive")" > SHA256SUMS)
fi

linux_home=$test_root/linux-home
run_install "$linux_home" Linux x86_64
[ -x "$linux_home/.local/bin/menvane" ]
[ -f "$linux_home/.config/systemd/user/menvane.service" ]

mac_home=$test_root/mac-home
run_install "$mac_home" Darwin x86_64
[ -x "$mac_home/.local/bin/menvane" ]
[ -f "$mac_home/Library/LaunchAgents/com.jonasrodrigues93.menvane.plist" ]
grep -F '<string>serve</string>' "$mac_home/Library/LaunchAgents/com.jonasrodrigues93.menvane.plist" >/dev/null

published_home=$test_root/published-home
mkdir -p "$published_home"
HOME="$published_home" \
XDG_CONFIG_HOME="$published_home/.config" \
MENVANE_TEST_PLATFORM=Linux \
MENVANE_TEST_ARCHITECTURE=x86_64 \
MENVANE_TEST_FIXTURE_ARCHIVE="$fixture_archive" \
MENVANE_TEST_FIXTURE_CHECKSUMS="$fixture_dir/SHA256SUMS" \
PATH="$fake_bin:/usr/bin:/bin" \
    sh -s -- --version 0.1.0 < "$script_dir/install.sh"
[ -x "$published_home/.local/bin/menvane" ]

bad_home=$test_root/bad-home
mkdir -p "$bad_home"
if HOME="$bad_home" \
XDG_CONFIG_HOME="$bad_home/.config" \
MENVANE_TEST_PLATFORM=Linux \
MENVANE_TEST_ARCHITECTURE=x86_64 \
MENVANE_TEST_FIXTURE_ARCHIVE="$fixture_archive" \
MENVANE_TEST_FIXTURE_CHECKSUMS="$fixture_dir/SHA256SUMS" \
MENVANE_TEST_BAD_CHECKSUM=1 \
PATH="$fake_bin:/usr/bin:/bin" \
    "$script_dir/install.sh" --version 0.1.0; then
    printf '%s\n' "installer accepted an invalid checksum" >&2
    exit 1
fi
[ ! -e "$bad_home/.local/bin/menvane" ]

source_home=$test_root/source-home
source_marker=$test_root/cargo-was-used
mkdir -p "$source_home"
if HOME="$source_home" \
    XDG_CONFIG_HOME="$source_home/.config" \
    MENVANE_TEST_PLATFORM=Linux \
    MENVANE_TEST_ARCHITECTURE=x86_64 \
    MENVANE_TEST_DOWNLOAD_FAILURE=1 \
    MENVANE_TEST_CARGO_MARKER="$source_marker" \
    PATH="$source_bin:$fake_bin:/usr/bin:/bin" \
        "$script_dir/install.sh"; then
    printf '%s\n' "installer unexpectedly succeeded without a published binary" >&2
    exit 1
fi
[ ! -e "$source_home/.local/bin/menvane" ]
[ ! -e "$source_marker" ]
