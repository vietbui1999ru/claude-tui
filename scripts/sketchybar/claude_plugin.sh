#!/usr/bin/env bash
# Claude usage for SketchyBar.
#
# Prints the same one line as `claude-tui status --line` (AC-10), e.g.
#   C 5h 34% 7d 12% · X 7d 8% · G $3.20 ⚠limit 14:05
# The command resolves the daemon socket through paths::socket_path() and exits
# 0 even when the daemon is down, so this script needs neither socat, jq nor bc.
#
# SketchyBar config example:
#   sketchybar --add item usage right \
#     --set usage script="/path/to/claude_plugin.sh" \
#                  update_freq=60
#
# Set CLAUDE_TUI_BIN when the binary is not on PATH.
set -o pipefail

# GUI apps such as SketchyBar run with a minimal PATH.
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$PATH"

BIN="${CLAUDE_TUI_BIN:-claude-tui}"

LABEL=""
COLOR="0xff8caaf0"   # Default blue

if ! command -v "$BIN" >/dev/null 2>&1; then
    LABEL="usage: $BIN not found"
    COLOR="0xfff05050"   # Red
else
    LABEL=$("$BIN" status --line 2>/dev/null)
    if [ -z "$LABEL" ]; then
        LABEL="usage: no output"
        COLOR="0xfff05050"   # Red
    else
        case "$LABEL" in
            *"daemon down"*)
                COLOR="0xff8a8a8a"   # Grey: nothing to show
                ;;
            *"limit"*)
                COLOR="0xfff0b432"   # Yellow: a limit hit in the last 24 hours
                ;;
        esac
    fi
fi

# Update the item when running as a plugin.
if [ -n "$NAME" ]; then
    sketchybar --set "$NAME" \
        label="$LABEL" \
        label.color="$COLOR" 2>/dev/null
fi

# Also output to stdout for standalone use.
echo "$LABEL"

exit 0
