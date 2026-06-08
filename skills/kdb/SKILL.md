---
name: kdb
description: Work with q / kdb+ — run queries against a running q process and analyze the results, write or review q/k code, and set up or operate a kdb-tick stack (tickerplant / RDB / HDB). Use whenever the user mentions q, kdb, kdb+, tick, tickerplant, qsql, a .q file, or asks to query/inspect a q process. Drives the native `q-cli` IPC client.
---

# kdb / q assistant

This skill drives **`q-cli`** — a native Rust kdb+ IPC client that speaks the
kdb+ wire protocol directly over TCP. No `q.exe`, no license, no startup noise.

**Prerequisite:** the `q-cli` binary must be on PATH —
`cargo install --git https://github.com/kdevkr/q-cli` (or build the repo with
`cargo build --release` and put `q-cli` on PATH).

## Commands (call via the **Bash** tool; `q-cli` is on PATH)

```
q-cli [OUT] query    <conn> "<q expr>"|-   run a q expr   (- = stdin)
q-cli [OUT] run      <conn> <path.q>|-     send a .q file (- = stdin)
q-cli [OUT] tables|meta|count|describe|schema <conn> [table]   schema discovery
q-cli [OUT] functions <conn> [ns]          server-side helpers + arity
q-cli [OUT] info|time <conn> ["<q expr>"]  health snapshot / profile a query
q-cli       gc|ping  <conn>                .Q.gc[] / liveness probe
q-cli       web|trace <action> <conn>      HTTP tuning / error backtraces
q-cli       config   <init|path|list|add>  server profiles
```

- **`query`/`run`** run a q expr / send a `.q` file (`-` = read from stdin).
- **`tables`/`meta`/`count`/`describe`/`schema`** — schema discovery. `schema`
  maps the **whole process** in one call (rows/cols/partition per table) and
  `describe <t>` profiles **one table** in one call (partitioning + columns + rows
  + sample). Start with `schema`, then `describe` the tables you care about.
- **`functions [ns]`** — list server-side helpers + arity; reuse them, don't
  reinvent. **`info`/`time`/`gc`** — health snapshot / profile a query (`ms`+count,
  not data) / garbage-collect. **`ping`** — handshake probe. **`web`/`trace`** —
  tune built-in HTTP / wrap handlers in `.Q.trp` for call-stack backtraces.

**Full per-command, flag, `<conn>`, and config detail:
[references/commands.md](references/commands.md).** The essentials below:

- **`<conn>`** — `host:port[:user:pass]`, or a config name (`@name`/`name` →
  config; `@`/`default` → default). Config is global + project (project wins);
  set up with `q-cli config init`.
- **`[OUT]`** (default aligned text, 50-row cap) — `--json` (array of row objects)
  · `--csv` (uncapped) · `--console` (q formats via `.Q.s`) · `--max-rows N`
  (0 = all) · `--timeout MS` (0 = forever) · `--readonly` (refuse mutating q).
- **Exit codes** — branch on these, not text: `0` ok · `2` usage/policy · `3`
  connection (refused/unreachable) · `4` q error · `5` timeout (query too slow —
  **not** a refused connection). `--json` errors: `{"error","kind"}` on stderr.

## Key gotchas

- **Quote q expressions in single quotes** (`'select from t'`). q symbols use
  backticks (`` `sym ``); a backtick inside double quotes is command-substitution /
  an escape in bash & PowerShell — single quotes pass it through verbatim.
  `` `AAPL `` silently becomes `AAPL` (undefined var → `'AAPL` error) if double-quoted.
- Failures print `ERR ...` on stderr with a non-zero exit. `trace on` upgrades a
  bare `'type` into a full call stack — turn it on while debugging.
- `web`/`trace`/handler changes are **runtime-only** (reset on q restart). To
  persist, add the equivalent `.z.*` lines to the q startup script.

## Workflow A — query & analyze a running q process

1. **Confirm host:port** (ask if unknown), then `q-cli ping <hp>` to verify.
   `q-cli info <hp>` is a good first look (version, memory, handles).
2. **Discover schema before querying** — never assume columns. Map the whole
   process first with `q-cli schema <hp>`, then drill into a table with
   `q-cli describe <hp> <table>`; `tables`/`meta`/`count` are finer-grained fallbacks.
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
- Reuse server-side helpers rather than reinventing them — list them with
  `q-cli functions <hp>` (root) or `q-cli functions <hp> .u` (a namespace).
- Validate snippets with `q-cli run <hp> file.q` (or `query`) against a scratch
  process, then read the output — show the result, not just the code. Turn on
  `trace` while debugging to see where a failing snippet errors.

## References

- **[references/commands.md](references/commands.md)** — full command, flag,
  `<conn>`, config, and exit-code reference.
- **[references/partitioning.md](references/partitioning.md)** — partitioned HDBs:
  the four partition domains (`date`/`month`/`year`/`int`), date vs int/hourly
  partitioning, the virtual partition column, `.Q.dpft` write-down, `par.txt`
  segmentation, query patterns, and caveats. Read it for anything about
  partitioning, HDB layout, write-down, or large histories.
- **[references/tick.md](references/tick.md)** — kdb-tick stack (tickerplant / RDB /
  HDB) setup and operation, and the end-of-day write-down flow.

## Notes

- A q server started with `-p N` binds IPv4 `0.0.0.0`; `q-cli` already tries both
  `::1` and `127.0.0.1` for `localhost`, so either form works.
- On Windows, avoid low ports in reserved ranges (e.g. 5000 may fail with WSAEACCES
  on hosts with Hyper-V/WSL); high ports are safe for scratch servers.
- Output caps text/JSON tables at 50 rows; `--csv` is uncapped. Say so if a result
  looks truncated.
