<p align="center">
  <img src="brand/brainlog.png" alt="brainlog" width="128" />
</p>

<h1 align="center">brainlog</h1>

<p align="center">
Transparent process wrapper that captures stdout, stderr, and stdin — with an MCP server so LLM agents can read your terminal output.
</p>

<p align="center">
  <a href="https://urjitbhatia.github.io/brainlog/"><strong>Website</strong></a>
</p>

---

Brainlog wraps any command via PTY, recording every byte of I/O while the process runs exactly as it would without brainlog. Colors, interactive prompts, TUI apps — everything works transparently. An MCP server gives LLM agents direct access to discover, read, search, and watch process output without the user copy-pasting anything.

## Install

Install the latest release binary (macOS and Linux) — no repo checkout, no Rust toolchain required:

```bash
curl -fsSL https://raw.githubusercontent.com/urjitbhatia/brainlog/master/install.sh | sh
```

This downloads the prebuilt binary for your platform (Apple Silicon & Intel macOS, x86_64 & aarch64 Linux), verifies its checksum, and installs it to `~/.local/bin`. Linux builds are statically linked (musl), so they run on any distro including Alpine. Set `BRAINLOG_INSTALL_DIR` to install elsewhere, or `BRAINLOG_VERSION` to pin a specific version.

### From source

With a Rust toolchain installed, build straight from the repo:

```bash
cargo install --git https://github.com/urjitbhatia/brainlog
```

Or from a local checkout:

```bash
cargo install --path .
```

> Note: brainlog is Unix-only — it relies on a pseudo-terminal, so macOS and Linux are supported but Windows is not.

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

Launch under the daemon (returns immediately, service runs in the background):

```bash
brainlog daemon start            # one-time, per machine
brainlog -D -n my-api node server.js
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

### Daemon mode

Run services in the background under a single supervisor, instead of holding
a terminal foreground per process:

```bash
brainlog daemon start             # start the singleton daemon (one per user)
brainlog daemon status            # show running daemon + supervised services
brainlog daemon stop              # stop the daemon (sends SIGTERM to its children)

brainlog -D -n api node server.js # launch under the daemon, returns immediately
brainlog run --daemon -- ./run.sh # same, via the explicit subcommand
```

Each `-D` invocation hands the command to the daemon, which spawns a detached
brainlog wrapper for it. Logs, names, tags, and `--restart` all behave
exactly as they do in foreground mode — `brainlog logs <name>`, `brainlog
kill <name>`, etc. work unchanged. Closing the terminal does not stop the
service; only `brainlog kill <name>` or `brainlog daemon stop` does.

The daemon is a per-user singleton enforced by `fcntl` locking on
`~/.brainlog/daemon.pid`; a second `daemon start` is a no-op.

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

## Releasing

Releases are built and published automatically by the [`Release` workflow](.github/workflows/release.yml). To cut a release:

1. Bump `version` in `Cargo.toml` (and run a build so `Cargo.lock` updates), then commit to `master`.
2. Tag the commit with a matching `v` prefix and push it:

   ```bash
   git tag v0.4.1
   git push origin v0.4.1
   ```

The workflow builds binaries for macOS (Apple Silicon + Intel) and Linux (x86_64 + aarch64, statically linked via musl), packages each as a `.tar.gz` with a SHA-256 checksum, and attaches them to a [GitHub Release](https://github.com/urjitbhatia/brainlog/releases). The tag version must match `Cargo.toml` or the workflow fails. Once a Release is published, the [install script](install.sh) can find and download those binaries. You can also trigger a build manually from the Actions tab via **workflow_dispatch**.

## License

MIT
