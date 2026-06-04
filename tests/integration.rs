//! Integration tests for the brainlog CLI.
//!
//! Each test creates an isolated HOME directory so brainlog's SQLite DB
//! and log files don't interfere with each other or the real user data.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
        .query_row("SELECT status, exit_code FROM runs LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
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
    assert!(
        log_path.join("stdout.log").exists(),
        "stdout.log should exist"
    );
    assert!(
        log_path.join("stderr.log").exists(),
        "stderr.log should exist"
    );
    assert!(
        log_path.join("stdin.log").exists(),
        "stdin.log should exist"
    );
    assert!(
        !log_path.join("combined.log").exists(),
        "combined.log should no longer be written (redundant storage removed)"
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
        list.stdout.contains("CREATED"),
        "list should have CREATED header"
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
    assert!(
        list.stdout.contains("Executable:"),
        "should show executable"
    );
    assert!(
        list.stdout.contains("Latest Run:"),
        "should show latest run"
    );
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
        search.stdout.contains("1 log match"),
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
    env.run(&["run", "--name", "svc-a", "--", "sh", "-c", "echo MARKER_A"]);
    env.run(&["run", "--name", "svc-b", "--", "sh", "-c", "echo MARKER_B"]);

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

// ─── MCP server integration ─────────────────────────────────────────

impl BrainlogEnv {
    /// Spawn `brainlog mcp` with stdin/stdout piped for JSON-RPC.
    fn spawn_mcp(&self) -> std::process::Child {
        Command::new(&self.bin)
            .arg("mcp")
            .env("HOME", self.home.path())
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn brainlog mcp")
    }
}

/// Send a JSON-RPC message to the MCP server and read the response.
fn mcp_request(stdin: &mut impl Write, stdout: &mut impl BufRead, request: &str) -> String {
    // MCP uses newline-delimited JSON
    writeln!(stdin, "{}", request).expect("write to mcp stdin");
    stdin.flush().expect("flush mcp stdin");

    let mut line = String::new();
    stdout.read_line(&mut line).expect("read from mcp stdout");
    line
}

#[test]
fn mcp_initialize_and_list_tools() {
    let env = BrainlogEnv::new();
    let mut child = env.spawn_mcp();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    let init_resp = mcp_request(&mut stdin, &mut stdout, init_req);
    assert!(
        init_resp.contains("\"result\""),
        "initialize should return result: {}",
        init_resp
    );
    assert!(
        init_resp.contains("serverInfo") || init_resp.contains("capabilities"),
        "initialize should have server info: {}",
        init_resp
    );

    // Send initialized notification
    let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    writeln!(stdin, "{}", notif).unwrap();
    stdin.flush().unwrap();

    // List tools
    let tools_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let tools_resp = mcp_request(&mut stdin, &mut stdout, tools_req);
    assert!(
        tools_resp.contains("discover_services"),
        "should expose discover_services tool: {}",
        tools_resp
    );
    assert!(
        tools_resp.contains("get_logs"),
        "should expose get_logs tool: {}",
        tools_resp
    );
    assert!(
        tools_resp.contains("search_logs"),
        "should expose search_logs tool: {}",
        tools_resp
    );

    // Clean shutdown
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn mcp_discover_services_tool() {
    let env = BrainlogEnv::new();

    // First, create some data via the CLI
    env.run(&[
        "run",
        "--name",
        "mcp-test-svc",
        "--tag",
        "env:test",
        "--",
        "echo",
        "mcp-data",
    ]);

    let mut child = env.spawn_mcp();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    mcp_request(&mut stdin, &mut stdout, init_req);
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    // Call discover_services with group=false to get flat (per-service) output
    let call_req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"discover_services","arguments":{"group":false}}}"#;
    let call_resp = mcp_request(&mut stdin, &mut stdout, call_req);
    assert!(
        call_resp.contains("mcp-test-svc"),
        "discover_services should find the service: {}",
        call_resp
    );
    assert!(
        call_resp.contains("env"),
        "response should include tags: {}",
        call_resp
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn mcp_get_logs_tool() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "mcp-log-svc",
        "--",
        "sh",
        "-c",
        "echo mcp-log-output",
    ]);

    let mut child = env.spawn_mcp();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    mcp_request(&mut stdin, &mut stdout, init_req);
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    // Call get_logs by service name
    let call_req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_logs","arguments":{"id":"mcp-log-svc"}}}"#;
    let call_resp = mcp_request(&mut stdin, &mut stdout, call_req);
    assert!(
        call_resp.contains("mcp-log-output"),
        "get_logs should return captured output: {}",
        call_resp
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn mcp_search_logs_tool() {
    let env = BrainlogEnv::new();
    env.run(&[
        "run",
        "--name",
        "mcp-search-svc",
        "--",
        "sh",
        "-c",
        "echo FIND_ME_MCP; echo IGNORE_THIS",
    ]);

    let mut child = env.spawn_mcp();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    mcp_request(&mut stdin, &mut stdout, init_req);
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    // Call search_logs
    let call_req = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_logs","arguments":{"pattern":"FIND_ME"}}}"#;
    let call_resp = mcp_request(&mut stdin, &mut stdout, call_req);
    assert!(
        call_resp.contains("FIND_ME_MCP"),
        "search_logs should find the pattern: {}",
        call_resp
    );
    assert!(
        !call_resp.contains("IGNORE_THIS"),
        "search_logs should not match non-matching lines: {}",
        call_resp
    );

    drop(stdin);
    let _ = child.wait();
}

// ─── Daemon mode ─────────────────────────────────────────────────────

impl BrainlogEnv {
    /// Spawn a daemon for this isolated environment and return immediately.
    /// Returns the daemon's pid file path so the caller can wait/cleanup.
    fn start_daemon(&self) -> std::path::PathBuf {
        let res = self.run(&["daemon", "start"]);
        assert_eq!(res.exit_code, 0, "daemon start failed: {res:?}");
        self.home.path().join(".brainlog/daemon.pid")
    }

    fn stop_daemon(&self) {
        let _ = self.run(&["daemon", "stop"]);
    }

    /// Wait up to ~3s for a predicate over the DB connection to return true.
    fn wait_for_db<F: Fn(&rusqlite::Connection) -> bool>(&self, pred: F) -> bool {
        for _ in 0..150 {
            if self.db_path().exists() {
                let conn = self.open_db();
                if pred(&conn) {
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }
}

#[test]
fn daemon_status_when_not_running() {
    let env = BrainlogEnv::new();
    let res = env.run(&["daemon", "status"]);
    assert_eq!(res.exit_code, 0);
    assert!(
        res.stdout.contains("stopped"),
        "status should say stopped: {res:?}"
    );
}

#[test]
fn daemon_start_then_status_reports_running() {
    let env = BrainlogEnv::new();
    let pid_file = env.start_daemon();
    assert!(pid_file.exists(), "pid file should exist after start");

    let res = env.run(&["daemon", "status"]);
    assert_eq!(res.exit_code, 0);
    assert!(
        res.stdout.contains("running"),
        "status should say running: {res:?}"
    );

    env.stop_daemon();
    // Pid file should be cleaned up by the daemon's Drop impl.
    for _ in 0..50 {
        if !pid_file.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!pid_file.exists(), "pid file should be removed after stop");
}

#[test]
fn daemon_double_start_is_noop() {
    let env = BrainlogEnv::new();
    let _pid_file = env.start_daemon();
    // Second start should report "already running" and exit 0 without
    // bringing up a second daemon (the pid file lock prevents that anyway).
    let res = env.run(&["daemon", "start"]);
    assert_eq!(res.exit_code, 0);
    assert!(
        res.stderr.contains("already running") || res.stdout.contains("already running"),
        "second start should say already running: {res:?}"
    );
    env.stop_daemon();
}

#[test]
fn run_daemon_flag_autostarts_daemon() {
    let env = BrainlogEnv::new();
    // No daemon running yet — `-D` should bring one up transparently.
    let res = env.run(&["-D", "--name", "autostart-svc", "echo", "auto"]);
    assert_eq!(res.exit_code, 0, "should succeed via autostart: {res:?}");
    assert!(
        res.stderr.contains("not running, starting it"),
        "should announce autostart: {res:?}"
    );
    assert!(
        res.stdout.contains("spawned"),
        "should still report spawn: {res:?}"
    );

    // Daemon should now be alive.
    let status = env.run(&["daemon", "status"]);
    assert!(
        status.stdout.contains("running"),
        "autostarted daemon should be running: {status:?}"
    );

    env.stop_daemon();
}

#[test]
fn run_daemon_flag_dispatches_to_daemon() {
    let env = BrainlogEnv::new();
    env.start_daemon();

    let res = env.run(&[
        "run",
        "--daemon",
        "--name",
        "daemon-svc",
        "--",
        "echo",
        "from-daemon",
    ]);
    assert_eq!(res.exit_code, 0, "spawn under daemon failed: {res:?}");
    assert!(
        res.stdout.contains("spawned"),
        "should print spawned: {res:?}"
    );

    // The daemon's wrapper writes service+run records into the shared DB.
    let found = env.wait_for_db(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM services WHERE name = 'daemon-svc'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    });
    assert!(found, "service should appear in DB");

    env.stop_daemon();
}

#[test]
fn daemon_status_lists_supervised_services() {
    let env = BrainlogEnv::new();
    env.start_daemon();

    // Run a long-ish command so it's still alive when we check status.
    env.run(&["-D", "--name", "long-svc", "sh", "-c", "sleep 2; echo done"]);

    // Give the daemon a moment to register the child.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let res = env.run(&["daemon", "status"]);
    assert_eq!(res.exit_code, 0);
    assert!(
        res.stdout.contains("long-svc"),
        "status should list the service: {res:?}"
    );

    env.stop_daemon();
}
