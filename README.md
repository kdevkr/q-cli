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
q-cli       gc     <conn>            # .Q.gc[] -> bytes returned to the OS
q-cli       ping   <conn>
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
`@`=default). Config: `$Q_CLI_CONFIG` or `~/.config/q-cli/servers.conf`:
```text
default = local
local   = localhost:5555
prod    = bigbox:5001:user:pass
```

**Output modes** (default: aligned text):
- `--json` / `-j` — JSON. Tables → array of row objects; keyed tables merge
  key+value columns; temporal types → formatted strings. Text/JSON cap at 50 rows.
- `--csv` — CSV (tables only, RFC4180-escaped, **uncapped**).
- `--console` — the q server formats the result with `.Q.s` (max fidelity).

**Subcommands** — `tables` / `meta <t>` / `count <t>` wrap `tables[]` / `meta t` /
`count t` for quick schema discovery.

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
