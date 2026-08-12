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

if [ "$(uname -s)" = Linux ]; then
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
else
    install -m 755 "$binary" "$installed_binary"
    printf 'Installed %s; automatic startup is currently supported only on Linux\n' "$installed_binary"
fi
