//! Commands as data: the `CMDS` table is the single source of truth. Dispatch,
//! the usage text, and the unknown-command list are all derived from it, so
//! adding a command is one row here — not three edits across the file.

use std::time::Duration;

use crate::conn::Conn;
use crate::error::{E, R};
use crate::k::K;
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
    pub scoped: bool,
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
    /// `<conn> <path.q>` — read .q file, split into statements, sequentially execute.
    Run,
    /// `<conn>` — handshake probe; prints `pong`.
    Ping,
    /// `<action> <conn>` — map `action` to a fixed q expression (web / trace).
    /// Note the reversed arg order: the action comes before the connection.
    Action(fn(&str) -> R),
    /// `<args...>` — no connection (config profile management).
    Config,
}

pub const CMDS: &[Cmd] = &[
    Cmd { name: "query", help: "<conn> \"<q expr>\"|-        run a q expression (- = stdin)",
          spec: Spec::Eval { build: build_query, guard: true } },
    Cmd { name: "run", help: "<conn> <path.q>|-          send a .q file (- = stdin)",
          spec: Spec::Run },
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
        Spec::Run => {
            let conn = match resolve(pos.get(1), mode) {
                Ok(c) => c,
                Err(e) => return finish(Err(e), opts),
            };
            let path = pos.get(2).map(|s| s.as_str()).unwrap_or("");
            finish(do_run(&conn, path, opts), opts)
        }
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

/// Read all of stdin as the q source — the `-` argument for query/run/time, so
/// generated or multi-line expressions can be piped in instead of quoted inline.
fn slurp_stdin() -> R {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| E::usage(format!("cannot read stdin: {}", e)))?;
    Ok(s)
}

fn build_query(e: &str) -> R {
    if e == "-" {
        slurp_stdin()
    } else {
        Ok(e.to_string())
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct QStatement {
    pub line_no: usize, // 1-indexed
    pub text: String,
}

/// Split a q script into top-level statements according to q syntax rules:
/// - A line with non-whitespace at column 0 starts a new statement.
/// - Indented lines (starting with space or tab) continue the current statement.
/// - Single-line comments start with `/` at column 0.
/// - Block comments start with `/` alone on a line and end with `\` alone on a line.
/// - A lone `\` at column 0 stops processing (EOF marker in q scripts).
pub fn split_q_script(src: &str) -> Vec<QStatement> {
    let mut stmts = Vec::new();
    let mut current_stmt: Option<(usize, Vec<&str>)> = None;
    let mut in_block_comment = false;

    let finish_stmt = |cur: &mut Option<(usize, Vec<&str>)>, stmts: &mut Vec<QStatement>| {
        if let Some((line_no, lines)) = cur.take() {
            let joined = lines.join("\n").trim().to_string();
            if !joined.is_empty() {
                stmts.push(QStatement { line_no, text: joined });
            }
        }
    };

    for (idx, line) in src.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim_end();

        if in_block_comment {
            if trimmed == "\\" {
                in_block_comment = false;
            }
            continue;
        }

        // A slash alone on a line starts a multi-line comment block
        if trimmed == "/" {
            in_block_comment = true;
            finish_stmt(&mut current_stmt, &mut stmts);
            continue;
        }

        // A backslash alone on a line ends script execution (q EOF)
        if trimmed == "\\" {
            finish_stmt(&mut current_stmt, &mut stmts);
            break;
        }

        if line.starts_with('/') {
            finish_stmt(&mut current_stmt, &mut stmts);
            continue;
        }

        if trimmed.trim().is_empty() {
            finish_stmt(&mut current_stmt, &mut stmts);
            continue;
        }

        let first_char = line.chars().next().unwrap();
        if first_char == ' ' || first_char == '\t' {
            if let Some((_, ref mut lines)) = current_stmt {
                lines.push(trimmed);
            } else {
                current_stmt = Some((line_no, vec![trimmed]));
            }
        } else {
            finish_stmt(&mut current_stmt, &mut stmts);
            current_stmt = Some((line_no, vec![trimmed]));
        }
    }
    finish_stmt(&mut current_stmt, &mut stmts);

    stmts
}

fn build_time(expr: &str) -> R {
    let expr = if expr == "-" { slurp_stdin()? } else { expr.to_string() };
    if expr.is_empty() {
        Err(E::usage("usage: q-cli time <conn> <expr>"))
    } else {
        Ok(query::time_q(&expr))
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
    let expr_scoped = if opts.scoped {
        format!("{{[]\n{}\n}}[]", expr)
    } else {
        expr.to_string()
    };
    // --console: let q render the result string (.Q.s) for max fidelity.
    let to_send = if opts.out == OutMode::Console {
        format!(".Q.s value \"{}\"", q_escape(&expr_scoped))
    } else {
        expr_scoped
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

fn do_run(conn: &str, path: &str, opts: &Opts) -> R {
    let content = if path == "-" {
        slurp_stdin()?
    } else if path.is_empty() {
        return Err(E::usage("usage: q-cli run <conn> <path.q>|-"));
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| E::usage(format!("cannot read file '{}': {}", path, e)))?
    };

    let stmts = split_q_script(&content);
    if stmts.is_empty() {
        return Ok(String::new());
    }

    if opts.readonly {
        for s in &stmts {
            ro(&s.text, opts)?;
        }
    }

    let mut c = Conn::open(conn, opts.timeout)?;
    let mut last_k = K::Null;
    for s in &stmts {
        let to_send = if opts.scoped {
            format!("{{[]\n{}\n}}[]", s.text)
        } else {
            s.text.clone()
        };
        match c.sync(&to_send) {
            Ok(k) => {
                last_k = k;
            }
            Err(e) => {
                return Err(E::query(format!("line {}: {}", s.line_no, e.msg)));
            }
        }
    }

    Ok(match opts.out {
        OutMode::Json => render::to_json(&last_k, opts.max_rows),
        OutMode::Csv => render::to_csv(&last_k),
        OutMode::Text | OutMode::Console => render::to_text(&last_k, opts.max_rows),
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

/// Blank out the contents of q double-quoted string literals so the readonly
/// scan matches keywords in *code*, not in *data* — e.g. `like "*delete*"` must
/// not trip the `delete` denylist. Backslash escapes inside a string are
/// honoured. (A symbol literal like `` `delete `` is still scanned — rare, and
/// erring toward over-blocking is the safe default for a readonly guard.)
fn strip_q_strings(expr: &str) -> String {
    let mut out = String::with_capacity(expr.len());
    let mut chars = expr.chars();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next(); // skip the escaped char (e.g. \" or \\)
                }
                '"' => {
                    in_str = false;
                    out.push('"');
                }
                _ => {} // drop the string's contents
            }
        } else if c == '"' {
            in_str = true;
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out
}

/// Readonly guard: in readonly mode, reject arbitrary q that looks mutating.
/// This is a heuristic denylist (q is not SQL), not a sandbox.
fn ro(expr: &str, opts: &Opts) -> Result<(), E> {
    if !opts.readonly {
        return Ok(());
    }
    let lower = strip_q_strings(expr).to_lowercase();
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
        "exit", "dpft", "dpfts", "rename", "value",
    ];
    for tok in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if DENIED.contains(&tok) {
            return Err(E::usage(format!(
                "readonly: blocked '{}'; unset --readonly / Q_CLI_READONLY to allow",
                tok
            )));
        }
    }
    if lower.contains("@[") || lower.contains(".[") {
        return Err(E::usage(
            "readonly: blocked amend ('@[' / '.['); unset --readonly / Q_CLI_READONLY to allow".to_string(),
        ));
    }
    if lower.contains(".z.") || lower.contains(".ps.") {
        return Err(E::usage(
            "readonly: blocked system/handler namespace mutation ('.z.' / '.ps.'); unset --readonly / Q_CLI_READONLY to allow".to_string(),
        ));
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
        let out = matches!(c.spec, Spec::Eval { .. } | Spec::Run);
        s.push_str(&format!(
            "  q-cli {}{:<9} {}\n",
            if out { "[OUT] " } else { "      " },
            c.name,
            c.help
        ));
    }
    s.push_str(
        "\n\
         query/run/time accept '-' to read the q source from stdin.\n\
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
         \x20 --scoped        evaluate inside {[] ... }[] to isolate locals\n\
         \n\
         EXIT: 0 ok · 2 usage/policy · 3 connection · 4 q error · 5 timeout\n",
    );
    eprint!("{}", s);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro_opts() -> Opts {
        Opts { out: OutMode::Text, max_rows: 0, readonly: true, scoped: false, timeout: None }
    }

    #[test]
    fn strip_keeps_code_drops_string_contents() {
        assert_eq!(strip_q_strings("a\"delete\"b"), "a\"\"b");
        assert_eq!(strip_q_strings("like \"*x\\\"y*\""), "like \"\"");
        assert_eq!(strip_q_strings("no strings here"), "no strings here");
    }

    #[test]
    fn readonly_blocks_mutating_code() {
        assert!(ro("delete from t", &ro_opts()).is_err());
        assert!(ro("`t insert (1;2)", &ro_opts()).is_err());
        assert!(ro("save `:x set til 3", &ro_opts()).is_err());
        assert!(ro("(`:f) 0: \"x\"", &ro_opts()).is_err()); // file I/O
    }

    #[test]
    fn readonly_allows_reads_and_string_literal_keywords() {
        assert!(ro("select from t", &ro_opts()).is_ok());
        // the keyword appears only inside a string literal -> not a mutation
        assert!(ro("select from t where note like \"*delete*\"", &ro_opts()).is_ok());
    }

    #[test]
    fn readonly_off_allows_everything() {
        let opts = Opts { readonly: false, ..ro_opts() };
        assert!(ro("delete from t", &opts).is_ok());
    }

    #[test]
    fn table_name_validation() {
        assert!(need_table("trade").is_ok());
        assert!(need_table(".ns.trade").is_ok());
        assert!(need_table("t_1").is_ok());
        assert!(need_table("foo bar").is_err());
        assert!(need_table("").is_err());
        assert!(need_table("1trade").is_err()); // must not start with a digit
    }

    #[test]
    fn readonly_blocks_value_amend_and_namespaces() {
        assert!(ro("value \"1+1\"", &ro_opts()).is_err());
        assert!(ro("@[`t;`a;:;1]", &ro_opts()).is_err());
        assert!(ro(".[t;(0;`a);:;1]", &ro_opts()).is_err());
        assert!(ro(".z.pg:{x}", &ro_opts()).is_err());
        assert!(ro(".ps.f:{x}", &ro_opts()).is_err());
    }

    #[test]
    fn split_q_script_handles_functions_comments_and_multiline() {
        let script = r#"
/
ignored in block comment
\
f:{[x]
  a:x+1;
  a
 }
/ line comment
g:{x*2}
h:{1}
\
ignored after eof
"#;
        let stmts = split_q_script(script);
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0].line_no, 5);
        assert!(stmts[0].text.starts_with("f:{[x]"));
        assert!(stmts[0].text.contains("a:x+1;"));
        assert_eq!(stmts[1].line_no, 10);
        assert_eq!(stmts[1].text, "g:{x*2}");
        assert_eq!(stmts[2].line_no, 11);
        assert_eq!(stmts[2].text, "h:{1}");
    }
}
