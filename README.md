# q-cli

A native **kdb+/q IPC client** in Rust. Connects to a running q process over TCP or TLS,
speaking the kdb+ wire protocol directly — no `q.exe` or license required.
Single self-contained binary, cross-platform.

## Build & Install

```sh
# Standard build (zero external dependencies)
cargo build --release

# With optional TLS support
cargo build --release --features tls

cp target/release/q-cli ~/.local/bin/
```

- **Windows**: Built with OS-native Schannel (no external DLLs required).
- **Linux**: `--features tls` requires OpenSSL dev headers (`libssl-dev` / `openssl-devel`).
- **macOS**: Built with OS-native Security.framework.

Install directly from git:
```sh
cargo install --git https://github.com/kdevkr/q-cli --features tls
```

Or self-update anytime via `q-cli update`.

## Quick Start

```sh
# Basic query & table listing
q-cli ping localhost:5000
q-cli query localhost:5000 'select avg price by sym from trade'
q-cli tables localhost:5000

# JSON or CSV output
q-cli --json query @prod 'select from trade'
q-cli --csv  query @prod 'select from trade' > trade.csv

# Server profiles (~/.config/q-cli/servers.conf)
q-cli config add prod bigbox:5001:alice:secret
q-cli query @prod 'select count i from trade'
```

> See [**`docs/examples.md`**](docs/examples.md) for advanced queries, script execution, and piping.

## Commands

| Command | Arguments | Description |
|---|---|---|
| `query` | `<conn> "<expr>"\|-` | Evaluate q expression (`-` for stdin) |
| `run` | `<conn> <file.q>\|-` | Execute `.q` script file (`-` for stdin) |
| `ping` | `<conn>` | Connection probe (`pong`) |
| `tables` | `<conn>` | List all tables |
| `views` | `<conn>` | List all views |
| `meta` | `<conn> <table>` | Table column types and attributes |
| `count` | `<conn> <table>` | Table row count |
| `describe` | `<conn> <table>` | Comprehensive table profile in 1 round-trip |
| `schema` | `<conn>` | Overview of all tables in 1 round-trip |
| `functions` | `<conn> [ns]` | Defined functions with arities (root or `.ns`) |
| `info` | `<conn>` | Server health snapshot (version, pid, port, memory) |
| `time` | `<conn> "<expr>"\|-` | Measure query time (ms) and row count |
| `gc` | `<conn>` | Trigger `.Q.gc[]` to free memory to OS |
| `web` | `<off\|get-ok\|status> <conn>` | Configure built-in HTTP handling at runtime |
| `trace` | `<on\|off\|status> <conn>` | Wrap handlers with `.Q.trp` for error call stacks |
| `config` | `<init\|path\|list\|add> [--project]` | Manage server profiles |
| `update` | `[--check] [--force]` | Self-update q-cli to the latest release |

`<conn>` accepts `[scheme://]host:port[:user:pass]`, `@profile`, or `@` (default server).
To keep a password out of the command line, pass `host:port:user` and set `Q_CLI_PASSWORD`.

## Options & Flags

| Flag | Env Var | Description |
|---|---|---|
| `--text` | `Q_CLI_OUT=text` | Aligned text output (default) |
| `--json`, `-j` | `Q_CLI_OUT=json` | Output JSON array of row objects |
| `--csv` | `Q_CLI_OUT=csv` | RFC 4180 CSV output (tables only; uncapped) |
| `--console` | `Q_CLI_OUT=console` | Format with server-side `.Q.s` (max fidelity) |
| `--max-rows N` | `Q_CLI_MAX_ROWS=N` | Row limit for text/json (default `50`; `0` = unlimited) |
| `--timeout MS` | `Q_CLI_TIMEOUT=MS` | Query timeout in ms (default `30000`; `0` = wait forever) |
| - | `Q_CLI_MAX_RESPONSE_MB=N` | Largest response accepted, after decompression (default `256`) |
| `--readonly` | `Q_CLI_READONLY=1` | Enforce read-only on the server via `reval` (kdb+ 3.3+) |
| `--scoped` | - | Wrap execution in `{[] ... }[]` to isolate locals |
| `--atomic` | - | Run `.q` script as a single protected IPC transaction |
| `--dry-run` | - | Validate syntax with server-side `parse` without mutating |
| `--tls` | `Q_CLI_TLS=1` | Connect over TLS/SSL (mitigates CWE-319) |
| `--tls-no-verify`, `--insecure` | `Q_CLI_TLS_NO_VERIFY=1` | Skip TLS cert verification (dev) |

> See [**`docs/security.md`**](docs/security.md) for `--readonly`, TLS, and access control details.

## Exit Codes

| Code | Meaning | Details |
|:---:|---|---|
| `0` | Success | Command completed successfully |
| `2` | Usage / Policy | Bad arguments, unrecognized server, or `--readonly` block |
| `3` | Connection Error | Unreachable host, authentication failure, handshake timeout |
| `4` | Server Error | Remote q exception (`'type`, `'length`, etc.) |
| `5` | Timeout | Query round-trip exceeded `--timeout` |

With `--json`, errors print as `{"error":"...","kind":"..."}` to stderr.

## Documentation

- [**Usage Examples (`docs/examples.md`)**](docs/examples.md):
  Script execution, piping, CSV exports, schema inspection.
- [**Security & Internals (`docs/security.md`)**](docs/security.md):
  Readonly guard, TLS configuration, IPC wire protocol.
- [**Agent Skill (`skills/kdb/SKILL.md`)**](skills/kdb/SKILL.md):
  Claude / AI agent integration via `npx skills add kdevkr/q-cli -y -g`.
