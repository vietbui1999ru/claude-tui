# Claude TUI - System Architecture

**Date:** 2026-03-05
**Author:** Architect (Opus)
**Status:** Complete

---

## Table of Contents

1. [Cargo Workspace Structure](#1-cargo-workspace-structure)
2. [Data Models (claude-common)](#2-data-models-claude-common)
3. [IPC Protocol (JSON-RPC over Unix Socket)](#3-ipc-protocol-json-rpc-over-unix-socket)
4. [SQLite Schema](#4-sqlite-schema)
5. [Error Taxonomy](#5-error-taxonomy)
6. [Module API Boundaries](#6-module-api-boundaries)
7. [Collector Design](#7-collector-design)

---

## 1. Cargo Workspace Structure

### Workspace Root: `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = [
    "crates/claude-common",
    "crates/claude-daemon",
    "crates/claude-tui",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"
rust-version = "1.85"

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
chrono = { version = "0.4", features = ["serde"] }
tokio = { version = "1.43", features = ["full"] }
thiserror = "2.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1.11", features = ["v4", "serde"] }
```

### `crates/claude-common/Cargo.toml`

```toml
[package]
name = "claude-common"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }
```

### `crates/claude-daemon/Cargo.toml`

```toml
[package]
name = "claude-daemon"
version.workspace = true
edition.workspace = true

[[bin]]
name = "claude-daemon"
path = "src/main.rs"

[dependencies]
claude-common = { path = "../claude-common" }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
tokio = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
uuid = { workspace = true }
rusqlite = { version = "0.32", features = ["bundled", "chrono"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
notify = { version = "7.0", features = ["macos_fsevent"] }
notify-debouncer-mini = "0.5"
directories = "6.0"
```

### `crates/claude-tui/Cargo.toml`

```toml
[package]
name = "claude-tui"
version.workspace = true
edition.workspace = true

[[bin]]
name = "claude-tui"
path = "src/main.rs"

[dependencies]
claude-common = { path = "../claude-common" }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
tokio = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
ratatui = "0.29"
crossterm = "0.28"
```

---

## 2. Data Models (claude-common)

All shared types live in `crates/claude-common/src/models.rs`.

### ModelType

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Opus,
    Sonnet,
    Haiku,
}

impl ModelType {
    /// Price per million input tokens in USD.
    pub fn input_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 15.0,
            ModelType::Sonnet => 3.0,
            ModelType::Haiku => 0.80,
        }
    }

    /// Price per million output tokens in USD.
    pub fn output_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 75.0,
            ModelType::Sonnet => 15.0,
            ModelType::Haiku => 4.0,
        }
    }

    /// Price per million cache read tokens in USD.
    pub fn cache_read_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 1.50,
            ModelType::Sonnet => 0.30,
            ModelType::Haiku => 0.08,
        }
    }

    /// Price per million cache write tokens in USD.
    pub fn cache_write_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 18.75,
            ModelType::Sonnet => 3.75,
            ModelType::Haiku => 1.0,
        }
    }

    /// Compute cost for a given token breakdown.
    pub fn compute_cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> f64 {
        let input = input_tokens as f64 * self.input_price_per_m() / 1_000_000.0;
        let output = output_tokens as f64 * self.output_price_per_m() / 1_000_000.0;
        let cache_read = cache_read_tokens as f64 * self.cache_read_price_per_m() / 1_000_000.0;
        let cache_write = cache_write_tokens as f64 * self.cache_write_price_per_m() / 1_000_000.0;
        input + output + cache_read + cache_write
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelType::Opus => "Opus",
            ModelType::Sonnet => "Sonnet",
            ModelType::Haiku => "Haiku",
        }
    }
}

impl fmt::Display for ModelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parse from full model ID strings like "claude-opus-4-6" or "claude-sonnet-4-6".
impl std::str::FromStr for ModelType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        if lower.contains("opus") {
            Ok(ModelType::Opus)
        } else if lower.contains("sonnet") {
            Ok(ModelType::Sonnet)
        } else if lower.contains("haiku") {
            Ok(ModelType::Haiku)
        } else {
            Err(format!("unknown model: {s}"))
        }
    }
}
```

### SessionStatus

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Streaming,
    Idle,
    Completed,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Streaming => f.write_str("Streaming"),
            SessionStatus::Idle => f.write_str("Idle"),
            SessionStatus::Completed => f.write_str("Completed"),
        }
    }
}
```

### UsageRecord

```rust
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Auto-incremented database ID. None before insertion.
    pub id: Option<i64>,
    /// Unique identifier for deduplication.
    pub uuid: Uuid,
    /// When this usage event occurred.
    pub timestamp: DateTime<Utc>,
    /// Which model was used.
    pub model: ModelType,
    /// Number of input tokens (non-cached).
    pub input_tokens: u64,
    /// Number of output tokens.
    pub output_tokens: u64,
    /// Number of cache read tokens.
    pub cache_read_tokens: u64,
    /// Number of cache write tokens.
    pub cache_write_tokens: u64,
    /// Computed cost in USD.
    pub cost_usd: f64,
    /// Session this request belongs to, if known.
    pub session_id: Option<String>,
    /// Project/directory context, if known.
    pub project: Option<String>,
    /// Data source: "api" or "log".
    pub source: DataSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataSource {
    Api,
    Log,
}
```

### ActiveSession

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    pub session_id: String,
    pub model: ModelType,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub cost_usd: f64,
    pub request_count: u32,
    pub status: SessionStatus,
    pub project: Option<String>,
}
```

### BudgetConfig

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Maximum daily spend in USD. None means no limit.
    pub daily_limit_usd: Option<f64>,
    /// Maximum weekly spend in USD. None means no limit.
    pub weekly_limit_usd: Option<f64>,
    /// Maximum monthly spend in USD. None means no limit.
    pub monthly_limit_usd: Option<f64>,
    /// Percentage (0.0 - 1.0) at which to trigger an alert.
    pub alert_threshold_pct: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_limit_usd: None,
            weekly_limit_usd: None,
            monthly_limit_usd: None,
            alert_threshold_pct: 0.80,
        }
    }
}
```

### DailyAggregate

```rust
use chrono::NaiveDate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyAggregate {
    pub date: NaiveDate,
    pub model: ModelType,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub total_cost_usd: f64,
    pub request_count: u64,
    pub session_count: u64,
}
```

### TimeRange (query helper)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeWindow {
    Day,
    Week,
    Month,
    Quarter,
}
```

---

## 3. IPC Protocol (JSON-RPC over Unix Socket)

The daemon exposes a Unix domain socket at `$XDG_RUNTIME_DIR/claude-daemon.sock` (fallback: `/tmp/claude-daemon-{uid}.sock`). The protocol is JSON-RPC 2.0 over newline-delimited JSON (one JSON object per line).

All types below live in `crates/claude-common/src/protocol.rs`.

### Envelope

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,  // Always "2.0"
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,  // Always "2.0"
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}
```

### Error Codes

| Code | Meaning |
|------|---------|
| -32700 | Parse error |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |
| -1 | Collector unavailable |
| -2 | Storage error |

### RPC Methods

#### `status` - Current daemon status (for menu bar / SketchyBar)

**Params:** None

**Response:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub daemon_uptime_secs: u64,
    pub active_sessions: u32,
    pub current_model: Option<ModelType>,
    pub cost_today_usd: f64,
    pub budget_pct: Option<f64>,  // 0.0 - 1.0, None if no budget set
    pub collector_status: CollectorStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectorStatus {
    Api,
    Log,
    Offline,
}
```

#### `usage.query` - Query raw usage records

**Params:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageQueryParams {
    #[serde(default)]
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub model: Option<ModelType>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 { 100 }
```

**Response:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageQueryResponse {
    pub records: Vec<UsageRecord>,
    pub total_count: u64,
}
```

#### `usage.summary` - Aggregated usage summary

**Params:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageSummaryParams {
    pub window: TimeWindow,
    #[serde(default)]
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub model: Option<ModelType>,
}
```

**Response:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageSummaryResponse {
    pub aggregates: Vec<DailyAggregate>,
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_requests: u64,
}
```

#### `sessions.list` - List sessions

**Params:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsListParams {
    #[serde(default)]
    pub status: Option<SessionStatus>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}
```

**Response:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsListResponse {
    pub sessions: Vec<ActiveSession>,
    pub total_count: u64,
}
```

#### `sessions.get` - Get specific session

**Params:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsGetParams {
    pub session_id: String,
}
```

**Response:** `ActiveSession` (direct)

#### `budget.get` - Get budget configuration

**Params:** None

**Response:** `BudgetConfig` (direct)

#### `budget.set` - Update budget configuration

**Params:** `BudgetConfig` (direct)

**Response:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct BudgetSetResponse {
    pub success: bool,
}
```

#### `models.compare` - Comparative model statistics

**Params:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelsCompareParams {
    #[serde(default)]
    pub time_range: Option<TimeRange>,
}
```

**Response:**
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct ModelsCompareResponse {
    pub models: Vec<ModelStats>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelStats {
    pub model: ModelType,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub request_count: u64,
    pub avg_input_per_request: f64,
    pub avg_output_per_request: f64,
    pub avg_cost_per_request: f64,
}
```

### Helper: typed dispatch

To avoid stringly-typed method dispatch in the daemon, define an enum:

```rust
#[derive(Debug)]
pub enum RpcMethod {
    Status,
    UsageQuery(UsageQueryParams),
    UsageSummary(UsageSummaryParams),
    SessionsList(SessionsListParams),
    SessionsGet(SessionsGetParams),
    BudgetGet,
    BudgetSet(BudgetConfig),
    ModelsCompare(ModelsCompareParams),
}

impl RpcMethod {
    pub fn from_request(req: &RpcRequest) -> Result<Self, RpcError> {
        match req.method.as_str() {
            "status" => Ok(RpcMethod::Status),
            "usage.query" => {
                let params = serde_json::from_value(req.params.clone())
                    .map_err(|e| RpcError { code: -32602, message: e.to_string(), data: None })?;
                Ok(RpcMethod::UsageQuery(params))
            }
            // ... same pattern for each method
            _ => Err(RpcError { code: -32601, message: format!("unknown method: {}", req.method), data: None }),
        }
    }
}
```

---

## 4. SQLite Schema

Database location: `$XDG_DATA_HOME/claude-daemon/usage.db` (fallback: `~/.local/share/claude-daemon/usage.db`).

### Table: `usage_records`

```sql
CREATE TABLE IF NOT EXISTS usage_records (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid            TEXT NOT NULL UNIQUE,
    timestamp       TEXT NOT NULL,           -- ISO 8601 / RFC 3339
    model           TEXT NOT NULL,           -- "opus", "sonnet", "haiku"
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd        REAL NOT NULL DEFAULT 0.0,
    session_id      TEXT,
    project         TEXT,
    source          TEXT NOT NULL DEFAULT 'api'  -- "api" or "log"
);

-- Primary query: records by time range
CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_records(timestamp);

-- Filter by model within time range
CREATE INDEX IF NOT EXISTS idx_usage_model_timestamp ON usage_records(model, timestamp);

-- Filter by session
CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_records(session_id) WHERE session_id IS NOT NULL;

-- Filter by project
CREATE INDEX IF NOT EXISTS idx_usage_project ON usage_records(project) WHERE project IS NOT NULL;

-- Deduplication lookup
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_uuid ON usage_records(uuid);
```

### Table: `daily_aggregates`

Pre-computed at end of each day, and recomputed on-demand for the current day.

```sql
CREATE TABLE IF NOT EXISTS daily_aggregates (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL,           -- YYYY-MM-DD
    model           TEXT NOT NULL,
    total_input_tokens    INTEGER NOT NULL DEFAULT 0,
    total_output_tokens   INTEGER NOT NULL DEFAULT 0,
    total_cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    total_cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    total_cost_usd        REAL NOT NULL DEFAULT 0.0,
    request_count         INTEGER NOT NULL DEFAULT 0,
    session_count         INTEGER NOT NULL DEFAULT 0,
    UNIQUE(date, model)
);

CREATE INDEX IF NOT EXISTS idx_daily_date ON daily_aggregates(date);
CREATE INDEX IF NOT EXISTS idx_daily_model_date ON daily_aggregates(model, date);
```

### Table: `sessions`

```sql
CREATE TABLE IF NOT EXISTS sessions (
    session_id      TEXT PRIMARY KEY,
    model           TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    last_activity   TEXT NOT NULL,
    total_input_tokens    INTEGER NOT NULL DEFAULT 0,
    total_output_tokens   INTEGER NOT NULL DEFAULT 0,
    total_cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    total_cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd        REAL NOT NULL DEFAULT 0.0,
    request_count   INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'idle',  -- "streaming", "idle", "completed"
    project         TEXT
);

CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at);
```

### Table: `budget_config`

Single-row table (enforced by CHECK constraint on id).

```sql
CREATE TABLE IF NOT EXISTS budget_config (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    daily_limit_usd     REAL,
    weekly_limit_usd    REAL,
    monthly_limit_usd   REAL,
    alert_threshold_pct REAL NOT NULL DEFAULT 0.80
);

-- Seed default row
INSERT OR IGNORE INTO budget_config (id, alert_threshold_pct) VALUES (1, 0.80);
```

### Table: `schema_version` (migrations)

```sql
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);
```

The daemon runs migrations on startup. Each migration is a numbered SQL script. The current schema corresponds to version 1.

---

## 5. Error Taxonomy

All errors live in `crates/claude-common/src/lib.rs` (re-exported from submodules as needed).

### Hierarchy

```rust
use thiserror::Error;

/// Top-level application error.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("collector error: {0}")]
    Collector(#[from] CollectorError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("ipc error: {0}")]
    Ipc(#[from] IpcError),

    #[error("config error: {0}")]
    Config(String),
}

/// Errors from the data collector subsystem.
#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("api request failed: {0}")]
    ApiRequest(#[source] reqwest::Error),

    #[error("api returned error {status}: {body}")]
    ApiResponse { status: u16, body: String },

    #[error("api rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("api authentication failed: {0}")]
    AuthError(String),

    #[error("failed to parse log line: {0}")]
    LogParse(String),

    #[error("log file watch error: {0}")]
    LogWatch(#[source] notify::Error),

    #[error("log file not found: {path}")]
    LogNotFound { path: String },
}

/// Errors from SQLite storage.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[source] rusqlite::Error),

    #[error("migration failed at version {version}: {reason}")]
    Migration { version: u32, reason: String },

    #[error("database not found at {path}")]
    NotFound { path: String },

    #[error("query error: {0}")]
    Query(String),
}

/// Errors from the IPC layer.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("connection failed: {0}")]
    Connection(#[source] std::io::Error),

    #[error("socket bind failed at {path}: {source}")]
    SocketBind { path: String, source: std::io::Error },

    #[error("serialization error: {0}")]
    Serialization(#[source] serde_json::Error),

    #[error("deserialization error: {0}")]
    Deserialization(#[source] serde_json::Error),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("request timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("daemon not running (socket not found at {path})")]
    DaemonNotRunning { path: String },
}
```

### Converting IPC errors to JSON-RPC error codes

```rust
impl IpcError {
    pub fn to_rpc_error(&self) -> RpcError {
        let code = match self {
            IpcError::Serialization(_) | IpcError::Deserialization(_) => -32700,
            IpcError::Protocol(_) => -32600,
            _ => -32603,
        };
        RpcError {
            code,
            message: self.to_string(),
            data: None,
        }
    }
}
```

---

## 6. Module API Boundaries

### crate: `claude-common`

**Depends on:** nothing (leaf crate)

**Exports:**

```
claude_common::
    models::
        ModelType               (enum: Opus, Sonnet, Haiku)
        SessionStatus           (enum: Streaming, Idle, Completed)
        DataSource              (enum: Api, Log)
        UsageRecord             (struct)
        ActiveSession           (struct)
        BudgetConfig            (struct)
        DailyAggregate          (struct)
        TimeRange               (struct)
        TimeWindow              (enum: Day, Week, Month, Quarter)

    protocol::
        RpcRequest              (struct)
        RpcResponse             (struct)
        RpcError                (struct)
        RpcMethod               (enum, typed dispatch)
        StatusResponse          (struct)
        CollectorStatus         (enum)
        UsageQueryParams        (struct)
        UsageQueryResponse      (struct)
        UsageSummaryParams      (struct)
        UsageSummaryResponse    (struct)
        SessionsListParams      (struct)
        SessionsListResponse    (struct)
        SessionsGetParams       (struct)
        BudgetSetResponse       (struct)
        ModelsCompareParams     (struct)
        ModelsCompareResponse   (struct)
        ModelStats              (struct)

    errors::
        AppError                (enum)
        CollectorError          (enum)
        StorageError            (enum)
        IpcError                (enum)

    (re-exported at crate root via `pub use`)
```

### crate: `claude-daemon`

**Depends on:** `claude-common`

**Internal modules (not publicly exported -- this is a binary crate):**

```
claude_daemon::
    main.rs
        main()                  -- Tokio entry point, sets up tracing, starts subsystems

    collector.rs
        Collector               (struct)
            ::new(config, storage_tx) -> Self
            ::run(&self) -> Result<(), CollectorError>
                -- Spawns API poller + log watcher tasks
        ApiPoller               (struct, private)
            ::poll_once(&self) -> Result<Vec<UsageRecord>, CollectorError>
        LogWatcher              (struct, private)
            ::start(&self) -> Result<(), CollectorError>
        CollectorConfig         (struct)
            api_key: Option<String>
            poll_interval_secs: u64        (default: 60)
            log_paths: Vec<PathBuf>
            fallback_to_logs: bool         (default: true)

    storage.rs
        Storage                 (struct)
            ::new(db_path) -> Result<Self, StorageError>
            ::migrate(&self) -> Result<(), StorageError>
            ::insert_usage(&self, record: &UsageRecord) -> Result<(), StorageError>
            ::insert_usage_batch(&self, records: &[UsageRecord]) -> Result<(), StorageError>
            ::query_usage(&self, params: &UsageQueryParams) -> Result<UsageQueryResponse, StorageError>
            ::get_summary(&self, params: &UsageSummaryParams) -> Result<UsageSummaryResponse, StorageError>
            ::get_daily_aggregates(&self, start: NaiveDate, end: NaiveDate, model: Option<ModelType>) -> Result<Vec<DailyAggregate>, StorageError>
            ::recompute_daily_aggregate(&self, date: NaiveDate) -> Result<(), StorageError>
            ::upsert_session(&self, session: &ActiveSession) -> Result<(), StorageError>
            ::list_sessions(&self, params: &SessionsListParams) -> Result<SessionsListResponse, StorageError>
            ::get_session(&self, id: &str) -> Result<Option<ActiveSession>, StorageError>
            ::get_budget(&self) -> Result<BudgetConfig, StorageError>
            ::set_budget(&self, config: &BudgetConfig) -> Result<(), StorageError>
            ::get_cost_today(&self) -> Result<f64, StorageError>
            ::get_model_stats(&self, params: &ModelsCompareParams) -> Result<ModelsCompareResponse, StorageError>

    ipc.rs
        IpcServer               (struct)
            ::new(socket_path, storage) -> Self
            ::run(&self) -> Result<(), IpcError>
                -- Accepts connections, spawns per-client handlers
        handle_request(storage, request: RpcRequest) -> RpcResponse
            -- Routes RpcMethod to Storage calls, wraps results
```

### crate: `claude-tui`

**Depends on:** `claude-common`

**Internal modules (binary crate):**

```
claude_tui::
    main.rs
        main()                  -- Tokio entry point, initializes terminal, runs app

    app.rs
        App                     (struct)
            ::new(client) -> Self
            ::run(&mut self, terminal) -> Result<(), AppError>
            ::handle_event(&mut self, event: Event) -> Result<(), AppError>
        Tab                     (enum: Tokens, Costs, Models, Live)

    client.rs
        DaemonClient            (struct)
            ::connect(socket_path) -> Result<Self, IpcError>
            ::status(&self) -> Result<StatusResponse, IpcError>
            ::query_usage(&self, params: UsageQueryParams) -> Result<UsageQueryResponse, IpcError>
            ::get_summary(&self, params: UsageSummaryParams) -> Result<UsageSummaryResponse, IpcError>
            ::list_sessions(&self, params: SessionsListParams) -> Result<SessionsListResponse, IpcError>
            ::get_session(&self, id: &str) -> Result<ActiveSession, IpcError>
            ::get_budget(&self) -> Result<BudgetConfig, IpcError>
            ::set_budget(&self, config: BudgetConfig) -> Result<BudgetSetResponse, IpcError>
            ::compare_models(&self, params: ModelsCompareParams) -> Result<ModelsCompareResponse, IpcError>

    widgets/
        token_chart.rs
            TokenChartWidget    (struct) -- Bar chart of tokens over time
                ::render(&self, area: Rect, buf: &mut Buffer, data: &[DailyAggregate])

        cost_breakdown.rs
            CostBreakdownWidget (struct) -- Cost tables and cumulative spend line
                ::render(&self, area: Rect, buf: &mut Buffer, data: &UsageSummaryResponse)

        model_compare.rs
            ModelCompareWidget  (struct) -- Side-by-side model stats table
                ::render(&self, area: Rect, buf: &mut Buffer, data: &ModelsCompareResponse)

        live_monitor.rs
            LiveMonitorWidget   (struct) -- Active session list with streaming indicators
                ::render(&self, area: Rect, buf: &mut Buffer, sessions: &[ActiveSession])
```

### Dependency Graph

```
claude-common  (leaf: no internal deps)
     ^
     |
     +-- claude-daemon  (depends on claude-common)
     |
     +-- claude-tui     (depends on claude-common)
```

`claude-daemon` and `claude-tui` never depend on each other. They communicate exclusively over the Unix socket IPC protocol defined in `claude-common`.

---

## 7. Collector Design

### 7.1 Anthropic API Polling

The daemon's `ApiPoller` periodically calls the Anthropic usage API to fetch token consumption data.

**Configuration:**
- `poll_interval_secs`: Default 60 seconds. Configurable.
- `api_key`: Read from `ANTHROPIC_API_KEY` env var, or from `~/.config/claude-daemon/config.toml`.

**Polling strategy:**
1. On startup, query the API for usage since the last recorded timestamp in the database (or last 24 hours if database is empty).
2. Every `poll_interval_secs`, query for usage since the last successfully fetched timestamp.
3. Each response is paginated; follow pagination cursors until all pages are consumed.
4. Deduplicate via UUID: each usage event has a unique ID. The `uuid` column with a UNIQUE index prevents double-counting.

**Rate limiting:**
- If the API returns 429, read the `Retry-After` header.
- Back off for the specified duration (or default 60s if header absent).
- Use exponential backoff with jitter for consecutive failures: `min(base * 2^attempt + jitter, max_backoff)`.
- `base`: 5s, `max_backoff`: 300s, `jitter`: 0-5s random.

**Error handling:**
- 401/403: Log `CollectorError::AuthError`, stop API polling, switch to log fallback.
- 429: `CollectorError::RateLimited`, apply backoff.
- 5xx: `CollectorError::ApiResponse`, apply exponential backoff.
- Network errors: `CollectorError::ApiRequest`, apply exponential backoff.

### 7.2 Local Log File Watcher

Uses the `notify` crate (with FSEvents backend on macOS) to watch Claude log files for changes.

**Log file locations (searched in order):**
1. `~/.claude/logs/` (Claude Code logs)
2. `~/.config/claude/logs/` (alternative location)
3. Custom paths from config

**Watcher design:**
1. On startup, identify existing log files via glob patterns (`*.jsonl`, `*.log`).
2. Set up a `notify` watcher on the parent directories.
3. On file change events, read new lines from the last-known file offset.
4. Parse each line according to the expected JSON format.
5. Normalize parsed data into `UsageRecord` structs.
6. Deduplicate: generate a deterministic UUID from (timestamp, model, token counts) for log-sourced records.

**Log line parsing:**
The parser expects JSONL format with at minimum:
```json
{"timestamp":"...","model":"claude-sonnet-4-6","usage":{"input_tokens":1234,"output_tokens":567}}
```

The parser is lenient: unknown fields are ignored, missing optional fields default to zero/None.

### 7.3 Fallback Logic

The collector runs both subsystems but prioritizes API data:

```
                   +-----------+
                   | Collector |
                   +-----+-----+
                         |
              +----------+----------+
              |                     |
        +-----+------+      +------+-----+
        | API Poller  |      | Log Watcher|
        | (primary)   |      | (fallback) |
        +-----+------+      +------+-----+
              |                     |
              +----------+----------+
                         |
                    +----+-----+
                    | Dedup &  |
                    | Normalize|
                    +----+-----+
                         |
                    +----+-----+
                    | Storage  |
                    +----------+
```

**State machine:**
- `ApiActive`: API poller is working. Log watcher runs in background but its records are marked `source: Log`. API records (`source: Api`) take precedence in case of overlap.
- `LogFallback`: API poller has failed 3+ consecutive times or returned auth error. Log watcher becomes the primary source. API poller retries every 5 minutes.
- `Offline`: Neither source is producing data. Daemon is still running and serving cached data over IPC.

**Deduplication rules:**
- Records with the same UUID are silently skipped (UNIQUE constraint).
- For log-sourced records where we generate a synthetic UUID: hash of `(timestamp_epoch_ms, model, input_tokens, output_tokens)`. This prevents duplicate insertion when reading the same log line twice, while still allowing genuinely distinct requests with coincidentally similar data (the timestamp's millisecond precision disambiguates).

### 7.4 Data Normalization Pipeline

```
Raw API Response / Raw Log Line
         |
    [Parse] -- extract fields, handle missing/optional
         |
    [Validate] -- reject if timestamp is in the future,
         |        reject if all token counts are zero
         |
    [Enrich] -- compute cost_usd via ModelType::compute_cost()
         |       assign DataSource tag (Api or Log)
         |       generate UUID if not present (log source)
         |
    [Normalize] -- clamp negative values to 0
         |         truncate cost_usd to 6 decimal places
         |
    UsageRecord (ready for insertion)
```

### 7.5 Aggregate Maintenance

The daemon maintains `daily_aggregates` to enable fast chart rendering:

- **End-of-day rollup:** At midnight UTC (or on the first poll after midnight), the daemon recomputes the aggregate for the previous day.
- **Current-day aggregate:** Recomputed every 5 minutes and on every IPC query that touches today's data. This is computed via a live SQL query over `usage_records` rather than reading the stale `daily_aggregates` row.
- **Backfill:** On first startup, compute aggregates for all historical days in the database.

---

## Appendix A: Configuration File

Location: `~/.config/claude-daemon/config.toml`

```toml
[api]
# Anthropic API key (overridden by ANTHROPIC_API_KEY env var)
# api_key = "sk-ant-..."
poll_interval_secs = 60

[log]
# Additional log file paths to watch
# paths = ["~/.claude/logs/"]
fallback_enabled = true

[storage]
# Override database path
# db_path = "~/.local/share/claude-daemon/usage.db"

[daemon]
# Override socket path
# socket_path = "/tmp/claude-daemon.sock"
```

## Appendix B: Socket Path Resolution

```rust
pub fn socket_path() -> PathBuf {
    // 1. Check config file override
    // 2. Check $CLAUDE_DAEMON_SOCKET env var
    // 3. Use $XDG_RUNTIME_DIR/claude-daemon.sock
    // 4. Fall back to /tmp/claude-daemon-{uid}.sock
    if let Ok(path) = std::env::var("CLAUDE_DAEMON_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("claude-daemon.sock");
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/claude-daemon-{uid}.sock"))
}
```

## Appendix C: SketchyBar / Menu Bar Query

Shell scripts query the daemon via `socat`:

```bash
#!/bin/bash
SOCKET="${CLAUDE_DAEMON_SOCKET:-/tmp/claude-daemon-$(id -u).sock}"
RESPONSE=$(echo '{"jsonrpc":"2.0","id":1,"method":"status","params":{}}' \
    | socat - UNIX-CONNECT:"$SOCKET" 2>/dev/null)

if [ $? -ne 0 ]; then
    echo "offline"
    exit 0
fi

# Parse with jq
ACTIVE=$(echo "$RESPONSE" | jq -r '.result.active_sessions // 0')
MODEL=$(echo "$RESPONSE" | jq -r '.result.current_model // "none"')
COST=$(echo "$RESPONSE" | jq -r '.result.cost_today_usd // 0')
BUDGET=$(echo "$RESPONSE" | jq -r '.result.budget_pct // "N/A"')

# Format for display
if [ "$ACTIVE" -gt 0 ]; then
    INDICATOR="*"
else
    INDICATOR="-"
fi

echo "${INDICATOR} ${MODEL} | \$${COST}"
```
