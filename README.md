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

## License

MIT
