#!/usr/bin/env bash
# Claude Usage Monitor - SketchyBar Plugin
# Queries the claude-daemon and formats output for SketchyBar.
# Requires: socat, jq, bc
#
# SketchyBar config example:
#   sketchybar --add item claude right \
#     --set claude script="/path/to/claude_plugin.sh" \
#                  update_freq=60
set -o pipefail

# Ensure Homebrew tools are available (GUI apps like SketchyBar use minimal PATH)
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

# Dependency checks
for cmd in socat jq bc; do
    command -v "$cmd" >/dev/null 2>&1 || { echo "Error: '$cmd' is required but not installed." >&2; exit 1; }
done

# Resolve symlinks so the script can find its siblings when invoked via a symlink
SCRIPT_PATH="$0"
if [ -L "$SCRIPT_PATH" ]; then
    SCRIPT_PATH="$(readlink "$SCRIPT_PATH")"
fi
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"

# Source shared functions
source "$SCRIPT_DIR/../lib/claude_common.sh"

SOCKET="${CLAUDE_DAEMON_SOCKET:-/tmp/claude-daemon-$(id -u).sock}"

# Query daemon
RESPONSE=$(echo '{"jsonrpc":"2.0","id":1,"method":"status","params":{}}' \
    | socat -t2 - UNIX-CONNECT:"$SOCKET" 2>/dev/null)

if [ $? -ne 0 ] || [ -z "$RESPONSE" ]; then
    sketchybar --set "$NAME" icon="x" label="offline" icon.color="0xfff05050" 2>/dev/null
    echo "x offline"
    exit 0
fi

# Parse response
ACTIVE=$(echo "$RESPONSE" | jq -r '.result.active_sessions // 0')
MODEL=$(echo "$RESPONSE" | jq -r '.result.current_model // "none"')
COST=$(echo "$RESPONSE" | jq -r '.result.cost_today_usd // 0')
BUDGET_PCT=$(echo "$RESPONSE" | jq -r '.result.budget_pct // empty')

# Icon
if [ -n "$BUDGET_PCT" ]; then
    OVER=$(echo "$BUDGET_PCT > 1.0" | bc -l 2>/dev/null)
    if [ "$OVER" = "1" ]; then
        ICON="!"
    elif [ "$ACTIVE" -gt 0 ] 2>/dev/null; then
        ICON="*"
    else
        ICON="-"
    fi
else
    if [ "$ACTIVE" -gt 0 ] 2>/dev/null; then
        ICON="*"
    else
        ICON="-"
    fi
fi

# Model name for display
case "$MODEL" in
    "opus")   MODEL_DISPLAY="Opus" ;;
    "sonnet") MODEL_DISPLAY="Sonnet" ;;
    "haiku")  MODEL_DISPLAY="Haiku" ;;
    *)        MODEL_DISPLAY="Idle" ;;
esac

COST_DISPLAY=$(printf "\$%.2f" "$COST")

# Build label and color based on whether a budget is configured
if [ -n "$BUDGET_PCT" ]; then
    BUDGET_BAR=$(build_budget_bar "$BUDGET_PCT")
    PCT_INT=$(echo "$BUDGET_PCT * 100" | bc -l 2>/dev/null | cut -d. -f1)
    LABEL="${MODEL_DISPLAY} | ${BUDGET_BAR} ${PCT_INT}% | ${COST_DISPLAY}"

    if [ "$PCT_INT" -lt 50 ] 2>/dev/null; then
        COLOR="0xff50c878"   # Green
    elif [ "$PCT_INT" -lt 75 ] 2>/dev/null; then
        COLOR="0xfff0b432"   # Yellow
    elif [ "$PCT_INT" -lt 90 ] 2>/dev/null; then
        COLOR="0xfff08232"   # Orange
    else
        COLOR="0xfff05050"   # Red
    fi
else
    # No budget set — just show model and cost
    LABEL="${MODEL_DISPLAY} | ${COST_DISPLAY}"
    COLOR="0xff8caaf0"       # Default blue
fi

# Update SketchyBar (if running as a plugin)
if [ -n "$NAME" ]; then
    sketchybar --set "$NAME" \
        icon="$ICON" \
        label="$LABEL" \
        label.color="$COLOR" 2>/dev/null
fi

# Also output to stdout for standalone use
echo "${ICON} ${LABEL}"

# Set update frequency based on activity
if [ -n "$NAME" ]; then
    if [ "$ACTIVE" -gt 0 ] 2>/dev/null; then
        sketchybar --set "$NAME" update_freq=5 2>/dev/null
    else
        sketchybar --set "$NAME" update_freq=60 2>/dev/null
    fi
fi

exit 0
