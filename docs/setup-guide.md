# Claude TUI - Setup & Configuration Guide

A Rust-based monitoring system for tracking Claude AI token usage and costs. Includes a background daemon, a terminal UI, and a SketchyBar widget.

---

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Building from Source](#building-from-source)
3. [The Daemon (claude-daemon)](#the-daemon)
4. [The Terminal UI (claude-tui)](#the-terminal-ui)
5. [SketchyBar Widget](#sketchybar-widget)
6. [How It Works](#how-it-works)
8. [Configuration Reference](#configuration-reference)
9. [Troubleshooting](#troubleshooting)

---

## Prerequisites

### Required

- **Rust 1.85+** (edition 2024)
- **macOS** (tested on Darwin; Linux should work but is untested)

### For SketchyBar integration

```bash
brew install socat jq
```

`bc` is included with macOS by default.

### Optional

- **SketchyBar** - for menu bar widget: `brew tap FelixKratz/formulae && brew install sketchybar`
---

## Building from Source

```bash
git clone <repo-url>
cd claude-tui

# Build everything (debug)
cargo build --workspace

# Build release binaries
cargo build --release --workspace

# Run tests
cargo test --workspace
```

The two binaries are:
- `target/release/claude-daemon` - background data collector
- `target/release/claude-tui` - terminal UI

---

## The Daemon

The daemon (`claude-daemon`) runs in the background, parses Claude Code's local conversation logs, stores usage data in SQLite, and serves it over a Unix socket.

### Data Source

The daemon reads Claude Code's conversation JSONL files from `~/.claude/projects/`. These contain per-request token usage for every Claude Code interaction. **No API key is required** for individual Claude subscribers - the daemon works entirely from local data.

> If you have an Anthropic Admin API key (`sk-ant-admin...`), the daemon can also poll the Usage API directly. Set `ANTHROPIC_ADMIN_KEY` to enable this. Individual subscription keys (`sk-ant-api...`) are ignored since they lack admin permissions.

### Starting the Daemon

```bash
# Foreground (see logs)
./target/release/claude-daemon

# Background
./target/release/claude-daemon &

# With debug logging
RUST_LOG=debug ./target/release/claude-daemon
```

On first launch the daemon will:
1. Create the SQLite database at `~/.local/share/claude-daemon/usage.db`
2. Create the Unix socket at `/tmp/claude-daemon-{uid}.sock`
3. Recursively scan `~/.claude/projects/` and parse all conversation JSONL files
4. Insert usage records into the database (deduplicates via UUID)

Subsequent runs only read new data from each file (cursor-based tracking).

### Running as a LaunchAgent (auto-start on login)

Create `~/Library/LaunchAgents/com.claude-tui.daemon.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claude-tui.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/path/to/claude-daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/claude-daemon.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/claude-daemon.stderr.log</string>
</dict>
</plist>
```

Replace `/path/to/claude-daemon` with the actual path (e.g., `$HOME/repos/RustProjects/claude-tui/target/release/claude-daemon`).

```bash
# Load (starts immediately and on future logins)
launchctl load ~/Library/LaunchAgents/com.claude-tui.daemon.plist

# Unload
launchctl unload ~/Library/LaunchAgents/com.claude-tui.daemon.plist

# Check status
launchctl list | grep claude
```

### Verify the Daemon is Running

```bash
# Quick status check via socat
echo '{"jsonrpc":"2.0","id":1,"method":"status","params":{}}' \
    | socat -t2 - UNIX-CONNECT:/tmp/claude-daemon-$(id -u).sock | jq .
```

You should see a response with `cost_today_usd`, `active_sessions`, `collector_status`, etc.

---

## The Terminal UI

The TUI (`claude-tui`) connects to the daemon and displays an interactive dashboard.

### Running

```bash
# The daemon must be running first
./target/release/claude-tui
```

If the daemon isn't running, the TUI starts in **mock mode** with sample data.

### Keybindings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `r` | Refresh data |
| Left / Right | Switch tabs |
| `1` / `2` / `3` | Time range (Day/Week/Month) on Tokens tab |
| Up / Down | Scroll lists (Costs: project list, Live: session list) |

### Tabs

| Tab | Description |
|-----|-------------|
| **Tokens** | Bar chart of daily token usage with time range selector |
| **Costs** | Cost breakdown with per-project spending |
| **Models** | Side-by-side comparison of Opus/Sonnet/Haiku usage |
| **Live** | Active sessions with streaming indicators |

---

## SketchyBar Widget

Adds a Claude usage indicator to your SketchyBar menu bar.

### Setup

1. Make scripts executable:

```bash
chmod +x scripts/sketchybar/claude_plugin.sh
chmod +x scripts/lib/claude_common.sh
```

2. Add to your `~/.config/sketchybar/sketchybarrc`:

```bash
sketchybar --add item claude right \
    --set claude \
        script="$HOME/repos/RustProjects/claude-tui/scripts/sketchybar/claude_plugin.sh" \
        update_freq=60 \
        icon.font="Hack Nerd Font:Bold:14.0" \
        label.font="Hack Nerd Font:Regular:12.0"
```

Adjust the path and fonts to match your setup.

3. Reload SketchyBar:

```bash
sketchybar --reload
```

### What It Shows

```
ICON MODEL | BUDGET_BAR PCT% | $COST
```

- **Icon**: `*` = active session, `-` = idle, `!` = over budget
- **Model**: Opus / Sonnet / Haiku / Idle
- **Budget bar**: 6-char fill indicator (`+++---` = 50%)
- **Color**: Green (<50%) -> Yellow (<75%) -> Orange (<90%) -> Red (90%+)

### Adaptive Refresh

- Active sessions detected: updates every **5 seconds**
- Idle: updates every **60 seconds**

### Standalone Test

```bash
# Test without SketchyBar running
./scripts/sketchybar/claude_plugin.sh
# Output: - Idle | ------ 0% | $0.00
```

---

## How It Works

### Architecture

```
~/.claude/projects/**/*.jsonl     (Claude Code conversation logs)
            |
     [claude-daemon]              (background process)
       - Parses JSONL files
       - Tracks file cursors (only reads new data)
       - Stores records in SQLite
       - Serves data via Unix socket (JSON-RPC 2.0)
            |
     /tmp/claude-daemon-{uid}.sock
            |
     +------+
     |      |
   TUI  SketchyBar
```

### Data Flow

1. **Claude Code** writes conversation logs as JSONL to `~/.claude/projects/`
2. **claude-daemon** scans these files every 60 seconds, parsing only new bytes
3. Each `assistant` message with `usage` data becomes a `UsageRecord` in SQLite
4. **Clients** (TUI, scripts) query the daemon via the Unix socket
5. The daemon responds with aggregated stats, session lists, cost breakdowns

### What Gets Tracked

From each Claude Code response, the daemon extracts:
- **Model** (Opus, Sonnet, Haiku)
- **Input tokens** (uncached)
- **Output tokens**
- **Cache read tokens**
- **Cache creation tokens**
- **Session ID** and **timestamp**

Costs are computed using Anthropic's published per-model pricing.

---

## Configuration Reference

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ANTHROPIC_ADMIN_KEY` | (none) | Admin API key for Usage API (optional, `sk-ant-admin...`) |
| `CLAUDE_DAEMON_SOCKET` | `/tmp/claude-daemon-{uid}.sock` | Override socket path |
| `XDG_DATA_HOME` | `~/.local/share` | Override database directory |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

### File Locations

| File | Purpose |
|------|---------|
| `~/.local/share/claude-daemon/usage.db` | SQLite database |
| `/tmp/claude-daemon-{uid}.sock` | IPC socket |
| `~/.claude/projects/**/*.jsonl` | Claude Code conversation logs (read-only) |

### Daemon IPC Protocol

The daemon speaks JSON-RPC 2.0 over the Unix socket. Available methods:

| Method | Description |
|--------|-------------|
| `status` | Current status, cost today, active sessions |
| `usage.query` | Raw usage records with filtering |
| `usage.summary` | Aggregated usage by time window |
| `sessions.list` | List tracked sessions |
| `sessions.get` | Get a specific session by ID |
| `budget.get` | Get budget configuration |
| `budget.set` | Update budget limits |
| `models.compare` | Per-model usage statistics |

Example query:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"usage.summary","params":{"window":"day"}}' \
    | socat -t2 - UNIX-CONNECT:/tmp/claude-daemon-$(id -u).sock | jq .
```

---

## Troubleshooting

### Daemon won't start

```
failed to initialize storage: ...
```

Ensure the database directory exists:

```bash
mkdir -p ~/.local/share/claude-daemon
```

### Scripts show "offline"

1. Check the daemon is running: `pgrep claude-daemon`
2. Check the socket exists: `ls -la /tmp/claude-daemon-$(id -u).sock`
3. Test manually: `echo '{}' | socat -t2 - UNIX-CONNECT:/tmp/claude-daemon-$(id -u).sock`

### "socat not found" or "jq not found"

```bash
brew install socat jq
```

### No usage data appears

- The daemon only reads `~/.claude/projects/` — verify this directory exists and contains `.jsonl` files
- Check daemon logs: `RUST_LOG=debug ./target/release/claude-daemon`
- On first run, the initial scan of all historical files takes a few seconds

### SketchyBar plugin not updating

1. Verify the script is executable: `chmod +x scripts/sketchybar/claude_plugin.sh`
2. Test standalone: `./scripts/sketchybar/claude_plugin.sh`
3. Check SketchyBar logs: `brew services log sketchybar`
4. Reload: `sketchybar --reload`

### TUI shows mock data

The TUI starts in mock mode when it can't connect to the daemon. Start the daemon first:

```bash
./target/release/claude-daemon &
./target/release/claude-tui
```

### Socket permission errors

The socket is created with the daemon's user permissions. If running the TUI or scripts as a different user, they won't be able to connect. Ensure all components run as the same user.
