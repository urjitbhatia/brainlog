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

/// Process state and executable name (comm) for a PID via `ps`, used to
/// detect PID reuse and zombies. None when they can't be determined; callers
/// then fall back to existence-only liveness.
fn process_state_comm(pid: u32) -> Option<(String, String)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "state=", "-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    let (state, comm) = line.split_once(char::is_whitespace)?;
    Some((state.to_string(), comm.trim().to_string()))
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Whether `comm` (as reported by ps: a full path on macOS, truncated to
/// 15 bytes on Linux) plausibly names the `expected` executable.
fn comm_matches(comm: &str, expected: &str) -> bool {
    let comm_name = basename(comm);
    let expected_name = basename(expected);
    comm_name == expected_name || (comm_name.len() == 15 && expected_name.starts_with(&comm_name))
}

/// True when `pid` is a live process that still looks like one of `expected`.
/// Plain existence is not enough: PIDs are recycled, so a record from days ago
/// can point at an unrelated process (macOS caps PIDs at 99999).
fn process_is(pid: u32, expected: &[String]) -> bool {
    if !process_exists(pid) {
        return false;
    }
    match process_state_comm(pid) {
        // A zombie still occupies its PID but is dead for our purposes.
        Some((state, _)) if state.starts_with('Z') => false,
        Some((_, comm)) => expected.iter().any(|e| comm_matches(&comm, e)),
        None => true,
    }
}

/// Names a brainlog wrapper process can appear under: the conventional binary
/// name plus however this binary is actually named on disk.
fn wrapper_names() -> Vec<String> {
    let mut names = vec!["brainlog".to_string()];
    if let Ok(exe) = std::env::current_exe() {
        let name = basename(&exe.to_string_lossy());
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// True when `pid` is a live process that is (still) a brainlog wrapper,
/// rather than an unrelated process that inherited a recycled PID.
pub fn is_wrapper_process(pid: u32) -> bool {
    process_is(pid, &wrapper_names())
}

/// A `running` run counts as alive if its wrapper or its child is still up —
/// and the process behind each PID still matches what was recorded, so
/// recycled PIDs don't keep dead runs pinned to `running`. A run with neither
/// PID recorded has no live process behind it (the wrapper records its own
/// PID at creation, so this only occurs for legacy rows).
fn run_is_alive(run: &Run, wrapper_names: &[String], executable: Option<&str>) -> bool {
    if let Some(wrapper_pid) = run.wrapper_pid {
        if process_is(wrapper_pid, wrapper_names) {
            return true;
        }
    }
    if let Some(pid) = run.pid {
        let alive = match executable {
            Some(exe) => process_is(pid, &[exe.to_string()]),
            None => process_exists(pid),
        };
        if alive {
            return true;
        }
    }
    false
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
    let wrapper_names = wrapper_names();
    let mut transitioned = 0;
    for run in db.list_running_runs()? {
        let executable = db
            .get_service(&run.service_id)
            .ok()
            .flatten()
            .map(|s| s.executable);
        if run_is_alive(&run, &wrapper_names, executable.as_deref()) {
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
        setup_service_with_exe(db, id, "/usr/bin/test");
    }

    fn setup_service_with_exe(db: &Database, id: &str, executable: &str) {
        db.create_service(&Service {
            id: id.to_string(),
            name: Some(format!("{id}-name")),
            description: None,
            executable: executable.to_string(),
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
        // The child is this test process, so the service's executable must be
        // this binary for the identity check to recognise it.
        let own_exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        setup_service_with_exe(&db, "svc-rec3", &own_exe);
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
    fn recycled_wrapper_pid_is_marked_crashed() {
        // A live process that is NOT a brainlog wrapper occupying the recorded
        // wrapper PID must not keep the run pinned to `running`.
        let mut decoy = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();

        let db = Database::open_in_memory().unwrap();
        setup_service(&db, "svc-rec8");
        insert_run(
            &db,
            "run-rec8",
            "svc-rec8",
            RunStatus::Running,
            None,
            Some(decoy.id()),
            "/nonexistent",
        );

        let n = reconcile_stale_runs(&db).unwrap();
        let _ = decoy.kill();
        let _ = decoy.wait();

        assert_eq!(n, 1);
        let run = db.get_run("run-rec8").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Crashed);
    }

    #[test]
    fn comm_matching_handles_paths_and_truncation() {
        // macOS ps reports a full path
        assert!(comm_matches("/usr/local/bin/brainlog", "brainlog"));
        // Exact name
        assert!(comm_matches("tsh", "/usr/local/bin/tsh"));
        // Linux truncates comm to 15 bytes
        assert!(comm_matches("brainlog-abc123", "brainlog-abc123456"));
        // Different program entirely
        assert!(!comm_matches("sleep", "brainlog"));
    }

    #[test]
    fn zombie_process_counts_as_dead() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        // Wait for the child to exit and become a zombie (unreaped).
        for _ in 0..50 {
            if matches!(process_state_comm(pid), Some((s, _)) if s.starts_with('Z')) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!process_is(pid, &["true".to_string()]));
        let _ = child.wait();
    }

    #[test]
    fn own_process_passes_identity_check() {
        let own_exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(process_is(std::process::id(), &[own_exe]));
        assert!(!process_is(
            std::process::id(),
            &["definitely-not-this-binary".to_string()]
        ));
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
