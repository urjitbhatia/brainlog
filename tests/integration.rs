//! Integration tests for the brainlog CLI.
//!
//! Each test creates an isolated HOME directory so brainlog's SQLite DB
//! and log files don't interfere with each other or the real user data.

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Test harness: isolated brainlog environment.
struct BrainlogEnv {
    home: TempDir,
    bin: PathBuf,
}

impl BrainlogEnv {
    fn new() -> Self {
        let home = TempDir::new().expect("failed to create temp dir");
        let bin = Self::find_binary();
        Self { home, bin }
    }

    fn find_binary() -> PathBuf {
        // cargo test sets CARGO_BIN_EXE_brainlog or we find it in target/debug
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_brainlog") {
            return PathBuf::from(path);
        }
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let candidate = Path::new(manifest_dir).join("target/debug/brainlog");
        if candidate.exists() {
            return candidate;
        }
        panic!("Cannot find brainlog binary. Run `cargo build` first.");
    }

    /// Run a brainlog command in the isolated environment.
    fn run(&self, args: &[&str]) -> CmdResult {
        let output = Command::new(&self.bin)
            .args(args)
            .env("HOME", self.home.path())
            .env("RUST_LOG", "warn")
            // Ensure non-interactive (pipe mode) for predictable behavior
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("failed to execute brainlog");

        CmdResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }

    fn db_path(&self) -> PathBuf {
        self.home.path().join(".brainlog/brainlog.db")
    }

    fn open_db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.db_path()).expect("failed to open DB")
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct CmdResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

// ─── Step 1: Basic run + exit code ───────────────────────────────────

#[test]
fn run_echo_hello_succeeds() {
    let env = BrainlogEnv::new();
    let res = env.run(&["echo", "hello"]);

    assert_eq!(res.exit_code, 0, "exit code should be 0: {:?}", res);
    assert!(
        res.stdout.contains("hello"),
        "stdout should contain 'hello': {:?}",
        res
    );
}

#[test]
fn run_creates_db_and_records() {
    let env = BrainlogEnv::new();
    let res = env.run(&["echo", "test-db"]);
    assert_eq!(res.exit_code, 0);

    // DB should exist
    assert!(env.db_path().exists(), "DB file should be created");

    let conn = env.open_db();

    // Should have exactly one service
    let svc_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM services", [], |row| row.get(0))
        .unwrap();
    assert_eq!(svc_count, 1, "should have 1 service");

    // Should have exactly one run
    let run_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_count, 1, "should have 1 run");

    // Run should be completed with exit code 0
    let (status, exit_code): (String, i32) = conn
        .query_row(
            "SELECT status, exit_code FROM runs LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "completed");
    assert_eq!(exit_code, 0);
}

#[test]
fn run_creates_log_files() {
    let env = BrainlogEnv::new();
    env.run(&["echo", "log-test"]);

    let conn = env.open_db();
    let log_dir: String = conn
        .query_row("SELECT log_dir FROM runs LIMIT 1", [], |row| row.get(0))
        .unwrap();

    let log_path = Path::new(&log_dir);
    assert!(log_path.join("stdout.log").exists(), "stdout.log should exist");
    assert!(log_path.join("stderr.log").exists(), "stderr.log should exist");
    assert!(log_path.join("stdin.log").exists(), "stdin.log should exist");
    assert!(
        log_path.join("combined.log").exists(),
        "combined.log should exist"
    );

    // stdout.log should have nonzero size (echo wrote output)
    let stdout_size = std::fs::metadata(log_path.join("stdout.log"))
        .unwrap()
        .len();
    assert!(stdout_size > 0, "stdout.log should have content");
}

// ─── Step 2: Exit code preservation ──────────────────────────────────

#[test]
fn run_false_exits_with_code_1() {
    let env = BrainlogEnv::new();
    let res = env.run(&["false"]);
    assert_eq!(res.exit_code, 1, "brainlog should forward exit code 1");

    let conn = env.open_db();
    let (status, exit_code): (String, i32) = conn
        .query_row("SELECT status, exit_code FROM runs LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(exit_code, 1);
}

#[test]
fn run_preserves_arbitrary_exit_code() {
    let env = BrainlogEnv::new();
    let res = env.run(&["sh", "-c", "exit 42"]);
    assert_eq!(res.exit_code, 42, "brainlog should preserve exit code 42");
}

// ─── Step 3: List output ─────────────────────────────────────────────

#[test]
fn list_shows_header_and_service() {
    let env = BrainlogEnv::new();

    // Run a named command first
    env.run(&["run", "--name", "my-echo", "--", "echo", "hi"]);

    let list = env.run(&["list"]);
    assert_eq!(list.exit_code, 0);

    // Should have header
    assert!(list.stdout.contains("ID"), "list should have ID header");
    assert!(list.stdout.contains("NAME"), "list should have NAME header");
    assert!(
        list.stdout.contains("STATUS"),
        "list should have STATUS header"
    );
    assert!(
        list.stdout.contains("COMMAND"),
        "list should have COMMAND header"
    );

    // Should show service
    assert!(
        list.stdout.contains("my-echo"),
        "list should show service name: {}",
        list.stdout
    );
    assert!(
        list.stdout.contains("completed"),
        "list should show status: {}",
        list.stdout
    );
}

#[test]
fn list_verbose_shows_details() {
    let env = BrainlogEnv::new();
    env.run(&["run", "--name", "verbose-svc", "--", "echo", "detailed"]);

    let list = env.run(&["list", "-v"]);
    assert_eq!(list.exit_code, 0);
    assert!(list.stdout.contains("verbose-svc"), "should show name");
    assert!(list.stdout.contains("Executable:"), "should show executable");
    assert!(list.stdout.contains("Latest Run:"), "should show latest run");
    assert!(list.stdout.contains("Log Sizes:"), "should show log sizes");
    assert!(list.stdout.contains("Exit Code:"), "should show exit code");
}

#[test]
fn list_empty_shows_message() {
    let env = BrainlogEnv::new();
    let list = env.run(&["list"]);
    assert_eq!(list.exit_code, 0);
    assert!(
        list.stdout.contains("No services found"),
        "empty list should say so: {}",
        list.stdout
    );
}

// ─── Step 4: Logs + Search ───────────────────────────────────────────

#[test]
fn logs_shows_captured_output() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "log-svc",
        "--",
        "sh",
        "-c",
        "echo line1; echo line2; echo line3",
    ]);

    let logs = env.run(&["logs", "log-svc", "--tail", "2"]);
    assert_eq!(logs.exit_code, 0);
    // Should have the last 2 lines
    assert!(
        logs.stdout.contains("line2"),
        "tail should contain line2: {}",
        logs.stdout
    );
    assert!(
        logs.stdout.contains("line3"),
        "tail should contain line3: {}",
        logs.stdout
    );
}

#[test]
fn logs_head_shows_first_lines() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "head-svc",
        "--",
        "sh",
        "-c",
        "echo first; echo second; echo third",
    ]);

    let logs = env.run(&["logs", "head-svc", "--head", "1"]);
    assert_eq!(logs.exit_code, 0);
    assert!(
        logs.stdout.contains("first"),
        "head should contain first: {}",
        logs.stdout
    );
}

#[test]
fn search_finds_pattern() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "search-svc",
        "--",
        "sh",
        "-c",
        "echo INFO-started; echo ERROR-failure; echo INFO-done",
    ]);

    let search = env.run(&["search", "ERROR"]);
    assert_eq!(search.exit_code, 0);
    assert!(
        search.stdout.contains("ERROR-failure"),
        "should find ERROR match: {}",
        search.stdout
    );
    assert!(
        search.stdout.contains("1 match"),
        "should report 1 match: {}",
        search.stdout
    );
}

#[test]
fn search_no_matches() {
    let env = BrainlogEnv::new();
    env.run(&["run", "--name", "no-match", "--", "echo", "hello"]);

    let search = env.run(&["search", "NONEXISTENT_PATTERN_XYZ"]);
    assert_eq!(search.exit_code, 0);
    assert!(
        search.stdout.contains("No matches"),
        "should report no matches: {}",
        search.stdout
    );
}

// ─── Step 5: --name reuse + tags ─────────────────────────────────────

#[test]
fn same_name_reuses_service() {
    let env = BrainlogEnv::new();

    env.run(&["run", "--name", "reuse-svc", "--", "echo", "first"]);
    env.run(&["run", "--name", "reuse-svc", "--", "echo", "second"]);

    let conn = env.open_db();

    let svc_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM services", [], |row| row.get(0))
        .unwrap();
    assert_eq!(svc_count, 1, "should reuse same service");

    let run_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_count, 2, "should have 2 runs");
}

#[test]
fn tags_are_stored() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "tagged-svc",
        "--tag",
        "env:prod",
        "--tag",
        "team:backend",
        "--",
        "echo",
        "tagged",
    ]);

    let conn = env.open_db();
    let tag_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tag_count, 2, "should have 2 tags");

    let has_env: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM tags WHERE key = 'env' AND value = 'prod'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_env, "should have env:prod tag");
}

// ─── Step 6: Stderr capture + stream filtering ───────────────────────

#[test]
fn stderr_is_captured() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "stderr-svc",
        "--",
        "sh",
        "-c",
        "echo out-msg; echo err-msg >&2",
    ]);

    // stdout stream
    let stdout_logs = env.run(&["logs", "stderr-svc", "--stream", "stdout"]);
    assert!(
        stdout_logs.stdout.contains("out-msg"),
        "stdout stream should have out-msg: {}",
        stdout_logs.stdout
    );
    assert!(
        !stdout_logs.stdout.contains("err-msg"),
        "stdout stream should NOT have err-msg: {}",
        stdout_logs.stdout
    );

    // stderr stream
    let stderr_logs = env.run(&["logs", "stderr-svc", "--stream", "stderr"]);
    assert!(
        stderr_logs.stdout.contains("err-msg"),
        "stderr stream should have err-msg: {}",
        stderr_logs.stdout
    );

    // combined stream (default)
    let combined = env.run(&["logs", "stderr-svc"]);
    assert!(
        combined.stdout.contains("out-msg"),
        "combined should have out-msg: {}",
        combined.stdout
    );
    assert!(
        combined.stdout.contains("err-msg"),
        "combined should have err-msg: {}",
        combined.stdout
    );
}

#[test]
fn search_filters_by_service() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "svc-a",
        "--",
        "sh",
        "-c",
        "echo MARKER_A",
    ]);
    env.run(&[
        "run",
        "--name",
        "svc-b",
        "--",
        "sh",
        "-c",
        "echo MARKER_B",
    ]);

    let search_a = env.run(&["search", "--service", "svc-a", "MARKER"]);
    assert!(
        search_a.stdout.contains("MARKER_A"),
        "should find MARKER_A: {}",
        search_a.stdout
    );
    assert!(
        !search_a.stdout.contains("MARKER_B"),
        "should NOT find MARKER_B: {}",
        search_a.stdout
    );
}

// ─── Direct mode ─────────────────────────────────────────────────────

#[test]
fn direct_mode_works() {
    let env = BrainlogEnv::new();
    // Direct mode: `brainlog echo direct-test` (no "run" subcommand)
    let res = env.run(&["echo", "direct-test"]);
    assert_eq!(res.exit_code, 0);
    assert!(res.stdout.contains("direct-test"));

    // Should still create DB records
    assert!(env.db_path().exists());
}

#[test]
fn direct_mode_with_flags() {
    let env = BrainlogEnv::new();
    let res = env.run(&["--name", "direct-named", "echo", "flagged"]);
    assert_eq!(res.exit_code, 0);

    let conn = env.open_db();
    let name: Option<String> = conn
        .query_row("SELECT name FROM services LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(name.as_deref(), Some("direct-named"));
}

// ─── List filtering ──────────────────────────────────────────────────

#[test]
fn list_filters_by_name() {
    let env = BrainlogEnv::new();
    env.run(&["run", "--name", "web-app", "--", "echo", "1"]);
    env.run(&["run", "--name", "worker", "--", "echo", "2"]);

    let list = env.run(&["list", "--name", "web"]);
    assert!(
        list.stdout.contains("web-app"),
        "should find web-app: {}",
        list.stdout
    );
    assert!(
        !list.stdout.contains("worker"),
        "should not show worker: {}",
        list.stdout
    );
}
