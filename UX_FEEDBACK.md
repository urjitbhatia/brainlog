1. The list should order by createdAt descending, so newest first, and createdAt should be one of the default list columns without the verbose flag. 
2. We need to think about what happens when the same command is run repeatedly. For example, a local dev server that might be killed because of an error and then restarted. How do we resurface the fact that it's the same command but it's the latest run of the same command? How do we let people select from that history? I'm guessing that timestamp would be, again, the useful thing. Can we provide a -g or a - -group flag in list so that we group the same commands together by created at time. So for example if I have make dev for my local development server and if I run make dev again and again the group should be make dev and obviously that group will be based on the metadata except the created at and some of the other common args but we know if it was from the same directory and if I run make dev or if I run make dev with some extra arguments it should all be the same group based on the directory and the original command.
3. mcp should also let us search by cwd of the command we are tracking.
4. allow users to delete/cleanup old commands and their logs. brainlog purge --before 10[h|m|s|d] (hours,min,sec,days)
5. mcp list tool should also show latest commands first
6. is auto port detection based on file descriptors working?
7. when the command is exited, or dies, We should print a --resume flag Similar to what claude code does So that a user can rerun the command with... So that... and then it'll get registered under the same name as if it was restarted. something like `Run: brainlog --resume <name> <command> <args>` to register this as a restart. when you detect a --resume based restart, inject an artificial log so that brainlog knows it was restarted. also, when the command dies/is killed etc, note the way it was stopped in the artificail log/and the exit code of the process brainlog was monitoring. That way when we inspect the logs, we know exactly what happened. Also, --resume should not assume anything else other than the fact that the user just wants to register against the same name. They can run a completely different command or args, it doesn't matter. we will just make a new entry in the db for that command and rename the old row with _superseeded or something similar.
---

## MCP Agent Feedback (from Claude Code session, 2026-02-22)

Context: Used BrainLog MCP to observe API server logs during an end-to-end test of a Slack notification feature. Discovered services, tailed logs, searched for errors, and verified behavior in real-time.

7. **Service discovery is noisy with MCP self-entries** — `discover_services()` returned ~11 unnamed `--mcp` entries (brainlog's own server instances) alongside the 2 actual app services. A way to filter these out (e.g. `exclude_self: true` or auto-tagging system services) would reduce noise.

8. **Auto-naming from command + working directory** — Most services came back with `name: null`. The web UI was `pnpm run dev:with-binding` from `/Users/urjit/code/pimlico/web` but showed as unnamed. Inferring a name like `pimlico_web_dev` from the working dir basename + command would make discovery much faster without requiring manual tagging.

9. **ANSI escape codes in log output** — Logs include raw ANSI color codes (`\u001b[32m`, etc.) which add visual noise for programmatic consumers. A `strip_ansi: true` option on `get_logs` and `search_logs` would help.

10. **Port detection missed the web UI** — Web server was clearly listening on port 5174 (visible in its own logs: `Ready on http://localhost:5174`) but `ports` was empty and filtering by `port: 5174` returned nothing. Related to #6.

11. **`since` cursor for incremental log polling** — During the E2E test, I polled `get_logs(tail=20)` repeatedly and eyeballed what was new. A `since` parameter (timestamp or opaque cursor) that returns only lines newer than the last read would make incremental polling clean.

12. **`wait_for_pattern` blocking call** — A tool that blocks until a regex appears in the logs (with timeout), like Playwright's `wait_for_text`. Example: `wait_for_pattern(id, pattern="Added regulation|error", timeout=30)`. This turns "poll and hope" into precise observation — ideal for agents verifying async behavior end-to-end.

---

## Multi-Agent Observability (2026-02-23)

Context: Tested wrapping Claude Code with brainlog (`brainlog run --name "claude_observer_test" -- claude`) so one agent can observe another.

**What works:**
- `--name` flag is the clean way to identify agent sessions. The observing agent uses `discover_services(name="claude_observer_test")` and it works instantly.
- Grouping collapses 16 services to 4 groups — massively reduces noise for the observer.
- `tail_lines` on discover gives a quick preview without a follow-up `get_logs` call.
- Port detection found Claude Code's internal ports (55798, 55803).

**What doesn't work well:**
- Claude Code's output is TUI-based (cursor movements, screen redraws, DEC private mode sequences). The `strip_ansi` regex only handles CSI color codes and OSC sequences, not `\x1b[?2026h`, `\x1b[I`, `\x1b[O` etc. Need to expand the regex to cover DEC private mode and other terminal control sequences.
- No parseable session identity in Claude Code's stdout. The status line has project path + model but it's buried in TUI noise. Parsing it is fragile — `--name` is the right approach.
- `BRAINLOG_SERVICE_NAME` env var would be a nice alternative to `--name` for scripted/automated launches.

**Conclusion:** Agent-observes-agent via brainlog works. The `--name` + `discover_services` + `get_logs(strip_ansi=true)` pipeline is the path. Main gap is better terminal escape sequence stripping for TUI apps.

---

## Process Control

13. **Kill a running process** — `brainlog kill <name|id>` to send SIGTERM (or SIGKILL with `--force`) to a process being monitored by brainlog. Useful for killing stuck processes or using brainlog as a central service control plane. Should also be exposed as an MCP tool so agents can stop services programmatically.