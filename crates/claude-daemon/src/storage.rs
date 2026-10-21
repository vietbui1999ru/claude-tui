use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
// The status types below are the AC-9 aggregates: see `StatusAggregates`,
// `window_totals`, `tool_breakdown`, `source_updates` and `status_for`.
use claude_common::protocol::{
    ModelStats, ModelsCompareParams, ModelsCompareResponse, QuotaHitStatus, QuotaWindow,
    SessionsListParams, SessionsListResponse, SourceStatus, SubscriptionStatus, TokenTotals,
    ToolBreakdown, UsageQueryParams, UsageQueryResponse, UsageSummaryParams, UsageSummaryResponse,
};
use claude_common::{
    ActiveSession, BudgetConfig, CostSource, DailyAggregate, DataSource, ModelType, Provider,
    QuotaHit, QuotaSnapshot, SessionStatus, Subscription, UsageRecord,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StorageError;

/// Column order shared by every `usage_records` reader.
const USAGE_COLUMNS: &str = "id, uuid, timestamp, model, input_tokens, output_tokens, \
     cache_read_tokens, cache_write_tokens, cost_usd, session_id, project, source, \
     provider, subscription, cost_source, dedup_key";

const INSERT_USAGE_SQL: &str = "INSERT OR IGNORE INTO usage_records (uuid, timestamp, model, \
     input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd, session_id, \
     project, source, provider, subscription, cost_source, dedup_key) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";

pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open or create a SQLite database at the given path.
    /// For in-memory databases, pass ":memory:".
    pub fn new(db_path: &str) -> Result<Self, StorageError> {
        if db_path != ":memory:"
            && let Some(parent) = Path::new(db_path).parent()
        {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::Sqlite(e.to_string()))?;
        }

        let conn = Connection::open(db_path).map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let mut storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    /// Run database migrations to bring the schema up to date.
    pub fn migrate(&mut self) -> Result<(), StorageError> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version INTEGER PRIMARY KEY
                );",
            )
            .map_err(|e| StorageError::Migration {
                version: 0,
                reason: e.to_string(),
            })?;

        let current_version: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Migration {
                version: 0,
                reason: e.to_string(),
            })?;

        if current_version < 1 {
            self.migrate_v1()?;
        }
        if current_version < 2 {
            self.migrate_v2()?;
        }

        Ok(())
    }

    fn migrate_v1(&self) -> Result<(), StorageError> {
        let sql = r#"
            CREATE TABLE IF NOT EXISTS usage_records (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid            TEXT NOT NULL UNIQUE,
                timestamp       TEXT NOT NULL,
                model           TEXT NOT NULL,
                input_tokens    INTEGER NOT NULL DEFAULT 0,
                output_tokens   INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd        REAL NOT NULL DEFAULT 0.0,
                session_id      TEXT,
                project         TEXT,
                source          TEXT NOT NULL DEFAULT 'api'
            );

            CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_records(timestamp);
            CREATE INDEX IF NOT EXISTS idx_usage_model_timestamp ON usage_records(model, timestamp);
            CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_records(session_id) WHERE session_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_usage_project ON usage_records(project) WHERE project IS NOT NULL;

            CREATE TABLE IF NOT EXISTS daily_aggregates (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                date            TEXT NOT NULL,
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
                status          TEXT NOT NULL DEFAULT 'idle',
                project         TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at);

            CREATE TABLE IF NOT EXISTS budget_config (
                id                  INTEGER PRIMARY KEY CHECK (id = 1),
                daily_limit_usd     REAL,
                weekly_limit_usd    REAL,
                monthly_limit_usd   REAL,
                alert_threshold_pct REAL NOT NULL DEFAULT 0.80
            );

            INSERT OR IGNORE INTO budget_config (id, alert_threshold_pct) VALUES (1, 0.80);

            INSERT OR IGNORE INTO schema_version (version) VALUES (1);
        "#;

        self.conn
            .execute_batch(sql)
            .map_err(|e| StorageError::Migration {
                version: 1,
                reason: e.to_string(),
            })?;

        Ok(())
    }

    /// Migration v2: provider-aware usage rows, quota history and collector
    /// cursors. Runs in one transaction, so a failure leaves the v1 database
    /// untouched.
    ///
    /// `cost_usd` was `NOT NULL` in v1 and SQLite cannot drop a constraint in
    /// place, so the table is rebuilt: create, copy, drop, rename, reindex.
    fn migrate_v2(&mut self) -> Result<(), StorageError> {
        let sql = r#"
            CREATE TABLE usage_records_v2 (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid            TEXT NOT NULL UNIQUE,
                timestamp       TEXT NOT NULL,
                model           TEXT NOT NULL,
                input_tokens    INTEGER NOT NULL DEFAULT 0,
                output_tokens   INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd        REAL,
                session_id      TEXT,
                project         TEXT,
                source          TEXT NOT NULL DEFAULT 'log',
                provider        TEXT NOT NULL DEFAULT 'anthropic',
                subscription    TEXT NOT NULL DEFAULT 'claude',
                cost_source     TEXT NOT NULL DEFAULT 'computed',
                dedup_key       TEXT NOT NULL
            );

            INSERT INTO usage_records_v2 (
                id, uuid, timestamp, model, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens, cost_usd, session_id,
                project, source, provider, subscription, cost_source, dedup_key
            )
            SELECT
                id, uuid, timestamp, model, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens, cost_usd, session_id,
                project, source, 'anthropic', 'claude', 'computed', uuid
            FROM usage_records;

            DROP TABLE usage_records;
            ALTER TABLE usage_records_v2 RENAME TO usage_records;

            CREATE INDEX idx_usage_timestamp ON usage_records(timestamp);
            CREATE INDEX idx_usage_model_timestamp ON usage_records(model, timestamp);
            CREATE INDEX idx_usage_session ON usage_records(session_id) WHERE session_id IS NOT NULL;
            CREATE INDEX idx_usage_project ON usage_records(project) WHERE project IS NOT NULL;
            CREATE UNIQUE INDEX idx_usage_provider_dedup ON usage_records(provider, dedup_key);

            CREATE TABLE quota_snapshots (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                subscription  TEXT NOT NULL,
                "window"      TEXT NOT NULL,
                used_pct      REAL,
                resets_at     TEXT,
                source        TEXT NOT NULL,
                observed_at   TEXT NOT NULL,
                plan_type     TEXT
            );

            CREATE INDEX idx_quota_snapshots_lookup
                ON quota_snapshots(subscription, "window", observed_at);

            CREATE TABLE quota_hits (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                subscription  TEXT NOT NULL,
                period        TEXT,
                reset_at      TEXT,
                observed_at   TEXT NOT NULL,
                raw_kind      TEXT NOT NULL,
                dedup_key     TEXT NOT NULL UNIQUE
            );

            CREATE TABLE collector_cursors (
                collector_id  TEXT NOT NULL,
                "key"         TEXT NOT NULL,
                value         TEXT NOT NULL,
                PRIMARY KEY (collector_id, "key")
            );

            INSERT OR IGNORE INTO schema_version (version) VALUES (2);
        "#;

        let tx = self
            .conn
            .transaction()
            .map_err(|e| StorageError::Migration {
                version: 2,
                reason: e.to_string(),
            })?;
        tx.execute_batch(sql).map_err(|e| StorageError::Migration {
            version: 2,
            reason: e.to_string(),
        })?;
        tx.commit().map_err(|e| StorageError::Migration {
            version: 2,
            reason: e.to_string(),
        })?;

        Ok(())
    }

    // --- Usage Records ---

    /// Single-row insert. Used by the storage tests; collectors write batches.
    #[allow(dead_code)]
    pub fn insert_usage(&self, record: &UsageRecord) -> Result<(), StorageError> {
        insert_usage_row(&self.conn, record).map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    pub fn insert_usage_batch(&mut self, records: &[UsageRecord]) -> Result<(), StorageError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        for record in records {
            insert_usage_row(&tx, record).map_err(|e| StorageError::Sqlite(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    pub fn query_usage(
        &self,
        params: &UsageQueryParams,
    ) -> Result<UsageQueryResponse, StorageError> {
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref tr) = params.time_range {
            where_clauses.push("timestamp >= ?".to_string());
            bind_values.push(Box::new(tr.start.to_rfc3339()));
            where_clauses.push("timestamp <= ?".to_string());
            bind_values.push(Box::new(tr.end.to_rfc3339()));
        }
        if let Some(ref model) = params.model {
            where_clauses.push("model = ?".to_string());
            bind_values.push(Box::new(model.to_string().to_lowercase()));
        }
        if let Some(ref project) = params.project {
            where_clauses.push("project = ?".to_string());
            bind_values.push(Box::new(project.clone()));
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        // Get total count
        let count_sql = format!("SELECT COUNT(*) FROM usage_records {where_sql}");
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let total_count: u64 = self
            .conn
            .query_row(&count_sql, bind_refs.as_slice(), |row| row.get(0))
            .map_err(|e| StorageError::Query(e.to_string()))?;

        // Get records
        let query_sql = format!(
            "SELECT {USAGE_COLUMNS} FROM usage_records {where_sql} ORDER BY timestamp DESC LIMIT ? OFFSET ?"
        );
        let mut all_binds: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let limit_val = params.limit as i64;
        let offset_val = params.offset as i64;
        all_binds.push(&limit_val);
        all_binds.push(&offset_val);

        let mut stmt = self
            .conn
            .prepare(&query_sql)
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let records = stmt
            .query_map(all_binds.as_slice(), row_to_usage_record)
            .map_err(|e| StorageError::Query(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))?;

        Ok(UsageQueryResponse {
            records,
            total_count,
        })
    }

    pub fn get_summary(
        &self,
        params: &UsageSummaryParams,
    ) -> Result<UsageSummaryResponse, StorageError> {
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref tr) = params.time_range {
            where_clauses.push("timestamp >= ?".to_string());
            bind_values.push(Box::new(tr.start.to_rfc3339()));
            where_clauses.push("timestamp <= ?".to_string());
            bind_values.push(Box::new(tr.end.to_rfc3339()));
        }
        if let Some(ref model) = params.model {
            where_clauses.push("model = ?".to_string());
            bind_values.push(Box::new(model.to_string().to_lowercase()));
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        // Get aggregates by date and model
        let sql = format!(
            "SELECT DATE(timestamp) as date, model, \
             SUM(input_tokens) as total_input, \
             SUM(output_tokens) as total_output, \
             SUM(cache_read_tokens) as total_cache_read, \
             SUM(cache_write_tokens) as total_cache_write, \
             SUM(cost_usd) as total_cost, \
             COUNT(*) as req_count, \
             COUNT(DISTINCT session_id) as sess_count \
             FROM usage_records {where_sql} \
             GROUP BY date, model \
             ORDER BY date ASC"
        );

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let aggregates: Vec<DailyAggregate> = stmt
            .query_map(bind_refs.as_slice(), |row| {
                let date_str: String = row.get(0)?;
                let model_str: String = row.get(1)?;
                Ok(DailyAggregate {
                    date: NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
                        .unwrap_or_else(|_| Utc::now().date_naive()),
                    model: model_str,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    total_cache_read_tokens: row.get::<_, i64>(4)? as u64,
                    total_cache_write_tokens: row.get::<_, i64>(5)? as u64,
                    total_cost_usd: row.get(6)?,
                    request_count: row.get::<_, i64>(7)? as u64,
                    session_count: row.get::<_, i64>(8)? as u64,
                })
            })
            .map_err(|e| StorageError::Query(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let total_cost_usd: f64 = aggregates.iter().map(|a| a.total_cost_usd).sum();
        let total_input_tokens: u64 = aggregates.iter().map(|a| a.total_input_tokens).sum();
        let total_output_tokens: u64 = aggregates.iter().map(|a| a.total_output_tokens).sum();
        let total_requests: u64 = aggregates.iter().map(|a| a.request_count).sum();

        Ok(UsageSummaryResponse {
            aggregates,
            total_cost_usd,
            total_input_tokens,
            total_output_tokens,
            total_requests,
        })
    }

    /// Rows of the `daily_aggregates` rollup table.
    ///
    /// Nothing populates that table yet: the midnight scheduler is deferred
    /// (D3, m5, m8), and the `status` and summary paths read `usage_records`
    /// directly.
    #[allow(dead_code)]
    pub fn get_daily_aggregates(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        model: Option<ModelType>,
    ) -> Result<Vec<DailyAggregate>, StorageError> {
        let mut sql = "SELECT date, model, total_input_tokens, total_output_tokens, total_cache_read_tokens, total_cache_write_tokens, total_cost_usd, request_count, session_count FROM daily_aggregates WHERE date >= ? AND date <= ?".to_string();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        bind_values.push(Box::new(start.to_string()));
        bind_values.push(Box::new(end.to_string()));

        if let Some(ref m) = model {
            sql.push_str(" AND model = ?");
            bind_values.push(Box::new(m.to_string().to_lowercase()));
        }
        sql.push_str(" ORDER BY date ASC");

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let results = stmt
            .query_map(bind_refs.as_slice(), |row| {
                let date_str: String = row.get(0)?;
                let model_str: String = row.get(1)?;
                Ok(DailyAggregate {
                    date: NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
                        .unwrap_or_else(|_| Utc::now().date_naive()),
                    model: model_str,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    total_cache_read_tokens: row.get::<_, i64>(4)? as u64,
                    total_cache_write_tokens: row.get::<_, i64>(5)? as u64,
                    total_cost_usd: row.get(6)?,
                    request_count: row.get::<_, i64>(7)? as u64,
                    session_count: row.get::<_, i64>(8)? as u64,
                })
            })
            .map_err(|e| StorageError::Query(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))?;

        Ok(results)
    }

    /// Recompute one day of `daily_aggregates`; see [`Self::get_daily_aggregates`]
    /// for why it has no caller yet.
    #[allow(dead_code)]
    pub fn recompute_daily_aggregate(&self, date: NaiveDate) -> Result<(), StorageError> {
        let date_str = date.to_string();
        let date_next = date.succ_opt().unwrap_or(date).to_string();

        // Delete existing aggregates for this date
        self.conn
            .execute(
                "DELETE FROM daily_aggregates WHERE date = ?1",
                params![date_str],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Recompute from usage_records
        self.conn
            .execute(
                "INSERT INTO daily_aggregates (date, model, total_input_tokens, total_output_tokens, total_cache_read_tokens, total_cache_write_tokens, total_cost_usd, request_count, session_count)
                 SELECT DATE(timestamp) as d, model,
                        SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens), SUM(cache_write_tokens),
                        SUM(cost_usd), COUNT(*), COUNT(DISTINCT session_id)
                 FROM usage_records
                 WHERE timestamp >= ?1 AND timestamp < ?2
                 GROUP BY d, model",
                params![date_str, date_next],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        Ok(())
    }

    // --- Quota snapshots and hits ---
    //
    // Added by the v2 migration (AC-8). The collectors (Phase C) and the
    // status RPC (Phase D) are the callers, so the binary does not reference
    // this API yet; the storage tests do.

    #[allow(dead_code)]
    pub fn insert_quota_snapshot(&self, snapshot: &QuotaSnapshot) -> Result<(), StorageError> {
        self.conn
            .execute(
                "INSERT INTO quota_snapshots (subscription, \"window\", used_pct, resets_at, source, observed_at, plan_type) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    snapshot.subscription.as_str(),
                    snapshot.window,
                    snapshot.used_pct,
                    snapshot.resets_at.map(|ts| ts.to_rfc3339()),
                    snapshot.source.as_str(),
                    snapshot.observed_at.to_rfc3339(),
                    snapshot.plan_type,
                ],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    /// The newest snapshot per window for one subscription.
    #[allow(dead_code)]
    pub fn latest_quota(
        &self,
        subscription: Subscription,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, subscription, \"window\", used_pct, resets_at, source, observed_at, plan_type \
                 FROM quota_snapshots q \
                 WHERE q.subscription = ?1 \
                   AND q.id = (\
                       SELECT q2.id FROM quota_snapshots q2 \
                       WHERE q2.subscription = q.subscription AND q2.\"window\" = q.\"window\" \
                       ORDER BY q2.observed_at DESC, q2.id DESC LIMIT 1\
                   ) \
                 ORDER BY q.\"window\" ASC",
            )
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let snapshots = stmt
            .query_map(params![subscription.as_str()], row_to_quota_snapshot)
            .map_err(|e| StorageError::Query(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))?;

        Ok(snapshots)
    }

    /// Insert a quota hit. A repeated `dedup_key` is ignored.
    #[allow(dead_code)]
    pub fn insert_quota_hit(&self, hit: &QuotaHit) -> Result<(), StorageError> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO quota_hits (subscription, period, reset_at, observed_at, raw_kind, dedup_key) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    hit.subscription.as_str(),
                    hit.period,
                    hit.reset_at.map(|ts| ts.to_rfc3339()),
                    hit.observed_at.to_rfc3339(),
                    hit.raw_kind,
                    hit.dedup_key,
                ],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    /// The most recent quota hit for one subscription.
    #[allow(dead_code)]
    pub fn latest_quota_hit(
        &self,
        subscription: Subscription,
    ) -> Result<Option<QuotaHit>, StorageError> {
        self.conn
            .query_row(
                "SELECT id, subscription, period, reset_at, observed_at, raw_kind, dedup_key \
                 FROM quota_hits WHERE subscription = ?1 \
                 ORDER BY observed_at DESC, id DESC LIMIT 1",
                params![subscription.as_str()],
                row_to_quota_hit,
            )
            .optional()
            .map_err(|e| StorageError::Query(e.to_string()))
    }

    // --- Collector cursors ---

    #[allow(dead_code)]
    pub fn get_cursor(
        &self,
        collector_id: &str,
        key: &str,
    ) -> Result<Option<String>, StorageError> {
        self.conn
            .query_row(
                "SELECT value FROM collector_cursors WHERE collector_id = ?1 AND \"key\" = ?2",
                params![collector_id, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Query(e.to_string()))
    }

    /// Every stored `dedup_key`, for collector tests.
    #[cfg(test)]
    pub fn dedup_keys_for_test(&self) -> Vec<String> {
        let mut stmt = self
            .conn
            .prepare("SELECT dedup_key FROM usage_records ORDER BY dedup_key")
            .expect("prepare dedup_keys");
        stmt.query_map([], |row| row.get(0))
            .expect("query dedup_keys")
            .collect::<Result<Vec<_>, _>>()
            .expect("read dedup_keys")
    }

    /// Every stored cursor of one collector, as (key, value) pairs.
    pub fn cursors_for(&self, collector_id: &str) -> Result<Vec<(String, String)>, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT \"key\", value FROM collector_cursors WHERE collector_id = ?1")
            .map_err(|e| StorageError::Query(e.to_string()))?;
        let rows = stmt
            .query_map(params![collector_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| StorageError::Query(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))
    }

    #[allow(dead_code)]
    pub fn set_cursor(
        &self,
        collector_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), StorageError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO collector_cursors (collector_id, \"key\", value) \
                 VALUES (?1, ?2, ?3)",
                params![collector_id, key, value],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    // --- Sessions ---

    /// Upsert one session row. Used by the storage tests; the daemon rebuilds
    /// sessions from usage records instead.
    #[allow(dead_code)]
    pub fn upsert_session(&self, session: &ActiveSession) -> Result<(), StorageError> {
        let model_str = session.model.to_string().to_lowercase();
        let status_str = session.status.to_string().to_lowercase();

        self.conn
            .execute(
                "INSERT INTO sessions (session_id, model, started_at, last_activity, total_input_tokens, total_output_tokens, total_cache_read_tokens, total_cache_write_tokens, cost_usd, request_count, status, project)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(session_id) DO UPDATE SET
                     last_activity = excluded.last_activity,
                     total_input_tokens = excluded.total_input_tokens,
                     total_output_tokens = excluded.total_output_tokens,
                     total_cache_read_tokens = excluded.total_cache_read_tokens,
                     total_cache_write_tokens = excluded.total_cache_write_tokens,
                     cost_usd = excluded.cost_usd,
                     request_count = excluded.request_count,
                     status = excluded.status",
                params![
                    session.session_id,
                    model_str,
                    session.started_at.to_rfc3339(),
                    session.last_activity.to_rfc3339(),
                    session.total_input_tokens as i64,
                    session.total_output_tokens as i64,
                    session.total_cache_read_tokens as i64,
                    session.total_cache_write_tokens as i64,
                    session.cost_usd,
                    session.request_count as i64,
                    status_str,
                    session.project,
                ],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    pub fn list_sessions(
        &self,
        params: &SessionsListParams,
    ) -> Result<SessionsListResponse, StorageError> {
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref status) = params.status {
            where_clauses.push("status = ?".to_string());
            bind_values.push(Box::new(status.to_string().to_lowercase()));
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        // Count
        let count_sql = format!("SELECT COUNT(*) FROM sessions {where_sql}");
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let total_count: u64 = self
            .conn
            .query_row(&count_sql, bind_refs.as_slice(), |row| row.get(0))
            .map_err(|e| StorageError::Query(e.to_string()))?;

        // Query
        let query_sql = format!(
            "SELECT session_id, model, started_at, last_activity, total_input_tokens, total_output_tokens, total_cache_read_tokens, total_cache_write_tokens, cost_usd, request_count, status, project FROM sessions {where_sql} ORDER BY last_activity DESC LIMIT ? OFFSET ?"
        );
        let mut all_binds: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let limit_val = params.limit as i64;
        let offset_val = params.offset as i64;
        all_binds.push(&limit_val);
        all_binds.push(&offset_val);

        let mut stmt = self
            .conn
            .prepare(&query_sql)
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let sessions = stmt
            .query_map(all_binds.as_slice(), row_to_session)
            .map_err(|e| StorageError::Query(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))?;

        Ok(SessionsListResponse {
            sessions,
            total_count,
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Option<ActiveSession>, StorageError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, model, started_at, last_activity, total_input_tokens, total_output_tokens, total_cache_read_tokens, total_cache_write_tokens, cost_usd, request_count, status, project FROM sessions WHERE session_id = ?1",
            )
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![id], row_to_session)
            .map_err(|e| StorageError::Query(e.to_string()))?;

        match rows.next() {
            Some(Ok(session)) => Ok(Some(session)),
            Some(Err(e)) => Err(StorageError::Query(e.to_string())),
            None => Ok(None),
        }
    }

    /// Rebuild the sessions table from usage_records.
    /// Sessions with activity in the last 5 minutes are marked 'streaming',
    /// others are marked 'idle'.
    pub fn rebuild_sessions(&self) -> Result<(), StorageError> {
        let cutoff = (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        self.conn
            .execute_batch("DELETE FROM sessions;")
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO sessions (session_id, model, started_at, last_activity,
                     total_input_tokens, total_output_tokens,
                     total_cache_read_tokens, total_cache_write_tokens,
                     cost_usd, request_count, status, project)
                 SELECT
                     session_id,
                     model,
                     MIN(timestamp) as started_at,
                     MAX(timestamp) as last_activity,
                     SUM(input_tokens) as total_input_tokens,
                     SUM(output_tokens) as total_output_tokens,
                     SUM(cache_read_tokens) as total_cache_read_tokens,
                     SUM(cache_write_tokens) as total_cache_write_tokens,
                     SUM(cost_usd) as cost_usd,
                     COUNT(*) as request_count,
                     CASE WHEN MAX(timestamp) >= ?1 THEN 'streaming' ELSE 'idle' END as status,
                     project
                 FROM usage_records
                 WHERE session_id IS NOT NULL
                 GROUP BY session_id",
                params![cutoff],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    // --- Budget ---

    pub fn get_budget(&self) -> Result<BudgetConfig, StorageError> {
        self.conn
            .query_row(
                "SELECT daily_limit_usd, weekly_limit_usd, monthly_limit_usd, alert_threshold_pct FROM budget_config WHERE id = 1",
                [],
                |row| {
                    Ok(BudgetConfig {
                        daily_limit_usd: row.get(0)?,
                        weekly_limit_usd: row.get(1)?,
                        monthly_limit_usd: row.get(2)?,
                        alert_threshold_pct: row.get(3)?,
                    })
                },
            )
            .map_err(|e| StorageError::Query(e.to_string()))
    }

    pub fn set_budget(&self, config: &BudgetConfig) -> Result<(), StorageError> {
        if let Some(limit) = config.daily_limit_usd
            && limit < 0.0
        {
            return Err(StorageError::Query(
                "daily_limit_usd must be >= 0".to_string(),
            ));
        }
        if let Some(limit) = config.weekly_limit_usd
            && limit < 0.0
        {
            return Err(StorageError::Query(
                "weekly_limit_usd must be >= 0".to_string(),
            ));
        }
        if let Some(limit) = config.monthly_limit_usd
            && limit < 0.0
        {
            return Err(StorageError::Query(
                "monthly_limit_usd must be >= 0".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&config.alert_threshold_pct) {
            return Err(StorageError::Query(
                "alert_threshold_pct must be in 0.0..=1.0".to_string(),
            ));
        }
        self.conn
            .execute(
                "UPDATE budget_config SET daily_limit_usd = ?1, weekly_limit_usd = ?2, monthly_limit_usd = ?3, alert_threshold_pct = ?4 WHERE id = 1",
                params![
                    config.daily_limit_usd,
                    config.weekly_limit_usd,
                    config.monthly_limit_usd,
                    config.alert_threshold_pct,
                ],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    // --- Aggregation helpers ---

    pub fn get_cost_today(&self) -> Result<f64, StorageError> {
        let today_start = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let tomorrow_start = (Utc::now().date_naive() + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let cost: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM usage_records WHERE timestamp >= ?1 AND timestamp < ?2",
                params![today_start, tomorrow_start],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Query(e.to_string()))?;
        Ok(cost)
    }

    // --- Per-subscription status ---
    //
    // AC-9. Every aggregate runs in SQLite, so building a status response never
    // loads usage rows into memory.

    /// Per-subscription status for the `status` RPC.
    ///
    /// The result always carries `claude`, `chatgpt` and `opencode-go`, in that
    /// order, and then `other` when anything was recorded for it. `today` is
    /// the local calendar day; `last_7d` covers the local days from six days
    /// before today. A source or quota window with no update in 24 hours is
    /// stale (SPEC Boundaries).
    pub fn subscription_statuses(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<SubscriptionStatus>, StorageError> {
        let today_start = local_day_start(now, 0);
        let tomorrow_start = local_day_start(now, 1);
        let week_start = local_day_start(now, -6);

        let aggregates = StatusAggregates {
            today: self.window_totals(today_start, tomorrow_start)?,
            week: self.window_totals(week_start, tomorrow_start)?,
            week_tools: self.tool_breakdown(week_start, tomorrow_start)?,
            source_updates: self.source_updates()?,
        };

        let mut statuses = Vec::with_capacity(4);
        for subscription in [
            Subscription::Claude,
            Subscription::ChatGpt,
            Subscription::OpencodeGo,
        ] {
            statuses.push(self.status_for(subscription, now, &aggregates)?);
        }

        let other_has_data = aggregates.has_data(Subscription::Other)
            || self.latest_quota_hit(Subscription::Other)?.is_some();
        if other_has_data {
            statuses.push(self.status_for(Subscription::Other, now, &aggregates)?);
        }

        Ok(statuses)
    }

    /// One subscription's status, from the aggregates of one
    /// [`Self::subscription_statuses`] call.
    fn status_for(
        &self,
        subscription: Subscription,
        now: DateTime<Utc>,
        aggregates: &StatusAggregates,
    ) -> Result<SubscriptionStatus, StorageError> {
        let quota = self
            .latest_quota(subscription)?
            .into_iter()
            .map(|snapshot| QuotaWindow {
                stale: is_stale(snapshot.observed_at, now),
                expired: snapshot.resets_at.is_some_and(|resets_at| resets_at < now),
                window: snapshot.window,
                used_pct: snapshot.used_pct,
                resets_at: snapshot.resets_at,
                source: snapshot.source,
                observed_at: snapshot.observed_at,
            })
            .collect();

        let sources = aggregates
            .source_updates
            .get(&subscription)
            .map(|updates| {
                updates
                    .iter()
                    .map(|(source, last_update)| SourceStatus {
                        collector_id: collector_id_for(*source).to_string(),
                        last_update: Some(*last_update),
                        stale: is_stale(*last_update, now),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let last_quota_hit = self
            .latest_quota_hit(subscription)?
            .map(|hit| QuotaHitStatus {
                period: hit.period,
                reset_at: hit.reset_at,
                observed_at: hit.observed_at,
            });

        Ok(SubscriptionStatus {
            subscription,
            quota,
            today: aggregates
                .today
                .get(&subscription)
                .cloned()
                .unwrap_or_else(TokenTotals::empty),
            last_7d: aggregates
                .week
                .get(&subscription)
                .cloned()
                .unwrap_or_else(TokenTotals::empty),
            by_tool: aggregates
                .week_tools
                .get(&subscription)
                .cloned()
                .unwrap_or_default(),
            last_quota_hit,
            sources,
        })
    }

    /// Token and cost totals per subscription between `start` (inclusive) and
    /// `end` (exclusive).
    fn window_totals(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<HashMap<Subscription, TokenTotals>, StorageError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT subscription,\n                        COALESCE(SUM(input_tokens), 0),\n                        COALESCE(SUM(output_tokens), 0),\n                        COALESCE(SUM(cache_read_tokens), 0),\n                        COALESCE(SUM(cache_write_tokens), 0),\n                        SUM(cost_usd),\n                        SUM(CASE WHEN cost_source = 'computed' THEN 1 ELSE 0 END),\n                        SUM(CASE WHEN cost_source = 'unknown' THEN 1 ELSE 0 END)\n                 FROM usage_records\n                 WHERE timestamp >= ?1 AND timestamp < ?2\n                 GROUP BY subscription",
            )
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let rows = stmt
            .query_map(params![start.to_rfc3339(), end.to_rfc3339()], |row| {
                let subscription_str: String = row.get(0)?;
                Ok((
                    subscription_str.parse().unwrap_or(Subscription::Other),
                    row_to_token_totals(row, 1)?,
                ))
            })
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let mut totals = HashMap::new();
        for row in rows {
            let (subscription, totals_for_window) =
                row.map_err(|e| StorageError::Query(e.to_string()))?;
            totals.insert(subscription, totals_for_window);
        }
        Ok(totals)
    }

    /// Per provider/source totals per subscription between `start` and `end`,
    /// ordered by provider and source inside each subscription.
    fn tool_breakdown(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<HashMap<Subscription, Vec<ToolBreakdown>>, StorageError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT subscription, provider, source,\n                        COALESCE(SUM(input_tokens), 0),\n                        COALESCE(SUM(output_tokens), 0),\n                        COALESCE(SUM(cache_read_tokens), 0),\n                        COALESCE(SUM(cache_write_tokens), 0),\n                        SUM(cost_usd),\n                        SUM(CASE WHEN cost_source = 'computed' THEN 1 ELSE 0 END),\n                        SUM(CASE WHEN cost_source = 'unknown' THEN 1 ELSE 0 END)\n                 FROM usage_records\n                 WHERE timestamp >= ?1 AND timestamp < ?2\n                 GROUP BY subscription, provider, source\n                 ORDER BY subscription, provider, source",
            )
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let rows = stmt
            .query_map(params![start.to_rfc3339(), end.to_rfc3339()], |row| {
                let subscription_str: String = row.get(0)?;
                let provider_str: String = row.get(1)?;
                let source_str: String = row.get(2)?;
                Ok((
                    subscription_str.parse().unwrap_or(Subscription::Other),
                    ToolBreakdown {
                        provider: provider_str.parse().unwrap_or(Provider::Other),
                        source: source_str.parse().unwrap_or(DataSource::Log),
                        totals: row_to_token_totals(row, 3)?,
                    },
                ))
            })
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let mut breakdown: HashMap<Subscription, Vec<ToolBreakdown>> = HashMap::new();
        for row in rows {
            let (subscription, tool) = row.map_err(|e| StorageError::Query(e.to_string()))?;
            breakdown.entry(subscription).or_default().push(tool);
        }
        Ok(breakdown)
    }

    /// The newest usage record and quota snapshot per source, per
    /// subscription, ordered by source. A source with nothing stored is not
    /// listed.
    fn source_updates(&self) -> Result<SourceUpdates, StorageError> {
        let mut newest: HashMap<(Subscription, DataSource), DateTime<Utc>> = HashMap::new();

        for sql in [
            "SELECT subscription, source, MAX(timestamp) FROM usage_records \
             GROUP BY subscription, source",
            "SELECT subscription, source, MAX(observed_at) FROM quota_snapshots \
             GROUP BY subscription, source",
        ] {
            let mut stmt = self
                .conn
                .prepare(sql)
                .map_err(|e| StorageError::Query(e.to_string()))?;
            let rows = stmt
                .query_map([], row_to_source_update)
                .map_err(|e| StorageError::Query(e.to_string()))?;
            for row in rows {
                let (subscription, source, at) =
                    row.map_err(|e| StorageError::Query(e.to_string()))?;
                newest
                    .entry((subscription, source))
                    .and_modify(|current| {
                        if at > *current {
                            *current = at;
                        }
                    })
                    .or_insert(at);
            }
        }

        let mut updates: SourceUpdates = HashMap::new();
        for ((subscription, source), at) in newest {
            updates.entry(subscription).or_default().push((source, at));
        }
        for sources in updates.values_mut() {
            sources.sort_by_key(|(source, _)| collector_id_for(*source));
        }
        Ok(updates)
    }

    pub fn get_model_stats(
        &self,
        params: &ModelsCompareParams,
    ) -> Result<ModelsCompareResponse, StorageError> {
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref tr) = params.time_range {
            where_clauses.push("timestamp >= ?".to_string());
            bind_values.push(Box::new(tr.start.to_rfc3339()));
            where_clauses.push("timestamp <= ?".to_string());
            bind_values.push(Box::new(tr.end.to_rfc3339()));
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let sql = format!(
            "SELECT model, \
             SUM(input_tokens) as total_input, \
             SUM(output_tokens) as total_output, \
             SUM(cost_usd) as total_cost, \
             COUNT(*) as req_count \
             FROM usage_records {where_sql} \
             GROUP BY model"
        );

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| StorageError::Query(e.to_string()))?;

        let models = stmt
            .query_map(bind_refs.as_slice(), |row| {
                let model_str: String = row.get(0)?;
                let total_input: i64 = row.get(1)?;
                let total_output: i64 = row.get(2)?;
                let total_cost: f64 = row.get(3)?;
                let request_count: i64 = row.get(4)?;

                let req_f = if request_count > 0 {
                    request_count as f64
                } else {
                    1.0
                };

                Ok(ModelStats {
                    model: model_str,
                    total_input_tokens: total_input as u64,
                    total_output_tokens: total_output as u64,
                    total_cost_usd: total_cost,
                    request_count: request_count as u64,
                    avg_input_per_request: total_input as f64 / req_f,
                    avg_output_per_request: total_output as f64 / req_f,
                    avg_cost_per_request: total_cost / req_f,
                })
            })
            .map_err(|e| StorageError::Query(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Query(e.to_string()))?;

        Ok(ModelsCompareResponse { models })
    }
}

/// A source with no update in this many hours is stale (SPEC Boundaries).
const STALE_AFTER_HOURS: i64 = 24;

/// Newest update per source, per subscription.
type SourceUpdates = HashMap<Subscription, Vec<(DataSource, DateTime<Utc>)>>;

/// The aggregates of one `subscription_statuses` call, computed once and then
/// read for each subscription.
struct StatusAggregates {
    today: HashMap<Subscription, TokenTotals>,
    week: HashMap<Subscription, TokenTotals>,
    week_tools: HashMap<Subscription, Vec<ToolBreakdown>>,
    /// Newest record or snapshot per source, per subscription.
    source_updates: SourceUpdates,
}

impl StatusAggregates {
    /// Whether anything at all was collected for a subscription.
    fn has_data(&self, subscription: Subscription) -> bool {
        self.today.contains_key(&subscription)
            || self.week.contains_key(&subscription)
            || self.week_tools.contains_key(&subscription)
            || self.source_updates.contains_key(&subscription)
    }
}

/// A source or quota window with no update in 24 hours is stale.
fn is_stale(observed_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(observed_at) > chrono::Duration::hours(STALE_AFTER_HOURS)
}

/// Midnight local time `offset_days` from `now`, as UTC. "Today" is the local
/// calendar day (SPEC AC-9), not the UTC one.
fn local_day_start(now: DateTime<Utc>, offset_days: i64) -> DateTime<Utc> {
    use chrono::{Local, TimeZone};

    let date = now.with_timezone(&Local).date_naive() + chrono::Duration::days(offset_days);
    date.and_hms_opt(0, 0, 0)
        .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
        .map(|start| start.with_timezone(&Utc))
        .unwrap_or(now)
}

/// The collector that writes each source, named in a status response.
fn collector_id_for(source: DataSource) -> &'static str {
    match source {
        DataSource::Log => crate::collectors::claude::COLLECTOR_ID,
        DataSource::ClaudeQuota => crate::collectors::claude_quota::COLLECTOR_ID,
        DataSource::CodexRollout => crate::collectors::codex::COLLECTOR_ID,
        DataSource::PiSession => crate::collectors::pi::COLLECTOR_ID,
        DataSource::OpencodeDb => crate::collectors::opencode::COLLECTOR_ID,
    }
}

/// Read the seven aggregate columns starting at `base`: the four token sums,
/// the cost sum, the computed count and the unknown count.
fn row_to_token_totals(row: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<TokenTotals> {
    let input: i64 = row.get(base)?;
    let output: i64 = row.get(base + 1)?;
    let cache_read: i64 = row.get(base + 2)?;
    let cache_write: i64 = row.get(base + 3)?;
    let cost_usd: Option<f64> = row.get(base + 4)?;
    let computed: i64 = row.get(base + 5)?;
    let unknown: i64 = row.get(base + 6)?;

    // SPEC AC-9 states the rules in this order: any computed part wins, then
    // any unknown part, otherwise every part was reported by the provider.
    let cost_source = if computed > 0 {
        CostSource::Computed
    } else if unknown > 0 {
        CostSource::Unknown
    } else {
        CostSource::Reported
    };

    Ok(TokenTotals {
        input: input.max(0) as u64,
        output: output.max(0) as u64,
        cache_read: cache_read.max(0) as u64,
        cache_write: cache_write.max(0) as u64,
        // Collectors fold reasoning into `output_tokens`, so it cannot be
        // counted again here; see `TokenTotals::reasoning`.
        reasoning: 0,
        cost_usd,
        cost_source,
    })
}

/// Read a `(subscription, source, newest timestamp)` aggregate row. An
/// unrecognised label becomes the unknown variant, as in
/// [`row_to_usage_record`], so one odd row cannot fail the whole response.
fn row_to_source_update(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(Subscription, DataSource, DateTime<Utc>)> {
    let subscription_str: String = row.get(0)?;
    let source_str: String = row.get(1)?;
    let timestamp: String = row.get(2)?;

    Ok((
        subscription_str.parse().unwrap_or(Subscription::Other),
        source_str.parse().unwrap_or(DataSource::Log),
        parse_rfc3339(&timestamp, 2)?,
    ))
}

fn insert_usage_row(conn: &Connection, record: &UsageRecord) -> Result<usize, rusqlite::Error> {
    let model = record.model.to_lowercase();
    conn.execute(
        INSERT_USAGE_SQL,
        params![
            record.uuid.to_string(),
            record.timestamp.to_rfc3339(),
            model,
            record.input_tokens as i64,
            record.output_tokens as i64,
            record.cache_read_tokens as i64,
            record.cache_write_tokens as i64,
            record.cost_usd,
            record.session_id,
            record.project,
            record.source.as_str(),
            record.provider.as_str(),
            record.subscription.as_str(),
            record.cost_source.as_str(),
            record.dedup_key,
        ],
    )
}

fn row_to_usage_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageRecord> {
    let uuid_str: String = row.get(1)?;
    let ts_str: String = row.get(2)?;
    let model = row.get::<_, String>(3)?;
    let source_str: String = row.get(11)?;
    let provider_str: String = row.get(12)?;
    let subscription_str: String = row.get(13)?;
    let cost_source_str: String = row.get(14)?;
    let dedup_key: String = row.get(15)?;

    let uuid = uuid_str.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad uuid: {e}"),
            )),
        )
    })?;
    let timestamp = parse_rfc3339(&ts_str, 2)?;

    Ok(UsageRecord {
        id: row.get(0)?,
        uuid,
        timestamp,
        model,
        // Rows written by the v1 schema carry no provider, subscription or
        // cost source; the v2 migration backfills them, and an unrecognised
        // value is read as unknown rather than failing the whole query.
        provider: provider_str.parse().unwrap_or(Provider::Other),
        subscription: subscription_str.parse().unwrap_or(Subscription::Other),
        input_tokens: row.get::<_, i64>(4)? as u64,
        output_tokens: row.get::<_, i64>(5)? as u64,
        cache_read_tokens: row.get::<_, i64>(6)? as u64,
        cache_write_tokens: row.get::<_, i64>(7)? as u64,
        cost_usd: row.get(8)?,
        cost_source: cost_source_str.parse().unwrap_or(CostSource::Unknown),
        dedup_key,
        session_id: row.get(9)?,
        project: row.get(10)?,
        // v1 wrote `api` for rows it did not otherwise label; those were
        // Claude Code log rows.
        source: source_str.parse().unwrap_or(DataSource::Log),
    })
}

#[allow(dead_code)]
fn row_to_quota_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<QuotaSnapshot> {
    let subscription_str: String = row.get(1)?;
    let source_str: String = row.get(5)?;
    let observed_at_str: String = row.get(6)?;
    let resets_at_str: Option<String> = row.get(4)?;

    Ok(QuotaSnapshot {
        id: row.get(0)?,
        subscription: parse_column(&subscription_str, 1)?,
        window: row.get(2)?,
        used_pct: row.get(3)?,
        resets_at: resets_at_str
            .as_deref()
            .map(|ts| parse_rfc3339(ts, 4))
            .transpose()?,
        source: parse_column(&source_str, 5)?,
        observed_at: parse_rfc3339(&observed_at_str, 6)?,
        plan_type: row.get(7)?,
    })
}

#[allow(dead_code)]
fn row_to_quota_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<QuotaHit> {
    let subscription_str: String = row.get(1)?;
    let observed_at_str: String = row.get(4)?;
    let reset_at_str: Option<String> = row.get(3)?;

    Ok(QuotaHit {
        id: row.get(0)?,
        subscription: parse_column(&subscription_str, 1)?,
        period: row.get(2)?,
        reset_at: reset_at_str
            .as_deref()
            .map(|ts| parse_rfc3339(ts, 3))
            .transpose()?,
        observed_at: parse_rfc3339(&observed_at_str, 4)?,
        raw_kind: row.get(5)?,
        dedup_key: row.get(6)?,
    })
}

/// Parse an RFC 3339 timestamp column, keeping the column number in the error.
fn parse_rfc3339(text: &str, column: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad timestamp: {e}"),
                )),
            )
        })
}

/// Parse an enum stored as its `as_str` form. Quota rows are always written by
/// this crate, so an unknown value is a corrupt row, not a legacy one.
#[allow(dead_code)]
fn parse_column<T>(text: &str, column: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    text.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActiveSession> {
    let model_str: String = row.get(1)?;
    let started_str: String = row.get(2)?;
    let activity_str: String = row.get(3)?;
    let status_str: String = row.get(10)?;

    let model: ModelType = model_str.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let started_at = chrono::DateTime::parse_from_rfc3339(&started_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad timestamp: {e}"),
                )),
            )
        })?;
    let last_activity = chrono::DateTime::parse_from_rfc3339(&activity_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad timestamp: {e}"),
                )),
            )
        })?;

    let status = match status_str.as_str() {
        "streaming" => SessionStatus::Streaming,
        "completed" => SessionStatus::Completed,
        _ => SessionStatus::Idle,
    };

    Ok(ActiveSession {
        session_id: row.get(0)?,
        model,
        started_at,
        last_activity,
        total_input_tokens: row.get::<_, i64>(4)? as u64,
        total_output_tokens: row.get::<_, i64>(5)? as u64,
        total_cache_read_tokens: row.get::<_, i64>(6)? as u64,
        total_cache_write_tokens: row.get::<_, i64>(7)? as u64,
        cost_usd: row.get(8)?,
        request_count: row.get::<_, i64>(9)? as u32,
        status,
        project: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn test_storage() -> Storage {
        Storage::new(":memory:").expect("failed to create in-memory storage")
    }

    fn all_usage() -> UsageQueryParams {
        UsageQueryParams {
            time_range: None,
            model: None,
            project: None,
            limit: 100,
            offset: 0,
        }
    }

    /// A throwaway SQLite file that never touches the real home directory.
    struct TempDb(PathBuf);

    impl TempDb {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("claude-tui-test-{}.db", Uuid::new_v4())))
        }

        fn path(&self) -> &str {
            self.0.to_str().expect("temp path is not UTF-8")
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let path = format!("{}{suffix}", self.0.display());
                if let Err(e) = std::fs::remove_file(&path)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("failed to remove test database {path}: {e}");
                }
            }
        }
    }

    /// The v1 schema exactly as the previous release created it.
    const V1_USAGE_DDL: &str = r#"
        CREATE TABLE usage_records (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid            TEXT NOT NULL UNIQUE,
            timestamp       TEXT NOT NULL,
            model           TEXT NOT NULL,
            input_tokens    INTEGER NOT NULL DEFAULT 0,
            output_tokens   INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
            cache_write_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd        REAL NOT NULL DEFAULT 0.0,
            session_id      TEXT,
            project         TEXT,
            source          TEXT NOT NULL DEFAULT 'api'
        );
        CREATE INDEX idx_usage_timestamp ON usage_records(timestamp);
        CREATE INDEX idx_usage_model_timestamp ON usage_records(model, timestamp);
        CREATE INDEX idx_usage_session ON usage_records(session_id) WHERE session_id IS NOT NULL;
        CREATE INDEX idx_usage_project ON usage_records(project) WHERE project IS NOT NULL;
        CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
        INSERT INTO schema_version (version) VALUES (1);
    "#;

    fn make_record(model: ModelType, input: u64, output: u64) -> UsageRecord {
        // Storage tests only need a plausible positive cost; real prices come
        // from the price config, which is covered in claude-common.
        let cost = (input + output) as f64 / 1_000.0;
        let uuid = Uuid::new_v4();
        UsageRecord {
            id: None,
            uuid,
            timestamp: Utc::now(),
            model: model.to_string().to_lowercase(),
            provider: Provider::Anthropic,
            subscription: Subscription::Claude,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: Some(cost),
            cost_source: CostSource::Computed,
            dedup_key: uuid.to_string(),
            session_id: Some("test-session".to_string()),
            project: Some("test-project".to_string()),
            source: DataSource::Log,
        }
    }

    #[test]
    fn test_migration_creates_tables() {
        let storage = test_storage();
        // Verify tables exist by querying them
        let count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM budget_config", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1); // seeded row
    }

    #[test]
    fn test_insert_and_query_usage() {
        let storage = test_storage();
        let record = make_record(ModelType::Sonnet, 1000, 500);
        storage.insert_usage(&record).unwrap();

        let result = storage
            .query_usage(&UsageQueryParams {
                time_range: None,
                model: None,
                project: None,
                limit: 100,
                offset: 0,
            })
            .unwrap();

        assert_eq!(result.total_count, 1);
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].model, "sonnet");
        assert_eq!(result.records[0].input_tokens, 1000);
    }

    #[test]
    fn test_deduplication_via_uuid() {
        let storage = test_storage();
        let record = make_record(ModelType::Opus, 2000, 1000);
        storage.insert_usage(&record).unwrap();
        // Insert same UUID again -- should be ignored
        storage.insert_usage(&record).unwrap();

        let result = storage
            .query_usage(&UsageQueryParams {
                time_range: None,
                model: None,
                project: None,
                limit: 100,
                offset: 0,
            })
            .unwrap();
        assert_eq!(result.total_count, 1);
    }

    #[test]
    fn test_insert_batch() {
        let mut storage = test_storage();
        let records = vec![
            make_record(ModelType::Sonnet, 100, 50),
            make_record(ModelType::Opus, 200, 100),
            make_record(ModelType::Haiku, 300, 150),
        ];
        storage.insert_usage_batch(&records).unwrap();

        let result = storage
            .query_usage(&UsageQueryParams {
                time_range: None,
                model: None,
                project: None,
                limit: 100,
                offset: 0,
            })
            .unwrap();
        assert_eq!(result.total_count, 3);
    }

    #[test]
    fn test_query_with_model_filter() {
        let storage = test_storage();
        storage
            .insert_usage(&make_record(ModelType::Sonnet, 100, 50))
            .unwrap();
        storage
            .insert_usage(&make_record(ModelType::Opus, 200, 100))
            .unwrap();

        let result = storage
            .query_usage(&UsageQueryParams {
                time_range: None,
                model: Some(ModelType::Sonnet),
                project: None,
                limit: 100,
                offset: 0,
            })
            .unwrap();
        assert_eq!(result.total_count, 1);
        assert_eq!(result.records[0].model, "sonnet");
    }

    #[test]
    fn test_budget_get_default() {
        let storage = test_storage();
        let budget = storage.get_budget().unwrap();
        assert_eq!(budget.daily_limit_usd, None);
        assert!((budget.alert_threshold_pct - 0.80).abs() < 1e-10);
    }

    #[test]
    fn test_budget_set_and_get() {
        let storage = test_storage();
        let config = BudgetConfig {
            daily_limit_usd: Some(5.0),
            weekly_limit_usd: Some(25.0),
            monthly_limit_usd: Some(100.0),
            alert_threshold_pct: 0.90,
        };
        storage.set_budget(&config).unwrap();

        let retrieved = storage.get_budget().unwrap();
        assert_eq!(retrieved.daily_limit_usd, Some(5.0));
        assert_eq!(retrieved.weekly_limit_usd, Some(25.0));
        assert_eq!(retrieved.monthly_limit_usd, Some(100.0));
        assert!((retrieved.alert_threshold_pct - 0.90).abs() < 1e-10);
    }

    #[test]
    fn test_session_upsert_and_get() {
        let storage = test_storage();
        let now = Utc::now();
        let session = ActiveSession {
            session_id: "sess-1".to_string(),
            model: ModelType::Sonnet,
            started_at: now,
            last_activity: now,
            total_input_tokens: 5000,
            total_output_tokens: 2000,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            cost_usd: 0.045,
            request_count: 3,
            status: SessionStatus::Streaming,
            project: Some("test-proj".to_string()),
        };

        storage.upsert_session(&session).unwrap();
        let retrieved = storage.get_session("sess-1").unwrap().unwrap();
        assert_eq!(retrieved.session_id, "sess-1");
        assert_eq!(retrieved.total_input_tokens, 5000);
        assert_eq!(retrieved.status, SessionStatus::Streaming);

        // Update the session
        let updated = ActiveSession {
            total_input_tokens: 8000,
            status: SessionStatus::Idle,
            ..session
        };
        storage.upsert_session(&updated).unwrap();
        let retrieved = storage.get_session("sess-1").unwrap().unwrap();
        assert_eq!(retrieved.total_input_tokens, 8000);
        assert_eq!(retrieved.status, SessionStatus::Idle);
    }

    #[test]
    fn test_session_not_found() {
        let storage = test_storage();
        assert!(storage.get_session("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_list_sessions() {
        let storage = test_storage();
        let now = Utc::now();

        for i in 0..3 {
            let session = ActiveSession {
                session_id: format!("sess-{i}"),
                model: ModelType::Sonnet,
                started_at: now,
                last_activity: now,
                total_input_tokens: 1000 * (i + 1) as u64,
                total_output_tokens: 500,
                total_cache_read_tokens: 0,
                total_cache_write_tokens: 0,
                cost_usd: 0.01,
                request_count: 1,
                status: if i == 0 {
                    SessionStatus::Streaming
                } else {
                    SessionStatus::Idle
                },
                project: None,
            };
            storage.upsert_session(&session).unwrap();
        }

        let all = storage
            .list_sessions(&SessionsListParams {
                status: None,
                limit: 100,
                offset: 0,
            })
            .unwrap();
        assert_eq!(all.total_count, 3);

        let streaming = storage
            .list_sessions(&SessionsListParams {
                status: Some(SessionStatus::Streaming),
                limit: 100,
                offset: 0,
            })
            .unwrap();
        assert_eq!(streaming.total_count, 1);
    }

    #[test]
    fn test_get_cost_today() {
        let storage = test_storage();
        let record = make_record(ModelType::Sonnet, 1_000_000, 500_000);
        storage.insert_usage(&record).unwrap();

        let cost = storage.get_cost_today().unwrap();
        assert!(cost > 0.0);
    }

    #[test]
    fn test_get_model_stats() {
        let storage = test_storage();
        storage
            .insert_usage(&make_record(ModelType::Sonnet, 1000, 500))
            .unwrap();
        storage
            .insert_usage(&make_record(ModelType::Sonnet, 2000, 1000))
            .unwrap();
        storage
            .insert_usage(&make_record(ModelType::Opus, 500, 250))
            .unwrap();

        let stats = storage
            .get_model_stats(&ModelsCompareParams { time_range: None })
            .unwrap();
        assert_eq!(stats.models.len(), 2);

        let sonnet_stats = stats.models.iter().find(|m| m.model == "sonnet").unwrap();
        assert_eq!(sonnet_stats.request_count, 2);
        assert_eq!(sonnet_stats.total_input_tokens, 3000);
    }

    #[test]
    fn test_get_summary() {
        let storage = test_storage();
        storage
            .insert_usage(&make_record(ModelType::Sonnet, 1000, 500))
            .unwrap();
        storage
            .insert_usage(&make_record(ModelType::Opus, 2000, 1000))
            .unwrap();

        let summary = storage
            .get_summary(&UsageSummaryParams {
                window: claude_common::TimeWindow::Day,
                time_range: None,
                model: None,
            })
            .unwrap();

        assert_eq!(summary.total_requests, 2);
        assert!(summary.total_cost_usd > 0.0);
        assert_eq!(summary.total_input_tokens, 3000);
        assert_eq!(summary.total_output_tokens, 1500);
    }

    #[test]
    fn migrate_v2_upgrades_v1_database_without_row_loss() {
        let db = TempDb::new();
        let sonnet_uuid = Uuid::new_v4();
        let opus_uuid = Uuid::new_v4();

        {
            let conn = Connection::open(db.path()).unwrap();
            conn.execute_batch(V1_USAGE_DDL).unwrap();
            for (uuid, model, cost) in [
                (sonnet_uuid, "claude-sonnet-5-5", 0.25_f64),
                (opus_uuid, "claude-opus-5-5", 1.5_f64),
            ] {
                conn.execute(
                    "INSERT INTO usage_records (uuid, timestamp, model, input_tokens, output_tokens, cost_usd, session_id, project, source) \
                     VALUES (?1, ?2, ?3, 100, 50, ?4, 'sess-1', 'proj-1', 'log')",
                    params![uuid.to_string(), Utc::now().to_rfc3339(), model, cost],
                )
                .unwrap();
            }
        }

        let storage = Storage::new(db.path()).unwrap();

        let result = storage.query_usage(&all_usage()).unwrap();
        assert_eq!(result.total_count, 2);
        assert_eq!(result.records.len(), 2);
        for record in &result.records {
            assert_eq!(record.provider, Provider::Anthropic);
            assert_eq!(record.subscription, Subscription::Claude);
            assert_eq!(record.cost_source, CostSource::Computed);
            assert_eq!(record.dedup_key, record.uuid.to_string());
        }

        let version: i64 = storage
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);

        let cost: Option<f64> = storage
            .conn
            .query_row(
                "SELECT cost_usd FROM usage_records WHERE uuid = ?1",
                params![opus_uuid.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cost, Some(1.5));
    }

    #[test]
    fn migrate_v2_runs_twice_as_a_noop() {
        let mut storage = test_storage();
        storage
            .insert_usage(&make_record(ModelType::Sonnet, 10, 5))
            .unwrap();

        storage.migrate().unwrap();
        storage.migrate().unwrap();

        let version: i64 = storage
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);

        let versions: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 2);

        let result = storage.query_usage(&all_usage()).unwrap();
        assert_eq!(result.total_count, 1);
    }

    #[test]
    fn migrate_v2_null_cost_round_trips() {
        let storage = test_storage();
        let mut record = make_record(ModelType::Sonnet, 100, 50);
        record.model = "gpt-6.1-sol".to_string();
        record.provider = Provider::OpenAiCodex;
        record.subscription = Subscription::ChatGpt;
        record.cost_usd = None;
        record.cost_source = CostSource::Unknown;
        record.source = DataSource::CodexRollout;
        record.dedup_key = "response-1".to_string();
        storage.insert_usage(&record).unwrap();

        let result = storage.query_usage(&all_usage()).unwrap();
        assert_eq!(result.total_count, 1);
        let stored = &result.records[0];
        assert_eq!(stored.cost_usd, None);
        assert_eq!(stored.cost_source, CostSource::Unknown);
        assert_eq!(stored.provider, Provider::OpenAiCodex);
        assert_eq!(stored.subscription, Subscription::ChatGpt);
        assert_eq!(stored.source, DataSource::CodexRollout);
        assert_eq!(stored.dedup_key, "response-1");
        assert_eq!(stored.model, "gpt-6.1-sol");
    }

    #[test]
    fn migrate_v2_dedup_key_is_unique_per_provider() {
        let mut storage = test_storage();

        let mut anthropic = make_record(ModelType::Sonnet, 100, 50);
        anthropic.dedup_key = "shared-key".to_string();
        let mut duplicate = anthropic.clone();
        duplicate.uuid = Uuid::new_v4();
        let mut codex = duplicate.clone();
        codex.uuid = Uuid::new_v4();
        codex.provider = Provider::OpenAiCodex;
        codex.subscription = Subscription::ChatGpt;
        codex.source = DataSource::CodexRollout;

        storage
            .insert_usage_batch(&[anthropic, duplicate, codex])
            .unwrap();

        let result = storage.query_usage(&all_usage()).unwrap();
        assert_eq!(result.total_count, 2);
    }

    #[test]
    fn migrate_v2_quota_snapshot_keeps_latest_per_window() {
        let storage = test_storage();
        let now = Utc::now();

        let snapshot = |subscription: Subscription,
                        window: &str,
                        used_pct: f64,
                        observed_at: DateTime<Utc>,
                        plan_type: Option<&str>| QuotaSnapshot {
            id: None,
            subscription,
            window: window.to_string(),
            used_pct: Some(used_pct),
            resets_at: Some(observed_at + chrono::Duration::hours(1)),
            source: DataSource::CodexRollout,
            observed_at,
            plan_type: plan_type.map(str::to_string),
        };

        storage
            .insert_quota_snapshot(&snapshot(
                Subscription::ChatGpt,
                "primary",
                10.0,
                now - chrono::Duration::minutes(5),
                Some("plus"),
            ))
            .unwrap();
        storage
            .insert_quota_snapshot(&snapshot(
                Subscription::ChatGpt,
                "primary",
                42.0,
                now,
                Some("plus"),
            ))
            .unwrap();
        storage
            .insert_quota_snapshot(&snapshot(
                Subscription::ChatGpt,
                "secondary",
                7.0,
                now,
                None,
            ))
            .unwrap();
        storage
            .insert_quota_snapshot(&snapshot(
                Subscription::Claude,
                "five_hour",
                99.0,
                now,
                None,
            ))
            .unwrap();

        let chatgpt = storage.latest_quota(Subscription::ChatGpt).unwrap();
        assert_eq!(chatgpt.len(), 2);
        let primary = chatgpt
            .iter()
            .find(|q| q.window == "primary")
            .expect("primary window missing");
        assert_eq!(primary.used_pct, Some(42.0));
        assert_eq!(primary.plan_type.as_deref(), Some("plus"));
        assert_eq!(primary.source, DataSource::CodexRollout);
        assert_eq!(primary.observed_at, now);

        let claude = storage.latest_quota(Subscription::Claude).unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].window, "five_hour");
        assert_eq!(claude[0].used_pct, Some(99.0));

        assert!(
            storage
                .latest_quota(Subscription::OpencodeGo)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn migrate_v2_quota_hit_dedup_and_latest() {
        let storage = test_storage();
        let now = Utc::now();

        let hit = |dedup_key: &str, observed_at: DateTime<Utc>, period: Option<&str>| QuotaHit {
            id: None,
            subscription: Subscription::OpencodeGo,
            period: period.map(str::to_string),
            reset_at: Some(observed_at + chrono::Duration::hours(5)),
            observed_at,
            raw_kind: "go_usage_limit".to_string(),
            dedup_key: dedup_key.to_string(),
        };

        storage
            .insert_quota_hit(&hit(
                "entry-a",
                now - chrono::Duration::hours(1),
                Some("5h"),
            ))
            .unwrap();
        // Same dedup key: ignored, even though the observation time differs.
        storage
            .insert_quota_hit(&hit("entry-a", now, Some("5h")))
            .unwrap();
        storage
            .insert_quota_hit(&hit("entry-b", now, None))
            .unwrap();

        let latest = storage
            .latest_quota_hit(Subscription::OpencodeGo)
            .unwrap()
            .expect("quota hit missing");
        assert_eq!(latest.dedup_key, "entry-b");
        assert_eq!(latest.period, None);
        assert_eq!(latest.raw_kind, "go_usage_limit");
        assert_eq!(latest.subscription, Subscription::OpencodeGo);
        assert!(latest.reset_at.is_some());

        assert!(
            storage
                .latest_quota_hit(Subscription::Claude)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn status_rpc_subscriptions_cost_source_rules() {
        let mut storage = test_storage();
        let now = Utc::now();

        let mut computed = make_record(ModelType::Sonnet, 100, 10);
        computed.cost_usd = Some(0.5);
        computed.cost_source = CostSource::Computed;

        // Same subscription, one record with no configured price: the rule the
        // SPEC states first (any computed part) wins, and the sum covers the
        // known parts only.
        let mut unpriced = make_record(ModelType::Haiku, 7, 3);
        unpriced.uuid = Uuid::new_v4();
        unpriced.dedup_key = unpriced.uuid.to_string();
        unpriced.cost_usd = None;
        unpriced.cost_source = CostSource::Unknown;

        let mut reported = make_record(ModelType::Sonnet, 20, 2);
        reported.uuid = Uuid::new_v4();
        reported.dedup_key = reported.uuid.to_string();
        reported.provider = Provider::OpenAiCodex;
        reported.subscription = Subscription::ChatGpt;
        reported.source = DataSource::CodexRollout;
        reported.cost_usd = Some(0.25);
        reported.cost_source = CostSource::Reported;

        let mut all_unknown = make_record(ModelType::Opus, 5, 1);
        all_unknown.uuid = Uuid::new_v4();
        all_unknown.dedup_key = all_unknown.uuid.to_string();
        all_unknown.provider = Provider::OpencodeGo;
        all_unknown.subscription = Subscription::OpencodeGo;
        all_unknown.source = DataSource::OpencodeDb;
        all_unknown.cost_usd = None;
        all_unknown.cost_source = CostSource::Unknown;

        storage
            .insert_usage_batch(&[computed, unpriced, reported, all_unknown])
            .unwrap();

        let statuses = storage.subscription_statuses(now).unwrap();
        assert_eq!(statuses.len(), 3, "no `other` card without `other` data");

        let claude = &statuses[0];
        assert_eq!(claude.subscription, Subscription::Claude);
        assert_eq!(claude.today.cost_source, CostSource::Computed);
        assert_eq!(claude.today.cost_usd, Some(0.5));
        assert_eq!(claude.today.input, 107);

        let chatgpt = &statuses[1];
        assert_eq!(chatgpt.today.cost_source, CostSource::Reported);
        assert_eq!(chatgpt.today.cost_usd, Some(0.25));

        let opencode = &statuses[2];
        assert_eq!(opencode.today.cost_source, CostSource::Unknown);
        assert_eq!(opencode.today.cost_usd, None);
    }

    #[test]
    fn status_rpc_subscriptions_today_is_the_local_day() {
        let now = Utc::now();
        let today_start = local_day_start(now, 0);
        let tomorrow_start = local_day_start(now, 1);
        assert!(today_start <= now, "today starts at or before now");
        assert!(tomorrow_start > now, "tomorrow starts after now");
        let week_start = local_day_start(now, -6);
        assert_eq!(
            today_start.signed_duration_since(week_start),
            chrono::Duration::days(6)
        );
        assert_eq!(
            today_start.with_timezone(&chrono::Local).time(),
            chrono::NaiveTime::MIN
        );
    }

    #[test]
    fn status_rpc_subscriptions_stale_after_24h() {
        let now = Utc::now();
        assert!(!is_stale(now - chrono::Duration::hours(23), now));
        assert!(is_stale(now - chrono::Duration::hours(25), now));
    }

    #[test]
    fn migrate_v2_cursor_round_trip() {
        let storage = test_storage();

        assert_eq!(
            storage
                .get_cursor("codex-rollouts", "file:/a.jsonl")
                .unwrap(),
            None
        );

        storage
            .set_cursor("codex-rollouts", "file:/a.jsonl", "128")
            .unwrap();
        assert_eq!(
            storage
                .get_cursor("codex-rollouts", "file:/a.jsonl")
                .unwrap(),
            Some("128".to_string())
        );

        storage
            .set_cursor("codex-rollouts", "file:/a.jsonl", "256")
            .unwrap();
        assert_eq!(
            storage
                .get_cursor("codex-rollouts", "file:/a.jsonl")
                .unwrap(),
            Some("256".to_string())
        );
        assert_eq!(
            storage.get_cursor("pi-sessions", "file:/a.jsonl").unwrap(),
            None
        );
    }
}
