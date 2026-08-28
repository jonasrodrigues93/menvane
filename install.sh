#!/bin/sh
set -eu

usage() {
    printf '%s\n' "Usage: ./install.sh [--binary PATH] [--version VERSION]"
}

binary=
version=latest
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            binary=$2
            shift 2
            ;;
        --version)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            version=$2
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

case "$version" in
    latest) ;;
    ''|*[!A-Za-z0-9._-]*)
        printf '%s\n' "version must contain only letters, numbers, dots, underscores, and hyphens" >&2
        exit 2
        ;;
esac

release_repository=https://github.com/jonasrodrigues93/menvane
temporary_dir=
temporary_unit=
temporary_agent=
cleanup() {
    if [ -n "$temporary_dir" ] && [ -d "$temporary_dir" ]; then
        rm -r -- "$temporary_dir"
    fi
    if [ -n "$temporary_unit" ]; then
        rm -f "$temporary_unit"
    fi
    if [ -n "$temporary_agent" ]; then
        rm -f "$temporary_agent"
    fi
}
trap cleanup EXIT HUP INT TERM

platform=$(uname -s)
architecture=$(uname -m)
release_target=
case "$platform:$architecture" in
    Linux:x86_64|Linux:amd64)
        release_target=x86_64-unknown-linux-gnu
        ;;
    Linux:aarch64|Linux:arm64)
        release_target=aarch64-unknown-linux-gnu
        ;;
    Darwin:x86_64|Darwin:amd64)
        release_target=x86_64-apple-darwin
        ;;
    Darwin:arm64|Darwin:aarch64)
        release_target=aarch64-apple-darwin
        ;;
esac

downloader=
if command -v curl >/dev/null 2>&1; then
    downloader=curl
elif command -v wget >/dev/null 2>&1; then
    downloader=wget
fi

fetch() {
    case "$downloader" in
        curl)
            curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$2" "$1"
            ;;
        wget)
            wget --https-only --quiet --output-document="$2" "$1"
            ;;
        *)
            return 1
            ;;
    esac
}

download_binary() {
    archive_name=menvane-$release_target.tar.gz
    if [ "$version" = latest ]; then
        release_path=$release_repository/releases/latest/download
    else
        release_tag=$version
        case "$release_tag" in
            v*) ;;
            *) release_tag=v$release_tag ;;
        esac
        release_path=$release_repository/releases/download/$release_tag
    fi

    temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/menvane-install.XXXXXX")
    archive_path=$temporary_dir/$archive_name
    checksums_path=$temporary_dir/SHA256SUMS
    if ! fetch "$release_path/SHA256SUMS" "$checksums_path"; then
        return 1
    fi
    if ! fetch "$release_path/$archive_name" "$archive_path"; then
        return 1
    fi

    expected_checksum=$(awk -v name="$archive_name" '$2 == name { value = $1; count++ } END { if (count == 1) print value }' "$checksums_path")
    actual_checksum=
    if command -v sha256sum >/dev/null 2>&1; then
        actual_checksum=$(sha256sum "$archive_path" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual_checksum=$(shasum -a 256 "$archive_path" | awk '{print $1}')
    else
        printf '%s\n' "sha256sum or shasum is required to verify a published binary" >&2
        return 2
    fi
    if [ -z "$expected_checksum" ] || [ "$expected_checksum" != "$actual_checksum" ]; then
        printf 'Checksum verification failed for %s\n' "$archive_name" >&2
        return 2
    fi

    extract_dir=$temporary_dir/extracted
    install -d "$extract_dir"
    if [ "$(tar -tzf "$archive_path")" != menvane ]; then
        printf '%s\n' "published binary archive has an unexpected layout" >&2
        return 2
    fi
    if ! tar -xzf "$archive_path" -C "$extract_dir" menvane; then
        printf '%s\n' "could not extract the published binary archive" >&2
        return 2
    fi
    binary=$extract_dir/menvane
    if [ ! -f "$binary" ] || [ -L "$binary" ]; then
        printf '%s\n' "published binary archive does not contain a regular menvane executable" >&2
        return 2
    fi
}

if [ -z "$binary" ]; then
    [ -n "$release_target" ] || {
        printf '%s\n' "no published Menvane binary is available for this platform" >&2
        exit 1
    }
    [ -n "$downloader" ] || {
        printf '%s\n' "curl or wget is required to download a published Menvane binary" >&2
        exit 1
    }
    command -v tar >/dev/null 2>&1 || {
        printf '%s\n' "tar is required to extract a published Menvane binary" >&2
        exit 1
    }
    download_binary || {
        printf 'Could not download requested release %s\n' "$version" >&2
        exit 1
    }
fi

[ -f "$binary" ] || {
    printf 'Menvane binary not found: %s\n' "$binary" >&2
    exit 1
}

home=${HOME:?HOME is not set}
install_dir=$home/.local/bin
installed_binary=$install_dir/menvane
install -d "$install_dir"

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
    temporary_unit=

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
    temporary_agent=
    launch_domain="gui/$(id -u)"
    launchctl bootout "$launch_domain" "$launch_agent" >/dev/null 2>&1 || true
    launchctl bootstrap "$launch_domain" "$launch_agent"
    printf 'Installed %s and enabled the macOS LaunchAgent\n' "$installed_binary"
else
    install -m 755 "$binary" "$installed_binary"
    printf 'Installed %s; automatic startup is currently supported on Linux and macOS\n' "$installed_binary"
fi
