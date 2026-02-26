use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
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
        super::permissions::set_file_restricted(path);
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

    /// Find services whose ID starts with the given prefix.
    /// Returns at most `limit` matches, useful for prefix-based resolution.
    pub fn find_services_by_id_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<Service>> {
        let pattern = format!("{}%", prefix);
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, executable, command_line, working_dir, created_at, updated_at, enrichment_status
             FROM services WHERE id LIKE ?1 LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], row_to_service)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
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
            "INSERT INTO runs (id, service_id, pid, started_at, ended_at, exit_code, log_dir, status, wrapper_pid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run.id,
                run.service_id,
                run.pid,
                run.started_at.to_rfc3339(),
                run.ended_at.map(|t| t.to_rfc3339()),
                run.exit_code,
                run.log_dir,
                run.status.as_str(),
                run.wrapper_pid,
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
            "SELECT id, service_id, pid, started_at, ended_at, exit_code, log_dir, status, wrapper_pid
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
            "SELECT id, service_id, pid, started_at, ended_at, exit_code, log_dir, status, wrapper_pid
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
             FROM services ORDER BY created_at DESC",
        )?;
        let services = stmt
            .query_map([], row_to_service)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list services")?;
        Ok(services)
    }

    pub fn list_runs(&self, service_id: &str) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, service_id, pid, started_at, ended_at, exit_code, log_dir, status, wrapper_pid
             FROM runs WHERE service_id = ?1 ORDER BY started_at DESC",
        )?;
        let runs = stmt
            .query_map(params![service_id], row_to_run)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list runs")?;
        Ok(runs)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn search_services(
        &self,
        name: Option<&str>,
        executable: Option<&str>,
        tag_filters: &[(String, String)],
        status: Option<&str>,
        port: Option<u16>,
        cwd: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Service>> {
        let mut sql = String::from(
            "SELECT DISTINCT s.id, s.name, s.description, s.executable, s.command_line, s.working_dir, s.created_at, s.updated_at, s.enrichment_status
             FROM services s",
        );
        let mut joins = Vec::new();
        let mut conditions = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        for (k, v) in tag_filters {
            let idx = param_values.len() + 1;
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM tags t WHERE t.service_id = s.id AND t.key = ?{} AND t.value = ?{})",
                idx, idx + 1
            ));
            param_values.push(Box::new(k.clone()));
            param_values.push(Box::new(v.clone()));
        }

        if let Some(port_val) = port {
            joins.push(" JOIN runs r ON r.service_id = s.id JOIN ports p ON p.run_id = r.id");
            let idx = param_values.len() + 1;
            conditions.push(format!("p.port = ?{}", idx));
            param_values.push(Box::new(port_val as i64));
        }

        if let Some(name_filter) = name {
            let idx = param_values.len() + 1;
            conditions.push(format!(
                "(s.name LIKE ?{idx} OR s.id LIKE ?{idx} OR s.executable LIKE ?{idx})"
            ));
            param_values.push(Box::new(format!("%{}%", name_filter)));
        }

        if let Some(exe_filter) = executable {
            let idx = param_values.len() + 1;
            conditions.push(format!("s.executable LIKE ?{}", idx));
            param_values.push(Box::new(format!("%{}%", exe_filter)));
        }

        if let Some(cwd_filter) = cwd {
            let idx = param_values.len() + 1;
            conditions.push(format!("s.working_dir LIKE ?{}", idx));
            param_values.push(Box::new(format!("%{}%", cwd_filter)));
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
        sql.push_str(" ORDER BY s.created_at DESC");
        let idx = param_values.len() + 1;
        sql.push_str(&format!(" LIMIT ?{}", idx));
        param_values.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let services = stmt
            .query_map(params_refs.as_slice(), row_to_service)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to search services")?;
        Ok(services)
    }

    /// Resolve a user-provided identifier to a log directory path.
    ///
    /// Resolution order:
    /// 1. Exact match on run ID
    /// 2. Exact match on service ID (returns latest run's log_dir)
    /// 3. Exact match on service name (returns latest run's log_dir)
    /// 4. Prefix match on service ID (exactly 1 match required)
    /// 5. Error with helpful suggestion
    pub fn resolve_log_dir(&self, id: &str) -> Result<String> {
        // 1. Try as run ID first
        if let Some(run) = self.get_run(id)? {
            return Ok(run.log_dir);
        }

        // 2. Try as service ID — get latest run
        if let Some(run) = self.get_latest_run(id)? {
            return Ok(run.log_dir);
        }

        // 3. Try as service name
        if let Some(service) = self.find_service_by_name(id)? {
            if let Some(run) = self.get_latest_run(&service.id)? {
                return Ok(run.log_dir);
            }
            anyhow::bail!("Service '{}' has no runs", id);
        }

        // 4. Try prefix match on service ID (SQL LIKE query, limit to 2 to detect ambiguity)
        let matches = self.find_services_by_id_prefix(id, 2)?;

        match matches.len() {
            1 => {
                if let Some(run) = self.get_latest_run(&matches[0].id)? {
                    return Ok(run.log_dir);
                }
                anyhow::bail!(
                    "Service '{}' (matched from prefix '{}') has no runs",
                    matches[0].id,
                    id
                );
            }
            n if n > 1 => {
                let ids: Vec<_> = matches.iter().map(|s| s.id.as_str()).collect();
                anyhow::bail!(
                    "Ambiguous prefix '{}' matches {} services: {}",
                    id,
                    n,
                    ids.join(", ")
                );
            }
            _ => {}
        }

        anyhow::bail!(
            "No service or run found matching '{}'. Use `brainlog list` to see available services.",
            id
        );
    }

    /// Search services whose metadata (name, command line, tag values) matches
    /// the given regex pattern. Returns matching services along with the field
    /// that matched (for display purposes).
    pub fn search_services_by_pattern(&self, pattern: &Regex) -> Result<Vec<ServiceMetadataMatch>> {
        let services = self.list_services()?;
        let mut matches = Vec::new();

        for service in services {
            let mut matched_fields = Vec::new();

            // Check service name
            if let Some(ref name) = service.name {
                if pattern.is_match(name) {
                    matched_fields.push(format!("name: {}", name));
                }
            }

            // Check command line (joined as a single string)
            let cmd_line = service.command_line.join(" ");
            if pattern.is_match(&cmd_line) {
                matched_fields.push(format!("command: {}", cmd_line));
            }

            // Check individual command line arguments
            if matched_fields.iter().all(|f| !f.starts_with("command:")) {
                for arg in &service.command_line {
                    if pattern.is_match(arg) {
                        matched_fields.push(format!("command: {}", cmd_line));
                        break;
                    }
                }
            }

            // Check tag values
            let tags = self.get_tags(&service.id)?;
            for tag in &tags {
                if pattern.is_match(&tag.value) || pattern.is_match(&tag.key) {
                    matched_fields.push(format!("tag: {}:{}", tag.key, tag.value));
                }
            }

            // Check description
            if let Some(ref desc) = service.description {
                if pattern.is_match(desc) {
                    matched_fields.push(format!("description: {}", desc));
                }
            }

            if !matched_fields.is_empty() {
                // Get latest run status for display
                let latest_run = self.get_latest_run(&service.id)?;
                let status = latest_run
                    .as_ref()
                    .map(|r| r.status.as_str().to_string())
                    .unwrap_or_else(|| "no runs".to_string());

                matches.push(ServiceMetadataMatch {
                    service,
                    matched_fields,
                    status,
                });
            }
        }

        Ok(matches)
    }

    pub fn count_runs(&self, service_id: &str) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE service_id = ?1",
            params![service_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn list_services_grouped(&self) -> Result<Vec<ServiceGroup>> {
        let services = self.list_services()?;
        let mut groups: std::collections::HashMap<(String, String), Vec<Service>> =
            std::collections::HashMap::new();

        for service in services {
            let key = (service.executable.clone(), service.working_dir.clone());
            groups.entry(key).or_default().push(service);
        }

        let mut result: Vec<ServiceGroup> = Vec::new();
        for ((executable, working_dir), svcs) in groups {
            let mut total_runs: usize = 0;
            let mut latest_run_at: Option<chrono::DateTime<Utc>> = None;
            let mut latest_run_status: Option<RunStatus> = None;

            for svc in &svcs {
                total_runs += self.count_runs(&svc.id)?;
                if let Some(run) = self.get_latest_run(&svc.id)? {
                    let dominated = latest_run_at.map(|t| run.started_at > t).unwrap_or(true);
                    if dominated {
                        latest_run_at = Some(run.started_at);
                        latest_run_status = Some(run.status);
                    }
                }
            }

            result.push(ServiceGroup {
                executable,
                working_dir,
                run_count: total_runs,
                latest_run_at,
                latest_run_status,
                services: svcs,
            });
        }

        // Sort by most recent run first (groups with no runs go last)
        result.sort_by(|a, b| b.latest_run_at.cmp(&a.latest_run_at));

        Ok(result)
    }

    /// Find services whose latest run ended before `cutoff`, or that have no runs
    /// and were created before `cutoff`. When `include_running` is true, also
    /// includes services with running/stale runs that started before the cutoff.
    pub fn find_purgeable_services(
        &self,
        cutoff: &chrono::DateTime<Utc>,
        include_running: bool,
    ) -> Result<Vec<PurgeCandidate>> {
        let cutoff_str = cutoff.to_rfc3339();

        let running_clause = if include_running {
            "OR (
                -- Services with running/stale runs started before cutoff (force mode)
                NOT EXISTS (
                    SELECT 1 FROM runs r WHERE r.service_id = s.id
                    AND r.started_at >= ?1
                )
                AND EXISTS (
                    SELECT 1 FROM runs r WHERE r.service_id = s.id
                )
            )"
        } else {
            ""
        };

        let sql = format!(
            "
            SELECT s.id, s.name, s.executable, s.command_line
            FROM services s
            WHERE (
                -- Services where the most recent run ended before cutoff
                EXISTS (
                    SELECT 1 FROM runs r WHERE r.service_id = s.id
                )
                AND NOT EXISTS (
                    SELECT 1 FROM runs r WHERE r.service_id = s.id
                    AND (r.ended_at IS NULL OR r.ended_at >= ?1)
                )
            )
            OR (
                -- Services with no runs, created before cutoff
                NOT EXISTS (
                    SELECT 1 FROM runs r WHERE r.service_id = s.id
                )
                AND s.created_at < ?1
            )
            {running_clause}
        "
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let candidates: Vec<PurgeCandidate> = stmt
            .query_map(params![cutoff_str], |row| {
                let command_line_json: String = row.get(3)?;
                Ok(PurgeCandidate {
                    service_id: row.get(0)?,
                    name: row.get(1)?,
                    executable: row.get(2)?,
                    command_line: serde_json::from_str(&command_line_json).unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to find purgeable services")?;

        Ok(candidates)
    }

    /// Get all log directories for a service's runs.
    pub fn get_run_log_dirs(&self, service_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT log_dir FROM runs WHERE service_id = ?1")?;
        let dirs = stmt
            .query_map(params![service_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()
            .context("Failed to get run log dirs")?;
        Ok(dirs)
    }

    /// Delete a service and all its associated data (runs, ports, tags) from the DB.
    /// Returns the number of runs deleted.
    pub fn delete_service_cascade(&self, service_id: &str) -> Result<usize> {
        // Delete ports for all runs of this service
        self.conn.execute(
            "DELETE FROM ports WHERE run_id IN (SELECT id FROM runs WHERE service_id = ?1)",
            params![service_id],
        )?;

        // Delete runs
        let runs_deleted = self.conn.execute(
            "DELETE FROM runs WHERE service_id = ?1",
            params![service_id],
        )?;

        // Delete tags
        self.conn.execute(
            "DELETE FROM tags WHERE service_id = ?1",
            params![service_id],
        )?;

        // Delete the service itself
        self.conn
            .execute("DELETE FROM services WHERE id = ?1", params![service_id])?;

        Ok(runs_deleted)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mark an existing service as superseded by renaming it with a `_superseded_<timestamp>` suffix.
    /// Returns the old service if found.
    pub fn supersede_service(&self, name: &str) -> Result<Option<Service>> {
        let existing = self.find_service_by_name(name)?;
        if let Some(ref service) = existing {
            let timestamp = Utc::now().format("%Y%m%d%H%M%S");
            let new_name = format!("{}_superseded_{}", name, timestamp);
            self.conn.execute(
                "UPDATE services SET name = ?1, updated_at = ?2 WHERE id = ?3",
                params![new_name, Utc::now().to_rfc3339(), service.id],
            )?;
        }
        Ok(existing)
    }
}

/// Information about a service that is a candidate for purging.
#[derive(Debug, Clone)]
pub struct PurgeCandidate {
    pub service_id: String,
    pub name: Option<String>,
    pub executable: String,
    pub command_line: Vec<String>,
}

/// A service that matched a metadata search, along with what fields matched.
#[derive(Debug)]
pub struct ServiceMetadataMatch {
    pub service: Service,
    pub matched_fields: Vec<String>,
    pub status: String,
}

impl serde::Serialize for ServiceMetadataMatch {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ServiceMetadataMatch", 3)?;
        state.serialize_field("service", &self.service)?;
        state.serialize_field("matched_fields", &self.matched_fields)?;
        state.serialize_field("status", &self.status)?;
        state.end()
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
            wrapper_pid: None,
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
            .search_services(Some("web"), None, &[], None, None, None, 100)
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
                None,
                100,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "svc-st1");
    }

    #[test]
    fn search_services_by_multiple_tags_uses_and() {
        let db = Database::open_in_memory().unwrap();
        // Service A has both env:prod AND team:backend
        db.create_service(&make_service("svc-mt1", Some("app-both")))
            .unwrap();
        db.add_tag("svc-mt1", "env", "prod").unwrap();
        db.add_tag("svc-mt1", "team", "backend").unwrap();

        // Service B has only env:prod
        db.create_service(&make_service("svc-mt2", Some("app-one")))
            .unwrap();
        db.add_tag("svc-mt2", "env", "prod").unwrap();

        // Filter by both tags — only Service A should match (AND semantics)
        let results = db
            .search_services(
                None,
                None,
                &[
                    ("env".to_string(), "prod".to_string()),
                    ("team".to_string(), "backend".to_string()),
                ],
                None,
                None,
                None,
                100,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "svc-mt1");

        // Filter by single tag — both services should match
        let results = db
            .search_services(
                None,
                None,
                &[("env".to_string(), "prod".to_string())],
                None,
                None,
                None,
                100,
            )
            .unwrap();
        assert_eq!(results.len(), 2);
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
            .search_services(None, None, &[], None, Some(3000), None, 100)
            .unwrap();
        assert_eq!(results.len(), 1);

        let empty = db
            .search_services(None, None, &[], None, Some(9999), None, 100)
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

    #[test]
    fn enrichment_preserves_user_provided_name() {
        let db = Database::open_in_memory().unwrap();
        // Service created with a user-provided name
        db.create_service(&make_service("svc-preserve", Some("my-api")))
            .unwrap();

        // Simulate enrichment passing None for name (as it should when user provided one)
        db.update_service_enrichment(
            "svc-preserve",
            None, // name should not be overwritten
            Some("An auto-generated description"),
            &EnrichmentStatus::Completed,
        )
        .unwrap();

        let svc = db.get_service("svc-preserve").unwrap().unwrap();
        // Name must remain the user-provided value
        assert_eq!(svc.name.as_deref(), Some("my-api"));
        // Description should still be enriched
        assert_eq!(
            svc.description.as_deref(),
            Some("An auto-generated description")
        );
        assert_eq!(svc.enrichment_status, EnrichmentStatus::Completed);
    }

    #[test]
    fn enrichment_sets_name_when_no_user_name() {
        let db = Database::open_in_memory().unwrap();
        // Service created without a user-provided name
        db.create_service(&make_service("svc-noname", None))
            .unwrap();

        // Simulate enrichment providing both name and description
        db.update_service_enrichment(
            "svc-noname",
            Some("LLM Generated Name"),
            Some("LLM generated description"),
            &EnrichmentStatus::Completed,
        )
        .unwrap();

        let svc = db.get_service("svc-noname").unwrap().unwrap();
        // Name should be set by enrichment since user didn't provide one
        assert_eq!(svc.name.as_deref(), Some("LLM Generated Name"));
        assert_eq!(
            svc.description.as_deref(),
            Some("LLM generated description")
        );
        assert_eq!(svc.enrichment_status, EnrichmentStatus::Completed);
    }

    #[test]
    fn enrichment_does_not_overwrite_user_name_with_llm_name() {
        let db = Database::open_in_memory().unwrap();
        // Service created with a user-provided name
        db.create_service(&make_service("svc-keep", Some("my-api")))
            .unwrap();

        // BUG SCENARIO: If enrichment incorrectly passes an LLM name,
        // COALESCE will overwrite the user's name. The fix ensures we
        // pass None for name when the user provided one.
        //
        // This test verifies the COALESCE behavior: passing a non-None
        // name WILL overwrite (demonstrating why the fix is needed).
        db.update_service_enrichment(
            "svc-keep",
            Some("Node Express Server"), // LLM-generated name
            Some("A Node.js Express server"),
            &EnrichmentStatus::Completed,
        )
        .unwrap();

        let svc = db.get_service("svc-keep").unwrap().unwrap();
        // COALESCE(?1, name) with non-NULL ?1 uses ?1, so name gets overwritten.
        // This is the behavior that the enrichment layer must prevent
        // by passing None when has_user_name is true.
        assert_eq!(svc.name.as_deref(), Some("Node Express Server"));
    }

    #[test]
    fn list_services_sorted_by_created_at_descending() {
        let db = Database::open_in_memory().unwrap();

        let mut older = make_service("svc-old", Some("older-svc"));
        older.created_at = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&older).unwrap();

        let mut newer = make_service("svc-new", Some("newer-svc"));
        newer.created_at = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&newer).unwrap();

        let list = db.list_services().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "svc-new", "newest created_at should come first");
        assert_eq!(list[1].id, "svc-old", "oldest created_at should come last");
    }

    #[test]
    fn search_services_sorted_by_created_at_descending() {
        let db = Database::open_in_memory().unwrap();

        let mut older = make_service("svc-search-old", Some("web-old"));
        older.created_at = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&older).unwrap();

        let mut newer = make_service("svc-search-new", Some("web-new"));
        newer.created_at = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&newer).unwrap();

        let results = db
            .search_services(Some("web"), None, &[], None, None, None, 100)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].id, "svc-search-new",
            "newest created_at should come first in search"
        );
        assert_eq!(
            results[1].id, "svc-search-old",
            "oldest created_at should come last in search"
        );
    }

    #[test]
    fn supersede_service_renames_old() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-sup", Some("my-app")))
            .unwrap();

        let old = db.supersede_service("my-app").unwrap().unwrap();
        assert_eq!(old.id, "svc-sup");
        assert_eq!(old.name.as_deref(), Some("my-app"));

        // Old service should no longer be findable by original name
        assert!(db.find_service_by_name("my-app").unwrap().is_none());

        // Old service should have a superseded name
        let updated = db.get_service("svc-sup").unwrap().unwrap();
        assert!(updated.name.unwrap().starts_with("my-app_superseded_"));
    }

    #[test]
    fn supersede_nonexistent_returns_none() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.supersede_service("nonexistent").unwrap().is_none());
    }

    #[test]
    fn enrichment_description_updated_regardless_of_user_name() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-desc", Some("my-api")))
            .unwrap();

        // Even when we preserve the user's name (pass None), description should still update
        db.update_service_enrichment(
            "svc-desc",
            None,
            Some("Enhanced description from LLM"),
            &EnrichmentStatus::Completed,
        )
        .unwrap();

        let svc = db.get_service("svc-desc").unwrap().unwrap();
        assert_eq!(svc.name.as_deref(), Some("my-api"));
        assert_eq!(
            svc.description.as_deref(),
            Some("Enhanced description from LLM")
        );
    }

    // --- Metadata search tests ---

    fn make_service_with_cmd(id: &str, name: Option<&str>, command: &[&str]) -> Service {
        Service {
            id: id.to_string(),
            name: name.map(|s| s.to_string()),
            description: Some("test service".to_string()),
            executable: command.first().unwrap_or(&"/usr/bin/test").to_string(),
            command_line: command.iter().map(|s| s.to_string()).collect(),
            working_dir: "/tmp".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            enrichment_status: EnrichmentStatus::Pending,
        }
    }

    #[test]
    fn metadata_search_by_command_name() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-ms1",
            Some("my-false"),
            &["false"],
        ))
        .unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-ms2",
            Some("my-echo"),
            &["echo", "hello"],
        ))
        .unwrap();

        let pattern = Regex::new("false").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].service.id, "svc-ms1");
        assert!(results[0]
            .matched_fields
            .iter()
            .any(|f| f.starts_with("command:")));
    }

    #[test]
    fn metadata_search_by_service_name() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-mn1",
            Some("web-api"),
            &["node", "server.js"],
        ))
        .unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-mn2",
            Some("worker"),
            &["python", "worker.py"],
        ))
        .unwrap();

        let pattern = Regex::new("web-api").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].service.id, "svc-mn1");
        assert!(results[0]
            .matched_fields
            .iter()
            .any(|f| f.starts_with("name:")));
    }

    #[test]
    fn metadata_search_by_tag() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-mt1",
            Some("tagged-svc"),
            &["echo", "hi"],
        ))
        .unwrap();
        db.add_tag("svc-mt1", "env", "production").unwrap();

        db.create_service(&make_service_with_cmd(
            "svc-mt2",
            Some("untagged"),
            &["echo", "bye"],
        ))
        .unwrap();

        let pattern = Regex::new("production").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].service.id, "svc-mt1");
        assert!(results[0]
            .matched_fields
            .iter()
            .any(|f| f.starts_with("tag:")));
    }

    #[test]
    fn metadata_search_matches_multiple_fields() {
        let db = Database::open_in_memory().unwrap();
        // Service where "test" appears in both name and command
        db.create_service(&make_service_with_cmd(
            "svc-mm1",
            Some("test-runner"),
            &["test", "--verbose"],
        ))
        .unwrap();

        let pattern = Regex::new("test").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        // Should match both name and command fields
        let has_name_match = results[0]
            .matched_fields
            .iter()
            .any(|f| f.starts_with("name:"));
        let has_cmd_match = results[0]
            .matched_fields
            .iter()
            .any(|f| f.starts_with("command:"));
        assert!(has_name_match, "Should match service name");
        assert!(has_cmd_match, "Should match command line");
    }

    #[test]
    fn metadata_search_no_matches() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-nm1",
            Some("my-app"),
            &["node", "app.js"],
        ))
        .unwrap();

        let pattern = Regex::new("zzz_nonexistent").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn metadata_search_regex_pattern() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-rx1",
            Some("web-server-1"),
            &["nginx"],
        ))
        .unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-rx2",
            Some("web-server-2"),
            &["apache"],
        ))
        .unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-rx3",
            Some("worker"),
            &["sidekiq"],
        ))
        .unwrap();

        // Regex matching "web-server-\d+"
        let pattern = Regex::new(r"web-server-\d+").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn metadata_search_includes_run_status() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-rs1",
            Some("status-svc"),
            &["echo", "hello"],
        ))
        .unwrap();
        db.create_run(&make_run("run-rs1", "svc-rs1", RunStatus::Failed))
            .unwrap();

        let pattern = Regex::new("status-svc").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "failed");
    }

    #[test]
    fn metadata_search_no_runs_shows_no_runs_status() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with_cmd(
            "svc-nr1",
            Some("no-run-svc"),
            &["echo"],
        ))
        .unwrap();

        let pattern = Regex::new("no-run-svc").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "no runs");
    }

    #[test]
    fn metadata_search_by_description() {
        let db = Database::open_in_memory().unwrap();
        let mut svc = make_service_with_cmd("svc-md1", Some("my-svc"), &["echo"]);
        svc.description = Some("A production database service".to_string());
        db.create_service(&svc).unwrap();

        let pattern = Regex::new("database").unwrap();
        let results = db.search_services_by_pattern(&pattern).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0]
            .matched_fields
            .iter()
            .any(|f| f.starts_with("description:")));
    }

    // --- Grouping tests ---

    fn make_service_with(
        id: &str,
        name: Option<&str>,
        executable: &str,
        command_line: &[&str],
        working_dir: &str,
    ) -> Service {
        Service {
            id: id.to_string(),
            name: name.map(|s| s.to_string()),
            description: Some("test".to_string()),
            executable: executable.to_string(),
            command_line: command_line.iter().map(|s| s.to_string()).collect(),
            working_dir: working_dir.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            enrichment_status: EnrichmentStatus::Pending,
        }
    }

    #[test]
    fn list_services_grouped_basic() {
        let db = Database::open_in_memory().unwrap();

        db.create_service(&make_service_with(
            "g1",
            Some("make-dev"),
            "make",
            &["make", "dev"],
            "/home/user/project",
        ))
        .unwrap();
        db.create_service(&make_service_with(
            "g2",
            Some("make-dev-port"),
            "make",
            &["make", "dev", "--port=3000"],
            "/home/user/project",
        ))
        .unwrap();
        db.create_service(&make_service_with(
            "g3",
            Some("cargo-test"),
            "cargo",
            &["cargo", "test"],
            "/home/user/project",
        ))
        .unwrap();

        let groups = db.list_services_grouped().unwrap();
        assert_eq!(groups.len(), 2);

        let make_group = groups.iter().find(|g| g.executable == "make").unwrap();
        assert_eq!(make_group.services.len(), 2);
        assert_eq!(make_group.working_dir, "/home/user/project");

        let cargo_group = groups.iter().find(|g| g.executable == "cargo").unwrap();
        assert_eq!(cargo_group.services.len(), 1);
    }

    #[test]
    fn list_services_grouped_run_counts() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with(
            "rc1",
            Some("app1"),
            "node",
            &["node", "app.js"],
            "/project",
        ))
        .unwrap();
        db.create_service(&make_service_with(
            "rc2",
            Some("app2"),
            "node",
            &["node", "app.js", "--watch"],
            "/project",
        ))
        .unwrap();

        let mut run1 = make_run("run-rc1a", "rc1", RunStatus::Completed);
        run1.started_at = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&run1).unwrap();

        let mut run2 = make_run("run-rc1b", "rc1", RunStatus::Completed);
        run2.started_at = chrono::DateTime::parse_from_rfc3339("2024-02-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&run2).unwrap();

        let mut run3 = make_run("run-rc2a", "rc2", RunStatus::Running);
        run3.started_at = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&run3).unwrap();

        let groups = db.list_services_grouped().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].run_count, 3);
        assert_eq!(groups[0].latest_run_status, Some(RunStatus::Running));
    }

    #[test]
    fn list_services_grouped_sorted_by_latest_run() {
        let db = Database::open_in_memory().unwrap();

        db.create_service(&make_service_with(
            "s1",
            Some("old"),
            "python",
            &["python", "old.py"],
            "/old",
        ))
        .unwrap();
        db.create_service(&make_service_with(
            "s2",
            Some("new"),
            "python",
            &["python", "new.py"],
            "/new",
        ))
        .unwrap();

        let mut old_run = make_run("run-old", "s1", RunStatus::Completed);
        old_run.started_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&old_run).unwrap();

        let mut new_run = make_run("run-new", "s2", RunStatus::Running);
        new_run.started_at = chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_run(&new_run).unwrap();

        let groups = db.list_services_grouped().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].working_dir, "/new");
        assert_eq!(groups[1].working_dir, "/old");
    }

    #[test]
    fn list_services_grouped_no_runs() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service_with(
            "nr1",
            Some("no-run"),
            "echo",
            &["echo", "hello"],
            "/tmp",
        ))
        .unwrap();

        let groups = db.list_services_grouped().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].run_count, 0);
        assert!(groups[0].latest_run_at.is_none());
        assert!(groups[0].latest_run_status.is_none());
    }

    #[test]
    fn count_runs_returns_correct_count() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("cr-svc", Some("counter")))
            .unwrap();

        assert_eq!(db.count_runs("cr-svc").unwrap(), 0);

        db.create_run(&make_run("cr-run1", "cr-svc", RunStatus::Completed))
            .unwrap();
        db.create_run(&make_run("cr-run2", "cr-svc", RunStatus::Running))
            .unwrap();

        assert_eq!(db.count_runs("cr-svc").unwrap(), 2);
    }

    #[test]
    fn purge_finds_old_services_with_ended_runs() {
        let db = Database::open_in_memory().unwrap();
        let mut old_svc = make_service("svc-old", Some("old-app"));
        old_svc.created_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&old_svc).unwrap();

        let mut old_run = make_run("run-old", "svc-old", RunStatus::Completed);
        old_run.started_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        old_run.ended_at = Some(
            chrono::DateTime::parse_from_rfc3339("2023-01-01T01:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        db.create_run(&old_run).unwrap();

        // Recent service should not be purgeable
        db.create_service(&make_service("svc-new", Some("new-app")))
            .unwrap();
        let mut new_run = make_run("run-new", "svc-new", RunStatus::Completed);
        new_run.ended_at = Some(Utc::now());
        db.create_run(&new_run).unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(1);
        let candidates = db.find_purgeable_services(&cutoff, false).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].service_id, "svc-old");
    }

    #[test]
    fn purge_finds_services_with_no_runs() {
        let db = Database::open_in_memory().unwrap();
        let mut old_svc = make_service("svc-noruns", Some("no-runs-app"));
        old_svc.created_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&old_svc).unwrap();

        // New service with no runs should NOT be found
        db.create_service(&make_service("svc-new-noruns", Some("new-no-runs")))
            .unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(1);
        let candidates = db.find_purgeable_services(&cutoff, false).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].service_id, "svc-noruns");
    }

    #[test]
    fn purge_skips_services_with_running_runs() {
        let db = Database::open_in_memory().unwrap();
        let mut svc = make_service("svc-running", Some("running-app"));
        svc.created_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&svc).unwrap();

        let mut run = make_run("run-running", "svc-running", RunStatus::Running);
        run.started_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        run.ended_at = None;
        db.create_run(&run).unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(1);
        let candidates = db.find_purgeable_services(&cutoff, false).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn purge_force_includes_running_services() {
        let db = Database::open_in_memory().unwrap();
        let mut svc = make_service("svc-stale", Some("stale-app"));
        svc.created_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.create_service(&svc).unwrap();

        let mut run = make_run("run-stale", "svc-stale", RunStatus::Running);
        run.started_at = chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        run.ended_at = None;
        db.create_run(&run).unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(1);
        // Without force: skipped
        let candidates = db.find_purgeable_services(&cutoff, false).unwrap();
        assert!(candidates.is_empty());
        // With force: included
        let candidates = db.find_purgeable_services(&cutoff, true).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].service_id, "svc-stale");
    }

    #[test]
    fn delete_service_cascade_removes_all_related_data() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-del", Some("delete-me")))
            .unwrap();
        db.add_tag("svc-del", "env", "test").unwrap();
        db.create_run(&make_run("run-del1", "svc-del", RunStatus::Completed))
            .unwrap();
        db.create_run(&make_run("run-del2", "svc-del", RunStatus::Completed))
            .unwrap();
        db.add_port("run-del1", 8080, "tcp").unwrap();

        let runs_deleted = db.delete_service_cascade("svc-del").unwrap();
        assert_eq!(runs_deleted, 2);

        // Verify everything is gone
        assert!(db.get_service("svc-del").unwrap().is_none());
        assert!(db.get_tags("svc-del").unwrap().is_empty());
        assert!(db.list_runs("svc-del").unwrap().is_empty());
        assert!(db.get_ports("run-del1").unwrap().is_empty());
    }

    #[test]
    fn get_run_log_dirs_returns_all_dirs() {
        let db = Database::open_in_memory().unwrap();
        db.create_service(&make_service("svc-logs", Some("log-app")))
            .unwrap();

        let mut run1 = make_run("run-l1", "svc-logs", RunStatus::Completed);
        run1.log_dir = "/tmp/logs/run-l1".to_string();
        db.create_run(&run1).unwrap();

        let mut run2 = make_run("run-l2", "svc-logs", RunStatus::Completed);
        run2.log_dir = "/tmp/logs/run-l2".to_string();
        db.create_run(&run2).unwrap();

        let dirs = db.get_run_log_dirs("svc-logs").unwrap();
        assert_eq!(dirs.len(), 2);
        assert!(dirs.contains(&"/tmp/logs/run-l1".to_string()));
        assert!(dirs.contains(&"/tmp/logs/run-l2".to_string()));
    }

    #[test]
    fn purge_empty_db_returns_no_candidates() {
        let db = Database::open_in_memory().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(1);
        let candidates = db.find_purgeable_services(&cutoff, false).unwrap();
        assert!(candidates.is_empty());
    }
}

fn parse_datetime(s: &str) -> Result<chrono::DateTime<chrono::Utc>, rusqlite::Error> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn row_to_service(row: &rusqlite::Row<'_>) -> Result<Service, rusqlite::Error> {
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
        created_at: parse_datetime(&created_at_str)?,
        updated_at: parse_datetime(&updated_at_str)?,
        enrichment_status: EnrichmentStatus::parse(&enrichment_str),
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> Result<Run, rusqlite::Error> {
    let started_at_str: String = row.get(3)?;
    let ended_at_str: Option<String> = row.get(4)?;
    let status_str: String = row.get(7)?;

    Ok(Run {
        id: row.get(0)?,
        service_id: row.get(1)?,
        pid: row.get::<_, Option<i64>>(2)?.map(|p| p as u32),
        started_at: parse_datetime(&started_at_str)?,
        ended_at: ended_at_str.map(|s| parse_datetime(&s)).transpose()?,
        exit_code: row.get(5)?,
        log_dir: row.get(6)?,
        status: RunStatus::parse(&status_str),
        wrapper_pid: row.get::<_, Option<i64>>(8)?.map(|p| p as u32),
    })
}
