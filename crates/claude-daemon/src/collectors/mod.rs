//! Source collectors: one per provider data source, behind [`SourceCollector`].
//!
//! Every collector is read-only toward its source. Progress is kept as a
//! [`Cursor`] that the run loop persists per collector id, so a restart
//! resumes where the last run stopped instead of rescanning.

pub mod claude;
pub mod claude_quota;
pub mod codex;
pub mod opencode;
pub mod pi;
pub mod reset_text;

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use claude_common::{CollectorError, QuotaHit, QuotaSnapshot, UsageRecord};
use tracing::warn;

/// Longest line the JSONL reader will hand to a parser. A longer line is
/// skipped (with a warning), so one corrupt line cannot exhaust memory.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Named resume positions for one collector, e.g. a byte offset per file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor {
    values: BTreeMap<String, String>,
}

impl Cursor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    /// Every (key, value) pair, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The value at `key` parsed as an unsigned integer; `0` when absent or malformed.
    pub fn get_u64(&self, key: &str) -> u64 {
        self.get(key).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    pub fn set_u64(&mut self, key: impl Into<String>, value: u64) {
        self.set(key, value.to_string());
    }
}

/// Everything one collection pass produced.
#[derive(Debug, Default)]
pub struct CollectOutput {
    pub records: Vec<UsageRecord>,
    pub snapshots: Vec<QuotaSnapshot>,
    pub hits: Vec<QuotaHit>,
    /// The cursor to persist if the pass succeeds.
    pub cursor: Cursor,
}

/// One provider data source.
pub trait SourceCollector {
    /// Stable id, e.g. "codex-rollouts". Also the key of its persisted cursor.
    fn id(&self) -> &'static str;
    /// New data since `cursor`. Read-only: never writes to the source.
    fn collect(&mut self, cursor: &Cursor) -> Result<CollectOutput, CollectorError>;
}

/// The complete lines of a file starting at byte `offset`.
#[derive(Debug, Default)]
pub struct JsonlChunk {
    /// Complete, `\n`-terminated lines without the terminator, in file order.
    pub lines: Vec<String>,
    /// Offset just past the last complete line. Resume from here next time.
    pub next_offset: u64,
}

/// Read the complete lines of `path` from `offset`.
///
/// The returned `next_offset` only advances past a `\n`, so a line still being
/// written is picked up whole on the next pass. A file shorter than `offset`
/// was truncated or replaced, so reading restarts at 0. Lines longer than
/// [`MAX_LINE_BYTES`] are skipped.
pub fn read_jsonl_from(path: &Path, offset: u64) -> Result<JsonlChunk, CollectorError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    let len = file
        .metadata()
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?
        .len();
    let start = if offset > len { 0 } else { offset };
    if start == len {
        return Ok(JsonlChunk {
            lines: Vec::new(),
            next_offset: start,
        });
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    let Some(last_newline) = buf.iter().rposition(|&b| b == b'\n') else {
        // No complete line yet.
        return Ok(JsonlChunk {
            lines: Vec::new(),
            next_offset: start,
        });
    };
    let complete = &buf[..=last_newline];

    let mut lines = Vec::new();
    for raw in complete.split(|&b| b == b'\n') {
        if raw.is_empty() {
            continue;
        }
        if raw.len() > MAX_LINE_BYTES {
            warn!(
                "skipping {}-byte line in {} (over {MAX_LINE_BYTES} bytes)",
                raw.len(),
                path.display()
            );
            continue;
        }
        lines.push(String::from_utf8_lossy(raw).into_owned());
    }

    Ok(JsonlChunk {
        lines,
        next_offset: start + complete.len() as u64,
    })
}

/// Recursively collect files under `dir` for which `accept` returns true.
/// Unreadable subdirectories are logged and skipped. A missing root is an
/// empty result, not an error: a provider that is not installed has no files.
pub fn walk_files(dir: &Path, accept: &dyn Fn(&Path) -> bool) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    walk_into(dir, accept, &mut found);
    found.sort();
    found
}

fn walk_into(dir: &Path, accept: &dyn Fn(&Path) -> bool, found: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!("cannot read {}: {e}", dir.display());
            }
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, accept, found);
        } else if accept(&path) {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-tui-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn jsonl_reader_stops_at_last_newline() {
        let path = temp_file("a.jsonl", b"{\"a\":1}\n{\"b\":2}\n{\"c\":");
        let chunk = read_jsonl_from(&path, 0).unwrap();
        assert_eq!(chunk.lines, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert_eq!(chunk.next_offset, 16);
    }

    #[test]
    fn jsonl_reader_resumes_after_partial_line_completes() {
        let path = temp_file("b.jsonl", b"{\"a\":1}\n{\"b\":");
        let first = read_jsonl_from(&path, 0).unwrap();
        assert_eq!(first.lines.len(), 1);
        std::fs::write(&path, b"{\"a\":1}\n{\"b\":2}\n").unwrap();
        let second = read_jsonl_from(&path, first.next_offset).unwrap();
        assert_eq!(second.lines, vec!["{\"b\":2}"]);
    }

    #[test]
    fn jsonl_reader_resets_when_file_shrinks() {
        let path = temp_file("c.jsonl", b"{\"a\":1}\n{\"b\":2}\n");
        let chunk = read_jsonl_from(&path, 9999).unwrap();
        assert_eq!(chunk.lines.len(), 2);
        assert_eq!(chunk.next_offset, 16);
    }

    #[test]
    fn jsonl_reader_skips_oversize_line() {
        let mut body = vec![b'x'; MAX_LINE_BYTES + 1];
        body.push(b'\n');
        body.extend_from_slice(b"{\"ok\":true}\n");
        let path = temp_file("d.jsonl", &body);
        let chunk = read_jsonl_from(&path, 0).unwrap();
        assert_eq!(chunk.lines, vec!["{\"ok\":true}"]);
    }

    #[test]
    fn cursor_round_trips_u64() {
        let mut cursor = Cursor::new();
        cursor.set_u64("file", 42);
        assert_eq!(cursor.get_u64("file"), 42);
        assert_eq!(cursor.get_u64("missing"), 0);
    }

    #[test]
    fn walk_files_missing_root_is_empty() {
        let found = walk_files(Path::new("/definitely/not/here/claude-tui"), &|_| true);
        assert!(found.is_empty());
    }
}

/// Run one collector for one pass: load its cursor, collect, store the output,
/// then persist the new cursor. A failure leaves the stored cursor untouched,
/// so the next pass retries the same data.
pub fn run_once(
    collector: &mut dyn SourceCollector,
    storage: &mut crate::storage::Storage,
) -> Result<usize, CollectorError> {
    let id = collector.id();
    let mut cursor = Cursor::new();
    // Stored cursors are a flat key/value map; load the ones this collector wrote.
    for (key, value) in storage
        .cursors_for(id)
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?
    {
        cursor.set(key, value);
    }

    let output = collector.collect(&cursor)?;
    let count = output.records.len();

    storage
        .insert_usage_batch(&output.records)
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    for snapshot in &output.snapshots {
        storage
            .insert_quota_snapshot(snapshot)
            .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    }
    for hit in &output.hits {
        storage
            .insert_quota_hit(hit)
            .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    }
    for (key, value) in output.cursor.iter() {
        storage
            .set_cursor(id, key, value)
            .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
    }
    Ok(count)
}

/// Paths and settings the collectors read. Constructors take this so tests
/// point it at fixtures and never touch the real home directory.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    pub poll_interval_secs: u64,
    pub claude_roots: Vec<std::path::PathBuf>,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 60,
            claude_roots: default_claude_roots(),
        }
    }
}

fn default_claude_roots() -> Vec<std::path::PathBuf> {
    match std::env::var("HOME") {
        Ok(home) => {
            let home = std::path::PathBuf::from(home);
            vec![
                // Primary: Claude Code stores per-request usage in conversation JSONL files.
                home.join(".claude").join("projects"),
                // Legacy fallback paths.
                home.join(".claude").join("logs"),
                home.join(".config").join("claude").join("logs"),
            ]
        }
        Err(_) => Vec::new(),
    }
}

/// Build every collector the daemon runs.
pub fn default_collectors(
    config: &CollectorConfig,
    prices: claude_common::PriceBook,
) -> Vec<Box<dyn SourceCollector + Send>> {
    vec![Box::new(claude::ClaudeLogCollector::new(
        config.claude_roots.clone(),
        prices,
    ))]
}

/// Poll every collector on an interval until cancelled. One collector's error
/// is logged and does not stop the others. Records and snapshots go through
/// storage, and the rebuilt session table is refreshed when new usage arrives.
pub async fn run_loop(
    config: CollectorConfig,
    mut collectors: Vec<Box<dyn SourceCollector + Send>>,
    storage: std::sync::Arc<tokio::sync::Mutex<crate::storage::Storage>>,
    collector_status: std::sync::Arc<tokio::sync::Mutex<claude_common::protocol::CollectorStatus>>,
    cancel: tokio_util::sync::CancellationToken,
) {
    use claude_common::protocol::CollectorStatus;
    use tracing::{error, info};

    *collector_status.lock().await = CollectorStatus::Log;
    info!("collectors starting");

    let mut interval =
        tokio::time::interval(tokio::time::Duration::from_secs(config.poll_interval_secs));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("collectors shutting down");
                break;
            }
            _ = interval.tick() => {}
        }

        for collector in collectors.iter_mut() {
            let mut storage = storage.lock().await;
            match run_once(collector.as_mut(), &mut storage) {
                Ok(0) => {}
                Ok(n) => {
                    info!("{}: stored {n} new usage records", collector.id());
                    if let Err(e) = storage.rebuild_sessions() {
                        error!("failed to rebuild sessions: {e}");
                    }
                }
                Err(e) => warn!("{}: collection failed: {e}", collector.id()),
            }
        }
    }
}
