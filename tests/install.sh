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
printf '%s\n' "${MENVANE_TEST_PLATFORM:?MENVANE_TEST_PLATFORM is not set}"
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

run_install() {
    test_home=$1
    platform=$2
    mkdir -p "$test_home"
    HOME="$test_home" \
    XDG_CONFIG_HOME="$test_home/.config" \
    MENVANE_TEST_PLATFORM="$platform" \
    PATH="$fake_bin:$PATH" \
        "$script_dir/install.sh" --binary "$binary"
}

linux_home=$test_root/linux-home
run_install "$linux_home" Linux
[ -x "$linux_home/.local/bin/menvane" ]
[ -f "$linux_home/.config/systemd/user/menvane.service" ]

mac_home=$test_root/mac-home
run_install "$mac_home" Darwin
[ -x "$mac_home/.local/bin/menvane" ]
[ -f "$mac_home/Library/LaunchAgents/com.jonasrodrigues93.menvane.plist" ]
grep -F '<string>serve</string>' "$mac_home/Library/LaunchAgents/com.jonasrodrigues93.menvane.plist" >/dev/null
