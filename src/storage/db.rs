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
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
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
            params![
                status.as_str(),
                exit_code,
                Utc::now().to_rfc3339(),
                run_id,
            ],
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
            .query_map([], |row| {
                row_to_service_rusqlite(row)
            })?
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
            .query_map(params![service_id], |row| row_to_run_rusqlite(row))?
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

        if port.is_some() {
            joins.push(" JOIN runs r ON r.service_id = s.id JOIN ports p ON p.run_id = r.id");
            let idx = param_values.len() + 1;
            conditions.push(format!("p.port = ?{}", idx));
            param_values.push(Box::new(port.unwrap() as i64));
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
            .query_map(params_refs.as_slice(), |row| row_to_service_rusqlite(row))?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to search services")?;
        Ok(services)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
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
        enrichment_status: EnrichmentStatus::from_str(&enrichment_str),
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
        status: RunStatus::from_str(&status_str),
    })
}
