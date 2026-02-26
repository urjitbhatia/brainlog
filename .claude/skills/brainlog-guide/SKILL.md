# Brainlog Guide

You have access to Brainlog via MCP. Brainlog wraps terminal commands and captures their stdout, stderr, and stdin — so you can read what commands have printed without asking the user to copy-paste.

## When to use Brainlog

- **After running a command**: You ran `make build`, `npm start`, `cargo test` etc. via Bash — check brainlog for the output instead of parsing the Bash result, especially for long-running commands.
- **Debugging failures**: Something failed or is misbehaving. Use `discover_services` to see tracked commands, then `get_logs` to read their stderr.
- **Verifying async startup**: You started a server or background process. Use `wait_for_pattern` to block until it prints "listening on port", "ready", "started" etc. before proceeding.
- **Searching for errors**: Use `search_logs` with a regex like `error|panic|fatal|ENOENT` across all tracked commands to find problems fast.
- **Monitoring**: Poll a running command's output using `get_logs` with the `since` parameter to only see new output since your last check.

## Workflow patterns

### Check why something failed
1. `discover_services` — find the command, check its status and exit code
2. `get_logs(id, stream="stderr")` — read the error output
3. `search_logs(pattern="error|failed")` — if the error isn't obvious, search for it

### Start and verify a server
1. Run the server command via Bash
2. `wait_for_pattern(id, pattern="listening|ready|started", timeout=30)` — confirm it's up
3. Proceed with your next steps knowing the server is ready

### Incremental monitoring
1. `get_logs(id, lines=10)` — get the latest output
2. Note the timestamp of the last line
3. Later: `get_logs(id, since=<timestamp>)` — get only new output since then

## When NOT to use Brainlog

- If the user already pasted the error or output — don't re-fetch it
- For commands not wrapped by brainlog — `discover_services` will tell you what's tracked
- For simple one-shot commands where Bash output is sufficient
