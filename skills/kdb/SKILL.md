---
name: kdb
description: Work with q / kdb+ — run queries against a running q process and analyze the results, write or review q/k code, and set up or operate a kdb-tick stack (tickerplant / RDB / HDB). Use whenever the user mentions q, kdb, kdb+, tick, tickerplant, qsql, a .q file, or asks to query/inspect a q process. Drives the native `q-cli` IPC client.
---

# kdb / q assistant

This skill drives **`q-cli`** — a native Rust kdb+ IPC client that speaks the
kdb+ wire protocol directly over TCP. No `q.exe`, no license, no startup noise.

**Prerequisite:** the `q-cli` binary must be on PATH. Install it with:
```sh
cargo install --git https://github.com/kdevkr/q-cli
```
(or build the repo with `cargo build --release` and put `q-cli` on PATH).

## The tool: how to call it

Use the **Bash** tool. `q-cli` is on PATH:

```
q-cli [OUT] query  <conn> "<q expression>"
q-cli [OUT] run    <conn> <path.q>
q-cli [OUT] tables <conn>
q-cli [OUT] meta   <conn> <table>
q-cli [OUT] count  <conn> <table>
q-cli [OUT] info   <conn>
q-cli [OUT] time   <conn> "<q expr>"
q-cli       gc     <conn>
q-cli       ping   <conn>
q-cli       web    <off|get-ok|status> <conn>
q-cli       trace  <on|off|status>     <conn>
q-cli       config <init|path|list|add>
```

**Commands**
- **`query`** → run a q expression, print result. **`run`** → send a `.q` file
  (single expression / `;`-separated; multi-statement scripts: load on server).
- **`ping`** → `pong` if the handshake succeeds.
- **`tables`** / **`meta <t>`** / **`count <t>`** → fast schema discovery.
- **`info`** → server health snapshot (version, pid, port, used/heap/peak memory,
  open handles, timer) as a dict.
- **`time <expr>`** → time the expression on the server; returns `ms` + result
  `count` (not the data). Use to profile a query before pulling its full result.
- **`gc`** → `.Q.gc[]` (garbage-collect, returns bytes freed to the OS). Can briefly
  pause the process — confirm on prod.
- **`web <off|get-ok|status>`** → tune the q process' built-in HTTP serving (q serves
  IPC + HTTP on one port). `off` closes all HTTP; `get-ok` makes GET return `200 OK`
  and closes POST; `status` shows `.z.ph`/`.z.pp`.
- **`trace <on|off|status>`** → wrap `.z.pg`/`.z.ps` with `.Q.trp` so query errors
  return a `.Q.sbt` backtrace (call stack) instead of a bare `'type`. Great for
  debugging a failing query.
- **`config`** → manage server profiles (see below).

**`<conn>`** — `host:port[:user:pass]` literal, OR a config name:
- `@name` / `name` → look up `servers.{name}` in the config file.
- `@` / `default` → the configured default server.
- Config: `$Q_CLI_CONFIG` or `~/.config/q-cli/servers.conf`, lines
  `name = host:port[:user:pass]`, plus `default = <name>`.
- First-time setup: `q-cli config init` scaffolds the file (won't overwrite);
  `q-cli config add <name> <conn>` adds an entry; `q-cli config list` shows them
  (passwords masked); `q-cli config path` prints the location.

**`[OUT]`** output mode (default: aligned text, capped at 50 rows):
- `--json` → JSON; tables → **array of row objects**, keyed tables merge key+value.
- `--csv` → CSV (tables only, **uncapped** — for export/piping).
- `--console` → let the **q server** format via `.Q.s` (max fidelity for exotic /
  deeply-nested types).

**Gotchas**
- Failures print `ERR ...` on stderr, non-zero exit (connection refused, bad query
  → `q error '...`, unknown server, etc.). `trace on` turns the bare error into a
  full call stack.
- **Quote q expressions in single quotes** (`'select from t'`). q symbols use
  backticks (`` `sym ``); a backtick inside double quotes is command-substitution /
  an escape char in bash & PowerShell — single quotes pass it through verbatim.
  `` `AAPL `` silently becomes `AAPL` (undefined var → `'AAPL` error) if double-quoted.
- Many `web`/`trace`/handler changes are **runtime-only** (reset on q restart). To
  persist, add the equivalent `.z.*` lines to the q startup script.

## Workflow A — query & analyze a running q process

1. **Confirm host:port** (ask if unknown), then `q-cli ping <hp>` to verify.
   `q-cli info <hp>` is a good first look (version, memory, handles).
2. **Discover schema before querying** — never assume columns:
   `q-cli tables <hp>`, `q-cli meta <hp> <table>`, `q-cli query <hp> '5#trade'`.
3. **Aggregate, then explain.** Prefer `select ... by ...` over pulling raw rows;
   read the returned table and tell the user what it shows. Use `--json` if you
   need to post-process numbers; `time` to profile a heavy query first.
4. **Guard side effects.** `query` runs arbitrary q on the server. Never send
   `delete`/`update`/`hdel`/`system`/`exit` without explicit user confirmation —
   treat the connection as production unless told otherwise.

## Workflow B — write / review q code

- Match kdb idioms: vector-first, avoid loops, use `select`/`update`/`exec`, qsql
  over manual indexing, functional forms only when needed.
- Reuse any helper functions already defined on the server rather than reinventing
  them (check with `q-cli query <hp> 'key \`.f'` or list root vars).
- Validate snippets with `q-cli run <hp> file.q` (or `query`) against a scratch
  process, then read the output — show the result, not just the code. Turn on
  `trace` while debugging to see where a failing snippet errors.

## Workflow C — kdb-tick setup & operation

A standard tick stack:
- **tickerplant:** `q tick.q <schema> <logdir> -p 5010`
- **RDB:** `q tick/r.q :5010 :5012 -p 5011`
- **HDB:** `q <hdbpath> -p 5012`

Operate via `q-cli`:
- Verify each process is up: `q-cli ping localhost:<port>` / `q-cli info ...`.
- Inspect tp subscribers: `q-cli query localhost:5010 '.u.w'`.
- RDB row counts: `q-cli query localhost:5011 'count each tables[]'`.
- For end-of-day, confirm the `.u.end` / `.Q.dpft` flow before triggering anything
  that writes/clears partitions.

## Notes
- A q server started with `-p N` binds IPv4 `0.0.0.0`; `q-cli` already tries both
  `::1` and `127.0.0.1` for `localhost`, so either form works.
- On Windows, avoid low ports in reserved ranges (e.g. 5000 may fail with WSAEACCES
  on hosts with Hyper-V/WSL); high ports are safe for scratch servers.
- Output rendering caps text/JSON tables at 50 rows; `--csv` is uncapped. Say so if
  a result looks truncated.
