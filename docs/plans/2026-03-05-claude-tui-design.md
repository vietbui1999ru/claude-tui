# Claude TUI - Design Document

**Date:** 2026-03-05
**Status:** Approved

## Overview

A Rust-based terminal UI application for monitoring, tracking, and analyzing Claude AI model token usage and costs. Built with a client-server architecture: a background daemon collects data while multiple frontends (TUI, macOS menu bar, SketchyBar) display it.

## Architecture: Client-Server

```
Anthropic API / Local Logs
        |
  claude-usage-daemon (background service)
    |-- SQLite database (persistent storage)
    |-- Unix domain socket IPC (JSON-RPC)
        |
  +---------------+---------------+------------------+
  | claude-tui    | menubar.sh    | sketchybar/      |
  | (ratatui)     | (bash)        | claude_plugin.sh |
  +---------------+---------------+------------------+
```

**Data sources (with fallback):**
1. Primary: Anthropic API usage/billing endpoints
2. Fallback: Local log file parsing (Claude CLI, Claude Code logs)

## Project Structure

```
claude-tui/
  Cargo.toml                      # Workspace root
  crates/
    claude-daemon/                 # Background data collection service
      src/
        main.rs                    # Daemon entry, Unix socket server
        collector.rs               # Anthropic API poller + log watcher
        storage.rs                 # SQLite persistence (rusqlite)
        ipc.rs                     # Unix socket IPC protocol
    claude-tui/                    # Ratatui terminal dashboard
      src/
        main.rs                    # TUI entry point
        app.rs                     # App state & event loop
        widgets/
          token_chart.rs           # Token usage over time (bar chart)
          cost_breakdown.rs        # Cost per model/project
          model_compare.rs         # Model comparison view
          live_monitor.rs          # Real-time session monitor
        client.rs                  # IPC client to daemon
    claude-common/                 # Shared types and protocol
      src/
        lib.rs
        models.rs                  # UsageRecord, CostEntry, Session, etc.
        protocol.rs                # IPC message types (JSON-RPC)
  scripts/
    menubar.sh                     # macOS menu bar widget
    sketchybar/
      claude_plugin.sh             # SketchyBar plugin
    aerospace/
      claude_workspace.sh          # AeroSpace workspace indicator
```

## Core Data Models

```rust
struct UsageRecord {
    id: i64,
    timestamp: DateTime<Utc>,
    model: ModelType,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
    session_id: Option<String>,
    project: Option<String>,
}

enum ModelType { Opus, Sonnet, Haiku }

struct ActiveSession {
    session_id: String,
    model: ModelType,
    started_at: DateTime<Utc>,
    tokens_so_far: u64,
    status: SessionStatus,
}

enum SessionStatus { Streaming, Idle, Completed }

struct BudgetConfig {
    daily_limit_usd: f64,
    monthly_limit_usd: f64,
    alert_threshold_pct: f64,
}
```

## TUI Dashboard

Four tab views:
1. **Tokens** - Token usage over time (bar chart, 7d/30d/90d)
2. **Costs** - Cost breakdown by model, project, cumulative spend
3. **Models** - Side-by-side model comparison (tokens, cost, latency)
4. **Live** - Real-time active session monitor with streaming indicator

Sidebar: Today's summary (model, sessions, tokens, cost, budget progress bar)

Keybindings: q=quit, r=refresh, arrows=navigate tabs, up/down=scroll

## macOS Widgets

### Menu bar (SketchyBar item)
Displays: `[active indicator] [model name] | [budget bar] | [$cost]`
Example: `* Sonnet | -------- 78% | $3.47`

### SketchyBar plugin
Shell script queries daemon via socat on Unix socket.
Returns JSON: `{"model":"Sonnet","budget_pct":0.78,"cost_today":3.47,"active":true}`

### AeroSpace integration
Workspace indicator showing active model in the workspace bar.

## Key Dependencies

- ratatui + crossterm (TUI rendering)
- rusqlite (SQLite storage)
- tokio (async runtime)
- reqwest (Anthropic API client)
- serde + serde_json (serialization)
- chrono (timestamps)
- notify (file watching for log fallback)

## Agent Team Design

### Team: claude-tui-team (Parallel with Sync Points)

**Phase 1 - Foundation (Parallel):**
- Architect (Opus): Module structure, data models, IPC protocol, error taxonomy
- Designer (Haiku): TUI layout mockups, widget designs, color scheme, SketchyBar format

**Phase 2 - Planning (Sequential):**
- Planner (Sonnet): Merge arch + design, create ordered task list, define acceptance criteria

**Phase 3 - Implementation (Parallel):**
- Coder A (Sonnet): claude-daemon + claude-common crates
- Coder B (Sonnet): claude-tui + shell scripts

**Phase 4 - Review (Parallel):**
- Reviewer (Sonnet): Code quality, style, idiomatic Rust, test coverage
- Devil's Advocate (Opus): Edge cases, failure modes, security holes, design challenges
- Rust Memory Verifier (Haiku): Ownership/lifetimes, unsafe patterns, memory leaks

**Sync rules:**
- Phase 2 blocks on Phase 1 completion
- Phase 3 blocks on Phase 2 completion
- Phase 4 blocks on Phase 3 completion
- Phase 4 findings loop back to Coders for fixes
