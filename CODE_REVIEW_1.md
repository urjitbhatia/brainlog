# Brainlog Code Review — Pre-1.0.0 Audit

Date: 2026-02-22 (updated 2026-02-23, all merges complete)
Scope: Full codebase analysis of current `master` (commit `9bdc3c2`)

---

## 1. Logic Errors

### ~~1.1 Exit code truncation to `u8` (src/main.rs:21,34)~~ FIXED
~~`ExitCode::from(code as u8)` truncates exit codes > 255.~~
**Fixed in `worktree-agent-ab3a6de7` branch** (commit `1f2445a`). Added `exit_code_to_u8()` helper that clamps negative values to 1 and values >255 to 255. Comprehensive tests cover all edge cases (0, 1, 42, 127, 128, 130, 255, 256, -1, i32::MAX, i32::MIN).

### ~~1.2 `follow_logs` reads entire file on every poll (src/cli/logs.rs:72,79)~~ FIXED
~~On each iteration, `follow_logs` calls `reader.read_frames()` which reads the *entire* file from the beginning, then slices `all_frames[shown_frames..]`. For large log files, this becomes O(n^2) over time. Additionally, line 72 reads all frames just to count them (`reader.read_frames()?.len()`), immediately discarding the data — but the initial `read_tail(10)` already read from the end. These two reads are inconsistent: `shown_frames` is set to the total frame count, but only the last 10 were displayed, which is correct behavior but the double-read is wasteful.~~
**Fixed in `worktree-agent-a98e2b91` branch** (commit `4d2e686`). Added `LogReader::read_frames_from_offset()` that seeks to a byte position and reads only new frames. Rewrote `follow_logs` to track byte offset instead of re-reading the entire file each poll. Removed redundant `read_frames().len()` call. 3 new tests.

### ~~1.3 `read_head` and `read_tail` read entire file (src/storage/logfile.rs:138-147)~~ FIXED
~~Both `read_head(n)` and `read_tail(n)` call `read_frames()` first (reads entire file), then slice. `read_head(1)` reads every frame just to return the first one. `read_tail(n)` could seek from the end instead of reading everything.~~
**Fixed in `worktree-agent-a1e5b610` branch** (commit `e907d8c`). `read_head(n)` now stops after reading n frames (early return). `read_tail(n)` streams through frames with a `VecDeque` ring buffer of size n, never building a full Vec. 11 new tests including equivalence test against `read_frames()`.

### 1.4 `buffer_size` tracking is incorrect (src/storage/logfile.rs:58)
`buffer_size += encoded.len() * 2` — the `* 2` accounts for writing to both the stream file and combined file, but the actual buffered data is in the OS file buffers, not tracked by this variable. After flushing combined + stream-specific, the other two stream files are also flushed (lines 61-63), which is unnecessary since they had no new data. More importantly, if multiple frames arrive between flushes, only the stream-specific + combined files have unflushed data, but all four are always flushed.

### ~~1.5 Port polling loop never terminates (src/platform/mod.rs:12-23)~~ FIXED
~~`poll_ports` runs an infinite loop that never checks if the child process has exited. After the child terminates, the tokio task continues polling `lsof` on a dead PID forever (or until the parent exits). The task is fire-and-forget with no cancellation token.~~
**Fixed in `worktree-agent-aca55bb7` branch** (commit `996a6c8`). Added `CancellationToken` from `tokio-util`. Polling loop uses `tokio::select!` to race between tick and cancellation. Token cancelled and handle awaited after child exits in `run.rs`.

### 1.6 LLM enrichment opens a *second* database connection (src/llm/enrichment.rs:87,99)
The enrichment task opens a new `Database::open(&config.db_path())` instead of reusing the existing connection. With WAL mode this generally works, but it means the enrichment task re-initializes the schema and opens a fresh connection each time. If the database file is locked or the path changes, this could silently fail (and the `let _ = db.update_service_enrichment(...)` discards errors).

### ~~1.7 Tag format not validated (src/cli/run.rs:34)~~ FIXED
~~Tags without a `:` separator are silently dropped. `--tag "invalid"` produces no error and no tag. Users get no feedback that their tag was ignored.~~
**Fixed in `worktree-agent-a085fac8` branch** (commit `6bbe5ac`). Added `parse_tag()` and `validate_tags()` with clear error messages. Tags without `:` now return an error explaining the `key:value` format. 10 unit tests.

### ~~1.8 `update_run_pid` called after `spawn_wrapped` returns (src/cli/run.rs:68-70)~~ FIXED
~~The PID is written to the database only *after* `spawn_wrapped` completes — meaning the child has already exited. This is too late for any concurrent consumer (e.g., the MCP server showing "running" processes) to use the PID. The background port detection task also receives the PID too late since `spawn_wrapped` is awaited first, by which point the process is dead.~~
**Fixed on master** (commit `91724e2`). `spawn_wrapped` now accepts a `oneshot::Sender<u32>` and sends the PID immediately after fork/spawn. A background task awaits the PID and records it in the DB + starts port detection while the child is still running.

### ~~1.9 Enrichment overwrites user-provided name (src/llm/enrichment.rs:88)~~ FIXED
~~`update_service_enrichment` uses `COALESCE(?1, name)` which overwrites the user's `--name`.~~
**Fixed in `worktree-agent-a32eddcf` branch** (commits `cc69dfc`, `ecfc47e`). Added `has_user_name: bool` parameter to `enrich_service()`. When the user provided `--name`, the LLM-generated name is discarded and `None` is passed to `update_service_enrichment`, preserving the user's choice. Description enrichment still works in both cases. Four new tests verify the behavior.

### ~~1.10 `search_services` SQL injection-safe but logically incorrect for multi-tag filters (src/storage/db.rs:256-264)~~ FIXED
~~When multiple tag filters are provided, they're joined with `OR`, meaning `--tag env:prod --tag team:backend` matches services with *either* tag, not services that have *both* tags. This is likely the wrong semantic for filtering.~~
**Fixed in `worktree-agent-a64ff592` branch** (commit `154d1ea`). Replaced JOIN+OR approach with per-tag `EXISTS` subqueries joined by AND. New test `search_services_by_multiple_tags_uses_and` verifies AND semantics.

### ~~1.11 Partial ID match in `resolve_log_dir` can match wrong service (src/cli/logs.rs:55-62)~~ FIXED
~~The partial match iterates all services and returns the first one whose ID starts with the given prefix. If multiple services share a prefix, the wrong one may be returned without warning.~~
**Fixed on master** (commit `7da4fa0`). `resolve_log_dir` now uses SQL `LIKE` with a limit of 2 — if multiple services match the prefix, it returns an explicit "Ambiguous prefix" error listing the matching IDs.

---

## 2. Maintainability Issues

### ~~2.1 Duplicated `resolve_log_dir` functions~~ FIXED
~~`src/cli/logs.rs:35-65` and `src/mcp/tools.rs:168-181` contain nearly identical `resolve_log_dir` implementations. The CLI version also does partial ID matching while the MCP version does not — inconsistent behavior.~~
**Fixed on master** (commit `68c0b6c`). Consolidated into a `Database::resolve_log_dir()` method.

### ~~2.2 Hardcoded `KNOWN_SUBCOMMANDS` list (src/cli/mod.rs:105-116)~~ FIXED
~~This list must be manually updated whenever a new subcommand is added. If a developer adds a subcommand to the `Commands` enum but forgets this list, direct mode will swallow it. There's no compile-time check to keep them in sync.~~
**Fixed on master** (commit `8a30b90`). Now derived from clap `Commands` enum at compile time.

### ~~2.3 No schema versioning or migrations (src/storage/schema.rs)~~ FIXED
~~The schema uses `CREATE TABLE IF NOT EXISTS` which means columns cannot be added/modified in future versions. There's no version tracking or migration system. Shipping 1.0.0 with this means any schema change in 1.1 will require manual migration tooling.~~
**Fixed in `worktree-agent-a262d98f` branch** (commit `17818eb`). Added `schema_version` table, migration framework with version check on open, and error on newer-than-supported versions. 7 unit tests covering fresh db, re-open, newer version error, and pre-versioning upgrade.

### 2.4 `Database` is not `Send` or thread-safe (src/storage/db.rs:9-11) — BY DESIGN
`Database` wraps a `rusqlite::Connection` which is not `Send`. This forces the enrichment and port detection tasks to open their own connections. This is the correct pattern for SQLite — per-task connections with WAL mode handle concurrent access safely. `Arc<Mutex<Connection>>` would introduce mutex contention in async contexts, which is worse.

### ~~2.5 String-typed enums everywhere~~ FIXED
~~`LogsArgs.stream`, `SearchArgs.stream`, etc. are all `String` instead of enums.~~
**Fixed in `worktree-agent-ae1bcd57` branch** (commits `260b099`, `ee0334e`, `5d5a70a`). Added `StreamFilter` enum (Stdout/Stderr/Stdin/Combined) and `LogMode` enum (Head/Tail/Range) with `clap::ValueEnum`, `serde::Deserialize`, and `schemars::JsonSchema` derives. Updated CLI args, `LogReader::new()`, and all MCP types/tools. Invalid values now produce a clear clap error listing valid options. Comprehensive tests for serde roundtrip, defaults, and string conversions.

### ~~2.6 Error handling inconsistency~~ FIXED
~~Some places use `let _ = ...` to discard errors (enrichment, signal forwarding, PTY I/O), while the main path uses `anyhow::Result`. The fire-and-forget pattern makes it hard to diagnose failures in production. At minimum, errors should be logged via `tracing`.~~
**Fixed in `worktree-agent-aea108b2` branch** (commit `64066cd`). Replaced 16 instances of `let _ =` across 7 files with `tracing::warn!`/`tracing::error!` with descriptive messages. Only remaining `let _ =` is for an unused variable on non-macOS.

### ~~2.7 `row_to_service` / `row_to_service_rusqlite` dual functions (src/storage/db.rs:556-581)~~ FIXED
~~There are two almost-identical row mapping functions — one returning `anyhow::Result`, the other `rusqlite::Error` — just to satisfy different calling contexts. This is unnecessary indirection.~~
**Fixed on master** (commit `0dac778`). Removed redundant wrapper functions.

### ~~2.8 `unwrap_or_default()` on datetime parsing (src/storage/db.rs:573-578)~~ FIXED
~~If the stored RFC3339 string is malformed, `parse_from_rfc3339` returns the epoch (1970-01-01) silently. This masks data corruption issues.~~
**Fixed on master** (commit `40538ef`). Replaced with proper error propagation.

### ~~2.9 `thiserror` and `base64` are unused dependencies (Cargo.toml:24,29)~~ FIXED
~~`thiserror = "2"` is declared but no custom error types use `#[derive(Error)]`. `base64 = "0.22"` is declared but never used anywhere in the codebase. Dead dependencies increase compile times.~~
**Removed in `worktree-agent-ac1565b1` branch** (commit `f27d0a7`). Both dependencies deleted from Cargo.toml.

### 2.10 `async-trait` may be unnecessary — WON'T FIX
`async-trait` is required because `LlmClient` is used as `Box<dyn LlmClient>`. Native `async fn in trait` (stable since Rust 1.75) produces `impl Future` return types that make the trait not dyn-compatible. Removing `async-trait` would require an enum dispatch pattern, which adds complexity for no practical benefit.

---

## 3. Potential Optimizations

### ~~3.1 Log file reading is fully synchronous and unbuffered~~ FIXED
~~`LogReader` uses `std::fs::File` with raw `read_exact` calls. For large log files, a `BufReader` wrapper would significantly reduce syscall overhead. Additionally, all log reading happens synchronously in an async context — blocking the tokio runtime.~~
**Fixed on master** (commit `18303d5`). All `File::open` calls in `LogReader` now wrap with `BufReader`. `read_one_frame` made generic over `impl Read`.

### ~~3.2 `read_range` and `read_tail` read entire file~~ FIXED
~~Currently reads the entire file to get the last N frames. The binary frame format has fixed-size headers, but variable payloads prevent simple reverse seeking. However, the file could be memory-mapped or scanned from a known offset (stored in metadata) to avoid reading the entire file.~~
**Fixed on master** (commit `18303d5`). `read_range` now streams frame-by-frame, skipping frames before `start_time` and breaking early past `end_time`. `read_tail` already used a ring buffer (fixed earlier in 1.3).

### ~~3.3 `search` reads all frames into memory (src/storage/logfile.rs:170)~~ FIXED
~~Even for `max_matches = 1`, the entire file is read into a `Vec<Frame>` first. A streaming approach that reads and matches frame-by-frame would use constant memory.~~
**Fixed on master** (commit `18303d5`). `search` now streams frame-by-frame, stops at `max_matches` without loading the entire file.

### 3.4 Combined log is redundant storage
Every frame is written to both a stream-specific file and `combined.log`, doubling disk usage. The combined view could be reconstructed by merge-sorting the three stream files by timestamp, trading CPU at read time for 50% storage savings.

### ~~3.5 `list_services` in `resolve_log_dir` loads all services for partial match (src/cli/logs.rs:55)~~ FIXED
~~A SQL `WHERE id LIKE ?1 || '%'` query would be far more efficient than loading all services and filtering in Rust.~~
**Fixed on master** (commit `7da4fa0`). Replaced with SQL `LIKE` query.

### ~~3.6 `reqwest::Client` is reconstructed per LLM call (src/llm/openai.rs:62-64)~~ FIXED
~~Each `complete()` call builds a new `reqwest::Client`. The client should be constructed once and reused, as it manages a connection pool internally.~~
**Fixed on master** (commit `7afde24`). Client is now reused across LLM calls.

### ~~3.7 `serde_json::to_string_pretty` in MCP responses (src/mcp/mod.rs:54,72,90)~~ FIXED
~~Pretty-printing JSON for machine-to-machine MCP communication adds unnecessary whitespace. `to_string` would be more efficient.~~
**Fixed on master** (commit `f652d67`). Uses compact JSON serialization.

### ~~3.8 Unnecessary cloning in `handle_run` (src/cli/run.rs:88-93)~~ FIXED
~~Multiple values are cloned to move into the enrichment tokio::spawn closure: `service_id_enrich`, `config_enrich`, `working_dir_enrich`, `command_enrich`, `tags_enrich`, `desc_enrich`. The original values are not used after this point, so they could be moved directly.~~
**Fixed on master** (commit `3da4fb0`). Values moved into closures instead of cloning.

---

## 4. UX Improvements

### ~~4.1 No `--version` output in direct mode~~ FIXED
~~Running `brainlog --version` correctly shows version info, but `KNOWN_SUBCOMMANDS` includes `--version` only to avoid misinterpreting it as a command. The version string itself isn't configured in `Cargo.toml` metadata (no `authors`, `license`, or `repository` fields).~~
**Fixed on master** (commit `5c8ec58`). Added `license`, `repository`, and `authors` to Cargo.toml. Subcommand list is now derived from clap enum (2.2).

### ~~4.2 No progress or status indicators during run~~ FIXED
~~When wrapping a long-running process, brainlog provides no indication that it's capturing logs. A brief startup message (e.g., `[brainlog] Capturing output...` on stderr) would confirm it's active without polluting stdout.~~
**Fixed on master** (commit `dae4753`). Added startup indicator showing which command is being captured.

### 4.3 `list` output truncates service ID to 8 characters (src/cli/list.rs:42,91-96)
IDs are UUIDs (36 chars) but only 8 are shown. With many services, 8-char prefixes may collide. Additionally, the `{:<8}` fixed width means IDs are never padded or truncated consistently — if the ID is shorter than 8 chars (it won't be, but the display assumes it).

### ~~4.4 No way to delete services or runs~~ FIXED
~~There's no `brainlog delete` or `brainlog clean` command. Old services and their log files accumulate indefinitely. Users have no way to reclaim disk space short of manually deleting `~/.brainlog/`.~~
**Fixed in `worktree-agent-a81875a5` branch** (commit `e2a8663`). Added `brainlog delete` subcommand with `--force` flag, resolves by run ID / service ID / service name, cascade-deletes ports/runs/tags and log directories. 6 unit tests.

### ~~4.5 No confirmation or output after `run` completes~~ FIXED
~~After a wrapped command finishes, brainlog exits silently. A brief summary on stderr (e.g., `[brainlog] Run abc12345 completed (exit 0), logs at ~/.brainlog/logs/...`) would help users find their logs.~~
**Fixed on master** (commit `f14de06`). Prints completion summary with run ID and log path.

### 4.6 `follow` mode has no way to exit cleanly (src/cli/logs.rs:75-87)
The follow loop is infinite with no Ctrl+C handling or timeout. While Ctrl+C will kill the process, there's no "Press q to quit" or graceful shutdown messaging.

### ~~4.7 `search` date formatting loses timezone info (src/cli/search.rs:43)~~ FIXED
~~`DateTime::from_timestamp(...).unwrap_or_default()` creates a UTC datetime, but the `%H:%M:%S` format shows time without any timezone indicator. Users in non-UTC timezones will see confusing timestamps.~~
**Fixed on master** (commit `7ac630d`). Search result timestamps now include `UTC` indicator.

### ~~4.8 No color support in terminal output~~ FIXED
~~`list`, `logs`, and `search` output is plain text. Colorizing error-level log lines, status indicators (running=green, failed=red), and search match highlights would improve readability.~~
**Fixed on master** (commit `1beb57f`). Added terminal colours using owo-colors with TTY detection.

### ~~4.9 `--tag` requires `key:value` format with no guidance on error~~ FIXED
~~Passing `--tag production` silently does nothing. Passing `--tag a:b:c` splits on the first colon, making the value `b:c`, which may be unexpected.~~
**Fixed together with 1.7** in `worktree-agent-a085fac8` branch.

### ~~4.10 `logs` with unknown service gives a confusing error~~ FIXED
~~`brainlog logs nonexistent` returns `No service or run found matching 'nonexistent'` — it could suggest running `brainlog list` to see available services.~~
**Fixed on master** (commit `c97deca`). Error messages now suggest `brainlog list` when a target is not found.

### 4.11 No `--json` output option
For scripting and piping, there's no way to get machine-readable output from `list`, `logs`, or `search`. The MCP server provides structured data, but CLI users have to parse tabular text.

### ~~4.12 `search` only searches log content, not metadata~~ FIXED
~~`brainlog search false` intuitively feels like it should find the service that ran `false`, but it only searches log file content via regex. Users expect search to also match against command names, service names, ports, and tags.~~
**Fixed in `worktree-agent-a1b9bb69` branch** (commits `c6f0c97`, `ff6c1fd`, `bbac2f3`). Search now matches service metadata (name, command, tags, description) by default, with `--logs-only` flag to restrict to log content only. `ServiceMetadataMatch` struct and `search_services_by_pattern()` method added. 9 unit tests.

---

## 5. Documentation & README

### ~~5.1 No README.md exists~~ FIXED
~~The repository has no README. For a 1.0.0 release, this is essential.~~
**Fixed previously** (commit `089009b`). README.md with install, usage, MCP server, storage format, and license sections.

### 5.2 No inline documentation on public API
`src/lib.rs` exports all modules publicly but has no doc comments. Key types like `Database`, `LogWriter`, `LogReader`, `Config`, `Frame`, `Service`, `Run` have no `///` documentation. This matters for anyone consuming brainlog as a library crate.

### ~~5.3 No `config.yaml` example or documentation~~ FIXED
~~Users must read source code to discover configuration options.~~
**Fixed in `worktree-agent-a5648cd3` branch** (commit `548a5e1`). Added `examples/config.yaml` with all 5 config sections fully documented with defaults and explanations.

### ~~5.4 No `--help` text for direct mode~~ FIXED
~~`brainlog --help` shows clap's generated help for subcommand mode, but doesn't explain that `brainlog <cmd>` works as a direct wrapper.~~
**Fixed in `worktree-agent-a5648cd3` branch** (commit `d903476`). Added `long_about` to Cli struct showing direct mode, explicit mode, and management commands with examples.

### ~~5.5 MCP tool descriptions are minimal~~ FIXED
~~The MCP tool descriptions (e.g., "Discover tracked services") are functional but don't explain expected input formats (e.g., tag format `key:value`), default values, or example usage. LLM agents benefit from richer tool descriptions.~~
**Fixed on master**. All four tool descriptions now include parameter formats (tag syntax, regex syntax, nanosecond timestamps), default values, usage patterns (e.g., incremental polling with `since`), and behavioral notes (e.g., `wait_for_pattern` defaults to matching only new lines).

### 5.6 No CHANGELOG or release notes
For a 1.0.0 release, a CHANGELOG.md describing the initial feature set establishes a baseline for future releases.

### ~~5.7 No LICENSE file~~ FIXED
~~The Cargo.toml has no `license` field and there's no LICENSE file.~~
**Fixed in `worktree-agent-af784c5c` branch** (commit `469dbda`). Added MIT LICENSE file and `license = "MIT"` to Cargo.toml.

### ~~5.8 `plan.md` and `prompts/` are development artifacts~~ FIXED
~~These files document the AI-assisted development process but shouldn't be in a 1.0 release. They should either be removed or moved to a `docs/` directory.~~
**Fixed on master** (commit `19056f4`). Removed development planning artifacts.

---

## 6. Security Considerations

### ~~6.1 API keys stored in plaintext YAML (src/config/mod.rs:22)~~ FIXED
~~`LlmConfig.api_key` is stored as `Option<String>` in `~/.brainlog/config.yaml`. The config file has no special permissions set, and the key appears in plaintext. Should support environment variable references (e.g., `$ANTHROPIC_API_KEY`) or keychain integration.~~
**Fixed in `worktree-agent-a20abee6` branch** (commit `3d37ab9`). Added `resolve_api_key()` with 3-tier resolution: `$ENV_VAR` expansion, literal passthrough, provider-based fallback (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`). 9 unit tests.

### ~~6.2 No file permission restrictions on config or database~~ FIXED
~~`~/.brainlog/` directory, `config.yaml`, and `brainlog.db` are created with default permissions (typically 0644/0755). The database contains full command histories and the config may contain API keys. These should be created with restrictive permissions (0600/0700).~~
**Fixed in `worktree-agent-a6fb0518` branch** (commits `8d77423`, `5be2958`). New `permissions` module with `create_dir_restricted` (0700) and `set_file_restricted` (0600). Applied to db dir/file, log dirs, and log files. Best-effort on restricted filesystems. 4 unit tests.

### 6.3 Command injection via Anthropic CLI (src/llm/anthropic.rs:38-42)
The Anthropic client shells out to `claude -p <prompt>`. While `tokio::process::Command` passes arguments safely (not through a shell), the `claude` binary itself may interpret the prompt content in unexpected ways. Additionally, if `claude` is not installed, the error message could be confusing.

### 6.4 Regex denial of service in search (src/cli/search.rs:13, src/mcp/tools.rs:112)
User-supplied regex patterns are compiled without size or complexity limits. Pathological patterns (e.g., `(a+)+$`) can cause exponential backtracking. The `regex` crate has built-in protections, but no explicit timeout or complexity limit is set.

### 6.5 No input sanitization on MCP tool parameters
MCP tool inputs (service names, search patterns, IDs) are passed directly to SQL queries via parameterized statements (safe from SQL injection), but could contain excessively long strings. No length limits are enforced.

### 6.6 Log files may contain sensitive data
Brainlog captures all stdout/stderr/stdin, which may include passwords, tokens, database credentials, or other secrets printed by the wrapped process. There's no filtering, redaction, or warning about this. The log files are readable by the user's default umask.

### 6.7 `unsafe` usage in PTY code (src/process/pty.rs:33,136,150)
Three unsafe blocks: `forkpty`, `libc::poll`, and `BorrowedFd::borrow_raw`. The `forkpty` call is inherently unsafe but correctly used. The `poll` call and `borrow_raw` are safe in practice but the `borrow_raw` fd could theoretically be invalid if the master fd was already closed.

### 6.8 Port detection via `lsof` runs with parent's privileges (src/platform/macos.rs:4)
`lsof -p <pid>` is invoked with the same user privileges as brainlog. This is generally fine, but if brainlog is run as root (e.g., wrapping a privileged service), `lsof` could expose information about all processes.

### 6.9 No authentication on MCP server
The MCP server (stdio transport) has no authentication mechanism. Any process that can connect to its stdio can query all service logs and metadata. This is expected for stdio-based MCP, but should be documented as a trust boundary.

### 6.10 `enrichment.project_file_patterns` reads arbitrary files
The enrichment system reads files matching configured patterns (package.json, Cargo.toml, etc.) from the working directory and sends their contents to an LLM. If the working directory contains sensitive files that match these patterns, their contents could be exfiltrated to the LLM provider.

---

## Summary

| Category | Critical | Major | Minor | Fixed |
|----------|----------|-------|-------|-------|
| Logic Errors | 0 | 0 | 2 (1.4, 1.6) | ~~1.1~~, ~~1.2~~, ~~1.3~~, ~~1.5~~, ~~1.7~~, ~~1.8~~, ~~1.9~~, ~~1.10~~, ~~1.11~~ |
| Maintainability | 0 | 0 | 0 | ~~2.1~~, ~~2.2~~, ~~2.3~~, 2.4 (by design), ~~2.5~~, ~~2.6~~, ~~2.7~~, ~~2.8~~, ~~2.9~~, 2.10 (won't fix) |
| Optimizations | 0 | 0 | 1 (3.4) | ~~3.1~~, ~~3.2~~, ~~3.3~~, ~~3.5~~, ~~3.6~~, ~~3.7~~, ~~3.8~~ |
| UX | 0 | 1 (4.11) | 2 (4.3, 4.6) | ~~4.1~~, ~~4.2~~, ~~4.4~~, ~~4.5~~, ~~4.7~~, ~~4.8~~, ~~4.9~~, ~~4.10~~, ~~4.12~~ |
| Documentation | 0 | 1 (5.2) | 1 (5.6) | ~~5.1~~, ~~5.3~~, ~~5.4~~, ~~5.5~~, ~~5.7~~, ~~5.8~~ |
| Security | 0 | 1 (6.6) | 6 | ~~6.1~~, ~~6.2~~ |

**Original top-10 priorities — ALL DONE:**
1. ~~Fix exit code truncation (1.1)~~ -- DONE
2. ~~Add README and LICENSE (5.1, 5.7)~~ -- DONE
3. ~~Restrict file permissions on config/db (6.2)~~ -- DONE
4. ~~Support env vars for API keys (6.1)~~ -- DONE
5. ~~Add schema versioning (2.3)~~ -- DONE
6. ~~Use typed enums for stream/mode args (2.5)~~ -- DONE
7. ~~Add `delete`/`clean` command (4.4)~~ -- DONE
8. ~~Fix enrichment overwriting user-provided names (1.9)~~ -- DONE
9. ~~Remove unused deps `thiserror`, `base64` (2.9)~~ -- DONE
10. ~~Add example config and direct-mode help text (5.3, 5.4)~~ -- DONE

**Additional fixes (beyond top 10):**
11. ~~Tag validation with error feedback (1.7 + 4.9)~~ -- DONE
12. ~~Search metadata matching (4.12)~~ -- DONE
13. ~~Port polling cancellation (1.5)~~ -- DONE
14. ~~Multi-tag AND filter (1.10)~~ -- DONE
15. ~~Error handling consistency (2.6)~~ -- DONE
16. ~~PID recorded immediately after spawn (1.8)~~ -- DONE
17. ~~Partial ID ambiguity detection (1.11)~~ -- DONE
18. ~~Cargo.toml metadata: authors, license, repository (4.1)~~ -- DONE
19. ~~Search timestamps include UTC indicator (4.7)~~ -- DONE
20. ~~Unknown service suggests `brainlog list` (4.10)~~ -- DONE
21. ~~Duplicated resolve_log_dir consolidated (2.1)~~ -- DONE
22. ~~KNOWN_SUBCOMMANDS derived from clap enum (2.2)~~ -- DONE
23. ~~Redundant row_to_service wrappers removed (2.7)~~ -- DONE
24. ~~datetime parsing error propagation (2.8)~~ -- DONE
25. ~~SQL LIKE for prefix match (3.5)~~ -- DONE
26. ~~reqwest::Client reuse (3.6)~~ -- DONE
27. ~~Compact JSON in MCP (3.7)~~ -- DONE
28. ~~Values moved into closures (3.8)~~ -- DONE
29. ~~Startup indicator (4.2)~~ -- DONE
30. ~~Completion summary (4.5)~~ -- DONE
31. ~~Terminal colours (4.8)~~ -- DONE
32. ~~Development artifacts removed (5.8)~~ -- DONE
33. ~~BufReader for log I/O (3.1)~~ -- DONE
34. ~~Streaming read_range (3.2)~~ -- DONE
35. ~~Streaming search (3.3)~~ -- DONE
36. ~~Rich MCP tool descriptions (5.5)~~ -- DONE
37. Database not Send (2.4) -- BY DESIGN (SQLite + WAL per-task connections is correct)
38. async-trait (2.10) -- WON'T FIX (required for dyn LlmClient compatibility)

---

## Merged Branches

All branches merged to master and verified (2026-02-23). Worktrees cleaned up.

| Branch | Fix | Status |
|--------|-----|--------|
| `worktree-agent-ab3a6de7` | 1.1 Exit code truncation | MERGED |
| `worktree-agent-a32eddcf` | 1.9 Enrichment name overwrite | MERGED |
| `worktree-agent-ae1bcd57` | 2.5 Typed stream/mode enums | MERGED |
| `worktree-agent-ac1565b1` | 2.9 Remove unused deps | MERGED |
| `worktree-agent-a6fb0518` | 6.2 File permissions | MERGED |
| `worktree-agent-a20abee6` | 6.1 Env var API keys | MERGED |
| `worktree-agent-a81875a5` | 4.4 Delete command | MERGED |
| `worktree-agent-af784c5c` | 5.7 LICENSE file | MERGED |
| `worktree-agent-a5648cd3` | 5.3/5.4 Example config + help | MERGED |
| `worktree-agent-a64ff592` | 1.10 Multi-tag AND filter | MERGED |
| `worktree-agent-aca55bb7` | 1.5 Port polling cancellation | MERGED |
| `worktree-agent-aea108b2` | 2.6 Error handling consistency | MERGED |
| `worktree-agent-a085fac8` | 1.7/4.9 Tag validation | MERGED |
| `worktree-agent-a262d98f` | 2.3 Schema versioning | MERGED |
| `worktree-agent-a1b9bb69` | 4.12 Search metadata | MERGED |
| `worktree-agent-a98e2b91` | 1.2 follow_logs O(n^2) fix | MERGED |
| `worktree-agent-a1e5b610` | 1.3 read_head/read_tail efficiency | MERGED |
