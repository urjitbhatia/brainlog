<p align="center">
  <img src="brand/brainlog.png" alt="brainlog" width="128" />
</p>

<h1 align="center">brainlog</h1>

<p align="center">
Transparent process wrapper that captures stdout, stderr, and stdin — with an MCP server so LLM agents can read your terminal output.
</p>

---

Brainlog wraps any command via PTY, recording every byte of I/O while the process runs exactly as it would without brainlog. Colors, interactive prompts, TUI apps — everything works transparently. An MCP server gives LLM agents direct access to discover, read, search, and watch process output without the user copy-pasting anything.

## Install

```bash
cargo install --path .
```

Or with `cargo-binstall` (if a release binary is available):

```bash
cargo binstall brainlog
```

## Quick start

Wrap any command — brainlog is invisible to the process:

```bash
brainlog node server.js
brainlog python app.py
brainlog cargo build
```

Name your services for easy reference:

```bash
brainlog -n my-api node server.js
brainlog -n worker -t env:prod -d "background job runner" ./run.sh
```

Auto-restart on exit:

```bash
brainlog run --restart -- node server.js
```

Resume a previous service (new run, same name):

```bash
brainlog run --resume my-api -- node server.js
```

## CLI

### View logs

```bash
brainlog list                     # list tracked services (newest first)
brainlog list -g                  # group by executable + working directory
brainlog logs <id>                # view logs (by name, service ID, or run ID)
brainlog logs <id> --tail 50      # last 50 lines
brainlog logs <id> -f             # follow (like tail -f)
brainlog logs <id> -s stderr      # stderr only
```

### Search

```bash
brainlog search "ERROR|WARN"                    # regex across all services
brainlog search "panic" --service my-api        # scoped to one service
```

### Process control

```bash
brainlog kill my-api              # send SIGTERM (graceful)
brainlog kill my-api -f           # send SIGKILL (force)
brainlog kill my-api -s HUP      # send specific signal
brainlog restart my-api           # restart via wrapper (SIGUSR1)
```

### Housekeeping

```bash
brainlog purge --before 7d                     # delete services older than 7 days
brainlog purge --before 1h --name tmp --dry-run # preview what would be purged
```

## MCP server

```bash
brainlog mcp
```

### Setup with Claude Code

Add to your `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "brainlog": {
      "command": "brainlog",
      "args": ["mcp"]
    }
  }
}
```

### Tools

| Tool | Description |
|------|-------------|
| `list_recent_runs` | Last N runs across all services, newest first — best for "what just happened?" |
| `discover_services` | Find tracked services by name, cwd, tags, port, executable, status, or exit code |
| `get_logs` | Read stdout/stderr/stdin by service ID, run ID, or working directory |
| `search_logs` | Regex search across all services with timestamps |
| `wait_for_pattern` | Block until a regex appears in output (like Playwright's `waitForText`) |
| `kill_service` | Send a signal to stop a running process |
| `restart_service` | Restart a running process via its wrapper |

### Agent workflow

1. **`list_recent_runs`** — "What just ran?" See the last N runs with status, exit codes, and optional log previews.
2. **`discover_services`** — "What's running in this project?" Filter by `cwd` to scope to the current repo.
3. **`get_logs`** — Read output by ID or by `cwd` shorthand (no need to discover first).
4. **`search_logs`** — Find errors, warnings, or specific patterns across all tracked commands.
5. **`wait_for_pattern`** — Start a server, then wait for `"listening on port 3000"` before proceeding.

## How it works

- **PTY proxy**: Brainlog allocates a pseudo-terminal so the wrapped process behaves identically — `isatty()` returns true, colors and interactive prompts work, terminal dimensions are inherited.
- **Framed binary logs**: Every byte of stdout, stderr, and stdin is recorded with nanosecond timestamps in a compact frame format.
- **Auto-naming**: Services without `-n` get a derived name from the working directory and executable (e.g. `myproject/node-a3f2c1`).
- **LLM enrichment**: When configured, brainlog uses an LLM to generate a human-readable service name and description from the command line and initial output.

## Storage

```
~/.brainlog/
  brainlog.db                          # SQLite (WAL mode) — service + run metadata
  config.yaml                          # optional LLM enrichment config
  logs/<run-id>/
    stdout.log, stderr.log, stdin.log  # per-stream framed binary logs
    combined.log                       # interleaved stream
```

Frame format: `[timestamp_ns:u64 LE][stream_type:u8][length:u32 LE][payload]`

## License

MIT
