# Security, Protection & Protocol Internals

Technical details on `q-cli` security boundaries, client-side safety guards, and IPC protocol internals.

## Server-Side Access Control (Primary Protection)

`q-cli`'s client-side `--readonly` checks are supplementary guards against accidental operations and **cannot replace server-side access control**.

1. **Authentication**: Start q with `-U <password-file>` for user/password verification, or `-u <password-file>` to restrict remote file and system commands.
2. **Authorization**: Implement allowlists or read-only checks in `.z.pg` (synchronous queries) and `.z.ps` (asynchronous messages) in the server startup script.

Refer to the official KX documentation for [Command-line Security Options](https://code.kx.com/q/basics/cmdline/) and [IPC Authorization](https://code.kx.com/q/basics/ipc/).

---

## Client-Side `--readonly` Guard

When `--readonly` (or `Q_CLI_READONLY=1`) is enabled, user-supplied expressions (`query`, `time`, `run`) are sent as `reval parse "<expr>"`, so the **server** rejects writes, system commands, and handle I/O (`'noupdate` / `'sys`) however the expression is spelled. This requires kdb+ 3.3+; older servers fail closed with `'reval`.

The text guard below runs first and only gives earlier, clearer errors — it is not the security boundary (e.g. `-6!-5!"delete from t"` passes it and is stopped by `reval`). `q-cli` inspects expressions before transmission and blocks:

- **Mutating keywords**: `delete`, `update`, `insert`, `upsert`, `set`, `hdel`, `hopen`, `hclose`, `system`, `exit`, `dpft`, `0:`, `1:`, `2:`, `3:`, `value`, `get`, `eval`, `save`, `load`.
- **System commands**: Any backslash statement (`\cd`, `\p`, `\rm`, etc.).
- **Mode switches**: Language prefixes like `k)` and `q)`.
- **File / Handle symbols**: `\`:path\` or `\`:host:port\`.
- **Dynamic casts**: Symbol casts such as `$"delete"` or `11h$"set"`.
- **Higher-order amends**: `@`, `.`, or `!` used with adverbial modifications (`@/`, `.[...]`).
- **Lambda scoping**: Calls on identifiers inside lambdas are restricted to prevent indirect IPC handle execution. Keywords inside string literals (e.g. `where text like "*delete*"`) are ignored to avoid false positives.
- **Server mutations**: Blocks `web off`, `web get-ok`, `trace on`, and `trace off` (which rebind `.z.*` handlers); `status` queries remain allowed.

## Untrusted Config and Servers

- **Project config**: a `.q-cli.conf` found from the current directory overrides global profiles. When it repoints a name that the global config already defines, `q-cli` prints `warning: project <path> overrides global server(s): <names>` on stderr and `config list` tags the entry `[project, overrides global]`, so a checked-out repository cannot silently redirect `prod`.
- **Credentials**: a password in `host:port:user:pass` on the command line is visible to other local processes and lands in shell history. Prefer `host:port:user` with `Q_CLI_PASSWORD`, or a profile. `Q_CLI_PASSWORD` is sent to whichever host the connection resolves to, so set it per command rather than exporting it, and heed the project-override warning above. Profile files store passwords in plaintext; on Unix they are created `0600`, on Windows they inherit the folder's ACL. Without TLS the login is sent in cleartext (see below).
- **Self-update**: `q-cli update` replaces its own binary as the current user. If that location is not writable, reinstall under a user-writable directory rather than running the updater as root/administrator.
- **Response size**: a response larger than 256 MB (after decompression) is rejected; raise the cap with `Q_CLI_MAX_RESPONSE_MB`. The payload buffer grows with the bytes actually received, so a bare length header cannot force a large allocation.

---

## TLS / SSL Encryption (CWE-319 Mitigation)

- **Optional build feature**: Compiled with `--features tls` using `native-tls`.
- **OS Native Channels**:
  - Windows: Schannel (no MSVC or OpenSSL DLL dependencies).
  - macOS: Security.framework.
  - Linux: OpenSSL (`libssl-dev` / `openssl-devel`).
- **Connection schemes**:
  - `tcps://host:port` or `tls://host:port`: Explicitly enables TLS.
  - `tcp://host:port`: Explicitly forces plaintext TCP (overrides `--tls` / `Q_CLI_TLS=1`).
  - `--tls` / `Q_CLI_TLS=1`: Enables TLS for schemeless connections (`host:port`).
- **Verification policy**:
  - Full certificate and hostname verification is enforced by default.
  - `--tls-no-verify`, `--insecure`, or `Q_CLI_TLS_NO_VERIFY=1` skips verification for local development (emits a single warning to stderr).
  - Ambient environment variables like `SSL_VERIFY_SERVER` are intentionally ignored to prevent unintended MITM exposure.

---

## Runtime Tracing (`trace`) Internals

- `q-cli trace on <conn>` wraps the active `.z.pg` and `.z.ps` handlers with `.Q.trp`, returning `.Q.sbt` call stacks on error.
- Existing authorization logic inside `.z.pg` and `.z.ps` continues to execute.
- Prior handler state is preserved under the reserved symbol `.qcli.traceState`.
- `q-cli trace off <conn>` restores the saved handlers.
- If handlers are modified by another tool while tracing is active, `on` and `off` fail safely to avoid overwriting third-party state.

---

## IPC Request Flow

1. **Connection & Handshake**: TCP connect (capped at 5s), optional TLS handshake, send credentials byte sequence (`creds + \x03\x00`), validate 1-byte capability response.
2. **Synchronous Query**: Serialize expression as a char vector (type 10) inside a sync message (`msgtype = 1`), send, and read the 8-byte response header + payload.
3. **Deserialization**: Parse the returned K object ([src/k.rs](../src/k.rs)).
4. **Rendering**: Format into text, JSON, or CSV ([src/render.rs](../src/render.rs)). Text and console output (and error messages) escape control characters such as ESC as `\x1b` so server data cannot drive the terminal; JSON escapes them per the JSON spec, and CSV leaves control characters as-is but prefixes text cells and headers that start with `=`, `+`, `-`, `@`, tab or CR with `'`, so a spreadsheet shows them as text instead of running them as formulas (numeric cells are untouched).
