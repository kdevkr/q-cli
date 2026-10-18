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
    pub atomic: bool,   // execute entire script in a single atomic block
    pub dry_run: bool,  // simulate execution in an isolated sandbox to verify syntax and runtime without mutations
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
            // Support both `<action> <conn>` and `<conn> <action>`:
            // e.g. `web off localhost:5000` OR `web localhost:5000 off`
            let a1 = pos.get(1).map(|s| s.as_str()).unwrap_or("");
            let a2 = pos.get(2).map(|s| s.as_str()).unwrap_or("");

            let (action, conn_tok) = if a1.contains(':') || a1.starts_with('@') || (a1 == "@" && !a2.is_empty()) {
                (a2, pos.get(1))
            } else if !a2.is_empty() && (make(a2).is_ok() || a2 == "status") {
                (a2, pos.get(1))
            } else {
                (a1, pos.get(2))
            };

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
                Ok(expr) => run_on(conn_tok, &expr, opts),
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
/// - A line with non-whitespace at column 0 starts a new statement, UNLESS:
///   - It is inside open brackets/braces/parentheses (`{`, `(`, `[`).
///   - It is inside a multi-line string literal (`"..."`).
/// - Indented lines (starting with space or tab) continue the current statement.
/// - Single-line comments start with `/` at column 0 or preceded by whitespace.
/// - Block comments start with `/` alone on a line and end with `\` alone on a line.
/// - A lone `\` at column 0 stops processing (EOF marker in q scripts).
/// - Delimiter mismatches or unclosed strings return a syntax error before any
///   statement is executed, preventing partial script application.
pub fn split_q_script(src: &str) -> Result<Vec<QStatement>, E> {
    let mut stmts = Vec::new();
    let mut current_stmt: Option<(usize, Vec<&str>)> = None;
    let mut in_block_comment = false;
    let mut in_str = false;
    let mut str_start_line = None;
    let mut delimiter_stack: Vec<(char, usize)> = Vec::new();

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

        let at_top_level = !in_str && delimiter_stack.is_empty();

        if at_top_level {
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
        }

        let first_char = line.chars().next();
        let is_indented = first_char.map(|c| c == ' ' || c == '\t').unwrap_or(false);
        let was_in_str = in_str;
        let line_to_push = if was_in_str { line } else { trimmed };

        if at_top_level && !is_indented {
            finish_stmt(&mut current_stmt, &mut stmts);
            current_stmt = Some((line_no, vec![line_to_push]));
        } else if let Some((_, ref mut lines)) = current_stmt {
            lines.push(line_to_push);
        } else if !trimmed.trim().is_empty() && !line.starts_with('/') {
            current_stmt = Some((line_no, vec![line_to_push]));
        }

        // Scan characters in line to update delimiter stack and string literal status
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut prev_ws = true;
        while i < chars.len() {
            let c = chars[i];
            if in_str {
                if c == '\\' {
                    i += 2; // skip escaped character (e.g. \" or \\)
                    continue;
                } else if c == '"' {
                    in_str = false;
                    str_start_line = None;
                    prev_ws = false;
                    i += 1;
                    continue;
                } else {
                    i += 1;
                    continue;
                }
            }

            if c == '"' {
                in_str = true;
                str_start_line = Some(line_no);
                prev_ws = false;
                i += 1;
                continue;
            }

            // Inline comment: whitespace followed by '/' (excluding adverb '/:')
            if prev_ws && c == '/' {
                if i + 1 < chars.len() && chars[i + 1] == ':' {
                    // Adverb /: (each-right), not a comment
                } else {
                    break; // rest of the line is a comment
                }
            }

            match c {
                '{' | '(' | '[' => delimiter_stack.push((c, line_no)),
                '}' => {
                    if let Some(&(top, _)) = delimiter_stack.last() {
                        if top == '{' {
                            delimiter_stack.pop();
                        }
                    }
                }
                ')' => {
                    if let Some(&(top, _)) = delimiter_stack.last() {
                        if top == '(' {
                            delimiter_stack.pop();
                        }
                    }
                }
                ']' => {
                    if let Some(&(top, _)) = delimiter_stack.last() {
                        if top == '[' {
                            delimiter_stack.pop();
                        }
                    }
                }
                _ => {}
            }

            prev_ws = c.is_whitespace();
            i += 1;
        }

        // If string literal is still open at end of line, preserve trailing whitespace on opening line
        if in_str && !was_in_str {
            if let Some((_, ref mut lines)) = current_stmt {
                if let Some(last) = lines.last_mut() {
                    *last = line;
                }
            }
        }
    }
    finish_stmt(&mut current_stmt, &mut stmts);

    if let Some(start_line) = str_start_line {
        return Err(E::usage(format!(
            "syntax error: unclosed string literal starting at line {}",
            start_line
        )));
    }
    if let Some(&(ch, start_line)) = delimiter_stack.last() {
        return Err(E::usage(format!(
            "syntax error: unclosed '{}' starting at line {}",
            ch, start_line
        )));
    }

    Ok(stmts)
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

fn is_builtin_or_keyword(name: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "select", "exec", "update", "delete", "from", "by", "where",
        "count", "meta", "tables", "views", "cols", "keys", "key", "value",
        "first", "last", "sum", "avg", "min", "max", "prd", "med", "dev", "var",
        "null", "not", "and", "or", "all", "any", "distinct", "group", "in", "like",
        "bin", "binr", "asof", "lj", "uj", "ij", "pj", "aj", "aj0", "wj", "wj1",
        "xbar", "xprev", "prev", "next", "prior", "mavg", "msum", "mcount",
        "mdev", "mmin", "mmax", "ema", "deltas", "ratios", "differ",
        "fills", "floor", "ceil", "abs", "sqrt", "exp", "log", "sin", "cos", "tan",
        "asc", "desc", "xasc", "xdesc", "iasc", "idesc", "rank",
        "string", "type", "upper", "lower", "trim", "ltrim", "rtrim",
        "sv", "vs", "ss", "ssr", "raze", "enlist", "flip",
    ];
    BUILTINS.contains(&name.to_lowercase().as_str())
}

fn extract_candidate_func_names(expr: &str) -> Vec<String> {
    let clean = strip_q_code(expr);
    let tokens = tokenize_q_for_ro(&clean);
    let mut names = Vec::new();
    let mut prev_is_from = false;

    for (i, tok) in tokens.iter().enumerate() {
        if let QTok::Ident(name) = tok {
            let next = tokens.get(i + 1);
            let is_assign = matches!(
                next,
                Some(QTok::Colon) | Some(QTok::DoubleColon) | Some(QTok::CompoundColon(_))
            );
            let is_call = match next {
                Some(QTok::OpenBracket) | Some(QTok::OpenParen) => true,
                // Prefix function call: f x, f 10, f `trade, f {x+1}
                Some(QTok::Ident(n)) => !prev_is_from && !is_builtin_or_keyword(n),
                Some(QTok::Other) | Some(QTok::OpenBrace) => !prev_is_from,
                _ => false,
            };
            if !is_assign && is_call && !is_builtin_or_keyword(name) && !names.contains(name) {
                names.push(name.clone());
            }
        }
        prev_is_from = matches!(tok, QTok::From);
    }
    names
}

fn inspect_remote_funcs(conn: &mut Conn, expr: &str, max_depth: usize) -> Result<(), E> {
    let names = extract_candidate_func_names(expr);
    if names.is_empty() {
        return Ok(());
    }
    let syms = names.iter().map(|n| format!("`{}", n)).collect::<Vec<_>>().join("");
    let probe = format!(
        "{{[syms; max_d]\n\
           denied:`delete`update`insert`upsert`set`hdel`hopen`hclose`system`exit`dpft`dpfts`rename`value`get`eval`save`load`rsave`rload`dsave;\n\
           visited:()!();\n\
           violator:\"\";\n\
           queue:distinct syms;\n\
           steps:0;\n\
           max_steps:max_d * 20;\n\
           while[(steps < max_steps) and (count queue) and 0 = count violator;\n\
             curr:first queue;\n\
             queue:1 _ queue;\n\
             if[not curr in key visited;\n\
               visited[curr]:1;\n\
               v:@[value; curr; ::];\n\
               if[100h ~ type v;\n\
                 src:lower string v;\n\
                 if[any {{x like \"*\",string[y],\"*\"}}[src;] each denied; violator:string curr];\n\
                 if[any {{x like \"*\",y,\"*\"}}[src;] each (\"0:\";\"1:\";\"2:\"); violator:string curr];\n\
                 sub:distinct raze @[{{value[x] 3}}; v; `symbol$()];\n\
                 sub:sub where {{100h ~ type @[value; x; ::]}} each sub;\n\
                 queue:distinct queue,sub where not sub in key visited;\n\
               ];\n\
             ];\n\
             steps+:1;\n\
           ];\n\
           violator\n\
         }}[{}; {}]",
        syms, max_depth
    );
    match conn.sync(&probe) {
        Ok(K::CharV(violator)) if !violator.is_empty() => {
            Err(E::usage(format!(
                "readonly: blocked server function '{}' (mutating operation detected in call chain); unset --readonly / Q_CLI_READONLY to allow",
                violator
            )))
        }
        _ => Ok(()),
    }
}

fn render_output(k: &K, opts: &Opts) -> R {
    if matches!(opts.out, OutMode::Text | OutMode::Json) && opts.max_rows > 0 {
        if let Some(total) = render::table_rows(k) {
            if total > opts.max_rows {
                eprintln!(
                    "note: showing {} of {} rows (--max-rows {})",
                    opts.max_rows, total, opts.max_rows
                );
            }
        }
    }

    Ok(match opts.out {
        OutMode::Json => render::to_json(k, opts.max_rows),
        OutMode::Csv => render::to_csv(k),
        OutMode::Text | OutMode::Console => render::to_text(k, opts.max_rows),
    })
}

fn do_eval(conn: &str, expr: &str, opts: &Opts) -> R {
    let mut c = Conn::open(conn, opts.timeout)?;
    if opts.readonly {
        inspect_remote_funcs(&mut c, expr, 5)?;
    }
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
    render_output(&k, opts)
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

    let stmts = split_q_script(&content)?;
    if stmts.is_empty() {
        return Ok(String::new());
    }

    if opts.readonly {
        for s in &stmts {
            ro(&s.text, opts)?;
        }
    }

    let mut c = Conn::open(conn, opts.timeout)?;

    if opts.readonly {
        inspect_remote_funcs(&mut c, &content, 5)?;
    }

    let joined_stmts = stmts
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(";\n");

    // Dry-run mode: Execute inside an isolated sandbox lambda {[] ... }[]
    // Catches all syntax and runtime errors without mutating any global server state!
    if opts.dry_run {
        let sandbox = format!("{{[]\n{}\n}}[]", joined_stmts);
        let probe = format!("@[value; \"{}\"; {{'x}}]", q_escape(&sandbox));
        match c.sync(&probe) {
            Ok(_) => {
                return Ok(format!(
                    "dry-run: passed ({} statements validated, syntax and runtime sandbox OK)",
                    stmts.len()
                ));
            }
            Err(e) => {
                return Err(E::query(format!("dry-run failed: {}", e.msg)));
            }
        }
    }

    // Atomic mode: Send the entire script as a single protected IPC evaluation
    if opts.atomic {
        let body = if opts.scoped {
            format!("{{[]\n{}\n}}[]", joined_stmts)
        } else {
            joined_stmts
        };
        let to_send = if opts.out == OutMode::Console {
            format!(".Q.s @[value; \"{}\"; {{'x}}]", q_escape(&body))
        } else {
            format!("@[value; \"{}\"; {{'x}}]", q_escape(&body))
        };
        let last_k = c.sync(&to_send)?;
        return render_output(&last_k, opts);
    }

    // Sequential mode (default): Execute statement by statement with line-level failure reporting
    let mut last_k = K::Null;
    for (idx, s) in stmts.iter().enumerate() {
        let is_last = idx + 1 == stmts.len();
        let to_send = if opts.scoped {
            format!("{{[]\n{}\n}}[]", s.text)
        } else {
            s.text.clone()
        };
        let to_send = if is_last && opts.out == OutMode::Console {
            format!(".Q.s value \"{}\"", q_escape(&to_send))
        } else {
            to_send
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

    render_output(&last_k, opts)
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
#[cfg(test)]
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

/// Strip both string contents and comments (line comments, inline comments, block comments)
/// from a q expression so the readonly scan inspects code syntax without false positives.
pub fn strip_q_code(expr: &str) -> String {
    let mut out = String::with_capacity(expr.len());
    let mut in_str = false;
    let mut in_block_comment = false;

    for line in expr.lines() {
        let trimmed = line.trim();
        if in_block_comment {
            if trimmed == "\\" {
                in_block_comment = false;
            }
            out.push('\n');
            continue;
        }
        if !in_str {
            if trimmed == "/" {
                in_block_comment = true;
                out.push('\n');
                continue;
            }
            if trimmed == "\\" {
                break;
            }
            if line.starts_with('/') {
                out.push('\n');
                continue;
            }
        }

        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut prev_ws = true;
        while i < chars.len() {
            let c = chars[i];
            if in_str {
                if c == '\\' {
                    i += 2;
                    continue;
                } else if c == '"' {
                    in_str = false;
                    out.push('"');
                    prev_ws = false;
                    i += 1;
                    continue;
                } else {
                    i += 1;
                    continue;
                }
            }

            if c == '"' {
                in_str = true;
                out.push('"');
                prev_ws = false;
                i += 1;
                continue;
            }

            if prev_ws && c == '/' {
                if i + 1 < chars.len() && chars[i + 1] == ':' {
                    // Adverb /: (each-right), not a comment
                } else {
                    break;
                }
            }

            out.push(c);
            prev_ws = c.is_whitespace();
            i += 1;
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, PartialEq, Clone)]
enum QTok {
    Ident(String),
    Colon,
    DoubleColon,
    CompoundColon(char),
    Select,
    Exec,
    From,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    Semi,
    Other,
}

fn tokenize_q_for_ro(src: &str) -> Vec<QTok> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c == ':' {
            if i + 1 < chars.len() && chars[i + 1] == ':' {
                tokens.push(QTok::DoubleColon);
                i += 2;
            } else {
                tokens.push(QTok::Colon);
                i += 1;
            }
            continue;
        }

        if matches!(c, ',' | '+' | '-' | '*' | '%' | '&' | '|' | '^' | '_') {
            if i + 1 < chars.len() && chars[i + 1] == ':' {
                tokens.push(QTok::CompoundColon(c));
                i += 2;
                continue;
            }
        }

        match c {
            '(' => { tokens.push(QTok::OpenParen); i += 1; continue; }
            ')' => { tokens.push(QTok::CloseParen); i += 1; continue; }
            '[' => { tokens.push(QTok::OpenBracket); i += 1; continue; }
            ']' => { tokens.push(QTok::CloseBracket); i += 1; continue; }
            '{' => { tokens.push(QTok::OpenBrace); i += 1; continue; }
            '}' => { tokens.push(QTok::CloseBrace); i += 1; continue; }
            ';' => { tokens.push(QTok::Semi); i += 1; continue; }
            _ => {}
        }

        if c.is_ascii_alphabetic() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let lower = word.to_lowercase();
            match lower.as_str() {
                "select" => tokens.push(QTok::Select),
                "exec" => tokens.push(QTok::Exec),
                "from" => tokens.push(QTok::From),
                _ => tokens.push(QTok::Ident(word)),
            }
            continue;
        }

        tokens.push(QTok::Other);
        i += 1;
    }
    tokens
}

/// Readonly guard: in readonly mode, reject arbitrary q that looks mutating.
/// This blocks:
/// - File & dynamic library I/O (0:, 1:, 2:)
/// - System backslash commands (\)
/// - Mutating keywords (delete, update, insert, upsert, set, value, get, eval, save, load, etc.)
/// - Amends (@[, .[)
/// - System/handler namespace mutations (.z., .ps.)
/// - Global assignments (ident::) and compound assignments (ident,:, ident+:, etc.)
/// - Variable assignments (ident:) outside of select/exec column projections
fn ro(expr: &str, opts: &Opts) -> Result<(), E> {
    if !opts.readonly {
        return Ok(());
    }

    let clean = strip_q_code(expr);

    // Reject system backslash commands (e.g. \rm, \cd, \p, etc.) across statements or lines
    for stmt in clean.split(|c| c == ';' || c == '\n' || c == '\r') {
        let t = stmt.trim();
        if t.starts_with('\\') {
            return Err(E::usage(
                "readonly: blocked system command ('\\'); unset --readonly / Q_CLI_READONLY to allow"
                    .to_string(),
            ));
        }
    }

    let lower = clean.to_lowercase();
    for op in ["0:", "1:", "2:"] {
        if lower.contains(op) {
            return Err(E::usage(format!(
                "readonly: blocked '{}' (file/dynamic I/O); unset --readonly / Q_CLI_READONLY to allow",
                op
            )));
        }
    }

    const DENIED: &[&str] = &[
        "delete", "update", "insert", "upsert", "set", "hdel", "hopen", "hclose", "system",
        "exit", "dpft", "dpfts", "rename", "value", "get", "eval", "save", "load", "rsave",
        "rload", "dsave",
    ];
    for tok in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if DENIED.contains(&tok) {
            return Err(E::usage(format!(
                "readonly: blocked '{}'; unset --readonly / Q_CLI_READONLY to allow",
                tok
            )));
        }
    }

    // Amend @[...] or .[...] with optional whitespace before bracket
    let chars: Vec<char> = clean.chars().collect();
    for i in 0..chars.len() {
        if chars[i] == '@' || chars[i] == '.' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && chars[j] == '[' {
                return Err(E::usage(
                    "readonly: blocked amend ('@[' / '.['); unset --readonly / Q_CLI_READONLY to allow"
                        .to_string(),
                ));
            }
        }
    }

    if lower.contains(".z.") || lower.contains(".ps.") {
        return Err(E::usage(
            "readonly: blocked system/handler namespace mutation ('.z.' / '.ps.'); unset --readonly / Q_CLI_READONLY to allow".to_string(),
        ));
    }

    // Validate assignments: ident::, ident,:, and ident: (outside select/exec projection and outside lambda local scopes)
    let tokens = tokenize_q_for_ro(&clean);
    let mut scope_stack: Vec<bool> = Vec::new();
    let mut in_projection = false;
    let mut brace_depth: usize = 0;
    let mut prev_token: Option<&QTok> = None;

    for tok in &tokens {
        match tok {
            QTok::Select | QTok::Exec => {
                in_projection = true;
            }
            QTok::From => {
                in_projection = false;
            }
            QTok::OpenBrace => {
                brace_depth += 1;
                scope_stack.push(in_projection);
            }
            QTok::CloseBrace => {
                brace_depth = brace_depth.saturating_sub(1);
                if let Some(prev) = scope_stack.pop() {
                    in_projection = prev;
                }
            }
            QTok::OpenParen | QTok::OpenBracket => {
                scope_stack.push(in_projection);
            }
            QTok::CloseParen | QTok::CloseBracket => {
                if let Some(prev) = scope_stack.pop() {
                    in_projection = prev;
                }
            }
            QTok::Semi => {
                if scope_stack.is_empty() {
                    in_projection = false;
                }
            }
            QTok::DoubleColon => {
                if let Some(QTok::Ident(name)) = prev_token {
                    return Err(E::usage(format!(
                        "readonly: blocked global assignment ('{}::'); unset --readonly / Q_CLI_READONLY to allow",
                        name
                    )));
                }
            }
            QTok::CompoundColon(op) => {
                if let Some(QTok::Ident(name)) = prev_token {
                    return Err(E::usage(format!(
                        "readonly: blocked compound assignment ('{}{}:'); unset --readonly / Q_CLI_READONLY to allow",
                        name, op
                    )));
                }
            }
            QTok::Colon => {
                if let Some(QTok::Ident(name)) = prev_token {
                    if !in_projection && (brace_depth == 0 || name.contains('.')) {
                        return Err(E::usage(format!(
                            "readonly: blocked assignment ('{}:'); unset --readonly / Q_CLI_READONLY to allow",
                            name
                        )));
                    }
                }
            }
            _ => {}
        }
        prev_token = Some(tok);
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
         \x20 --atomic        run .q script as a single atomic protected block\n\
         \x20 --dry-run       simulate script in sandbox without mutating state\n\
         \n\
         EXIT: 0 ok · 2 usage/policy · 3 connection · 4 q error · 5 timeout\n",
    );
    eprint!("{}", s);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro_opts() -> Opts {
        Opts {
            out: OutMode::Text,
            max_rows: 0,
            readonly: true,
            scoped: false,
            atomic: false,
            dry_run: false,
            timeout: None,
        }
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
        let stmts = split_q_script(script).unwrap();
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0].line_no, 5);
        assert!(stmts[0].text.starts_with("f:{[x]"));
        assert!(stmts[0].text.contains("a:x+1;"));
        assert_eq!(stmts[1].line_no, 10);
        assert_eq!(stmts[1].text, "g:{x*2}");
        assert_eq!(stmts[2].line_no, 11);
        assert_eq!(stmts[2].text, "h:{1}");
    }

    #[test]
    fn split_q_script_handles_closing_brace_at_column_zero() {
        let script = "f:{[x]\n  a:x+1;\n  a*2\n}\ng:{x}";
        let stmts = split_q_script(script).unwrap();
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].line_no, 1);
        assert_eq!(stmts[0].text, "f:{[x]\n  a:x+1;\n  a*2\n}");
        assert_eq!(stmts[1].line_no, 5);
        assert_eq!(stmts[1].text, "g:{x}");
    }

    #[test]
    fn split_q_script_handles_multiline_string_and_detects_unclosed() {
        let script = "s:\"line 1\nline 2\";\nt:1";
        let stmts = split_q_script(script).unwrap();
        assert_eq!(stmts.len(), 2);

        let unclosed_brace = "f:{[x]\n  x+1";
        assert!(split_q_script(unclosed_brace).is_err());

        let unclosed_str = "s:\"hello";
        assert!(split_q_script(unclosed_str).is_err());
    }

    #[test]
    fn readonly_blocks_assignments_and_system_commands() {
        assert!(ro("t: 0#t", &ro_opts()).is_err());
        assert!(ro("trade: 1", &ro_opts()).is_err());
        assert!(ro("trade::1", &ro_opts()).is_err());
        assert!(ro("t,: (1; 2)", &ro_opts()).is_err());
        assert!(ro("t +: 1", &ro_opts()).is_err());
        assert!(ro("trade: select from trade", &ro_opts()).is_err());
        assert!(ro("\"lib.so\" 2: (\"f\"; 1)", &ro_opts()).is_err());
        assert!(ro("\\rm -rf /", &ro_opts()).is_err());
        assert!(ro("1+1; \\cd /", &ro_opts()).is_err());
        assert!(ro("1+1\n\\rm -rf /", &ro_opts()).is_err());
        assert!(ro("select from t\n\\p 5000", &ro_opts()).is_err());
        assert!(ro("get \"delete from t\"", &ro_opts()).is_err());
        assert!(ro("eval (insert; `t; 1)", &ro_opts()).is_err());
    }

    #[test]
    fn readonly_allows_select_aliases_comments_and_generic_null() {
        assert!(ro("select c: count i, max_p: max price from trade", &ro_opts()).is_ok());
        assert!(ro("exec p: last price from trade", &ro_opts()).is_ok());
        assert!(ro("(::)", &ro_opts()).is_ok());
        assert!(ro("f[::; 1]", &ro_opts()).is_ok());
        // Comments containing denied keywords must not block
        assert!(ro("select from trade / delete from trade", &ro_opts()).is_ok());
    }

    #[test]
    fn readonly_allows_lambda_local_variables_but_blocks_globals() {
        // Local variables inside { ... } should be allowed
        assert!(ro("{tmp: 1; select from t where x > tmp}[]", &ro_opts()).is_ok());
        assert!(ro("{[x] y: x + 1; y*2}[10]", &ro_opts()).is_ok());
        assert!(ro("{trade: 0#trade}[]", &ro_opts()).is_ok()); // local assignment in lambda

        // But global assignment :: or namespaced assignment with . inside lambda must still be blocked
        assert!(ro("{trade:: 0#trade}[]", &ro_opts()).is_err());
        assert!(ro("{x::1}[]", &ro_opts()).is_err());
        assert!(ro("{.t: 1}[]", &ro_opts()).is_err());
        assert!(ro("{.ns.x: 1}[]", &ro_opts()).is_err());
    }

    #[test]
    fn adverb_each_right_not_treated_as_comment() {
        let code = "res: x /: y;\nt: 1";
        let stmts = split_q_script(code).unwrap();
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].text, "res: x /: y;");

        let stripped = strip_q_code("1 2 , /: 3 4 / actual comment");
        assert!(stripped.contains("/:"));
        assert!(!stripped.contains("actual comment"));
    }

    #[test]
    fn split_q_script_preserves_multiline_string_trailing_whitespace() {
        let script = "s:\"line 1   \nline 2   \nline 3\";\nx:1";
        let stmts = split_q_script(script).unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].text.contains("line 1   \nline 2   \nline 3"));
    }

    #[test]
    fn candidate_function_names_extraction() {
        let f1 = extract_candidate_func_names("cleanup[]");
        assert_eq!(f1, vec!["cleanup"]);

        let f2 = extract_candidate_func_names(".ns.my_func[1; 2]");
        assert_eq!(f2, vec![".ns.my_func"]);

        let f3 = extract_candidate_func_names("select count i by sym from trade");
        assert!(f3.is_empty());

        let f4 = extract_candidate_func_names("cleanup 10; .ns.clean `trade; my_func arg");
        assert_eq!(f4, vec!["cleanup", ".ns.clean", "my_func"]);
    }
}
