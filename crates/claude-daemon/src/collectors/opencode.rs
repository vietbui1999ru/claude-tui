//! OpenCode's SQLite database (`~/.local/share/opencode/opencode.db`).
//!
//! The database is opened read-only (`?mode=ro`). Per-message usage comes from
//! assistant rows of `session_message` and, for older rows, the legacy
//! `message` table. The legacy `session` table is not used: it stopped
//! recording cost and tokens around OpenCode 1.18.
//!
//! Only the message tables are queried. The `credential`, `account` and
//! `control_account` tables hold secrets and are never selected from.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use claude_common::{
    CollectorError, CostSource, DataSource, Provider, UsageRecord, subscription_for,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use super::{CollectOutput, Cursor, SourceCollector};

/// Stable collector id, also the cursor namespace.
pub const COLLECTOR_ID: &str = "opencode-db";

/// Namespace for deterministic record ids. Distinct from the other collectors.
const OPENCODE_UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x5c, 0x27, 0xe1, 0x8a, 0x03, 0xbd, 0x4f, 0x61, 0x9e, 0x40, 0x7b, 0x12, 0xa8, 0xd5, 0x6e, 0x09,
]);

/// The cursor key for each table: the largest `time_created` (ms) read so far.
const CURSOR_SESSION_MESSAGE: &str = "session_message.time_created";
const CURSOR_LEGACY_MESSAGE: &str = "message.time_created";

pub struct OpencodeDbCollector {
    db_path: PathBuf,
}

impl OpencodeDbCollector {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

impl SourceCollector for OpencodeDbCollector {
    fn id(&self) -> &'static str {
        COLLECTOR_ID
    }

    fn collect(&mut self, cursor: &Cursor) -> Result<CollectOutput, CollectorError> {
        if !self.db_path.exists() {
            return Ok(CollectOutput::default());
        }
        let conn = open_read_only(&self.db_path)?;
        let mut out = CollectOutput::default();

        let since_new = cursor.get_u64(CURSOR_SESSION_MESSAGE) as i64;
        let since_legacy = cursor.get_u64(CURSOR_LEGACY_MESSAGE) as i64;

        let max_new = read_session_messages(&conn, since_new, &mut out)?;
        let max_legacy = read_legacy_messages(&conn, since_legacy, &mut out)?;

        out.cursor
            .set_u64(CURSOR_SESSION_MESSAGE, max_new.max(since_new) as u64);
        out.cursor
            .set_u64(CURSOR_LEGACY_MESSAGE, max_legacy.max(since_legacy) as u64);
        Ok(out)
    }
}

/// Open the database read-only. A read-only URI means SQLite never writes,
/// even if the collector is wrong about the schema.
fn open_read_only(path: &std::path::Path) -> Result<Connection, CollectorError> {
    let uri = format!("file:{}?mode=ro", path.display());
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    Connection::open_with_flags(uri, flags).map_err(|e| CollectorError::LogWatch(e.to_string()))
}

/// Read assistant rows of `session_message`. Returns the largest `time_created`
/// seen, so the caller can advance the cursor.
fn read_session_messages(
    conn: &Connection,
    since: i64,
    out: &mut CollectOutput,
) -> Result<i64, CollectorError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, type, data FROM session_message \
             WHERE type = 'assistant' AND json_extract(data, '$.time.created') > ?1 \
             ORDER BY json_extract(data, '$.time.created'), id",
        )
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    let rows = stmt
        .query_map([since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    let mut max_seen = since;
    for row in rows {
        let (id, session_id, data) = row.map_err(|e| CollectorError::LogWatch(e.to_string()))?;
        let Some(parsed) = parse_data(&data, &id, Some(&session_id), ModelShape::New) else {
            continue;
        };
        max_seen = max_seen.max(parsed.created_ms);
        push_record(out, parsed.into_record());
    }
    Ok(max_seen)
}

/// Read the legacy `message` table for rows older than `session_message` supports.
fn read_legacy_messages(
    conn: &Connection,
    since: i64,
    out: &mut CollectOutput,
) -> Result<i64, CollectorError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, time_created, data FROM message \
             WHERE time_created > ?1 ORDER BY time_created, id",
        )
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    let rows = stmt
        .query_map([since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    let mut max_seen = since;
    for row in rows {
        let (id, session_id, time_created, data) =
            row.map_err(|e| CollectorError::LogWatch(e.to_string()))?;
        // The cursor passes every row read, including rows that carry no usage.
        max_seen = max_seen.max(time_created);
        if let Some(parsed) = parse_data(&data, &id, Some(&session_id), ModelShape::Legacy) {
            push_record(out, parsed.into_record());
        }
    }
    Ok(max_seen)
}

/// Which JSON layout a row uses. `session_message` nests the model under
/// `model.{id,providerID}`; the legacy `message` table uses flat `modelID`
/// and `providerID`.
#[derive(Clone, Copy)]
enum ModelShape {
    New,
    Legacy,
}

/// One parsed OpenCode message, before it becomes a [`UsageRecord`].
struct Parsed {
    id: String,
    session_id: Option<String>,
    created_ms: i64,
    model: String,
    provider_name: String,
    tokens: Tokens,
    cost: Option<f64>,
}

#[derive(Default, Clone, Copy)]
struct Tokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Parsed {
    fn into_record(self) -> UsageRecord {
        let provider: Provider = self.provider_name.parse().unwrap_or(Provider::Other);
        let output = self.tokens.output + self.tokens.reasoning;
        let (cost_usd, cost_source) = match self.cost {
            Some(c) => (Some(c), CostSource::Reported),
            None => (None, CostSource::Unknown),
        };
        let timestamp = Utc
            .timestamp_millis_opt(self.created_ms)
            .single()
            .unwrap_or_else(Utc::now);
        UsageRecord {
            id: None,
            uuid: Uuid::new_v5(
                &OPENCODE_UUID_NAMESPACE,
                format!("{provider}:{}", self.id).as_bytes(),
            ),
            timestamp,
            model: self.model,
            provider,
            subscription: subscription_for(DataSource::OpencodeDb, provider),
            input_tokens: self.tokens.input,
            output_tokens: output,
            cache_read_tokens: self.tokens.cache_read,
            cache_write_tokens: self.tokens.cache_write,
            cost_usd,
            cost_source,
            dedup_key: self.id,
            session_id: self.session_id,
            project: None,
            source: DataSource::OpencodeDb,
        }
    }
}

/// Parse the JSON `data` column of one row. Returns `None` for rows with no
/// tokens, which are not usage.
fn parse_data(data: &str, id: &str, session_id: Option<&str>, shape: ModelShape) -> Option<Parsed> {
    let value: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            warn!("skipping OpenCode row {id}: bad JSON ({e})");
            return None;
        }
    };

    let (model, provider_name, created_ms) = match shape {
        ModelShape::New => {
            let model = value.get("model")?;
            (
                model.get("id").and_then(Value::as_str)?.to_string(),
                model
                    .get("providerID")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                value
                    .get("time")
                    .and_then(|t| t.get("created"))
                    .and_then(Value::as_i64)?,
            )
        }
        ModelShape::Legacy => (
            value.get("modelID").and_then(Value::as_str)?.to_string(),
            value
                .get("providerID")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value
                .get("time")
                .and_then(|t| t.get("created"))
                .and_then(Value::as_i64)?,
        ),
    };

    let tokens = value.get("tokens")?;
    let field = |name: &str| tokens.get(name).and_then(Value::as_u64).unwrap_or(0);
    let cache = tokens.get("cache");
    let cache_field = |name: &str| {
        cache
            .and_then(|c| c.get(name))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let parsed_tokens = Tokens {
        input: field("input"),
        output: field("output"),
        reasoning: field("reasoning"),
        cache_read: cache_field("read"),
        cache_write: cache_field("write"),
    };
    if parsed_tokens.input == 0
        && parsed_tokens.output == 0
        && parsed_tokens.reasoning == 0
        && parsed_tokens.cache_read == 0
        && parsed_tokens.cache_write == 0
    {
        return None;
    }

    // `cost` may be an integer or a real number; both are USD.
    let cost = value.get("cost").and_then(|c| {
        c.as_f64()
            .or_else(|| c.as_i64().map(|i| i as f64))
            .or_else(|| c.as_u64().map(|u| u as f64))
    });

    Some(Parsed {
        id: id.to_string(),
        session_id: session_id.map(str::to_string),
        created_ms,
        model: model.clone(),
        provider_name,
        tokens: parsed_tokens,
        cost,
    })
}

/// Add a record unless this pass already has its dedup key. The same message can
/// appear in both tables after a migration; storage keeps one copy.
fn push_record(out: &mut CollectOutput, record: UsageRecord) {
    if out
        .records
        .iter()
        .any(|r| r.provider == record.provider && r.dedup_key == record.dedup_key)
    {
        return;
    }
    out.records.push(record);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp OpenCode-shaped database. It has the message tables and the
    /// secret tables the collector must never read.
    fn make_db() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-tui-oc-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, data TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE session (id TEXT PRIMARY KEY, cost REAL);
             CREATE TABLE credential (id TEXT PRIMARY KEY, token TEXT);
             CREATE TABLE account (id TEXT PRIMARY KEY, email TEXT);
             CREATE TABLE control_account (id TEXT PRIMARY KEY, secret TEXT);

             INSERT INTO session_message VALUES ('sm-1', 'ses-1', 'assistant',
               '{\"cost\":0.0042,\"tokens\":{\"input\":1000,\"output\":200,\"reasoning\":10,\"cache\":{\"read\":300,\"write\":50}},\"model\":{\"id\":\"kimi-k2.6\",\"providerID\":\"opencode-go\"},\"time\":{\"created\":1772704860000}}');
             INSERT INTO session_message VALUES ('sm-2', 'ses-1', 'assistant',
               '{\"cost\":1,\"tokens\":{\"input\":10,\"output\":5,\"cache\":{\"read\":0,\"write\":0}},\"model\":{\"id\":\"glm-5\",\"providerID\":\"opencode-go\"},\"time\":{\"created\":1772704920000}}');
             INSERT INTO session_message VALUES ('sm-user', 'ses-1', 'user',
               '{\"tokens\":{\"input\":5},\"model\":{\"id\":\"kimi-k2.6\",\"providerID\":\"opencode-go\"},\"time\":{\"created\":1772704800000}}');

             INSERT INTO message VALUES ('legacy-1', 'ses-old', 1700000000000,
               '{\"role\":\"assistant\",\"cost\":0.02,\"tokens\":{\"input\":400,\"output\":80,\"cache\":{\"read\":10,\"write\":0}},\"modelID\":\"kimi-k2\",\"providerID\":\"opencode-go\",\"time\":{\"created\":1700000000000}}');
             INSERT INTO message VALUES ('legacy-zero', 'ses-old', 1700000001000,
               '{\"role\":\"assistant\",\"cost\":0,\"tokens\":{\"input\":0,\"output\":0},\"modelID\":\"kimi-k2\",\"providerID\":\"opencode-go\",\"time\":{\"created\":1700000001000}}');

             INSERT INTO session VALUES ('ses-1', 99.0);
             INSERT INTO credential VALUES ('c1', 'SECRET-TOKEN-VALUE');
             INSERT INTO account VALUES ('a1', 'person@example.com');
             INSERT INTO control_account VALUES ('ca1', 'SECRET-CONTROL');",
        )
        .unwrap();
        path
    }

    #[test]
    fn opencode_db_reads_session_message_and_legacy_rows() {
        let path = make_db();
        let mut collector = OpencodeDbCollector::new(path);
        let out = collector.collect(&Cursor::new()).unwrap();

        let keys: Vec<_> = out.records.iter().map(|r| r.dedup_key.clone()).collect();
        assert!(keys.iter().any(|k| k == "sm-1"));
        assert!(keys.iter().any(|k| k == "sm-2"));
        assert!(keys.iter().any(|k| k == "legacy-1"));
        // The user row and the zero-token legacy row are not usage.
        assert!(!keys.iter().any(|k| k == "sm-user"));
        assert!(!keys.iter().any(|k| k == "legacy-zero"));
    }

    #[test]
    fn opencode_db_cost_integer_and_real_and_reasoning_folded() {
        let path = make_db();
        let mut collector = OpencodeDbCollector::new(path);
        let out = collector.collect(&Cursor::new()).unwrap();

        let real = out.records.iter().find(|r| r.dedup_key == "sm-1").unwrap();
        assert_eq!(real.cost_usd, Some(0.0042));
        assert_eq!(real.cost_source, CostSource::Reported);
        assert_eq!(real.output_tokens, 210, "output + reasoning");
        assert_eq!(real.cache_read_tokens, 300);
        assert_eq!(real.subscription, claude_common::Subscription::OpencodeGo);

        let integer = out.records.iter().find(|r| r.dedup_key == "sm-2").unwrap();
        assert_eq!(integer.cost_usd, Some(1.0));
        assert_eq!(integer.provider, Provider::OpencodeGo);
    }

    #[test]
    fn opencode_db_cursor_is_max_time_per_table() {
        let path = make_db();
        let mut collector = OpencodeDbCollector::new(path);
        let first = collector.collect(&Cursor::new()).unwrap();
        assert_eq!(
            first.cursor.get_u64(CURSOR_SESSION_MESSAGE),
            1_772_704_920_000
        );
        // legacy-zero (1700000001000) carries no usage but is read, so it sets the cursor.
        assert_eq!(
            first.cursor.get_u64(CURSOR_LEGACY_MESSAGE),
            1_700_000_001_000
        );
        let second = collector.collect(&first.cursor).unwrap();
        assert!(second.records.is_empty());
    }

    #[test]
    fn opencode_db_never_selects_secret_tables() {
        // The collector's SQL is the only thing that touches the database, so
        // the source is the audit surface: no statement may name a secret table.
        let source = include_str!("opencode.rs");
        let sql_lines: Vec<&str> = source
            .lines()
            .filter(|l| l.contains("SELECT") || l.contains("FROM "))
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        assert!(!sql_lines.is_empty());
        for line in &sql_lines {
            let lower = line.to_ascii_lowercase();
            assert!(!lower.contains("credential"), "secret table in SQL: {line}");
            assert!(!lower.contains("account"), "secret table in SQL: {line}");
        }
    }

    #[test]
    fn opencode_db_opened_read_only_cannot_write() {
        let path = make_db();
        let conn = open_read_only(&path).unwrap();
        let write = conn.execute("INSERT INTO session VALUES ('x', 1.0)", []);
        assert!(write.is_err(), "a read-only handle must reject writes");
    }

    #[test]
    fn opencode_db_missing_file_is_empty() {
        let mut collector =
            OpencodeDbCollector::new(PathBuf::from("/definitely/not/here/opencode.db"));
        let out = collector.collect(&Cursor::new()).unwrap();
        assert!(out.records.is_empty());
    }
}
