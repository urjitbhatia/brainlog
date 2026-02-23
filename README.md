<p align="center">
  <img src="brand/brainlog.png" alt="brainlog" width="128" />
</p>

<h1 align="center">brainlog</h1>

<p align="center">
Transparent process wrapper that captures stdout, stderr, and stdin streams with an MCP server for LLM agent access.
</p>

---

Brainlog wraps any executable via PTY, recording every byte of I/O in a framed binary format while the process runs normally. An MCP server exposes the captured logs to LLM agents for discovery, retrieval, and search.

## Install

```
cargo install --path .
```

## Usage

Wrap any command directly:

```bash
brainlog node server.js
brainlog -n my-api python app.py
brainlog -n worker -t env:prod -d "background job runner" ./run.sh
```

Or use the explicit `run` subcommand:

```bash
brainlog run -- cargo build
```

### View logs

```bash
brainlog list                     # list tracked services
brainlog logs <service|run-id>    # view logs
brainlog logs <id> --tail 50      # last 50 lines
brainlog logs <id> -f             # follow (like tail -f)
brainlog logs <id> -s stderr      # stderr only
```

### Search

```bash
brainlog search "ERROR|WARN"
brainlog search "panic" --service my-api
```

### MCP server

```bash
brainlog mcp
```

Exposes three tools over stdio transport:

| Tool | Description |
|------|-------------|
| `discover_services` | Find tracked services by name, tags, port, executable, or status |
| `get_logs` | Retrieve logs with head/tail/range modes |
| `search_logs` | Regex search across services with timestamps and context |

## Storage

```
~/.brainlog/
  brainlog.db                          # SQLite (WAL mode) — service metadata
  logs/<run-id>/
    stdout.log, stderr.log, stdin.log  # framed binary logs
    combined.log                       # interleaved stream
```

Frame format: `[timestamp_ns:u64 LE][stream_type:u8][length:u32 LE][payload]`

## Agent Review

**Score: 7/10** — Reviewed by Claude Code (Opus 4.6), 2026-02-22

Used BrainLog MCP during a multi-step feature implementation (Slack notifications for auto-matched watchlist items). The core loop of discover → tail → search worked well and provided real value during end-to-end testing.

**What worked well:**
- Service discovery found the API server and web UI quickly
- Tailing logs gave real-time visibility into server behavior without asking the user to copy/paste terminal output
- Error search across services was fast and confirmed clean state after a database migration
- Overall, the tool tightened the feedback loop between "user does something in the UI" and "agent verifies what happened server-side"

**What would get it to 10/10:**
- Service discovery returns too many unnamed entries (MCP self-instances, no auto-naming)
- Log output includes raw ANSI escape codes — needs a strip option
- Port auto-detection didn't work for the web UI
- No incremental polling (`since` cursor) — had to re-fetch and eyeball diffs
- A `wait_for_pattern` blocking call would be transformative for E2E observation

See [UX_FEEDBACK.md](./UX_FEEDBACK.md) for detailed feedback.

## License

MIT
