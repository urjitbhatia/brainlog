//! Self-healing for runs stuck in `running`.
//!
//! The wrapper process records a run's exit status when its child terminates.
//! If the wrapper itself dies without cleanup — SIGKILL, orphaned by its
//! parent, machine crash — the run stays `running` in the database forever,
//! with no live process behind it. Read paths call [`reconcile_stale_runs`]
//! first so that `running` always means "actually alive right now".

use anyhow::Result;
use chrono::{DateTime, Utc};
use nix::errno::Errno;
use nix::sys::signal;
use nix::unistd::Pid;
use std::path::Path;

use super::models::{Run, RunStatus};
use super::Database;

/// Whether a PID refers to a live process. EPERM counts as alive: the process
/// exists but belongs to another user, and must not be marked crashed.
fn process_exists(pid: u32) -> bool {
    match signal::kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// A `running` run counts as alive if its wrapper or its child is still up.
/// A run with neither PID recorded has no live process behind it (the wrapper
/// records its own PID at creation, so this only occurs for legacy rows).
fn run_is_alive(run: &Run) -> bool {
    run.wrapper_pid.map(process_exists).unwrap_or(false)
        || run.pid.map(process_exists).unwrap_or(false)
}

/// Best-effort end time for a run that died without recording one: the newest
/// mtime among its log files, falling back to now.
fn infer_ended_at(log_dir: &str) -> DateTime<Utc> {
    let mut latest: Option<std::time::SystemTime> = None;
    if let Ok(entries) = std::fs::read_dir(Path::new(log_dir)) {
        for entry in entries.flatten() {
            if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                if latest.map(|t| mtime > t).unwrap_or(true) {
                    latest = Some(mtime);
                }
            }
        }
    }
    latest.map(DateTime::<Utc>::from).unwrap_or_else(Utc::now)
}

/// Sweep runs stuck in `running` whose wrapper and child processes are both
/// gone, transitioning them to `crashed` with `ended_at` inferred from log
/// file mtimes. Returns the number of runs transitioned.
pub fn reconcile_stale_runs(db: &Database) -> Result<usize> {
    let mut transitioned = 0;
    for run in db.list_running_runs()? {
        if run_is_alive(&run) {
            continue;
        }
        // Best-effort: a transiently locked database just means the record
        // heals on a later read instead of failing this one.
        match db.finalize_stale_run(&run.id, &RunStatus::Crashed, infer_ended_at(&run.log_dir)) {
            Ok(true) => transitioned += 1,
            Ok(false) => {}
            Err(e) => tracing::debug!("Could not finalize stale run {}: {e}", run.id),
        }
    }
    Ok(transitioned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{EnrichmentStatus, Service};

    /// A PID that cannot exist: above pid_max on Linux (default 4194304)
    /// and macOS (99999), while still fitting in i32.
    const DEAD_PID: u32 = 2_000_000_000;

    fn setup_service(db: &Database, id: &str) {
        db.create_service(&Service {
            id: id.to_string(),
            name: Some(format!("{id}-name")),
            description: None,
            executable: "/usr/bin/test".to_string(),
            command_line: vec!["test".to_string()],
            working_dir: "/tmp".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            enrichment_status: EnrichmentStatus::Skipped,
        })
        .unwrap();
    }

    fn insert_run(
        db: &Database,
        id: &str,
        service_id: &str,
        status: RunStatus,
        pid: Option<u32>,
        wrapper_pid: Option<u32>,
        log_dir: &str,
    ) {
        db.create_run(&Run {
            id: id.to_string(),
            service_id: service_id.to_string(),
            pid,
            started_at: Utc::now(),
            ended_at: None,
            exit_code: None,
            log_dir: log_dir.to_string(),
            status,
            wrapper_pid,
        })
        .unwrap();
    }

    #[test]
    fn dead_wrapper_and_no_child_pid_is_marked_crashed() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec1");
        insert_run(
            &db,
            "run-rec1",
            "svc-rec1",
            RunStatus::Running,
            None,
            Some(DEAD_PID),
            "/nonexistent",
        );

        let n = reconcile_stale_runs(&db).unwrap();
        assert_eq!(n, 1);

        let run = db.get_run("run-rec1").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Crashed);
        assert!(run.ended_at.is_some());
    }

    #[test]
    fn live_wrapper_stays_running() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec2");
        insert_run(
            &db,
            "run-rec2",
            "svc-rec2",
            RunStatus::Running,
            None,
            Some(std::process::id()),
            "/nonexistent",
        );

        let n = reconcile_stale_runs(&db).unwrap();
        assert_eq!(n, 0);
        let run = db.get_run("run-rec2").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Running);
        assert!(run.ended_at.is_none());
    }

    #[test]
    fn live_child_with_dead_wrapper_stays_running() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec3");
        insert_run(
            &db,
            "run-rec3",
            "svc-rec3",
            RunStatus::Running,
            Some(std::process::id()),
            Some(DEAD_PID),
            "/nonexistent",
        );

        let n = reconcile_stale_runs(&db).unwrap();
        assert_eq!(n, 0);
        let run = db.get_run("run-rec3").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Running);
    }

    #[test]
    fn run_with_no_pids_is_marked_crashed() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec4");
        insert_run(
            &db,
            "run-rec4",
            "svc-rec4",
            RunStatus::Running,
            None,
            None,
            "/nonexistent",
        );

        let n = reconcile_stale_runs(&db).unwrap();
        assert_eq!(n, 1);
        let run = db.get_run("run-rec4").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Crashed);
    }

    #[test]
    fn terminal_runs_are_untouched() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec5");
        insert_run(
            &db,
            "run-rec5",
            "svc-rec5",
            RunStatus::Completed,
            Some(DEAD_PID),
            Some(DEAD_PID),
            "/nonexistent",
        );

        let n = reconcile_stale_runs(&db).unwrap();
        assert_eq!(n, 0);
        let run = db.get_run("run-rec5").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Completed);
    }

    #[test]
    fn ended_at_inferred_from_log_mtime() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec6");

        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        std::fs::write(&log_path, b"output").unwrap();
        let mtime: DateTime<Utc> = std::fs::metadata(&log_path)
            .unwrap()
            .modified()
            .unwrap()
            .into();

        insert_run(
            &db,
            "run-rec6",
            "svc-rec6",
            RunStatus::Running,
            None,
            Some(DEAD_PID),
            &dir.path().to_string_lossy(),
        );

        reconcile_stale_runs(&db).unwrap();
        let run = db.get_run("run-rec6").unwrap().unwrap();
        let ended_at = run.ended_at.unwrap();
        let delta = (ended_at - mtime).num_milliseconds().abs();
        assert!(
            delta < 2000,
            "ended_at {ended_at} should match log mtime {mtime}"
        );
    }

    #[test]
    fn finalize_stale_run_guards_against_status_race() {
        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec7");
        insert_run(
            &db,
            "run-rec7",
            "svc-rec7",
            RunStatus::Running,
            None,
            Some(DEAD_PID),
            "/nonexistent",
        );

        // Wrapper wins the race and records a clean exit
        db.update_run_status("run-rec7", &RunStatus::Completed, Some(0))
            .unwrap();

        // finalize must be a no-op on the now-terminal row
        let updated = db
            .finalize_stale_run("run-rec7", &RunStatus::Crashed, Utc::now())
            .unwrap();
        assert!(!updated);
        let run = db.get_run("run-rec7").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.exit_code, Some(0));
    }
}
