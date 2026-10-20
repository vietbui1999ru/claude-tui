# Phase B: storage migration v2 (AC-8)

## Current state

- `crates/claude-daemon/src/storage.rs` keeps migrations in a custom `schema_version(version)` table, read with `MAX(version)`. Only `migrate_v1` exists, and every statement is `CREATE ... IF NOT EXISTS`.
- `usage_records` has a `uuid UNIQUE` column. `insert_usage_batch` uses `transaction()` with `INSERT OR IGNORE`.
- `row_to_usage_record` hardcodes `source` as `DataSource::Log`. The summary and aggregate readers fall back to `unwrap_or(ModelType::Sonnet)` (~lines 324 and 382).

## Do

Add `migrate_v2`, applied when the schema version is below 2, inside one transaction:

1. `usage_records`: add the columns `provider TEXT`, `subscription TEXT`, `cost_source TEXT`, `dedup_key TEXT`, and make `cost_usd` able to hold NULL (unknown cost).
   - SQLite cannot drop a NOT NULL constraint in place. If `cost_usd` is NOT NULL today, rebuild the table: create the new one, copy the rows, drop the old one, rename, and recreate the indexes.
   - Backfill v1 rows with `provider='anthropic'`, `subscription='claude'`, `cost_source='computed'` and `dedup_key = uuid`.
   - Add a unique index on `(provider, dedup_key)`.
2. `quota_snapshots(id, subscription, window, used_pct REAL NULL, resets_at TEXT NULL, source TEXT, observed_at TEXT, plan_type TEXT NULL)`, with an index on `(subscription, window, observed_at)`.
3. `quota_hits(id, subscription, period TEXT NULL, reset_at TEXT NULL, observed_at TEXT, raw_kind TEXT, dedup_key TEXT UNIQUE)`.
4. `collector_cursors(collector_id TEXT, key TEXT, value TEXT, PRIMARY KEY(collector_id, key))`.

Also:
- Storage methods to insert and read the new data:
  - `insert_quota_snapshot`;
  - `latest_quota(subscription) -> Vec<..>`, the latest row per window;
  - `insert_quota_hit`;
  - `latest_quota_hit(subscription)`;
  - `get_cursor` and `set_cursor`;
  - the new columns in `insert_usage_batch` and `row_to_usage_record`.
- Replace the `unwrap_or(ModelType::Sonnet)` fallbacks with real model strings (C1 residue).

Tests (names contain `migrate_v2`):
- a v1 database built with the v1 DDL and some rows upgrades with an unchanged row count and backfilled values;
- running the migration twice is a no-op;
- a NULL cost round-trips.
