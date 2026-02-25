use anyhow::{bail, Result};
use rusqlite::Connection;

/// Current schema version. Bump this and add a migration function when the schema changes.
const CURRENT_VERSION: i64 = 1;

/// The SQL for the initial v1 schema (tables + indexes).
const V1_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS services (
        id                TEXT PRIMARY KEY,
        name              TEXT,
        description       TEXT,
        executable        TEXT NOT NULL,
        command_line      TEXT NOT NULL,
        working_dir       TEXT NOT NULL,
        created_at        TEXT NOT NULL,
        updated_at        TEXT NOT NULL,
        enrichment_status TEXT NOT NULL DEFAULT 'pending'
    );

    CREATE TABLE IF NOT EXISTS runs (
        id          TEXT PRIMARY KEY,
        service_id  TEXT NOT NULL REFERENCES services(id),
        pid         INTEGER,
        started_at  TEXT NOT NULL,
        ended_at    TEXT,
        exit_code   INTEGER,
        log_dir     TEXT NOT NULL,
        status      TEXT NOT NULL DEFAULT 'running',
        wrapper_pid INTEGER
    );

    CREATE TABLE IF NOT EXISTS tags (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        service_id TEXT NOT NULL REFERENCES services(id),
        key        TEXT NOT NULL,
        value      TEXT NOT NULL,
        UNIQUE(service_id, key, value)
    );

    CREATE TABLE IF NOT EXISTS ports (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id      TEXT NOT NULL REFERENCES runs(id),
        port        INTEGER NOT NULL,
        protocol    TEXT NOT NULL DEFAULT 'tcp',
        detected_at TEXT NOT NULL,
        UNIQUE(run_id, port, protocol)
    );

    CREATE INDEX IF NOT EXISTS idx_services_name ON services(name);
    CREATE INDEX IF NOT EXISTS idx_services_executable ON services(executable);
    CREATE INDEX IF NOT EXISTS idx_runs_service_id ON runs(service_id);
    CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status);
    CREATE INDEX IF NOT EXISTS idx_tags_key_value ON tags(key, value);
    CREATE INDEX IF NOT EXISTS idx_ports_port ON ports(port);
";

/// Type alias for migration functions. Each takes a connection (already inside a transaction)
/// and applies the changes needed to move from version N to N+1.
type MigrationFn = fn(&Connection) -> Result<()>;

/// List of migrations, indexed by (from_version - 1). migrations[0] migrates v1 -> v2, etc.
/// Currently empty since v1 is the initial version.
fn migrations() -> Vec<MigrationFn> {
    vec![
        // When adding a migration from v1 -> v2, add a function here:
        // migrate_v1_to_v2,
    ]
}

/// Initialize the database schema with version tracking.
///
/// - Sets WAL mode and foreign keys.
/// - If no `schema_version` table exists, creates the v1 schema and sets version to 1.
///   (Handles both fresh databases and pre-versioning databases that already have tables.)
/// - If the stored version matches `CURRENT_VERSION`, proceeds normally.
/// - If the stored version is older, runs migrations sequentially to bring it up to date.
/// - If the stored version is newer than `CURRENT_VERSION`, returns an error.
///
/// The version check and any migrations are wrapped in a single transaction.
pub fn initialize(conn: &Connection) -> Result<()> {
    // These PRAGMAs must be set outside transactions
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    let has_version_table = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if has_version_table {
        // Database has version tracking — check and migrate if needed
        let db_version: i64 =
            conn.query_row("SELECT version FROM schema_version", [], |row| row.get(0))?;

        if db_version == CURRENT_VERSION {
            // Already up to date
            return Ok(());
        }

        if db_version > CURRENT_VERSION {
            bail!(
                "Database schema version {} is newer than supported version {}. \
                 Please upgrade brainlog.",
                db_version,
                CURRENT_VERSION
            );
        }

        // Run migrations from db_version to CURRENT_VERSION
        run_migrations(conn, db_version)?;
    } else {
        // No version table: either fresh database or pre-versioning database.
        // Create the schema (IF NOT EXISTS handles pre-versioning case) and set version.
        conn.execute_batch("BEGIN;")?;
        conn.execute_batch(V1_SCHEMA)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
        )?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [CURRENT_VERSION],
        )?;
        conn.execute_batch("COMMIT;")?;
    }

    Ok(())
}

/// Run migrations sequentially from `from_version` to `CURRENT_VERSION`.
/// The entire migration sequence is wrapped in a single transaction.
fn run_migrations(conn: &Connection, from_version: i64) -> Result<()> {
    let all_migrations = migrations();

    conn.execute_batch("BEGIN;")?;

    for version in from_version..CURRENT_VERSION {
        let idx = (version - 1) as usize;
        if idx >= all_migrations.len() {
            conn.execute_batch("ROLLBACK;")?;
            bail!(
                "Missing migration from version {} to {}",
                version,
                version + 1
            );
        }
        all_migrations[idx](conn)?;
    }

    conn.execute("UPDATE schema_version SET version = ?1", [CURRENT_VERSION])?;
    conn.execute_batch("COMMIT;")?;

    Ok(())
}

/// Returns the current schema version constant (for testing).
pub fn current_version() -> i64 {
    CURRENT_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn fresh_database_gets_version_1() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn opening_existing_v1_database_succeeds() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        // Second call should succeed without error (no migration needed)
        initialize(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn newer_version_returns_error() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        // Manually bump the version beyond CURRENT_VERSION
        conn.execute(
            "UPDATE schema_version SET version = ?1",
            [CURRENT_VERSION + 1],
        )
        .unwrap();

        let result = initialize(&conn);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("newer than supported"),
            "Expected 'newer than supported' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn migration_from_pre_versioning_database() {
        let conn = Connection::open_in_memory().unwrap();

        // Simulate a pre-versioning database: create tables without schema_version
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();

        // Verify schema_version table does NOT exist
        let has_version: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_version, 0);

        // Now run initialize — should detect missing version table and add it
        initialize(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);

        // Verify original tables still exist
        for table in &["services", "runs", "tags", "ports"] {
            let count: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                        table
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "Table {} should exist", table);
        }
    }

    #[test]
    fn schema_init_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        // Verify all five tables exist (four data tables + schema_version)
        for table in &["services", "runs", "tags", "ports", "schema_version"] {
            let count: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                        table
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "Table {} should exist", table);
        }
    }

    #[test]
    fn schema_init_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        // Second call should succeed without error
        initialize(&conn).unwrap();

        // Version should still be 1
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn schema_has_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count >= 5, "Expected at least 5 indexes, got {}", count);
    }
}
