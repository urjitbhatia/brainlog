---
name: followup questions to idea prompt
description: questions for the initial seed idea prompt
---

Clean repo, fresh start. Here are my clarifying questions:

  ---
  1. CLI Interface & Process Lifecycle

  - For usage like brainlog run -- npm start, should brainlog also support a daemon mode where it manages multiple services
   (brainlog start --name web-frontend -- npm start, brainlog stop web-frontend, brainlog list)? Or is each brainlog
  invocation purely a foreground wrapper (like time) and the MCP server discovers running instances through some shared
  state (e.g., a local directory of log files)?
  - When you say "capture stdin" — do you mean recording what the user types into the wrapped process (passthrough + tee),
  or is this mainly for non-interactive services where stdin is rarely used?

  2. Log Storage

  - Where should logs be persisted? Options:
    - A well-known directory like ~/.brainlog/logs/<service-id>/
    - A SQLite database for structured querying
    - Both (files for raw streams, SQLite for metadata/indexing)
  - Should logs be stored as raw byte streams (preserving ANSI color, binary data) or stripped to plain text?
  - Any retention policy, or infinite until manually purged?

  3. Service Metadata & Tagging

  - You mentioned tags, ports, file descriptors, executable name. How are these specified?
    - Tags: via CLI flags (brainlog run --tag project:pimlico --tag role:web-frontend -- npm start)?
    - Ports: auto-detected from the child process's network activity (e.g., via lsof/netstat polling), or user-declared
  (--port 3000)?
    - Description: free-text flag (--desc "Pimlico web frontend")?

  4. Service Identity

  - You mentioned brainlog generates a "unique service ID or handle." Should this be:
    - An auto-generated UUID/short-hash per invocation?
    - A user-provided name that persists across restarts (e.g., --name web-frontend so the same service always has the same
   identity)?
    - Both (user name + auto run-id per invocation)?

  5. MCP Server Architecture

  - Should the MCP server be a separate long-running process (brainlog mcp-server) that the LLM connects to? Or embedded
  into the brainlog wrapper itself?
  - For the "internal agent" that does fuzzy service matching (e.g., "the web frontend for the Pimlico project") — are you
  envisioning brainlog making an LLM API call itself to do the semantic matching, or should it be simpler (keyword/fuzzy
  search on tags + description) and let the calling LLM handle the reasoning?

  6. Log Querying

  - For the get_logs tool, should it support:
    - Tail mode (last N lines)?
    - Time-range filtering (logs from the last 5 minutes)?
    - Grep/search within logs?
    - Streaming/follow mode (like tail -f)?
  - How large could logs get? Should we think about pagination or chunking for LLM context window limits from the start?

  7. Platform Scope

  - macOS-first, or Linux + macOS from day one? Signal forwarding and port detection differ across platforms.

  8. Transparency Guarantees

  - You mentioned forwarding signals, networking, file descriptors. For signals and FDs, this is straightforward with
  exec-style spawning. For "networking state like ports" — do you mean the child should bind ports as if brainlog wasn't
  there (which happens naturally), or something more like network namespace proxying?

  ---
  These answers will significantly shape the architecture. Which of these do you want to nail down first?