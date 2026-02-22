use anyhow::Result;
use rusqlite::Connection;

pub fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    conn.execute_batch(
        "
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
            status      TEXT NOT NULL DEFAULT 'running'
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
        ",
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn schema_init_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        // Verify all four tables exist
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
    fn schema_init_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        // Second call should succeed without error
        initialize(&conn).unwrap();
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
