#!/bin/sh
set -eu

usage() {
    printf '%s\n' "Usage: ./install.sh [--binary PATH]"
}

binary=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            binary=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -z "$binary" ]; then
    command -v cargo >/dev/null 2>&1 || {
        printf '%s\n' "cargo is required to build Menvane" >&2
        exit 1
    }
    cargo build --release --locked --manifest-path "$script_dir/Cargo.toml"
    binary=$script_dir/target/release/menvane
fi

[ -f "$binary" ] || {
    printf 'Menvane binary not found: %s\n' "$binary" >&2
    exit 1
}

home=${HOME:?HOME is not set}
install_dir=$home/.local/bin
installed_binary=$install_dir/menvane
install -d "$install_dir"

platform=$(uname -s)
if [ "$platform" = Linux ]; then
    command -v systemctl >/dev/null 2>&1 || {
        printf '%s\n' "systemctl is required for automatic startup on Linux" >&2
        exit 1
    }
    config_home=${XDG_CONFIG_HOME:-$home/.config}
    unit_dir=$config_home/systemd/user
    unit=$unit_dir/menvane.service
    install -d "$unit_dir"
    escaped_binary=$(printf '%s' "$installed_binary" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g')
    temporary_unit=$unit.tmp.$$
    trap 'rm -f "$temporary_unit"' EXIT HUP INT TERM
    printf '%s\n' \
        '[Unit]' \
        'Description=Menvane local memory daemon and UI' \
        '' \
        '[Service]' \
        'Type=simple' \
        "ExecStart=\"$escaped_binary\" serve" \
        'Restart=on-failure' \
        'RestartSec=2s' \
        'TimeoutStopSec=5s' \
        '' \
        '[Install]' \
        'WantedBy=default.target' > "$temporary_unit"
    chmod 644 "$temporary_unit"
    mv "$temporary_unit" "$unit"
    trap - EXIT HUP INT TERM

    systemctl --user daemon-reload
    systemctl --user enable menvane.service
    systemctl --user stop menvane.service
    install -m 755 "$binary" "$installed_binary"
    systemctl --user restart --no-block menvane.service
    printf 'Installed %s and enabled menvane.service\n' "$installed_binary"
elif [ "$platform" = Darwin ]; then
    command -v launchctl >/dev/null 2>&1 || {
        printf '%s\n' "launchctl is required for automatic startup on macOS" >&2
        exit 1
    }
    install -m 755 "$binary" "$installed_binary"
    launch_agent_dir=$home/Library/LaunchAgents
    launch_agent=$launch_agent_dir/com.jonasrodrigues93.menvane.plist
    log_dir=$home/Library/Logs
    install -d "$launch_agent_dir" "$log_dir"
    escaped_binary=$(printf '%s' "$installed_binary" | sed 's/&/\\&amp;/g; s/</\\&lt;/g; s/>/\\&gt;/g; s/"/\\&quot;/g; s/'"'"'/\\&apos;/g')
    escaped_log_dir=$(printf '%s' "$log_dir" | sed 's/&/\\&amp;/g; s/</\\&lt;/g; s/>/\\&gt;/g; s/"/\\&quot;/g; s/'"'"'/\\&apos;/g')
    temporary_agent=$launch_agent.tmp.$$
    trap 'rm -f "$temporary_agent"' EXIT HUP INT TERM
    printf '%s\n' \
        '<?xml version="1.0" encoding="UTF-8"?>' \
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
        '<plist version="1.0">' \
        '<dict>' \
        '    <key>Label</key>' \
        '    <string>com.jonasrodrigues93.menvane</string>' \
        '    <key>ProgramArguments</key>' \
        '    <array>' \
        "        <string>$escaped_binary</string>" \
        '        <string>serve</string>' \
        '    </array>' \
        '    <key>RunAtLoad</key>' \
        '    <true/>' \
        '    <key>KeepAlive</key>' \
        '    <true/>' \
        "    <key>StandardOutPath</key>" \
        "    <string>$escaped_log_dir/menvane.log</string>" \
        "    <key>StandardErrorPath</key>" \
        "    <string>$escaped_log_dir/menvane.error.log</string>" \
        '</dict>' \
        '</plist>' > "$temporary_agent"
    chmod 644 "$temporary_agent"
    mv "$temporary_agent" "$launch_agent"
    trap - EXIT HUP INT TERM
    launch_domain="gui/$(id -u)"
    launchctl bootout "$launch_domain" "$launch_agent" >/dev/null 2>&1 || true
    launchctl bootstrap "$launch_domain" "$launch_agent"
    printf 'Installed %s and enabled the macOS LaunchAgent\n' "$installed_binary"
else
    install -m 755 "$binary" "$installed_binary"
    printf 'Installed %s; automatic startup is currently supported on Linux and macOS\n' "$installed_binary"
fi
