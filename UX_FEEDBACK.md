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

11. **[DONE] `since` cursor for incremental log polling** — `get_logs` now accepts a `since` parameter (nanoseconds since epoch). Works with head, tail, and range modes. Omit to get all frames (backward compatible).

14. **[DONE] Terminal output colours** — Coloured status indicators (green=running, yellow=completed, red=failed), bold headers, dim tips/paths, cyan resume commands. Uses `owo-colors` with TTY detection — plain text when piped.

**[DONE] `BRAINLOG_SERVICE_NAME` env var** — Alternative to `--name` for scripted/automated launches. Priority: `--name` flag > env var > `--resume` > derived name. Also propagated to child processes.

---

6. **[DONE] Port detection** — Implemented via `lsof` + `pgrep` on macOS with 2-second polling across the full process tree. Stores detected ports in SQLite, exposed via MCP `discover_services` with port filtering. Earlier reports of missed ports (#10) were likely timing issues, not a fundamental gap.

---

## Open

4. **Allow users to delete/cleanup old commands** — `brainlog purge --before` exists but could be more granular (e.g., purge by name pattern, interactive selection).

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
- `BRAINLOG_SERVICE_NAME` env var is now supported as an alternative to `--name`.

**Conclusion:** Agent-observes-agent via brainlog works. The `--name` + `discover_services` + `get_logs(strip_ansi=true)` pipeline is the path.
