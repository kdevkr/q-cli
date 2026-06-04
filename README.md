# q-cli

A native **kdb+/q IPC client** in Rust. Connects to a running q process over TCP,
speaking the kdb+ wire protocol directly — no `q.exe`, no license, no dependencies
(pure `std`). Single self-contained binary, cross-platform.

## Usage
```
q-cli [OUT] query  <conn> "<q expression>"
q-cli [OUT] run    <conn> <path.q>
q-cli [OUT] tables <conn>
q-cli [OUT] meta   <conn> <table>
q-cli [OUT] count  <conn> <table>
q-cli [OUT] describe <conn> <table>  # partitioning + schema + rows + sample (1 call)
q-cli [OUT] info   <conn>            # version, pid, port, memory, handles
q-cli [OUT] time   <conn> "<q expr>" # elapsed ms + result row count
q-cli       gc     <conn>            # .Q.gc[] -> bytes returned to the OS
q-cli       ping   <conn>
q-cli       web    <off|get-ok|status> <conn>   # tune built-in HTTP serving
q-cli       trace  <on|off|status> <conn>       # .Q.trp backtraces on errors
q-cli       config <init|path|list|add>
```

```sh
# first-time setup — scaffold ~/.config/q-cli/servers.conf, then edit it
q-cli config init
q-cli config add prod bigbox:5001:user:pass     # add/update an entry
q-cli config list                               # show servers (passwords masked)

q-cli ping  localhost:5555
q-cli query localhost:5555 'select avg price by sym from trade'
q-cli --json query @prod 'select sym,price from trade'    # array of row objects
q-cli --csv  query @prod 'select from trade' > trade.csv  # export (uncapped)
q-cli --console query @ 'flip 0!exec sym from trade'      # q renders it (.Q.s)
q-cli tables @          # list tables on the default server
q-cli meta  @ trade     # column schema
```

**`<conn>`** — `host:port[:user:pass]`, or a config name (`@name` / `name` /
`@`=default). Config is **two layers, project overriding global**:
```text
# global:  $Q_CLI_CONFIG, else ~/.config/q-cli/servers.conf
# project: nearest .q-cli.conf walking up from the current directory
default = local
local   = localhost:5555
prod    = bigbox:5001:user:pass
```
A per-project `.q-cli.conf` (e.g. checked into a repo) overlays the global one —
same-named entries (including `default`) win. `$Q_CLI_CONFIG`, if set, is used
alone. Manage either layer:
```sh
q-cli config init --project          # scaffold ./.q-cli.conf
q-cli config add  --project dev localhost:6000
q-cli config list                    # merged view, each entry tagged [global]/[project]
q-cli config path                    # show both layers and which project file is active
```

**Output modes & flags** (default: aligned text):
- `--json` / `-j` — JSON. Tables → array of row objects; keyed tables merge
  key+value columns; temporal types → formatted strings.
- `--csv` — CSV (tables only, RFC4180-escaped, **uncapped**).
- `--console` — the q server formats the result with `.Q.s` (max fidelity).
- `--max-rows N` — row cap for text/json (default 50; `0` = unlimited). When a
  table is capped, a `note: showing N of M rows` is printed to **stderr** so it's
  machine-detectable; stdout stays pure data.
- `--readonly` (or `Q_CLI_READONLY=1`) — reject arbitrary q that looks mutating
  (`delete`/`update`/`insert`/`upsert`/`set`/`hdel`/`hopen`/`hclose`/`system`/
  `exit`/`dpft`/`0:`/`1:`). A heuristic denylist for `query`/`run`/`time`, **not a
  sandbox** — guards an agent from accidentally mutating data.

**Exit codes** (for scripting / agents): `0` ok · `2` usage/policy (bad args,
unknown server, readonly block) · `3` connection (refused/timeout/handshake) ·
`4` q error (server returned `'…`). With `--json`, errors print as
`{"error":"…","kind":"usage|connect|query"}` on stderr.

**Subcommands** — `tables` / `meta <t>` / `count <t>` for quick schema discovery;
`describe <t>` returns partitioning + columns + row count + a small sample in
**one** call (JSON-friendly); `gc` runs `.Q.gc[]`; `info` is a health snapshot
(version/pid/port/used/heap/peak/handles/timer); `time <expr>` returns `ms` +
result `count` (not the data) — e.g. `q-cli time @ 'select avg price by sym from trade'`.

**`web` — built-in HTTP serving.** A q process serves IPC *and* HTTP on the same
port, so a browser to `http://host:port/` gets a default page. Tune it at runtime:
- `q-cli web off    <conn>` — close every HTTP request (GET + POST).
- `q-cli web get-ok <conn>` — GET returns `200 OK` (body `OK`); POST/other closed.
- `q-cli web status <conn>` — show the current `.z.ph` / `.z.pp` handlers.

Runtime-only (resets on restart). To persist, put the same lines in the q startup
script, e.g. for `get-ok`:
```q
.z.ph:{.h.hy[`txt;"OK"]};   / HTTP GET  -> 200 OK
.z.pp:{hclose .z.w};        / HTTP POST -> closed
```

**`trace` — backtraces on query errors.** By default a failing query returns just
`'type`; `q-cli trace on <conn>` wraps the IPC handlers with `.Q.trp` so errors
come back with a `.Q.sbt` call-stack (and also print it to the server console):
```
ERR q error 'type
  [3]  {x+`a}
         ^
  [2]  {x+`a}[1]
```
`trace off` restores the default handlers; `trace status` shows them. Runtime-only;
the equivalent startup line is:
```q
.z.pg:.z.ps:{.Q.trp[value;x;{-2 .Q.sbt y;'x,"\n",.Q.sbt y}]};
```

- Errors print `ERR ...` on stderr, exit code 1; a q-side error shows as
  `ERR q error '<msg>`.
- `localhost` tries both `::1` and `127.0.0.1` (a q server `-p N` binds IPv4 only).

## How it works
1. **Handshake** — TCP connect, send `creds + \x03\x00`, read 1-byte capability.
2. **Sync query** — serialize the expression as a char vector (type 10) inside a
   sync message (`msgtype=1`), send, read the 8-byte response header + payload.
3. **Deserialize** — parse the returned K object ([src/k.rs](src/k.rs)).
4. **Render** — text or JSON ([src/render.rs](src/render.rs)).

Compressed responses (non-loopback, >2 KB) are decompressed via the canonical
kdb+ algorithm; loopback connections are never compressed.

## Layout
| file | role |
|------|------|
| [src/main.rs](src/main.rs)     | CLI args, connection, handshake, sync send/recv |
| [src/k.rs](src/k.rs)           | K type model, wire deserialization, temporal formatting, decompress |
| [src/render.rs](src/render.rs) | text & JSON rendering |

## Build & install
```sh
cargo build --release            # in D:\q\q-cli
cp target/release/q-cli.exe ~/.local/bin/    # (Windows: Copy-Item ... )
```

Requires the Rust GNU toolchain on Windows (`stable-x86_64-pc-windows-gnu`) — no
MSVC build tools needed.

Or install straight from git:
```sh
cargo install --git https://github.com/kdevkr/q-cli
```

## Agent skill (`kdb`)

This repo also ships a Claude / agent **skill** at [`skills/kdb/SKILL.md`](skills/kdb/SKILL.md)
that teaches an agent how and when to drive `q-cli`. Install it with the
[`skills`](https://github.com/vercel-labs/skills) CLI:

```sh
npx skills add kdevkr/q-cli            # interactive: pick agent(s)
npx skills add kdevkr/q-cli --list     # list skills in this repo
npx skills add kdevkr/q-cli -y -g      # non-interactive, install globally
```

It installs `skills/kdb/SKILL.md` into your agent's skills dir (`.claude/skills/`
or `.agents/skills/`). The skill needs the `q-cli` binary on PATH — install it
with the `cargo install` line above.
