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
q-cli [OUT] tables   <conn>
q-cli [OUT] meta     <conn> <table>
q-cli [OUT] count    <conn> <table>
q-cli [OUT] describe <conn> <table>
q-cli [OUT] schema   <conn>
q-cli [OUT] functions <conn> [ns]
q-cli [OUT] info     <conn>
q-cli [OUT] time     <conn> "<q expr>"
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
- **`describe <t>`** → partitioning (`.Q.qp`/`.Q.pf`) + columns/types (`meta`) + row
  count + a small sample, in **one** call (JSON-friendly). Prefer this as the first
  look at an unknown table — it surfaces the partition column so you know what to
  constrain first.
- **`schema`** → every table on the process in **one** call: row count, column
  count, and partition field per table. The fastest first map of an unknown process
  — run it before `describe`-ing individual tables, so you query the right ones.
- **`functions [ns]`** → list the functions defined in a namespace (root by
  default; pass `.u`, `.Q`, etc.) with each one's arity. Use it to find and reuse
  server-side helpers (`vwap`, `.u.upd`, …) instead of reinventing them.
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
- Config is two layers, **project overriding global**: global (`$Q_CLI_CONFIG` or
  `~/.config/q-cli/servers.conf`) plus the nearest `.q-cli.conf` walking up from the
  CWD. Lines `name = host:port[:user:pass]`, plus `default = <name>`.
- First-time setup: `q-cli config init` scaffolds the global file (add `--project`
  for `./.q-cli.conf`); `q-cli config add [--project] <name> <conn>` adds an entry;
  `q-cli config list` shows the merged set tagged `[global]`/`[project]`;
  `q-cli config path` shows both layers.

**`[OUT]`** output mode (default: aligned text, capped at 50 rows):
- `--json` → JSON; tables → **array of row objects**, keyed tables merge key+value.
- `--csv` → CSV (tables only, **uncapped** — for export/piping).
- `--console` → let the **q server** format via `.Q.s` (max fidelity for exotic /
  deeply-nested types).
- `--max-rows N` → cap text/json at N rows (default 50; `0` = unlimited). On a cap,
  a `note: showing N of M rows` goes to **stderr** (stdout stays pure data) — use a
  small N to save context, `0` when you need every row.
- `--timeout MS` (or `Q_CLI_TIMEOUT`) → how long to wait for the query round-trip,
  ms (default 30000; **`0` = wait forever**; also caps connect at 5000). Only a
  round-trip timeout is **exit 5** (the query is too slow); an unreachable host or
  connect timeout is **exit 3**. Raise it before a known-heavy `select` (or use
  `0`); lower it (e.g. `--timeout 3000`) for a quick liveness probe.
- `--readonly` (or `Q_CLI_READONLY=1`) → refuse mutating q (`delete`/`update`/`set`/
  `hdel`/`system`/`exit`/`0:`…) on `query`/`run`/`time`, and block the
  server-mutating `web off`/`web get-ok` / `trace on`/`trace off` (their `status`
  forms stay allowed). Heuristic, not a sandbox.

**Exit codes** — branch on these instead of parsing text: `0` ok · `2` usage/policy
(bad args, unknown server, readonly block) · `3` connection (refused/unreachable,
incl. connect/handshake timeout — retry/ping) · `4` q error (fix the query) · `5`
timeout (the query round-trip exceeded `--timeout` — raise it or simplify the
query; **don't** treat it like a refused connection). With `--json`, errors are
`{"error","kind"}` on stderr.

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
2. **Discover schema before querying** — never assume columns. Map the whole
   process first with `q-cli schema <hp>` (every table → rows/cols/partition in one
   call), then drill into a specific table with `q-cli describe <hp> <table>`
   (partitioning + columns/types + row count + sample in one call); `tables`/`meta`/
   `count` are the finer-grained fallbacks.
3. **Check partitioning FIRST for any historical/HDB table, before writing a
   query.** Many tables are partitioned (by `date`, or `month`/`year`/`int`):
   - `q-cli query <hp> '.Q.qp trade'` → `1b` if partitioned.
   - `q-cli query <hp> '.Q.pf'` → the partition field (`` `date ``/`` `int ``/…).
   - `q-cli query <hp> '.Q.pv'` → which partitions exist (e.g. dates available).
   Then **always put the partition column first in the `where` clause** — q only
   opens the matching directories; without it, it scans the whole DB:
   ```
   select vwap:size wavg price by sym from trade
     where date=2020.06.25, sym=`AAPL        / date (the partition col) constrained FIRST
   ```
   For int/hourly-partitioned HDBs constrain on `int` instead of `date`. Background
   and details: read [references/partitioning.md](references/partitioning.md).
4. **Aggregate, then explain.** Prefer `select ... by ...` over pulling raw rows;
   read the returned table and tell the user what it shows. Use `--json` if you
   need to post-process numbers; `time` to profile a heavy query first.
5. **Guard side effects.** `query` runs arbitrary q on the server. Never send
   `delete`/`update`/`hdel`/`system`/`exit` without explicit user confirmation —
   treat the connection as production unless told otherwise.

## Workflow B — write / review q code

- Match kdb idioms: vector-first, avoid loops, use `select`/`update`/`exec`, qsql
  over manual indexing, functional forms only when needed.
- Reuse any helper functions already defined on the server rather than reinventing
  them — list them with `q-cli functions <hp>` (root) or `q-cli functions <hp> .u`
  (a namespace); it shows each function's arity.
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

## Reference resources
- **[references/partitioning.md](references/partitioning.md)** — partitioned HDBs:
  the four partition domains (`date`/`month`/`year`/`int`), date vs int/hourly
  partitioning, the virtual partition column, `.Q.dpft` write-down, `par.txt`
  segmentation, query patterns, and caveats. Read it whenever the user asks about
  partitioning, HDB layout, date vs int partitions, write-down, or large histories.

## Notes
- A q server started with `-p N` binds IPv4 `0.0.0.0`; `q-cli` already tries both
  `::1` and `127.0.0.1` for `localhost`, so either form works.
- On Windows, avoid low ports in reserved ranges (e.g. 5000 may fail with WSAEACCES
  on hosts with Hyper-V/WSL); high ports are safe for scratch servers.
- Output rendering caps text/JSON tables at 50 rows; `--csv` is uncapped. Say so if
  a result looks truncated.
