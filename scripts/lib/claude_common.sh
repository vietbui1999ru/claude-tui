# Shared functions for Claude usage monitor scripts.
# Source this file from other scripts:
#   source "$(dirname "$0")/../lib/claude_common.sh"
# or for nested scripts:
#   source "$(dirname "$0")/../../lib/claude_common.sh"

# Build a 6-character budget bar from a percentage (0.0 to 1.0+).
# Usage: build_budget_bar "0.67"
build_budget_bar() {
    local pct="$1"
    if [ -z "$pct" ]; then
        echo "------"
        return
    fi
    local filled
    filled=$(echo "$pct * 6" | bc -l 2>/dev/null | cut -d. -f1)
    filled=${filled:-0}
    if [ "$filled" -gt 6 ]; then filled=6; fi
    local empty=$((6 - filled))
    local bar=""
    for ((i=0; i<filled; i++)); do bar="${bar}+"; done
    for ((i=0; i<empty; i++)); do bar="${bar}-"; done
    echo "$bar"
}
