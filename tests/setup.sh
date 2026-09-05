#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$script_dir/target/release/menvane
[ -x "$binary" ] || { printf '%s\n' "release binary is required" >&2; exit 1; }

test_root=$(mktemp -d)
cleanup() { rm -r -- "$test_root"; }
trap cleanup EXIT HUP INT TERM
fake_bin=$test_root/bin
mkdir -p "$fake_bin"
cat > "$fake_bin/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "${MENVANE_SYSTEMCTL_LOG:?MENVANE_SYSTEMCTL_LOG is not set}"
exit 0
EOF
chmod 755 "$fake_bin/systemctl"

home=$test_root/home
manifest=$test_root/setup.toml
cat > "$manifest" <<EOF
schema_version = 1
home = "$home"
provider = "openai-api"
model = "gpt-test"
reasoning_effort = "low"
api_key = "setup-secret"
api_key_env = "OPENAI_API_KEY"
max_prompt_bytes = 12000
max_cards = 2
agents = []
EOF

output=$(HOME="$test_root/user" \
    PATH="$fake_bin:$PATH" \
    MENVANE_SYSTEMCTL_LOG="$test_root/systemctl.log" \
    "$binary" setup --from "$manifest" --non-interactive --output-json)
[ -f "$home/config.toml" ]
[ ! -e "$home/daemon.pid" ]
case "$(tr -d '\n' < "$test_root/systemctl.log")" in
    *"--user daemon-reload"*"--user enable --now menvane.service"*) ;;
    *) printf '%s\n' "setup did not start the service at the final step" >&2; exit 1 ;;
esac
case "$output" in
    *setup-secret*) printf '%s\n' "setup output leaked an API key" >&2; exit 1 ;;
    *) ;;
esac
