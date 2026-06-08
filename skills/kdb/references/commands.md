# q-cli command, flag & config reference

Full detail for every `q-cli` command, output flag, connection form, and exit
code. SKILL.md carries the quick summary; read this when you need the specifics.

## Synopsis

```
q-cli [OUT] query    <conn> "<q expression>"   # or `-` to read the expr from stdin
q-cli [OUT] run      <conn> <path.q>           # or `-` to read q source from stdin
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
q-cli       --version | -V | --help | -h
```

## Commands

- **`query`** → run a q expression, print result. **`run`** → send a `.q` file
  (single expression / `;`-separated; multi-statement scripts: load on server).
  Both accept `-` as the argument to read the q source from **stdin** (pipe a
  generated or multi-line expression).
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
- **`--version` / `-V`** → print the version and exit. **`--help` / `-h`** → usage.

## `<conn>` — connection target

`host:port[:user:pass]` literal, OR a config name:
- `@name` / `name` → look up `servers.{name}` in the config file.
- `@` / `default` → the configured default server.

Config is two layers, **project overriding global**: global (`$Q_CLI_CONFIG` or
`~/.config/q-cli/servers.conf`) plus the nearest `.q-cli.conf` walking up from the
CWD. Lines `name = host:port[:user:pass]`, plus `default = <name>`.

First-time setup:
- `q-cli config init` scaffolds the global file (add `--project` for `./.q-cli.conf`).
- `q-cli config add [--project] <name> <conn>` adds/updates an entry.
- `q-cli config list` shows the merged set tagged `[global]`/`[project]` (passwords
  masked).
- `q-cli config path` shows both layers and which project file is active.

## `[OUT]` — output mode (default: aligned text, capped at 50 rows)

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
  forms stay allowed). Heuristic, not a sandbox; keywords inside a string literal
  (e.g. `… like "*delete*"`) are ignored so reads aren't false-blocked.

## Exit codes — branch on these instead of parsing text

- `0` ok
- `2` usage/policy (bad args, unknown server, readonly block)
- `3` connection (refused/unreachable, incl. connect/handshake timeout — retry/ping)
- `4` q error (fix the query)
- `5` timeout (the query round-trip exceeded `--timeout` — raise it or simplify the
  query; **don't** treat it like a refused connection)

With `--json`, errors are `{"error","kind"}` on stderr.
