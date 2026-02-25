# BrainLog UX Feedback & Status

## Completed

1. **[DONE] List should order by createdAt descending** — `brainlog list` now shows newest first with `CREATED` as a default column.

2. **[DONE] Group repeated commands** — `brainlog list -g` (or `--group`) groups services by executable + working directory, showing run count and latest status. Collapses noise (e.g., 16 services to 4 groups).

3. **[DONE] MCP search by cwd** — `discover_services` accepts a `cwd` parameter for substring-matching the working directory of tracked commands.

5. **[DONE] MCP list shows latest commands first** — MCP `discover_services` returns services ordered by most recent activity.

7. **[DONE] Resume flag on exit** — When a command exits, brainlog prints a resume hint: `brainlog --resume <name> <command>` (for derived names) or `brainlog -n <name> <command>` (when `-n` was provided). The `--resume` flag registers the new run under the same service name. Derived names are compact: `workdir/executable-<hash>`.

8. **[DONE] Auto-naming from command + working directory** — Services without `-n` get a derived name like `brainlog/echo-f0fd68` (dir basename + executable + 6-char hash of full command). No more `name: null` in MCP results.

9. **[DONE] ANSI escape codes in log output** — `strip_ansi` parameter on `get_logs`, `search_logs`, and `wait_for_pattern`. Defaults to true. Uses `strip-ansi-escapes` crate with a proper VT parser for comprehensive stripping (CSI, OSC, DEC private mode, etc.).

12. **[DONE] `wait_for_pattern` blocking call** — MCP tool that blocks until a regex appears in logs (with timeout). Supports alternation regex, stream filtering, ANSI stripping, and configurable poll interval.

13. **[DONE] Kill a running process** — `brainlog kill <name|id>` sends SIGTERM (or `--force` for SIGKILL, `--signal` for custom) to the entire process tree. Also kills wrapper processes.

15. **[DONE] `wait_for_pattern` `since` parameter** — Defaults to current time so only new log lines are matched. Set `since=0` to search full history. Fixes the stale log matching issue where the tool returned immediately (0ms) matching old/buffered lines.

**[DONE] `brainlog restart` + `--restart` auto-restart** — `brainlog restart <name>` sends SIGUSR1 to the wrapper process to restart the child. `brainlog --restart <command>` auto-restarts on any exit (except SIGINT/SIGTERM). Wrapper PID stored in DB for remote restart.

**[DONE] `brainlog purge`** — `brainlog purge --before 10d` cleans up old services and logs. Supports `--command` filter and `--force` (also kills running/stale services).

**[DONE] List column truncation** — `brainlog list` truncates NAME and COMMAND columns to fit terminal width. Footer tip shows resume syntax.

**[DONE] Completion summary on exit** — Prints run ID and log path when a command finishes.

**[DONE] Startup indicator** — Shows which command is being captured when brainlog starts.

---

## Open

4. **Allow users to delete/cleanup old commands** — `brainlog purge --before` exists but could be more granular (e.g., purge by name pattern, interactive selection).

6. **Auto port detection based on file descriptors** — Port detection works for some cases (found Claude Code's internal ports) but missed a web server on port 5174 that was visible in its own logs. Needs investigation.

10. **Port detection missed the web UI** — Related to #6. Web server listening on port 5174 showed empty `ports` array. May need log-based port detection as a fallback.

11. **`since` cursor for incremental log polling** — `get_logs` could benefit from a `since` parameter (like `wait_for_pattern` now has) for clean incremental polling without re-reading old frames.

14. **Terminal output colours** — Would be nice to add colours to CLI output: `--resume` flag, list table headers, status indicators, etc.

---

## Multi-Agent Observability Notes (2026-02-23)

Context: Tested wrapping Claude Code with brainlog (`brainlog run --name "claude_observer_test" -- claude`) so one agent can observe another.

**What works:**
- `--name` flag is the clean way to identify agent sessions. The observing agent uses `discover_services(name="claude_observer_test")` and it works instantly.
- Grouping collapses 16 services to 4 groups — massively reduces noise for the observer.
- `tail_lines` on discover gives a quick preview without a follow-up `get_logs` call.
- Port detection found Claude Code's internal ports (55798, 55803).

**What doesn't work well:**
- Claude Code's output is TUI-based (cursor movements, screen redraws, DEC private mode sequences). The VT parser-based `strip_ansi` handles most cases now, but extremely complex TUI apps may still have artifacts.
- No parseable session identity in Claude Code's stdout. The status line has project path + model but it's buried in TUI noise. Parsing it is fragile — `--name` is the right approach.
- `BRAINLOG_SERVICE_NAME` env var would be a nice alternative to `--name` for scripted/automated launches.

**Conclusion:** Agent-observes-agent via brainlog works. The `--name` + `discover_services` + `get_logs(strip_ansi=true)` pipeline is the path.
