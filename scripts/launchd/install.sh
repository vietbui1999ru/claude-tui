#!/bin/zsh
# Install (or remove) a per-user launchd agent that keeps claude-daemon running.
# Usage: zsh scripts/launchd/install.sh [--uninstall]
# Expects claude-daemon at ~/.cargo/bin (cargo install --path crates/claude-daemon --locked).
set -eu

label=dev.claude-tui.daemon
plist="$HOME/Library/LaunchAgents/$label.plist"
bin="$HOME/.cargo/bin/claude-daemon"
log="$HOME/Library/Logs/claude-daemon.log"
domain="gui/$(id -u)"

if [[ ${1:-} == --uninstall ]]; then
  launchctl bootout "$domain/$label" 2>/dev/null || true
  rm -f "$plist"
  echo "removed $label"
  exit 0
fi

[[ -x $bin ]] || { echo "missing $bin; run: cargo install --path crates/claude-daemon --locked" >&2; exit 1; }
mkdir -p "${plist:h}" "${log:h}"

cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key><array><string>$bin</string></array>
  <key>EnvironmentVariables</key><dict><key>RUST_LOG</key><string>info</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>$log</string>
  <key>StandardErrorPath</key><string>$log</string>
</dict>
</plist>
EOF

# Reinstalling replaces a running agent, e.g. after cargo install of a new build.
launchctl bootout "$domain/$label" 2>/dev/null || true
launchctl bootstrap "$domain" "$plist"
echo "loaded $label; logs: $log"
