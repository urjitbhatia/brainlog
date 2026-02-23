use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::Path;

use super::models::*;
use super::schema;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            super::permissions::create_dir_restricted(parent)?;
        }
        let conn = Connection::open(path)?;
        // Restrict database file permissions to owner-only (0600)
        super::permissions::set_file_restricted(path)?;
        schema::initialize(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::initialize(&conn)?;
        Ok(Self { conn })
    }

    pub fn create_service(&self, service: &Service) -> Result<()> {
        let command_line_json = serde_json::to_string(&service.command_line)?;
        self.conn.execute(
            "INSERT INTO services (id, name, description, executable, command_line, working_dir, created_at, updated_at, enrichment_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                service.id,
                service.name,
                service.description,
                service.executable,
                command_line_json,
                service.working_dir,
                service.created_at.to_rfc3339(),
                service.updated_at.to_rfc3339(),
                service.enrichment_status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn find_service_by_name(&self, name: &str) -> Result<Option<Service>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, executable, command_line, working_dir, created_at, updated_at, enrichment_status
             FROM services WHERE name = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![name])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_service(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_service(&self, id: &str) -> Result<Option<Service>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, executable, command_line, working_dir, created_at, updated_at, enrichment_status
             FROM services WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_service(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn update_service_enrichment(
        &self,
        service_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        status: &EnrichmentStatus,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE services SET name = COALESCE(?1, name), description = COALESCE(?2, description), enrichment_status = ?3, updated_at = ?4 WHERE id = ?5",
            params![
                name,
                description,
                status.as_str(),
                Utc::now().to_rfc3339(),
                service_id,
            ],
        )?;
        Ok(())
    }

    pub fn create_run(&self, run: &Run) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (id, service_id, pid, started_at, ended_at, exit_code, log_dir, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.id,
                run.service_id,
                run.pid,
                run.started_at.to_rfc3339(),
                run.ended_at.map(|t| t.to_rfc3339()),
                run.exit_code,
                run.log_dir,
                run.status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn update_run_status(
        &self,
        run_id: &str,
        status: &RunStatus,
        exit_code: Option<i32>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?1, exit_code = ?2, ended_at = ?3 WHERE id = ?4",
            params![status.as_str(), exit_code, Utc::now().to_rfc3339(), run_id,],
        )?;
        Ok(())
    }

    pub fn update_run_pid(&self, run_id: &str, pid: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET pid = ?1 WHERE id = ?2",
            params![pid, run_id],
        )?;
        Ok(())
    }

    pub fn get_run(&self, id: &str) -> Result<Option<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, service_id, pid, started_at, ended_at, exit_code, log_dir, status
             FROM runs WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_run(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_latest_run(&self, service_id: &str) -> Result<Option<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, service_id, pid, started_at, ended_at, exit_code, log_dir, status
             FROM runs WHERE service_id = ?1 ORDER BY started_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![service_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_run(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn add_tag(&self, service_id: &str, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO tags (service_id, key, value) VALUES (?1, ?2, ?3)",
            params![service_id, key, value],
        )?;
        Ok(())
    }

    pub fn get_tags(&self, service_id: &str) -> Result<Vec<Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, service_id, key, value FROM tags WHERE service_id = ?1")?;
        let tags = stmt
            .query_map(params![service_id], |row| {
                Ok(Tag {
                    id: row.get(0)?,
                    service_id: row.get(1)?,
                    key: row.get(2)?,
                    value: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tags)
    }

    pub fn add_port(&self, run_id: &str, port: u16, protocol: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO ports (run_id, port, protocol, detected_at) VALUES (?1, ?2, ?3, ?4)",
            params![run_id, port, protocol, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_ports(&self, run_id: &str) -> Result<Vec<Port>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, port, protocol, detected_at FROM ports WHERE run_id = ?1",
        )?;
        let ports = stmt
            .query_map(params![run_id], |row| {
                let detected_at_str: String = row.get(4)?;
                Ok(Port {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    port: row.get::<_, i64>(2)? as u16,
                    protocol: row.get(3)?,
                    detected_at: chrono::DateTime::parse_from_rfc3339(&detected_at_str)
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ports)
    }

    pub fn list_services(&self) -> Result<Vec<Service>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, executable, command_line, working_dir, created_at, updated_at, enrichment_status
             FROM services ORDER BY updated_at DESC",
        )?;
        let services = stmt
            .query_map([], row_to_service_rusqlite)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list services")?;
        Ok(services)
    }

    pub fn list_runs(&self, service_id: &str) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, service_id, pid, started_at, ended_at, exit_code, log_dir, status
             FROM runs WHERE service_id = ?1 ORDER BY started_at DESC",
        )?;
        let runs = stmt
            .query_map(params![service_id], row_to_run_rusqlite)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list runs")?;
        Ok(runs)
    }

    pub fn search_services(
        &self,
        name: Option<&str>,
        executable: Option<&str>,
        tag_filters: &[(String, String)],
        status: Option<&str>,
        port: Option<u16>,
        limit: usize,
    ) -> Result<Vec<Service>> {
        let mut sql = String::from(
            "SELECT DISTINCT s.id, s.name, s.description, s.executable, s.command_line, s.working_dir, s.created_at, s.updated_at, s.enrichment_status
             FROM services s",
        );
        let mut joins = Vec::new();
        let mut conditions = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if !tag_filters.is_empty() {
            joins.push(" JOIN tags t ON t.service_id = s.id");
            let mut tag_conds = Vec::new();
            for (k, v) in tag_filters {
                let idx = param_values.len() + 1;
                tag_conds.push(format!("(t.key = ?{} AND t.value = ?{})", idx, idx + 1));
                param_values.push(Box::new(k.clone()));
                param_values.push(Box::new(v.clone()));
            }
            conditions.push(format!("({})", tag_conds.join(" OR ")));
        }

        if let Some(port_val) = port {
            joins.push(" JOIN runs r ON r.service_id = s.id JOIN ports p ON p.run_id = r.id");
            let idx = param_values.len() + 1;
            conditions.push(format!("p.port = ?{}", idx));
            param_values.push(Box::new(port_val as i64));
        }

        if let Some(name_filter) = name {
            let idx = param_values.len() + 1;
            conditions.push(format!("s.name LIKE ?{}", idx));
            param_values.push(Box::new(format!("%{}%", name_filter)));
        }

        if let Some(exe_filter) = executable {
            let idx = param_values.len() + 1;
            conditions.push(format!("s.executable LIKE ?{}", idx));
            param_values.push(Box::new(format!("%{}%", exe_filter)));
        }

        if let Some(status_filter) = status {
            joins.push(if port.is_some() {
                "" // already joined
            } else {
                " JOIN runs r ON r.service_id = s.id"
            });
            let idx = param_values.len() + 1;
            conditions.push(format!("r.status = ?{}", idx));
            param_values.push(Box::new(status_filter.to_string()));
        }

        for j in &joins {
            sql.push_str(j);
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY s.updated_at DESC");
        let idx = param_values.len() + 1;
        sql.push_str(&format!(" LIMIT ?{}", idx));
        param_values.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let services = stmt
            .query_map(params_refs.as_slice(), row_to_service_rusqlite)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to search services")?;
        Ok(services)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_service(id: &str, name: Option<&str>) -> Service {
        Service {
            id: id.to_string(),
            name: name.map(|s| s.to_string()),
            description: Some("test service".to_string()),
            executable: "/usr/bin/test".to_string(),
            command_line: vec!["test".to_string(), "--flag".to_string()],
            working_dir: "/tmp".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            enrichment_status: EnrichmentStatus::Pending,
        }
    }

    fn make_run(id: &str, service_id: &str, status: RunStatus) -> Run {
        Run {
            id: id.to_string(),
            service_id: service_id.to_string(),
            pid: Some(1234),
            started_at: Utc::now(),
            ended_at: None,
            exit_code: None,
            log_dir: "/tmp/logs".to_string(),
            status,
        }
    }

    #[test]
    fn create_and_get_service() {
        let db = Database::open_in_memory().unwrap();
        let svc = make_service("svc-001", Some("my-app"));
        db.create_service(&svc).unwrap();

        let fetched = db.get_service("svc-001").unwrap().unwrap();
        assert_eq!(fetched.id, "svc-001");
        assert_eq!(fetched.name.as_deref(), Some("my-app"));
        assert_eq!(fetched.executable, "/usr/bin/test");
    }

    #[test]
    fn find_service_by_name() {
        let db = Database::open_in_memory().unwrap();
        let svc = make_service("svc-002", Some("web-server"));
        db.create_service(&svc).unwrap();

        let found = db.find_service_by_name("web-server").unwrap().unwrap();
        assert_eq!(found.id, "svc-002");
        assert!(db.find_service_by_name("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_services_returns_all() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-a", Some("alpha")))
            .unwrap();
        db.create_service(&make_service("svc-b", Some("beta")))
            .unwrap();

        let list = db.list_services().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn create_and_get_run() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-r", Some("runner")))
            .unwrap();

        let run = make_run("run-001", "svc-r", RunStatus::Running);
        db.create_run(&run).unwrap();

        let fetched = db.get_run("run-001").unwrap().unwrap();
        assert_eq!(fetched.service_id, "svc-r");
        assert_eq!(fetched.status, RunStatus::Running);
    }

    #[test]
    fn update_run_status_and_exit_code() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-u", Some("updater")))
            .unwrap();
        db.create_run(&make_run("run-u1", "svc-u", RunStatus::Running))
            .unwrap();

        db.update_run_status("run-u1", &RunStatus::Completed, Some(0))
            .unwrap();
        let updated = db.get_run("run-u1").unwrap().unwrap();
        assert_eq!(updated.status, RunStatus::Completed);
        assert_eq!(updated.exit_code, Some(0));
        assert!(updated.ended_at.is_some());
    }

    #[test]
    fn latest_run_ordering() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-lr", Some("latest")))
            .unwrap();

        let mut run1 = make_run("run-lr1", "svc-lr", RunStatus::Completed);
        run1.started_at = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&run1).unwrap();

        let mut run2 = make_run("run-lr2", "svc-lr", RunStatus::Running);
        run2.started_at = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&run2).unwrap();

        let latest = db.get_latest_run("svc-lr").unwrap().unwrap();
        assert_eq!(latest.id, "run-lr2");
    }

    #[test]
    fn tags_crud() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-t", Some("tagged")))
            .unwrap();

        db.add_tag("svc-t", "env", "prod").unwrap();
        db.add_tag("svc-t", "team", "backend").unwrap();
        // Duplicate should be ignored (INSERT OR IGNORE)
        db.add_tag("svc-t", "env", "prod").unwrap();

        let tags = db.get_tags("svc-t").unwrap();
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn ports_crud() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-p", Some("ported")))
            .unwrap();
        db.create_run(&make_run("run-p1", "svc-p", RunStatus::Running))
            .unwrap();

        db.add_port("run-p1", 8080, "tcp").unwrap();
        db.add_port("run-p1", 443, "tcp").unwrap();
        // Duplicate should be ignored
        db.add_port("run-p1", 8080, "tcp").unwrap();

        let ports = db.get_ports("run-p1").unwrap();
        assert_eq!(ports.len(), 2);
        assert!(ports.iter().any(|p| p.port == 8080));
        assert!(ports.iter().any(|p| p.port == 443));
    }

    #[test]
    fn search_services_by_name() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-s1", Some("web-api")))
            .unwrap();
        db.create_service(&make_service("svc-s2", Some("web-frontend")))
            .unwrap();
        db.create_service(&make_service("svc-s3", Some("worker")))
            .unwrap();

        let results = db
            .search_services(Some("web"), None, &[], None, None, 100)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_services_by_tag() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-st1", Some("app1")))
            .unwrap();
        db.create_service(&make_service("svc-st2", Some("app2")))
            .unwrap();
        db.add_tag("svc-st1", "env", "prod").unwrap();

        let results = db
            .search_services(
                None,
                None,
                &[("env".to_string(), "prod".to_string())],
                None,
                None,
                100,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "svc-st1");
    }

    #[test]
    fn search_services_by_port() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-sp1", Some("http-svc")))
            .unwrap();
        db.create_run(&make_run("run-sp1", "svc-sp1", RunStatus::Running))
            .unwrap();
        db.add_port("run-sp1", 3000, "tcp").unwrap();

        let results = db
            .search_services(None, None, &[], None, Some(3000), 100)
            .unwrap();
        assert_eq!(results.len(), 1);

        let empty = db
            .search_services(None, None, &[], None, Some(9999), 100)
            .unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn update_enrichment() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-e", None)).unwrap();

        db.update_service_enrichment(
            "svc-e",
            Some("enriched-name"),
            Some("enriched desc"),
            &EnrichmentStatus::Completed,
        )
        .unwrap();

        let svc = db.get_service("svc-e").unwrap().unwrap();
        assert_eq!(svc.name.as_deref(), Some("enriched-name"));
        assert_eq!(svc.description.as_deref(), Some("enriched desc"));
        assert_eq!(svc.enrichment_status, EnrichmentStatus::Completed);
    }
}

fn row_to_service(row: &rusqlite::Row<'_>) -> Result<Service> {
    row_to_service_rusqlite(row).map_err(|e| anyhow::anyhow!("{}", e))
}

fn row_to_service_rusqlite(row: &rusqlite::Row<'_>) -> Result<Service, rusqlite::Error> {
    let command_line_json: String = row.get(4)?;
    let created_at_str: String = row.get(6)?;
    let updated_at_str: String = row.get(7)?;
    let enrichment_str: String = row.get(8)?;

    Ok(Service {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        executable: row.get(3)?,
        command_line: serde_json::from_str(&command_line_json).unwrap_or_default(),
        working_dir: row.get(5)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .unwrap_or_default()
            .with_timezone(&chrono::Utc),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
            .unwrap_or_default()
            .with_timezone(&chrono::Utc),
        enrichment_status: EnrichmentStatus::parse(&enrichment_str),
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> Result<Run> {
    row_to_run_rusqlite(row).map_err(|e| anyhow::anyhow!("{}", e))
}

fn row_to_run_rusqlite(row: &rusqlite::Row<'_>) -> Result<Run, rusqlite::Error> {
    let started_at_str: String = row.get(3)?;
    let ended_at_str: Option<String> = row.get(4)?;
    let status_str: String = row.get(7)?;

    Ok(Run {
        id: row.get(0)?,
        service_id: row.get(1)?,
        pid: row.get::<_, Option<i64>>(2)?.map(|p| p as u32),
        started_at: chrono::DateTime::parse_from_rfc3339(&started_at_str)
            .unwrap_or_default()
            .with_timezone(&chrono::Utc),
        ended_at: ended_at_str.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        }),
        exit_code: row.get(5)?,
        log_dir: row.get(6)?,
        status: RunStatus::parse(&status_str),
    })
}
