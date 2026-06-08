//! Commands as data: the `CMDS` table is the single source of truth. Dispatch,
//! the usage text, and the unknown-command list are all derived from it, so
//! adding a command is one row here — not three edits across the file.

use std::time::Duration;

use crate::conn::Conn;
use crate::error::{E, R};
use crate::query::{self, q_escape};
use crate::{config, render};

#[derive(Clone, Copy, PartialEq)]
pub enum OutMode {
    Text,
    Json,
    Csv,
    Console,
}

pub struct Opts {
    pub out: OutMode,
    pub max_rows: usize, // 0 = unlimited
    pub readonly: bool,
    pub timeout: Option<Duration>, // None = wait forever; bounds the query round-trip
}

/// One CLI command, expressed as data.
pub struct Cmd {
    pub name: &'static str,
    pub help: &'static str,
    pub spec: Spec,
}

/// How a command turns its positional args into work. Most commands are `Eval`
/// (build a q expression from the trailing arg, run it, render the result); the
/// other three variants capture the only real exceptions.
pub enum Spec {
    /// `<conn> [arg]` — build a q expression from `arg`, eval, render.
    /// `guard` runs the readonly check on the *built* expression first.
    Eval { build: fn(&str) -> R, guard: bool },
    /// `<conn>` — handshake probe; prints `pong`.
    Ping,
    /// `<action> <conn>` — map `action` to a fixed q expression (web / trace).
    /// Note the reversed arg order: the action comes before the connection.
    Action(fn(&str) -> R),
    /// `<args...>` — no connection (config profile management).
    Config,
}

pub const CMDS: &[Cmd] = &[
    Cmd { name: "query", help: "<conn> \"<q expr>\"          run a q expression",
          spec: Spec::Eval { build: |e| Ok(e.to_string()), guard: true } },
    Cmd { name: "run", help: "<conn> <path.q>            send a .q file",
          spec: Spec::Eval { build: build_run, guard: true } },
    Cmd { name: "tables", help: "<conn>                     list tables",
          spec: Spec::Eval { build: |_| Ok("tables[]".to_string()), guard: false } },
    Cmd { name: "meta", help: "<conn> <table>             column schema",
          spec: Spec::Eval { build: |t| need_table(t).map(|t| format!("meta {}", t)), guard: false } },
    Cmd { name: "count", help: "<conn> <table>             row count",
          spec: Spec::Eval { build: |t| need_table(t).map(|t| format!("count {}", t)), guard: false } },
    Cmd { name: "describe", help: "<conn> <table>             partitioning+schema+rows+sample",
          spec: Spec::Eval { build: |t| need_table(t).map(query::describe_q), guard: false } },
    Cmd { name: "schema", help: "<conn>                     all tables: rows/cols/partition",
          spec: Spec::Eval { build: |_| Ok(query::SCHEMA_Q.to_string()), guard: false } },
    Cmd { name: "functions", help: "<conn> [ns]                defined functions + arity",
          spec: Spec::Eval { build: |ns| Ok(query::functions_q(ns)), guard: false } },
    Cmd { name: "info", help: "<conn>                     version/pid/port/mem/handles",
          spec: Spec::Eval { build: |_| Ok(query::INFO_Q.to_string()), guard: false } },
    Cmd { name: "time", help: "<conn> \"<q expr>\"          elapsed ms + result count",
          spec: Spec::Eval { build: build_time, guard: true } },
    Cmd { name: "gc", help: "<conn>                     .Q.gc[] -> bytes freed to OS",
          spec: Spec::Eval { build: |_| Ok(".Q.gc[]".to_string()), guard: false } },
    Cmd { name: "ping", help: "<conn>                     handshake probe -> pong",
          spec: Spec::Ping },
    Cmd { name: "web", help: "<off|get-ok|status> <conn> tune built-in HTTP serving",
          spec: Spec::Action(web_expr) },
    Cmd { name: "trace", help: "<on|off|status> <conn>     .Q.trp backtraces on errors",
          spec: Spec::Action(trace_expr) },
    Cmd { name: "config", help: "<init|path|list|add> [--project]  manage server profiles",
          spec: Spec::Config },
];

/// Resolve `pos[0]` against `CMDS` and run it, returning the process exit code.
pub fn dispatch(pos: &[String], opts: &Opts) -> i32 {
    let mode = pos[0].as_str();
    let cmd = match CMDS.iter().find(|c| c.name == mode) {
        Some(c) => c,
        None => {
            let list = CMDS.iter().map(|c| c.name).collect::<Vec<_>>().join("|");
            return finish(Err(E::usage(format!("unknown mode '{}' ({})", mode, list))), opts);
        }
    };

    match &cmd.spec {
        Spec::Config => finish(config::run(&pos[1..]).map_err(E::usage), opts),
        Spec::Action(make) => {
            // <action> <conn> — action first, connection second.
            let action = pos.get(1).map(|s| s.as_str()).unwrap_or("");
            // Validate the action first (so a bad verb gets its own message),
            // then enforce readonly: for web/trace, every action except `status`
            // rebinds a server message handler (.z.ph/.z.pp/.z.pg/.z.ps) — a
            // server mutation the readonly guard must also cover, not just data.
            match make(action) {
                Err(e) => finish(Err(e), opts),
                Ok(_) if opts.readonly && action != "status" => finish(
                    Err(E::usage(format!(
                        "readonly: '{} {}' changes server handlers; unset --readonly / Q_CLI_READONLY to allow",
                        mode, action
                    ))),
                    opts,
                ),
                Ok(expr) => run_on(pos.get(2), &expr, opts),
            }
        }
        // <conn> — handshake probe.
        Spec::Ping => match resolve(pos.get(1), mode) {
            Ok(conn) => finish(do_ping(&conn, opts), opts),
            Err(e) => finish(Err(e), opts),
        },
        // <conn> [arg] — build a q expression, optionally guard, eval, render.
        Spec::Eval { build, guard } => {
            let conn = match resolve(pos.get(1), mode) {
                Ok(c) => c,
                Err(e) => return finish(Err(e), opts),
            };
            let arg = pos.get(2).cloned().unwrap_or_default();
            let r = build(&arg).and_then(|expr| {
                if *guard {
                    ro(&expr, opts)?;
                }
                do_eval(&conn, &expr, opts)
            });
            finish(r, opts)
        }
    }
}

/// Resolve the `<conn>` positional (a config name or `host:port`) into a
/// connection string, or a usage error naming the command if it's missing.
fn resolve(tok: Option<&String>, mode: &str) -> Result<String, E> {
    let tok = tok.ok_or_else(|| E::usage(format!("usage: q-cli {} <conn> ...", mode)))?;
    config::resolve_conn(tok).map_err(E::usage)
}

// --- Eval builders that need more than a one-line closure --------------------

fn build_run(path: &str) -> R {
    std::fs::read_to_string(path)
        .map_err(|e| E::usage(format!("cannot read file '{}': {}", path, e)))
}

fn build_time(expr: &str) -> R {
    if expr.is_empty() {
        Err(E::usage("usage: q-cli time <conn> <expr>"))
    } else {
        Ok(query::time_q(expr))
    }
}

// --- Action builders (web / trace) -------------------------------------------

/// `web <off|get-ok|status>` — tune the q process' built-in HTTP handlers
/// (.z.ph GET / .z.pp POST) at runtime.
fn web_expr(action: &str) -> R {
    Ok(match action {
        "off" => ".z.ph:{hclose .z.w};.z.pp:{hclose .z.w};\"web: off (all HTTP closed)\"",
        "get-ok" => {
            ".z.ph:{.h.hy[`txt;\"OK\"]};.z.pp:{hclose .z.w};\"web: get-ok (GET 200, POST closed)\""
        }
        "status" => "`ph`pp!(.z.ph;.z.pp)",
        _ => return Err(E::usage("web action must be: off | get-ok | status")),
    }
    .to_string())
}

/// `trace <on|off|status>` — wrap .z.pg/.z.ps with .Q.trp so query errors return
/// a .Q.sbt backtrace (and print it to the server console).
fn trace_expr(action: &str) -> R {
    const H: &str = "{.Q.trp[value;x;{-2 .Q.sbt y;'x,\"\\n\",.Q.sbt y}]}";
    Ok(match action {
        "on" => format!(".z.pg:{H};.z.ps:{H};\"trace: on\""),
        "off" => ".z.pg:value;.z.ps:value;\"trace: off\"".to_string(),
        "status" => {
            "`pg`ps!(@[{string value x};`.z.pg;\"(default)\"];@[{string value x};`.z.ps;\"(default)\"])".to_string()
        }
        _ => return Err(E::usage("trace action must be: on | off | status")),
    })
}

// --- Execution + output ------------------------------------------------------

fn do_ping(conn: &str, opts: &Opts) -> R {
    let mut c = Conn::open(conn, opts.timeout)?;
    let _ = c.sync("1+1")?;
    Ok("pong".to_string())
}

fn do_eval(conn: &str, expr: &str, opts: &Opts) -> R {
    let mut c = Conn::open(conn, opts.timeout)?;
    // --console: let q render the result string (.Q.s) for max fidelity.
    let to_send = if opts.out == OutMode::Console {
        format!(".Q.s value \"{}\"", q_escape(expr))
    } else {
        expr.to_string()
    };
    let k = c.sync(&to_send)?;

    // truncation signal on stderr (text/json only; csv & console are uncapped)
    if matches!(opts.out, OutMode::Text | OutMode::Json) && opts.max_rows > 0 {
        if let Some(total) = render::table_rows(&k) {
            if total > opts.max_rows {
                eprintln!(
                    "note: showing {} of {} rows (--max-rows {})",
                    opts.max_rows, total, opts.max_rows
                );
            }
        }
    }

    Ok(match opts.out {
        OutMode::Json => render::to_json(&k, opts.max_rows),
        OutMode::Csv => render::to_csv(&k),
        OutMode::Text | OutMode::Console => render::to_text(&k, opts.max_rows),
    })
}

/// Resolve a conn token and run a fixed (q-cli-generated) expression on it.
fn run_on(conn_tok: Option<&String>, expr: &str, opts: &Opts) -> i32 {
    let tok = match conn_tok {
        Some(c) => c,
        None => return finish(Err(E::usage("missing <conn>")), opts),
    };
    let r = config::resolve_conn(tok)
        .map_err(E::usage)
        .and_then(|conn| do_eval(&conn, expr, opts));
    finish(r, opts)
}

/// Print a result or a structured error, return the process exit code.
pub fn finish(result: R, opts: &Opts) -> i32 {
    match result {
        Ok(s) => {
            println!("{}", s);
            0
        }
        Err(e) => {
            if opts.out == OutMode::Json {
                eprintln!(
                    "{{\"error\":{},\"kind\":\"{}\"}}",
                    render::json_string(&e.msg),
                    e.kind.name()
                );
            } else {
                eprintln!("ERR {}", e.msg);
            }
            e.kind.code()
        }
    }
}

/// Readonly guard: in readonly mode, reject arbitrary q that looks mutating.
/// This is a heuristic denylist (q is not SQL), not a sandbox.
fn ro(expr: &str, opts: &Opts) -> Result<(), E> {
    if !opts.readonly {
        return Ok(());
    }
    let lower = expr.to_lowercase();
    for op in ["0:", "1:"] {
        if lower.contains(op) {
            return Err(E::usage(format!(
                "readonly: blocked '{}' (file I/O); unset --readonly / Q_CLI_READONLY to allow",
                op
            )));
        }
    }
    const DENIED: &[&str] = &[
        "delete", "update", "insert", "upsert", "set", "hdel", "hopen", "hclose", "system",
        "exit", "dpft", "dpfts", "rename",
    ];
    for tok in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if DENIED.contains(&tok) {
            return Err(E::usage(format!(
                "readonly: blocked '{}'; unset --readonly / Q_CLI_READONLY to allow",
                tok
            )));
        }
    }
    Ok(())
}

/// Validate a table argument is a plain q identifier (optionally namespaced)
/// before it's interpolated into a q expression, so a stray name like `foo bar`
/// gives a clean usage error (exit 2) instead of a confusing q parse error.
fn need_table(t: &str) -> Result<&str, E> {
    if t.is_empty() {
        return Err(E::usage("this command needs a table name"));
    }
    let first_ok = t
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '.');
    let rest_ok = t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if first_ok && rest_ok {
        Ok(t)
    } else {
        Err(E::usage(format!(
            "invalid table name '{}' (expected a q identifier, e.g. trade or .ns.trade)",
            t
        )))
    }
}

/// Usage text — the command list is generated from `CMDS`, so it never drifts.
pub fn print_usage() {
    let mut s = String::from("q-cli — native kdb+ IPC client\n\nUSAGE:\n");
    for c in CMDS {
        let out = matches!(c.spec, Spec::Eval { .. });
        s.push_str(&format!(
            "  q-cli {}{:<9} {}\n",
            if out { "[OUT] " } else { "      " },
            c.name,
            c.help
        ));
    }
    s.push_str(
        "\n\
         CONN: host:port[:user:pass] | @name | name | @ (default)\n\
         \x20 config (merged, project overrides global):\n\
         \x20   global  $Q_CLI_CONFIG or ~/.config/q-cli/servers.conf\n\
         \x20   project nearest .q-cli.conf up from CWD  (config --project writes ./.q-cli.conf)\n\
         \n\
         OUT (default: aligned text, 50-row cap):\n\
         \x20 --json, -j      JSON (tables -> array of row objects)\n\
         \x20 --csv           CSV (tables only; uncapped)\n\
         \x20 --console       let the q server format via .Q.s\n\
         \x20 --max-rows N    row cap for text/json (0 = unlimited)\n\
         \x20 --timeout MS    query wait in ms (default 30000; or Q_CLI_TIMEOUT)\n\
         \x20 --readonly      reject mutating q (or set Q_CLI_READONLY=1)\n\
         \n\
         EXIT: 0 ok · 2 usage/policy · 3 connection · 4 q error · 5 timeout\n",
    );
    eprint!("{}", s);
}
