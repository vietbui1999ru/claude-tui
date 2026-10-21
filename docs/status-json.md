# `claude-tui status --json`

The daemon is the single source of usage data. `claude-tui status --json` prints
the same numbers as the Overview tab as one JSON document on stdout, so the Pi
extensions and any other local script can read them without parsing provider
files themselves.

This document is the contract. Bump `v` when a consumer would have to change.
The command always exits 0, even when the daemon is down.

## Invocation

```sh
claude-tui status --json
```

The socket comes from `claude_common::socket_path()`: `CLAUDE_DAEMON_SOCKET`,
then `$XDG_RUNTIME_DIR/claude-daemon.sock`, then `/tmp/claude-daemon-<uid>.sock`.
Connect and read time out after two seconds.

## Two shapes

Daemon reachable:

```json
{
  "v": 1,
  "subscriptions": {
    "claude": { "...": "a subscription object" },
    "chatgpt": { "...": "a subscription object" },
    "opencode-go": { "...": "a subscription object" }
  },
  "generated_at": "2026-10-20T14:05:00+00:00"
}
```

Daemon not reachable:

```json
{ "v": 1, "daemon": "down" }
```

Key order inside an object is not significant; the examples are indented for
reading, the command prints one compact line.

| Key | Type | Meaning |
|---|---|---|
| `v` | integer | Schema version. Currently `1`. |
| `daemon` | string | Present only in the down shape, always `"down"`. `subscriptions` and `generated_at` are then absent. |
| `subscriptions` | object | One key per subscription. Only in the reachable shape. |
| `generated_at` | string | RFC 3339 timestamp of when the document was built. |

## `subscriptions`

`claude`, `chatgpt` and `opencode-go` are always keys in a reachable document. A
subscription the daemon has no data for is `null`, which is different from a key
that is missing. `other` appears only when the daemon recorded usage for a
provider it does not group.

Each present subscription is the daemon's `SubscriptionStatus`:

```json
{
  "subscription": "opencode-go",
  "quota": [
    {
      "window": "primary",
      "used_pct": 3.0,
      "resets_at": "2026-10-20T19:05:00Z",
      "source": "codex-rollout",
      "observed_at": "2026-10-20T13:42:11Z",
      "stale": false,
      "expired": false
    }
  ],
  "today": {
    "input": 410000,
    "output": 96000,
    "cache_read": 102500,
    "cache_write": 20500,
    "reasoning": 0,
    "cost_usd": 3.20,
    "cost_source": "reported"
  },
  "last_7d": { "...": "the same token-totals shape" },
  "by_tool": [
    {
      "provider": "opencode-go",
      "source": "pi-session",
      "totals": { "...": "the same token-totals shape" }
    }
  ],
  "last_quota_hit": {
    "period": "5h",
    "reset_at": "2026-10-20T16:05:00Z",
    "observed_at": "2026-10-20T13:25:00Z"
  },
  "sources": [
    {
      "collector_id": "pi-sessions",
      "last_update": "2026-10-20T13:44:02Z",
      "stale": false
    }
  ]
}
```

### `subscription`

`claude`, `chatgpt`, `opencode-go` or `other`.

### `quota`

The newest observation per window, sorted by window name. Empty when the
provider exposes no percentage (opencode-go) or nothing was collected.

| Field | Type | Meaning |
|---|---|---|
| `window` | string | As the provider reports it. Claude uses `five_hour`/`seven_day` with a collector suffix (`five_hour:live`, `seven_day:stale`, `five_hour:expired`). Codex uses `primary` (5-hour) and `secondary` (weekly). |
| `used_pct` | number or null | Percent used, 0–100. `null` when the provider exposes no percentage, or when an expired window was cleared instead of reported. |
| `resets_at` | string or null | RFC 3339 reset time. |
| `source` | string | `log`, `claude-quota`, `codex-rollout`, `pi-session` or `opencode-db`. |
| `observed_at` | string | When the daemon last saw this window. |
| `stale` | boolean | No observation in the last 24 hours. Never treat a stale percentage as current. |
| `expired` | boolean | The reset time has passed. |

### `today` and `last_7d`

`today` is the local calendar day; `last_7d` covers the six local days before
today plus today. Both use the same token-totals shape.

| Field | Type | Meaning |
|---|---|---|
| `input`, `output`, `cache_read`, `cache_write` | integer | Token counts. |
| `reasoning` | integer | Reasoning tokens. Collectors fold these into `output`, so this is `0` today. |
| `cost_usd` | number or null | USD over the records whose cost is known. `null` when every record in the window has an unknown cost. |
| `cost_source` | string | `reported` (the provider stated the cost; trust it), `computed` (the daemon derived it from configured prices; show it as an API-equivalent, the UI marks it `≈`), or `unknown` (no price configured for the model; the tokens are still counted). |

### `by_tool`

Per provider/source breakdown over the same seven days, so the two tools that
share one plan (`openai-codex` and the Codex CLI, `opencode-go` and OpenCode)
stay distinguishable. Fields: `provider`, `source`, `totals`.

### `last_quota_hit`

The most recent provider quota-limit event, or `null`.

| Field | Type | Meaning |
|---|---|---|
| `period` | string or null | The window named in the error text: `5h`, `weekly`, `monthly`, or `null` when the provider did not say. |
| `reset_at` | string or null | Reset time parsed from the error text. |
| `observed_at` | string | When the daemon recorded the hit. |

### `sources`

One entry per collector feeding this subscription: `collector_id`, `last_update`
(nullable when it has stored nothing yet) and `stale` (no update in 24 hours).

## What the document never contains

Usage metadata only. No prompt text, no response text, no credentials, no token
or API-key values, no file paths from provider data. The collectors open
provider data read-only.

## Reading it from a script

```sh
state=$(claude-tui status --json)
if [ "$(printf '%s' "$state" | jq -r .daemon)" = "down" ]; then
  echo "usage: daemon down"
  exit 0
fi
printf '%s' "$state" | jq -r '.subscriptions | to_entries[] |
  "\(.key): \(.value.today.cost_usd // "unknown") USD today"'
```

`claude-tui status --line` is the same data pre-rendered for sketchybar, for
callers that do not need to parse anything.
