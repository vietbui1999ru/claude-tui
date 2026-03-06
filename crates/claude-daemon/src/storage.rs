use std::path::Path;

use chrono::{NaiveDate, Utc};
use claude_common::{
    ActiveSession, BudgetConfig, DailyAggregate, DataSource, ModelType, SessionStatus,
    UsageRecord,
};
use claude_common::protocol::{
    ModelsCompareParams, ModelsCompareResponse, ModelStats, SessionsListParams,
    SessionsListResponse, UsageQueryParams, UsageQueryResponse, UsageSummaryParams,
    UsageSummaryResponse,
};
use rusqlite::{params, Connection};

use crate::StorageError;

pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open or create a SQLite database at the given path.
    /// For in-memory databases, pass ":memory:".
    pub fn new(db_path: &str) -> Result<Self, StorageError> {
        if db_path != ":memory:" {
            if let Some(parent) = Path::new(db_path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| StorageError::Sqlite(e.to_string()))?;
            }
        }

        let conn =
            Connection::open(db_path).map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    /// Run database migrations to bring the schema up to date.
    pub fn migrate(&self) -> Result<(), StorageError> {
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

        self.conn.execute_batch(sql).map_err(|e| StorageError::Migration {
            version: 1,
            reason: e.to_string(),
        })?;

        Ok(())
    }

    // --- Usage Records ---

    pub fn insert_usage(&self, record: &UsageRecord) -> Result<(), StorageError> {
        let model_str = record.model.to_string().to_lowercase();
        let source_str = match record.source {
            DataSource::Api => "api",
            DataSource::Log => "log",
        };

        self.conn
            .execute(
                "INSERT OR IGNORE INTO usage_records (uuid, timestamp, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd, session_id, project, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    record.uuid.to_string(),
                    record.timestamp.to_rfc3339(),
                    model_str,
                    record.input_tokens as i64,
                    record.output_tokens as i64,
                    record.cache_read_tokens as i64,
                    record.cache_write_tokens as i64,
                    record.cost_usd,
                    record.session_id,
                    record.project,
                    source_str,
                ],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        Ok(())
    }

    pub fn insert_usage_batch(&mut self, records: &[UsageRecord]) -> Result<(), StorageError> {
        let tx = self.conn.transaction().map_err(|e| StorageError::Sqlite(e.to_string()))?;
        for record in records {
            let model_str = record.model.to_string().to_lowercase();
            let source_str = match record.source {
                DataSource::Api => "api",
                DataSource::Log => "log",
            };
            tx.execute(
                "INSERT OR IGNORE INTO usage_records (uuid, timestamp, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd, session_id, project, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    record.uuid.to_string(),
                    record.timestamp.to_rfc3339(),
                    model_str,
                    record.input_tokens as i64,
                    record.output_tokens as i64,
                    record.cache_read_tokens as i64,
                    record.cache_write_tokens as i64,
                    record.cost_usd,
                    record.session_id,
                    record.project,
                    source_str,
                ],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;
        }
        tx.commit().map_err(|e| StorageError::Sqlite(e.to_string()))?;
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
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
        let total_count: u64 = self
            .conn
            .query_row(&count_sql, bind_refs.as_slice(), |row| row.get(0))
            .map_err(|e| StorageError::Query(e.to_string()))?;

        // Get records
        let query_sql = format!(
            "SELECT id, uuid, timestamp, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd, session_id, project, source FROM usage_records {where_sql} ORDER BY timestamp DESC LIMIT ? OFFSET ?"
        );
        let mut all_binds: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
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

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
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
                    model: model_str
                        .parse()
                        .unwrap_or(ModelType::Sonnet),
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

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
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
                    model: model_str.parse().unwrap_or(ModelType::Sonnet),
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

    pub fn recompute_daily_aggregate(&self, date: NaiveDate) -> Result<(), StorageError> {
        let date_str = date.to_string();
        let date_next = date.succ_opt().unwrap_or(date).to_string();

        // Delete existing aggregates for this date
        self.conn
            .execute("DELETE FROM daily_aggregates WHERE date = ?1", params![date_str])
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

    // --- Sessions ---

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
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
        let total_count: u64 = self
            .conn
            .query_row(&count_sql, bind_refs.as_slice(), |row| row.get(0))
            .map_err(|e| StorageError::Query(e.to_string()))?;

        // Query
        let query_sql = format!(
            "SELECT session_id, model, started_at, last_activity, total_input_tokens, total_output_tokens, total_cache_read_tokens, total_cache_write_tokens, cost_usd, request_count, status, project FROM sessions {where_sql} ORDER BY last_activity DESC LIMIT ? OFFSET ?"
        );
        let mut all_binds: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
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
            .execute_batch(
                "DELETE FROM sessions;"
            )
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
        if let Some(limit) = config.daily_limit_usd {
            if limit < 0.0 {
                return Err(StorageError::Query("daily_limit_usd must be >= 0".to_string()));
            }
        }
        if let Some(limit) = config.weekly_limit_usd {
            if limit < 0.0 {
                return Err(StorageError::Query("weekly_limit_usd must be >= 0".to_string()));
            }
        }
        if let Some(limit) = config.monthly_limit_usd {
            if limit < 0.0 {
                return Err(StorageError::Query("monthly_limit_usd must be >= 0".to_string()));
            }
        }
        if !(0.0..=1.0).contains(&config.alert_threshold_pct) {
            return Err(StorageError::Query("alert_threshold_pct must be in 0.0..=1.0".to_string()));
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
        let today_start = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339();
        let tomorrow_start = (Utc::now().date_naive() + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339();
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

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
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
                    model: model_str.parse().unwrap_or(ModelType::Sonnet),
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

fn row_to_usage_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageRecord> {
    let uuid_str: String = row.get(1)?;
    let ts_str: String = row.get(2)?;
    let model_str: String = row.get(3)?;
    let source_str: String = row.get(11)?;

    let uuid = uuid_str.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad uuid: {e}"))
        ))
    })?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad timestamp: {e}"))
            ))
        })?;
    let model: ModelType = model_str.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        ))
    })?;

    Ok(UsageRecord {
        id: row.get(0)?,
        uuid,
        timestamp,
        model,
        input_tokens: row.get::<_, i64>(4)? as u64,
        output_tokens: row.get::<_, i64>(5)? as u64,
        cache_read_tokens: row.get::<_, i64>(6)? as u64,
        cache_write_tokens: row.get::<_, i64>(7)? as u64,
        cost_usd: row.get(8)?,
        session_id: row.get(9)?,
        project: row.get(10)?,
        source: if source_str == "log" {
            DataSource::Log
        } else {
            DataSource::Api
        },
    })
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActiveSession> {
    let model_str: String = row.get(1)?;
    let started_str: String = row.get(2)?;
    let activity_str: String = row.get(3)?;
    let status_str: String = row.get(10)?;

    let model: ModelType = model_str.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        ))
    })?;
    let started_at = chrono::DateTime::parse_from_rfc3339(&started_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad timestamp: {e}"))
            ))
        })?;
    let last_activity = chrono::DateTime::parse_from_rfc3339(&activity_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad timestamp: {e}"))
            ))
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
    use chrono::Utc;
    use uuid::Uuid;

    fn test_storage() -> Storage {
        Storage::new(":memory:").expect("failed to create in-memory storage")
    }

    fn make_record(model: ModelType, input: u64, output: u64) -> UsageRecord {
        let cost = model.compute_cost(input, output, 0, 0);
        UsageRecord {
            id: None,
            uuid: Uuid::new_v4(),
            timestamp: Utc::now(),
            model,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: cost,
            session_id: Some("test-session".to_string()),
            project: Some("test-project".to_string()),
            source: DataSource::Api,
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
        assert_eq!(result.records[0].model, ModelType::Sonnet);
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
        storage.insert_usage(&make_record(ModelType::Sonnet, 100, 50)).unwrap();
        storage.insert_usage(&make_record(ModelType::Opus, 200, 100)).unwrap();

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
        assert_eq!(result.records[0].model, ModelType::Sonnet);
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
        storage.insert_usage(&make_record(ModelType::Sonnet, 1000, 500)).unwrap();
        storage.insert_usage(&make_record(ModelType::Sonnet, 2000, 1000)).unwrap();
        storage.insert_usage(&make_record(ModelType::Opus, 500, 250)).unwrap();

        let stats = storage
            .get_model_stats(&ModelsCompareParams { time_range: None })
            .unwrap();
        assert_eq!(stats.models.len(), 2);

        let sonnet_stats = stats.models.iter().find(|m| m.model == ModelType::Sonnet).unwrap();
        assert_eq!(sonnet_stats.request_count, 2);
        assert_eq!(sonnet_stats.total_input_tokens, 3000);
    }

    #[test]
    fn test_get_summary() {
        let storage = test_storage();
        storage.insert_usage(&make_record(ModelType::Sonnet, 1000, 500)).unwrap();
        storage.insert_usage(&make_record(ModelType::Opus, 2000, 1000)).unwrap();

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
}
