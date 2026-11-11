//! Commands as data: the `CMDS` table is the single source of truth. Dispatch,
//! the usage text, and the unknown-command list are all derived from it, so
//! adding a command is one row here — not three edits across the file.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::time::Duration;

use crate::config;
use crate::conn::Conn;
use crate::error::{CliError, CmdResult};
use crate::k::K;
use crate::query;
use crate::render;
use crate::update;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum OutMode {
    #[default]
    Text,
    Json,
    Csv,
    Console,
}

impl std::str::FromStr for OutMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("text") {
            Ok(Self::Text)
        } else if s.eq_ignore_ascii_case("json") {
            Ok(Self::Json)
        } else if s.eq_ignore_ascii_case("csv") {
            Ok(Self::Csv)
        } else if s.eq_ignore_ascii_case("console") {
            Ok(Self::Console)
        } else {
            Err(())
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Opts {
    pub out: OutMode,
    pub max_rows: usize, // 0 = unlimited
    pub readonly: bool,
    pub scoped: bool,
    pub atomic: bool,              // execute entire script in a single atomic block
    pub dry_run: bool,             // validate syntax on server without mutating state
    pub tls: bool,                 // connect over TLS/SSL (CWE-319 mitigation)
    pub tls_no_verify: bool,       // skip TLS certificate verification (dev only)
    pub timeout: Option<Duration>, // None = wait forever; bounds the query round-trip
}

/// One CLI command, expressed as data.
#[derive(Debug, Clone, Copy)]
pub struct Cmd {
    pub name: &'static str,
    pub help: &'static str,
    pub spec: Spec,
}

/// How a command turns its positional args into work. Most commands are `Eval`
/// (build a q expression from the trailing arg, run it, render the result); the
/// other variants capture the only real exceptions.
#[derive(Debug, Clone, Copy)]
pub enum Spec {
    /// `<conn> [arg]` — build a q expression from `arg`, eval, render.
    /// `guard` resolves stdin (`-`) and runs the readonly + remote-function
    /// checks on the user expression before evaluating.
    Eval {
        build: fn(&str) -> CmdResult,
        guard: bool,
    },
    /// `<conn> <path.q>` — read .q file, split into statements, sequentially execute.
    Run,
    /// `<conn>` — handshake probe; prints `pong`.
    Ping,
    /// `<action> <conn>` — map `action` to a fixed q expression (web / trace).
    /// Note the reversed arg order: the action comes before the connection.
    Action(fn(&str) -> CmdResult),
    /// `<args...>` — no connection (config profile management).
    Config,
    /// `<args...>` — no connection (self-update from GitHub release).
    Update,
}

pub const CMDS: &[Cmd] = &[
    Cmd {
        name: "query",
        help: "<conn> \"<q expr>\"|-        run a q expression (- = stdin)",
        spec: Spec::Eval {
            build: build_query,
            guard: true,
        },
    },
    Cmd {
        name: "run",
        help: "<conn> <path.q>|-          send a .q file (- = stdin)",
        spec: Spec::Run,
    },
    Cmd {
        name: "tables",
        help: "<conn>                     list tables",
        spec: Spec::Eval {
            build: |a| no_extra_arg(a, "tables").map(|()| "tables[]".to_string()),
            guard: false,
        },
    },
    Cmd {
        name: "views",
        help: "<conn>                      list views",
        spec: Spec::Eval {
            build: |a| no_extra_arg(a, "views").map(|()| "views[]".to_string()),
            guard: false,
        },
    },
    Cmd {
        name: "meta",
        help: "<conn> <table>             column schema",
        spec: Spec::Eval {
            build: |t| need_table(t).map(|t| format!("meta {t}")),
            guard: false,
        },
    },
    Cmd {
        name: "count",
        help: "<conn> <table>             row count",
        spec: Spec::Eval {
            build: |t| need_table(t).map(|t| format!("count {t}")),
            guard: false,
        },
    },
    Cmd {
        name: "describe",
        help: "<conn> <table>             partitioning+schema+rows+sample",
        spec: Spec::Eval {
            build: |t| need_table(t).map(query::describe_q),
            guard: false,
        },
    },
    Cmd {
        name: "schema",
        help: "<conn>                     all tables: rows/cols/partition",
        spec: Spec::Eval {
            build: |a| no_extra_arg(a, "schema").map(|()| query::SCHEMA_Q.to_string()),
            guard: false,
        },
    },
    Cmd {
        name: "functions",
        help: "<conn> [ns]                defined functions + arity",
        spec: Spec::Eval {
            build: |ns| need_namespace(ns).map(query::functions_q),
            guard: false,
        },
    },
    Cmd {
        name: "info",
        help: "<conn>                     version/pid/port/mem/handles",
        spec: Spec::Eval {
            build: |a| no_extra_arg(a, "info").map(|()| query::INFO_Q.to_string()),
            guard: false,
        },
    },
    Cmd {
        name: "time",
        help: "<conn> \"<q expr>\"|-        elapsed ms + result count (- = stdin)",
        spec: Spec::Eval {
            build: build_time,
            guard: true,
        },
    },
    Cmd {
        name: "gc",
        help: "<conn>                     .Q.gc[] -> bytes freed to OS",
        spec: Spec::Eval {
            build: |a| no_extra_arg(a, "gc").map(|()| ".Q.gc[]".to_string()),
            guard: true,
        },
    },
    Cmd {
        name: "ping",
        help: "<conn>                     handshake probe -> pong",
        spec: Spec::Ping,
    },
    Cmd {
        name: "web",
        help: "<off|get-ok|status> <conn> tune built-in HTTP serving",
        spec: Spec::Action(web_expr),
    },
    Cmd {
        name: "trace",
        help: "<on|off|status> <conn>     .Q.trp backtraces on errors",
        spec: Spec::Action(trace_expr),
    },
    Cmd {
        name: "config",
        help: "<init|path|list|add> [--project]  manage server profiles",
        spec: Spec::Config,
    },
    Cmd {
        name: "update",
        help: "[--check] [--force]          update q-cli to the latest release",
        spec: Spec::Update,
    },
];

/// Resolve `pos[0]` against `CMDS` and run it, returning the process exit code.
#[must_use]
pub fn dispatch(pos: &[String], opts: &Opts) -> i32 {
    let mode = pos[0].as_str();
    let Some(cmd) = CMDS.iter().find(|c| c.name == mode) else {
        let names: Vec<&str> = CMDS.iter().map(|c| c.name).collect();
        let list = names.join("|");
        return finish(
            Err(CliError::usage(&format!("unknown mode '{mode}' ({list})"))),
            opts,
        );
    };

    if !matches!(cmd.spec, Spec::Run) && opts.atomic {
        return finish(
            Err(CliError::usage(
                "--atomic is only supported with 'q-cli run'",
            )),
            opts,
        );
    }
    let supports_script_flags = matches!(cmd.spec, Spec::Run) || matches!(mode, "query" | "time");
    if opts.dry_run && !supports_script_flags {
        return finish(
            Err(CliError::usage(
                "--dry-run is only supported with 'query', 'time', and 'run'",
            )),
            opts,
        );
    }
    if opts.scoped && !supports_script_flags {
        return finish(
            Err(CliError::usage(
                "--scoped is only supported with 'query', 'time', and 'run'",
            )),
            opts,
        );
    }

    match &cmd.spec {
        Spec::Config => finish(
            config::run(&pos[1..]).map_err(|e| CliError::usage(&e)),
            opts,
        ),
        Spec::Update => finish(
            update::run(&pos[1..], opts),
            opts,
        ),
        Spec::Action(make) => {
            if let Some(extra) = pos.get(3) {
                return finish(
                    Err(CliError::usage(&format!(
                        "unexpected extra argument '{extra}' for '{mode}'"
                    ))),
                    opts,
                );
            }
            // Support both `<action> <conn>` and `<conn> <action>`:
            // e.g. `web off localhost:5000` OR `web localhost:5000 off`
            let a1 = pos.get(1).map(String::as_str).unwrap_or("");
            let a2 = pos.get(2).map(String::as_str).unwrap_or("");

            let a1_is_conn =
                a1.contains(':') || a1.starts_with('@') || (a1 == "@" && !a2.is_empty());
            let a1_is_action = !a1_is_conn && !a1.is_empty() && make(a1).is_ok();
            let a2_is_action = !a2.is_empty() && make(a2).is_ok();
            let (action, conn_tok) = if a1_is_action {
                (a1, pos.get(2).map(String::as_str))
            } else if a1_is_conn || a2_is_action {
                (a2, pos.get(1).map(String::as_str))
            } else {
                (a1, pos.get(2).map(String::as_str))
            };

            // Validate the action first (so a bad verb gets its own message),
            // then enforce readonly: for web/trace, every action except `status`
            // rebinds a server message handler (.z.ph/.z.pp/.z.pg/.z.ps) — a
            // server mutation the readonly guard must also cover, not just data.
            match make(action) {
                Err(e) => finish(Err(e), opts),
                Ok(_) if opts.readonly && action != "status" => finish(
                    Err(CliError::usage(&format!(
                        "readonly: '{mode} {action}' changes server handlers; \
                         unset --readonly / Q_CLI_READONLY to allow"
                    ))),
                    opts,
                ),
                Ok(expr) => run_on(conn_tok, &expr, opts),
            }
        }
        // <conn> — handshake probe.
        Spec::Ping => {
            if let Some(extra) = pos.get(2) {
                return finish(
                    Err(CliError::usage(&format!(
                        "unexpected extra argument '{extra}' for 'ping'"
                    ))),
                    opts,
                );
            }
            match resolve(pos.get(1).map(String::as_str), mode) {
                Ok(conn) => finish(do_ping(&conn, opts), opts),
                Err(e) => finish(Err(e), opts),
            }
        }
        Spec::Run => {
            if pos.get(1).is_none() {
                return finish(
                    Err(CliError::usage("usage: q-cli run <conn> <path.q>|-")),
                    opts,
                );
            }
            if let Some(extra) = pos.get(3) {
                return finish(
                    Err(CliError::usage(&format!(
                        "unexpected extra argument '{extra}' for 'run'"
                    ))),
                    opts,
                );
            }
            let path = pos.get(2).map(String::as_str).unwrap_or("");
            if path.is_empty() {
                return finish(
                    Err(CliError::usage("usage: q-cli run <conn> <path.q>|-")),
                    opts,
                );
            }
            if path != "-" {
                let r = prepare_run(path, opts).and_then(|(content, stmts)| {
                    let conn = resolve(pos.get(1).map(String::as_str), mode)?;
                    do_run(&conn, &content, &stmts, opts)
                });
                finish(r, opts)
            } else {
                let r = resolve(pos.get(1).map(String::as_str), mode).and_then(|conn| {
                    let (content, stmts) = prepare_run(path, opts)?;
                    do_run(&conn, &content, &stmts, opts)
                });
                finish(r, opts)
            }
        }
        // <conn> [arg] — build a q expression, optionally guard, eval, render.
        Spec::Eval { build, guard } => {
            if pos.get(1).is_none() {
                return finish(
                    Err(CliError::usage(&format!("usage: q-cli {mode} <conn> ..."))),
                    opts,
                );
            }
            if let Some(extra) = pos.get(3) {
                return finish(
                    Err(CliError::usage(&format!(
                        "unexpected extra argument '{extra}' for '{mode}' \
                         (quote the q expression as a single argument)"
                    ))),
                    opts,
                );
            }
            let arg = pos.get(2).map(String::as_str).unwrap_or("");
            let accepts_stdin = matches!(mode, "query" | "time");
            if arg != "-" || !accepts_stdin {
                // Validate inline arg/readonly before resolving <conn> so bad syntax or
                // missing args report usage errors even when no server config exists.
                let r = prepare_eval(arg, *build, *guard, accepts_stdin, opts).and_then(
                    |(expr, guard_raw, stmts)| {
                        let conn = resolve(pos.get(1).map(String::as_str), mode)?;
                        do_eval(&conn, &expr, guard_raw.as_deref(), &stmts, opts)
                    },
                );
                finish(r, opts)
            } else {
                // When reading from stdin (`-`), resolve <conn> first so an invalid
                // server profile fails immediately instead of blocking on stdin.
                let r = resolve(pos.get(1).map(String::as_str), mode).and_then(|conn| {
                    let (expr, guard_raw, stmts) =
                        prepare_eval(arg, *build, *guard, accepts_stdin, opts)?;
                    do_eval(&conn, &expr, guard_raw.as_deref(), &stmts, opts)
                });
                finish(r, opts)
            }
        }
    }
}

/// Resolve the `<conn>` positional (a config name or `host:port`) into a
/// connection string, or a usage error naming the command if it's missing.
fn resolve(tok: Option<&str>, mode: &str) -> Result<String, CliError> {
    let tok = tok.ok_or_else(|| CliError::usage(&format!("usage: q-cli {mode} <conn> ...")))?;
    config::resolve_conn(tok).map_err(|e| CliError::usage(&e))
}

// --- Eval builders that need more than a one-line closure --------------------

/// Read all of stdin as the q source — the `-` argument for query/run/time, so
/// generated or multi-line expressions can be piped in instead of quoted inline.
fn slurp_stdin() -> CmdResult {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| CliError::usage(&format!("cannot read stdin: {e}")))?;
    Ok(s)
}

fn no_extra_arg(arg: &str, mode: &str) -> Result<(), CliError> {
    if arg.is_empty() {
        Ok(())
    } else {
        Err(CliError::usage(&format!(
            "unexpected argument '{arg}' for '{mode}'"
        )))
    }
}

fn build_query(e: &str) -> CmdResult {
    if e.trim().is_empty() {
        Err(CliError::usage("usage: q-cli query <conn> <expr>|-"))
    } else {
        Ok(e.to_string())
    }
}

fn prepare_eval(
    arg: &str,
    build: fn(&str) -> CmdResult,
    guard: bool,
    accepts_stdin: bool,
    opts: &Opts,
) -> Result<(String, Option<String>, Vec<QStatement>), CliError> {
    if guard {
        let raw = if accepts_stdin && arg == "-" {
            slurp_stdin()?
        } else {
            arg.to_string()
        };
        if accepts_stdin {
            if raw.trim().is_empty() {
                build(&raw)?;
            }
            let stmts = split_q_script(&raw)?;
            if stmts.is_empty() {
                return Err(CliError::usage(
                    "expression contains no executable statements (only comments or whitespace)",
                ));
            }
            if opts.readonly {
                for s in &stmts {
                    check_readonly(&s.text, opts)?;
                }
            }
            let joined = join_stmts_for_block(&stmts);
            let effective_raw = if opts.scoped {
                format!("{{[]\n{joined}\n}}[]")
            } else {
                joined
            };
            let expr = build(&effective_raw)?;
            Ok((expr, Some(raw), stmts))
        } else {
            let expr = build(&raw)?;
            check_readonly(&expr, opts)?;
            let stmts = split_q_script(&expr)?;
            Ok((expr, None, stmts))
        }
    } else {
        let expr = build(arg)?;
        Ok((expr, None, Vec::new()))
    }
}

fn prepare_run(path: &str, opts: &Opts) -> Result<(String, Vec<QStatement>), CliError> {
    let content = if path == "-" {
        slurp_stdin()?
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| CliError::usage(&format!("cannot read file '{path}': {e}")))?
    };

    let stmts = split_q_script(&content)?;
    if opts.readonly {
        for s in &stmts {
            check_readonly(&s.text, opts)?;
        }
    }
    Ok((content, stmts))
}

#[derive(Debug, PartialEq, Eq, Clone)]
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
/// - All comments (block, line, and inline comments outside strings) are stripped from
///   the returned statement text so IPC `value` evaluation never has subsequent lines
///   or closing delimiters swallowed by a `/` comment.
/// - Delimiter mismatches or unclosed strings return a syntax error before any
///   statement is executed, preventing partial script application.
pub fn split_q_script(src: &str) -> Result<Vec<QStatement>, CliError> {
    let mut stmts = Vec::new();
    let mut current_stmt: Option<(usize, Vec<String>)> = None;
    let mut in_block_comment = false;
    let mut in_str = false;
    let mut str_start_line = None;
    let mut delimiter_stack: Vec<(char, usize)> = Vec::new();

    let finish_stmt = |cur: &mut Option<(usize, Vec<String>)>, stmts: &mut Vec<QStatement>| {
        if let Some((line_no, lines)) = cur.take() {
            let joined = lines.join("\n").trim().to_string();
            if !joined.is_empty() {
                stmts.push(QStatement {
                    line_no,
                    text: joined,
                });
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

        if !in_str {
            // A slash alone at column 0 starts a multi-line comment block
            if trimmed == "/" {
                in_block_comment = true;
                continue;
            }
            // A backslash alone at column 0 ends script execution (q EOF)
            if at_top_level && trimmed == "\\" {
                finish_stmt(&mut current_stmt, &mut stmts);
                break;
            }
            // Line comment at column 0
            if line.starts_with('/') {
                continue;
            }
            if trimmed.trim().is_empty() {
                continue;
            }
        }

        let first_char = line.chars().next();
        let is_indented = first_char.is_some_and(|c| c == ' ' || c == '\t');
        let was_in_str = in_str;

        // Scan characters in line to update delimiter stack, string status,
        // and strip inline comments.
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut prev_ws = true;
        let mut comment_pos = None;
        while i < chars.len() {
            let c = chars[i];
            if in_str {
                if c == '\\' {
                    i += 2; // skip escaped character (e.g. \" or \\)
                    continue;
                }
                if c == '"' {
                    in_str = false;
                    str_start_line = None;
                    prev_ws = false;
                }
                i += 1;
                continue;
            }

            if c == '"' {
                in_str = true;
                str_start_line = Some(line_no);
                prev_ws = false;
                i += 1;
                continue;
            }

            // Inline comment: whitespace followed by '/' (excluding adverb '/:')
            if is_inline_comment(&chars, i, prev_ws) {
                comment_pos = Some(i);
                break;
            }

            match c {
                '{' | '(' | '[' => delimiter_stack.push((c, line_no)),
                '}' => match delimiter_stack.pop() {
                    Some(('{', _)) => {}
                    Some((top, start)) => {
                        return Err(CliError::usage(&format!(
                            "syntax error: mismatched closing '}}' on line {line_no}, \
                             expected closing for '{top}' opened on line {start}"
                        )));
                    }
                    None => {
                        return Err(CliError::usage(&format!(
                            "syntax error: unexpected closing '}}' on line {line_no}"
                        )));
                    }
                },
                ')' => match delimiter_stack.pop() {
                    Some(('(', _)) => {}
                    Some((top, start)) => {
                        return Err(CliError::usage(&format!(
                            "syntax error: mismatched closing ')' on line {line_no}, \
                             expected closing for '{top}' opened on line {start}"
                        )));
                    }
                    None => {
                        return Err(CliError::usage(&format!(
                            "syntax error: unexpected closing ')' on line {line_no}"
                        )));
                    }
                },
                ']' => match delimiter_stack.pop() {
                    Some(('[', _)) => {}
                    Some((top, start)) => {
                        return Err(CliError::usage(&format!(
                            "syntax error: mismatched closing ']' on line {line_no}, \
                             expected closing for '{top}' opened on line {start}"
                        )));
                    }
                    None => {
                        return Err(CliError::usage(&format!(
                            "syntax error: unexpected closing ']' on line {line_no}"
                        )));
                    }
                },
                _ => {}
            }

            prev_ws = c.is_whitespace();
            i += 1;
        }

        let cleaned_line = if let Some(pos) = comment_pos {
            let without_comment: String = chars[..pos].iter().collect();
            without_comment.trim_end().to_string()
        } else if in_str || was_in_str {
            line.to_string()
        } else {
            trimmed.to_string()
        };

        if !in_str && !was_in_str && cleaned_line.trim().is_empty() {
            continue;
        }

        if at_top_level && !is_indented {
            finish_stmt(&mut current_stmt, &mut stmts);
            current_stmt = Some((line_no, vec![cleaned_line]));
        } else if let Some((_, ref mut lines)) = current_stmt {
            lines.push(cleaned_line);
        } else {
            current_stmt = Some((line_no, vec![cleaned_line]));
        }
    }
    finish_stmt(&mut current_stmt, &mut stmts);

    if let Some(start_line) = str_start_line {
        return Err(CliError::usage(&format!(
            "syntax error: unclosed string literal starting at line {start_line}"
        )));
    }
    if let Some(&(ch, start_line)) = delimiter_stack.last() {
        return Err(CliError::usage(&format!(
            "syntax error: unclosed '{ch}' starting at line {start_line}"
        )));
    }

    Ok(stmts)
}

fn build_time(expr: &str) -> CmdResult {
    if expr.trim().is_empty() {
        Err(CliError::usage("usage: q-cli time <conn> <expr>|-"))
    } else {
        Ok(query::time_q(expr))
    }
}

// --- Action builders (web / trace) -------------------------------------------

/// `web <off|get-ok|status>` — tune the q process' built-in HTTP handlers
/// (.z.ph GET / .z.pp POST) at runtime.
fn web_expr(action: &str) -> CmdResult {
    Ok(match action {
        "off" => {
            ".z.ph:{hclose .z.w};.z.pp:{hclose .z.w};\"web: off (all HTTP closed)\"".to_string()
        }
        "get-ok" => ".z.ph:{.h.hy[`txt;\"OK\"]};.z.pp:{hclose .z.w};\
                     \"web: get-ok (GET 200, POST closed)\""
            .to_string(),
        "status" => "`ph`pp!(@[{string value x};`.z.ph;\"(default)\"];\
                     @[{string value x};`.z.pp;\"(none)\"])"
            .to_string(),
        _ => return Err(CliError::usage("web action must be: off | get-ok | status")),
    })
}

/// Preserve the original IPC handlers while tracing, including absent defaults.
fn trace_expr(action: &str) -> CmdResult {
    const WRAP: &str = "{[handler;request] .Q.trp[handler;request;\
        {-2 .Q.sbt y;'x,\"\\n\",.Q.sbt y}]}";
    const CHECK: &str = "state:.qcli.traceState; \
        if[not \"q-cli trace v1\"~state`owner; \
          '\"q-cli trace state name already in use\"]; \
        current:{@[value;x;{::}]} each `.z.pg`.z.ps; \
        if[not current~state`wrapped; \
          '\"trace handlers changed; refusing to overwrite them \
            (manual reset: delete traceState from `.qcli)\"];";
    Ok(match action {
        "on" => format!(
            "{{[] if[`traceState in key `.qcli; {CHECK} :\"trace: on\"]; \
             present:{{@[{{value x;1b}};x;{{0b}}]}} each `.z.pg`.z.ps; \
             saved:{{@[value;x;{{value}}]}} each `.z.pg`.z.ps; \
             wrapped:({WRAP}[saved 0;];{WRAP}[saved 1;]); \
             `.qcli.traceState set `owner`present`handlers`wrapped!\
               (\"q-cli trace v1\";present;saved;wrapped); \
             .z.pg:wrapped 0; .z.ps:wrapped 1; \"trace: on\"}}[]"
        ),
        "off" => format!(
            "{{[] if[not `traceState in key `.qcli; :\"trace: off\"]; \
             {CHECK} \
             if[state[`present]0; .z.pg:state[`handlers]0]; \
             if[not state[`present]0; system \"x .z.pg\"]; \
             if[state[`present]1; .z.ps:state[`handlers]1]; \
             if[not state[`present]1; system \"x .z.ps\"]; \
             delete traceState from `.qcli; \"trace: off\"}}[]"
        ),
        "status" => "`pg`ps!(@[{string value x};`.z.pg;\"(default)\"];\
                     @[{string value x};`.z.ps;\"(default)\"])"
            .to_string(),
        _ => return Err(CliError::usage("trace action must be: on | off | status")),
    })
}

// --- Execution + output ------------------------------------------------------

fn do_ping(conn: &str, opts: &Opts) -> CmdResult {
    let mut c = Conn::open(conn, opts.timeout, opts.tls, opts.tls_no_verify)?;
    let _ = c.sync("1+1")?;
    Ok("pong".to_string())
}

fn is_builtin_or_keyword(name: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "select", "exec", "update", "delete", "from", "by", "where", "count", "meta", "tables",
        "views", "cols", "keys", "key", "value", "first", "last", "sum", "avg", "min", "max",
        "prd", "med", "dev", "var", "null", "not", "and", "or", "all", "any", "distinct", "group",
        "in", "like", "bin", "binr", "asof", "lj", "uj", "ij", "pj", "aj", "aj0", "wj", "wj1",
        "xbar", "xprev", "prev", "next", "prior", "mavg", "msum", "mcount", "mdev", "mmin", "mmax",
        "ema", "deltas", "ratios", "differ", "fills", "floor", "ceil", "abs", "sqrt", "exp", "log",
        "sin", "cos", "tan", "asc", "desc", "xasc", "xdesc", "iasc", "idesc", "rank", "string",
        "type", "upper", "lower", "trim", "ltrim", "rtrim", "sv", "vs", "ss", "ssr", "raze",
        "enlist", "flip", "i", "mod", "div", "cross", "cut", "md5", "except", "inter", "union",
    ];
    BUILTINS.contains(&name)
}

fn extract_candidate_func_names(expr: &str) -> Vec<String> {
    let clean = strip_q_code(expr);
    let tokens = tokenize_for_readonly(&clean);
    let mut names = Vec::new();

    for (i, tok) in tokens.iter().enumerate() {
        if let QTok::Ident(name) = tok {
            let next = tokens.get(i + 1);
            let is_assign = matches!(
                next,
                Some(QTok::Colon | QTok::DoubleColon | QTok::CompoundColon(_))
            );
            if !is_assign
                && is_valid_q_ident(name)
                && !is_builtin_or_keyword(name)
                && !names.contains(name)
            {
                names.push(name.clone());
            }
        }
    }
    names
}

fn inspect_remote_funcs(conn: &mut Conn, expr: &str, max_depth: usize) -> Result<(), CliError> {
    let names = extract_candidate_func_names(expr);
    if names.is_empty() {
        return Ok(());
    }
    let syms: String = names.iter().map(|n| format!("`{n}")).collect();
    let probe = format!(
        "{{[syms; max_d]\n\
           denied:`delete`update`insert`upsert`set`hdel`hopen`hclose`system`exit\
                  `dpft`dpfts`dsftg`chk`view`gc`rename`value`get`eval`save`load\
                  `rsave`rload`dsave`reval`parse`hsym`hdpf`xf;\n\
           visited:()!();\n\
           violator:\"\";\n\
           queue:distinct (),syms;\n\
           steps:0;\n\
           max_steps:max_d * 20;\n\
           while[(steps < max_steps) and (count queue) and 0 = count violator;\n\
             curr:first queue;\n\
             queue:1 _ queue;\n\
             if[not curr in key visited;\n\
               visited[curr]:1;\n\
               v:@[value; curr; ::];\n\
               t:type v;\n\
               if[(t >= 100h) and t <= 112h;\n\
                 s1:@[.Q.s1; v; \"\"];\n\
                 raw:ssr[ssr[ssr[s1; \"(::)\"; \" \"]; \"[[]::\"; \"[\"]; \";::\"; \";\"];\n\
                 if[raw like \"*.z.w*\"; violator:string curr];\n\
                 src:lower raw;\n\
                 globals:$[t = 100h; \
                    @[{{g:value[x] 3; $[11h = type g; distinct g; `symbol$()]}}; v; `symbol$()]; \
                    `symbol$()];\n\
                 if[any globals in denied; violator:string curr];\n\
                 clean_src:@[src; where not src in .Q.an,\"_\"; :; \" \"];\n\
                 toks:(`$\" \" vs clean_src) except enlist `;\n\
                 if[any toks in denied; violator:string curr];\n\
                 if[any {{x like \"*\",y,\"*\"}}[src;] each \
                    (\"0:\";\"1:\";\"2:\";\"3:\";\"::\";\"@[[]\";\".[[]\";\"![[]\";\"`:\";\
                     \".z.ps\";\".z.pg\";\".z.ph\";\".z.pp\";\".z.po\";\".z.pc\"); \
                    violator:string curr];\n\
                 cands:distinct globals,toks;\n\
                 sub:$[count cands; cands where \
                    {{t:type @[value; x; ::]; (t >= 100h) and t <= 112h}} each cands; \
                    `symbol$()];\n\
                 queue:distinct queue,sub except key visited;\n\
               ];\n\
             ];\n\
             steps+:1;\n\
           ];\n\
           violator\n\
         }}[{syms}; {max_depth}]"
    );
    match conn.sync(&probe)? {
        K::CharV(violator) if !violator.is_empty() => Err(CliError::usage(&format!(
            "readonly: blocked server function '{violator}' \
             (mutating operation detected in call chain); \
             unset --readonly / Q_CLI_READONLY to allow"
        ))),
        _ => Ok(()),
    }
}

fn render_output(k: &K, opts: &Opts) -> String {
    if matches!(opts.out, OutMode::Text | OutMode::Json) && opts.max_rows > 0 {
        if let Some(total) = render::table_rows(k) {
            if total > opts.max_rows {
                let max = opts.max_rows;
                eprintln!("note: showing {max} of {total} rows (--max-rows {max})");
            }
        }
    }

    match opts.out {
        OutMode::Json => render::to_json(k, opts.max_rows),
        OutMode::Csv => render::to_csv(k),
        OutMode::Text | OutMode::Console => render::to_text(k, opts.max_rows),
    }
}

fn prepare_stmt_for_block(stmt: &str) -> String {
    let t = stmt.trim().trim_end_matches(';').trim_end();
    if let Some(rest) = t.strip_prefix('\\') {
        let cmd = rest.trim();
        let escaped = query::q_escape(cmd);
        format!("system \"{escaped}\"")
    } else {
        t.to_string()
    }
}

#[inline]
fn is_inline_comment(chars: &[char], i: usize, prev_ws: bool) -> bool {
    prev_ws && chars[i] == '/' && chars.get(i + 1) != Some(&':')
}

fn join_stmts_for_block(stmts: &[QStatement]) -> String {
    let prepared_list: Vec<String> = stmts
        .iter()
        .map(|s| prepare_stmt_for_block(&s.text))
        .filter(|s| !s.is_empty())
        .collect();
    prepared_list.join(";\n")
}

fn validate_dry_run(
    conn: &mut Conn,
    stmts: &[QStatement],
    with_line_no: bool,
) -> Result<(), CliError> {
    for s in stmts {
        let prepared = prepare_stmt_for_block(&s.text);
        let escaped = query::q_escape(&prepared);
        let probe = format!("@[parse; \"{escaped}\"; {{'x}}]");
        if let Err(e) = conn.sync(&probe) {
            let line_no = s.line_no;
            let msg = if with_line_no {
                format!("dry-run failed on line {line_no}: {}", e.msg)
            } else {
                format!("dry-run failed: {}", e.msg)
            };
            return Err(CliError { kind: e.kind, msg });
        }
    }
    Ok(())
}

/// Readonly mode is enforced by the server, not by the text guard above: `reval`
/// rejects writes, system commands and handle I/O however the expression is spelled
/// (e.g. `-6!-5!"delete from t"`). Needs kdb+ 3.3+; older servers fail closed.
fn readonly_wrap(expr: &str, readonly: bool) -> String {
    if readonly {
        let escaped = query::q_escape(expr);
        format!("reval parse \"{escaped}\"")
    } else {
        expr.to_string()
    }
}

fn do_eval(
    conn: &str,
    expr: &str,
    guard_raw: Option<&str>,
    stmts: &[QStatement],
    opts: &Opts,
) -> CmdResult {
    let mut c = Conn::open(conn, opts.timeout, opts.tls, opts.tls_no_verify)?;
    if opts.readonly {
        if let Some(raw) = guard_raw {
            inspect_remote_funcs(&mut c, raw, 5)?;
        }
    }
    if opts.dry_run {
        validate_dry_run(&mut c, stmts, false)?;
        return Ok(
            "dry-run: passed (expression validated on server with parse, zero mutations)"
                .to_string(),
        );
    }
    // --console: let q render the result string (.Q.s) for max fidelity.
    let to_send = if opts.out == OutMode::Console {
        let escaped = query::q_escape(expr);
        format!(".Q.s value \"{escaped}\"")
    } else {
        expr.to_string()
    };
    // Canned commands (info, functions, ...) need `system` and are not user-controlled.
    let to_send = readonly_wrap(&to_send, opts.readonly && guard_raw.is_some());
    let k = c.sync(&to_send)?;
    Ok(render_output(&k, opts))
}

fn do_run(conn: &str, content: &str, stmts: &[QStatement], opts: &Opts) -> CmdResult {
    let mut c = Conn::open(conn, opts.timeout, opts.tls, opts.tls_no_verify)?;

    if opts.readonly {
        inspect_remote_funcs(&mut c, content, 5)?;
    }

    // Dry-run mode: Validate syntax on the server using q `parse` with zero side effects.
    // Even if the script contains mutating operations (insert, delete, set, ::),
    // `parse` parses and checks syntax without executing any statements against the database.
    if opts.dry_run {
        validate_dry_run(&mut c, stmts, true)?;
        let count = stmts.len();
        return Ok(format!(
            "dry-run: passed ({count} statements validated on server with parse, zero mutations)"
        ));
    }

    if stmts.is_empty() {
        return Ok(render_output(&K::Null, opts));
    }

    // Atomic or Scoped mode: Send the entire script in a single block so local variables
    // declared in earlier statements remain in scope across the script.
    if opts.atomic || opts.scoped {
        let joined_stmts = join_stmts_for_block(stmts);
        let body = if opts.scoped {
            format!("{{[]\n{joined_stmts}\n}}[]")
        } else {
            joined_stmts
        };
        let to_send = if opts.atomic {
            let escaped = query::q_escape(&body);
            if opts.out == OutMode::Console {
                format!(".Q.s @[value; \"{escaped}\"; {{'x}}]")
            } else {
                format!("@[value; \"{escaped}\"; {{'x}}]")
            }
        } else if opts.out == OutMode::Console {
            let escaped = query::q_escape(&body);
            format!(".Q.s value \"{escaped}\"")
        } else {
            body
        };
        let last_k = c.sync(&readonly_wrap(&to_send, opts.readonly))?;
        return Ok(render_output(&last_k, opts));
    }

    // Sequential mode (default): Execute statement by statement with line-level failure reporting
    let mut last_k = K::Null;
    for (idx, s) in stmts.iter().enumerate() {
        let is_last = idx + 1 == stmts.len();
        let to_send = if is_last && opts.out == OutMode::Console {
            let prepared = prepare_stmt_for_block(&s.text);
            let escaped = query::q_escape(&prepared);
            format!(".Q.s value \"{escaped}\"")
        } else {
            s.text.clone()
        };
        match c.sync(&readonly_wrap(&to_send, opts.readonly)) {
            Ok(k) => {
                last_k = k;
            }
            Err(e) => {
                let line_no = s.line_no;
                let msg = e.msg;
                return Err(CliError {
                    kind: e.kind,
                    msg: format!("line {line_no}: {msg}"),
                });
            }
        }
    }

    Ok(render_output(&last_k, opts))
}

/// Resolve a conn token and run a fixed (q-cli-generated) expression on it.
fn run_on(conn_tok: Option<&str>, expr: &str, opts: &Opts) -> i32 {
    let Some(tok) = conn_tok else {
        return finish(
            Err(CliError::usage(
                "missing <conn> (e.g. localhost:5000 or @server)",
            )),
            opts,
        );
    };
    let r = config::resolve_conn(tok)
        .map_err(|e| CliError::usage(&e))
        .and_then(|conn| do_eval(&conn, expr, None, &[], opts));
    finish(r, opts)
}

/// Print a result or a structured error, return the process exit code.
#[must_use]
pub fn finish(result: CmdResult, opts: &Opts) -> i32 {
    match result {
        Ok(s) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let s = match opts.out {
                OutMode::Text | OutMode::Console => render::escape_control(&s),
                OutMode::Json | OutMode::Csv => s,
            };
            if let Err(e) = writeln!(handle, "{s}") {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    return 0;
                }
                eprintln!("ERR failed writing to stdout: {e}");
                return 1;
            }
            0
        }
        Err(e) => {
            if opts.out == OutMode::Json {
                let err_json = render::json_string(&e.msg);
                let kind_name = e.kind.name();
                eprintln!("{{\"error\":{err_json},\"kind\":\"{kind_name}\"}}");
            } else {
                eprintln!("ERR {}", render::escape_control(&e.msg));
            }
            e.kind.code()
        }
    }
}

/// Strip both string contents and comments (line comments, inline comments, block comments)
/// from a q expression so the readonly scan inspects code syntax without false positives.
#[must_use]
pub fn strip_q_code(expr: &str) -> String {
    let mut out = String::with_capacity(expr.len());
    let mut in_str = false;
    let mut in_block_comment = false;
    let mut delim_depth: usize = 0;

    for line in expr.lines() {
        let trimmed = line.trim_end();
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
            if delim_depth == 0 && trimmed == "\\" {
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
                }
                if c == '"' {
                    in_str = false;
                    out.push('"');
                    prev_ws = false;
                }
                i += 1;
                continue;
            }

            if c == '"' {
                in_str = true;
                out.push('"');
                prev_ws = false;
                i += 1;
                continue;
            }

            if is_inline_comment(&chars, i, prev_ws) {
                break;
            }

            match c {
                '{' | '(' | '[' => delim_depth += 1,
                '}' | ')' | ']' => delim_depth = delim_depth.saturating_sub(1),
                _ => {}
            }

            out.push(c);
            prev_ws = c.is_whitespace();
            i += 1;
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum QTok {
    Ident(String),
    Symbol,
    StringLit,
    NumberLit,
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
    At,
    Dot,
    Other,
}

fn tokenize_for_readonly(src: &str) -> Vec<QTok> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            tokens.push(QTok::StringLit);
            continue;
        }

        if c == '`' {
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '.' | ':' | '`'))
            {
                i += 1;
            }
            tokens.push(QTok::Symbol);
            continue;
        }

        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
        {
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | ':'))
            {
                i += 1;
            }
            tokens.push(QTok::NumberLit);
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

        if matches!(
            c,
            ',' | '+' | '-' | '*' | '%' | '&' | '|' | '^' | '_' | '#' | '!'
        ) && i + 1 < chars.len()
            && chars[i + 1] == ':'
        {
            tokens.push(QTok::CompoundColon(c));
            i += 2;
            continue;
        }

        match c {
            '(' => {
                tokens.push(QTok::OpenParen);
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(QTok::CloseParen);
                i += 1;
                continue;
            }
            '[' => {
                tokens.push(QTok::OpenBracket);
                i += 1;
                continue;
            }
            ']' => {
                tokens.push(QTok::CloseBracket);
                i += 1;
                continue;
            }
            '{' => {
                tokens.push(QTok::OpenBrace);
                i += 1;
                continue;
            }
            '}' => {
                tokens.push(QTok::CloseBrace);
                i += 1;
                continue;
            }
            ';' => {
                tokens.push(QTok::Semi);
                i += 1;
                continue;
            }
            '@' => {
                tokens.push(QTok::At);
                i += 1;
                continue;
            }
            '.' if chars
                .get(i + 1)
                .is_none_or(|&n| n.is_ascii_whitespace() || n == '(' || n == '[') =>
            {
                tokens.push(QTok::Dot);
                i += 1;
                continue;
            }
            _ => {}
        }

        if c.is_ascii_alphabetic()
            || (c == '.' && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic()))
        {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            match word.as_str() {
                "select" => tokens.push(QTok::Select),
                "exec" => tokens.push(QTok::Exec),
                "from" => tokens.push(QTok::From),
                "where" | "by" | "in" | "within" | "like" | "and" | "or" => {
                    tokens.push(QTok::Other);
                }
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
/// - File & dynamic library I/O (0:, 1:, 2:, 3:)
/// - System backslash commands (\) and k)/q) language mode switches
/// - Mutating keywords (delete, update, insert, upsert, set, value, get, eval, save, load, etc.)
/// - Amends (@[, .[, ![) and @ / . / ! used as higher-order values or with adverbs
/// - Dynamic symbol casting (`$"...") and IPC handle string execution (h "...")
/// - System/handler namespace mutations (.z., .ps.)
/// - Global assignments (ident::) and compound assignments (ident,:, ident+:, etc.)
/// - Variable assignments (ident:) outside of select/exec column projections
fn check_readonly(expr: &str, opts: &Opts) -> Result<(), CliError> {
    if !opts.readonly {
        return Ok(());
    }

    let clean = strip_q_code(expr);

    // Reject system backslash commands (\rm, \cd, \p, etc.) and k)/q) language mode switches
    for stmt in clean.split([';', '\n', '\r']) {
        let t = stmt.trim();
        if t.starts_with('\\') {
            return Err(CliError::usage(
                "readonly: blocked system command ('\\'); \
                 unset --readonly / Q_CLI_READONLY to allow",
            ));
        }
        let tl = t.to_lowercase();
        if tl.starts_with("k)") || tl.starts_with("q)") {
            return Err(CliError::usage(
                "readonly: blocked language mode prefix ('k)' / 'q)'); \
                 unset --readonly / Q_CLI_READONLY to allow",
            ));
        }
    }

    let lower = clean.to_lowercase();
    if lower.contains("`:") {
        return Err(CliError::usage(
            "readonly: blocked file/IPC handle symbol ('`:'); \
             unset --readonly / Q_CLI_READONLY to allow",
        ));
    }
    for op in ["0:", "1:", "2:", "3:"] {
        if lower.contains(op) {
            return Err(CliError::usage(&format!(
                "readonly: blocked '{op}' (file/dynamic/IPC I/O); \
                 unset --readonly / Q_CLI_READONLY to allow"
            )));
        }
    }
    for sys_var in [
        ".z.ps", ".z.pg", ".z.ph", ".z.pp", ".z.po", ".z.pc", ".q.trp",
    ] {
        if lower.contains(sys_var) {
            return Err(CliError::usage(&format!(
                "readonly: blocked '{sys_var}'; unset --readonly / Q_CLI_READONLY to allow"
            )));
        }
    }
    // Block .z.w (lowercase only, avoiding false positive on read-only .z.W open handles)
    if clean.contains(".z.w") {
        return Err(CliError::usage(
            "readonly: blocked '.z.w'; unset --readonly / Q_CLI_READONLY to allow",
        ));
    }

    const DENIED: &[&str] = &[
        "delete", "update", "insert", "upsert", "set", "hdel", "hopen", "hclose", "system", "exit",
        "dpft", "dpfts", "dsftg", "chk", "view", "gc", "rename", "value", "get", "eval", "save",
        "load", "rsave", "rload", "dsave", "reval", "parse", "hsym", "hdpf", "xf",
    ];
    let lower_bytes = lower.as_bytes();
    let mut idx = 0;
    while idx < lower_bytes.len() {
        let b = lower_bytes[idx];
        if b.is_ascii_alphanumeric() || b == b'_' {
            let start = idx;
            while idx < lower_bytes.len()
                && (lower_bytes[idx].is_ascii_alphanumeric() || lower_bytes[idx] == b'_')
            {
                idx += 1;
            }
            let tok = &lower[start..idx];
            if DENIED.contains(&tok) {
                let is_safe_sym_literal = is_comparison_sym_literal(lower_bytes, start, idx);
                if !is_safe_sym_literal {
                    return Err(CliError::usage(&format!(
                        "readonly: blocked '{tok}'; unset --readonly / Q_CLI_READONLY to allow"
                    )));
                }
            }
        } else {
            idx += 1;
        }
    }

    // Check character-level patterns:
    // - Dynamic symbol cast from string (`$..., "s"$..., "S"$..., 11h$..., -11h$...)
    // - Amend @[...] / .[...] / ![...], adverbial !/ ./ @/, or higher-order @ / . / ! values
    let chars: Vec<char> = clean.chars().collect();
    for i in 0..chars.len() {
        // Block dynamic symbol casts (e.g. `$"del", "s"$"del", (-11h)$"del", (`)$"del")
        if chars[i] == '$' {
            let mut k = i;
            while k > 0 && chars[k - 1].is_ascii_whitespace() {
                k -= 1;
            }
            if k > 0 {
                let prev_c = chars[k - 1];
                let is_sym_target = prev_c == '`'
                    || prev_c == '"'
                    || prev_c == ')'
                    || prev_c == ']'
                    || (prev_c.is_ascii_alphanumeric() && {
                        let mut w_start = k - 1;
                        while w_start > 0 && chars[w_start - 1].is_ascii_alphanumeric() {
                            w_start -= 1;
                        }
                        let word: String = chars[w_start..k].iter().collect();
                        let wl = word.to_lowercase();
                        wl == "symbol" || wl == "s" || wl == "11h" || wl == "-11h"
                    });
                if is_sym_target {
                    let mut after = i + 1;
                    while after < chars.len() && chars[after].is_ascii_whitespace() {
                        after += 1;
                    }
                    let is_empty_cast =
                        after + 1 < chars.len() && chars[after] == '(' && chars[after + 1] == ')';
                    if !is_empty_cast {
                        return Err(CliError::usage(
                            "readonly: blocked dynamic symbol cast ('$'); \
                             unset --readonly / Q_CLI_READONLY to allow",
                        ));
                    }
                }
            }
        }

        if matches!(chars[i], '@' | '.' | '!') {
            // If '.' is part of a qualified identifier (.ns.x) or decimal (1.5), skip it
            if chars[i] == '.' {
                let prev_alnum = i > 0
                    && (chars[i - 1].is_ascii_alphanumeric()
                        || chars[i - 1] == '_'
                        || chars[i - 1] == '`');
                let next_alnum = i + 1 < chars.len()
                    && (chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_');
                if prev_alnum || next_alnum {
                    continue;
                }
                // Allow root namespace symbol `.` when passed to tables, views, key, count
                let mut before = i;
                while before > 0
                    && (chars[before - 1].is_ascii_whitespace() || chars[before - 1] == '[')
                {
                    before -= 1;
                }
                let mut w_start = before;
                while w_start > 0
                    && (chars[w_start - 1].is_ascii_alphabetic() || chars[w_start - 1] == '_')
                {
                    w_start -= 1;
                }
                let prev_word: String = chars[w_start..before].iter().collect();
                if matches!(
                    prev_word.to_lowercase().as_str(),
                    "tables" | "views" | "key" | "count" | "system"
                ) {
                    continue;
                }
            }
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_whitespace() || chars[j] == ')') {
                j += 1;
            }
            if j < chars.len() && chars[j] == '[' {
                return Err(CliError::usage(
                    "readonly: blocked amend ('@[' / '.[' / '!['); \
                     unset --readonly / Q_CLI_READONLY to allow",
                ));
            }
            // Block adverbs on @ / . / ! (e.g., !/, ./, @', !/:) or using @ / . / ! as a value
            // before `;`, `)`, `]`, `}`, `,` or end of expression
            if j >= chars.len()
                || matches!(chars[j], ';' | ')' | ']' | '}' | ',' | '/' | '\\' | '\'')
            {
                return Err(CliError::usage(&format!(
                    "readonly: blocked operator '{}' used as value or with adverb; \
                     unset --readonly / Q_CLI_READONLY to allow",
                    chars[i]
                )));
            }
        }
    }

    // Validate assignments and IPC handle calls:
    // - ident::, ident,:, and ident: (outside select/exec projection and lambda local scopes)
    // - handle "query" or handle["query"]
    let tokens = tokenize_for_readonly(&clean);
    let mut scope_stack: Vec<bool> = Vec::new();
    let mut in_projection = false;
    let mut brace_depth: usize = 0;
    let mut prev_token: Option<&QTok> = None;

    for tok in &tokens {
        match tok {
            QTok::Ident(name) => {
                let is_builtin = is_builtin_or_keyword(name);
                let prev_is_builtin = match prev_token {
                    Some(QTok::Ident(prev_name)) => is_builtin_or_keyword(prev_name),
                    _ => false,
                };
                if (matches!(prev_token, Some(QTok::NumberLit)) && !is_builtin)
                    || (brace_depth > 0
                        && matches!(prev_token, Some(QTok::Ident(_) | QTok::CloseParen))
                        && !is_builtin
                        && !prev_is_builtin)
                {
                    return Err(CliError::usage(
                        "readonly: blocked potential IPC handle invocation; \
                         unset --readonly / Q_CLI_READONLY to allow",
                    ));
                }
            }
            QTok::Select | QTok::Exec => {
                in_projection = true;
            }
            QTok::From => {
                in_projection = false;
            }
            QTok::StringLit => {
                if let Some(prev) = prev_token {
                    let prev_is_safe_str_op = match prev {
                        QTok::Ident(w) => {
                            matches!(
                                w.as_str(),
                                "like"
                                    | "ss"
                                    | "ssr"
                                    | "vs"
                                    | "sv"
                                    | "enlist"
                                    | "raze"
                                    | "count"
                                    | "first"
                                    | "last"
                                    | "reverse"
                                    | "upper"
                                    | "lower"
                                    | "trim"
                                    | "ltrim"
                                    | "rtrim"
                                    | "string"
                                    | "type"
                                    | "null"
                                    | "distinct"
                                    | "group"
                                    | "in"
                                    | "within"
                                    | "except"
                                    | "inter"
                                    | "union"
                                    | "cross"
                                    | "asc"
                                    | "desc"
                                    | "iasc"
                                    | "idesc"
                                    | "md5"
                                    | "cut"
                            )
                        }
                        _ => false,
                    };
                    if matches!(
                        prev,
                        QTok::NumberLit | QTok::CloseParen | QTok::CloseBracket | QTok::CloseBrace
                    ) || (matches!(prev, QTok::Ident(_))
                        && (brace_depth > 0 || !prev_is_safe_str_op))
                    {
                        return Err(CliError::usage(
                            "readonly: blocked potential IPC handle or dynamic string call; \
                             unset --readonly / Q_CLI_READONLY to allow",
                        ));
                    }
                }
            }
            QTok::OpenBrace => {
                brace_depth += 1;
                scope_stack.push(in_projection);
                in_projection = false;
            }
            QTok::CloseBrace => {
                brace_depth = brace_depth.saturating_sub(1);
                if let Some(prev) = scope_stack.pop() {
                    in_projection = prev;
                }
            }
            QTok::OpenBracket => {
                // A computed value or lambda argument can be an IPC handle.
                if matches!(
                    prev_token,
                    Some(QTok::NumberLit | QTok::CloseParen | QTok::CloseBracket)
                ) || (brace_depth > 0 && matches!(prev_token, Some(QTok::Ident(_))))
                {
                    return Err(CliError::usage(
                        "readonly: blocked potential IPC handle invocation; \
                         unset --readonly / Q_CLI_READONLY to allow",
                    ));
                }
                scope_stack.push(in_projection);
                in_projection = false;
            }
            QTok::OpenParen => {
                if matches!(prev_token, Some(QTok::NumberLit))
                    || (brace_depth > 0 && matches!(prev_token, Some(QTok::Ident(_))))
                {
                    return Err(CliError::usage(
                        "readonly: blocked potential IPC handle invocation; \
                         unset --readonly / Q_CLI_READONLY to allow",
                    ));
                }
                scope_stack.push(in_projection);
                in_projection = false;
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
            QTok::At | QTok::Dot => {
                if matches!(
                    prev_token,
                    Some(
                        QTok::NumberLit
                            | QTok::CloseParen
                            | QTok::CloseBracket
                            | QTok::CloseBrace
                    )
                ) || (brace_depth > 0 && matches!(prev_token, Some(QTok::Ident(_))))
                {
                    return Err(CliError::usage(
                        "readonly: blocked potential IPC handle invocation via '@' / '.'; \
                         unset --readonly / Q_CLI_READONLY to allow",
                    ));
                }
            }
            QTok::DoubleColon => match prev_token {
                Some(QTok::Ident(name)) => {
                    return Err(CliError::usage(&format!(
                        "readonly: blocked global assignment ('{name}::'); \
                             unset --readonly / Q_CLI_READONLY to allow"
                    )));
                }
                Some(QTok::CloseBracket | QTok::CloseParen) => {
                    return Err(CliError::usage(
                        "readonly: blocked global assignment ('::'); \
                             unset --readonly / Q_CLI_READONLY to allow",
                    ));
                }
                _ => {}
            },
            QTok::CompoundColon(op) => match prev_token {
                Some(QTok::Ident(name)) => {
                    return Err(CliError::usage(&format!(
                        "readonly: blocked compound assignment ('{name}{op}:'); \
                             unset --readonly / Q_CLI_READONLY to allow"
                    )));
                }
                Some(QTok::CloseBracket | QTok::CloseParen) => {
                    return Err(CliError::usage(&format!(
                        "readonly: blocked compound assignment ('{op}:'); \
                             unset --readonly / Q_CLI_READONLY to allow"
                    )));
                }
                _ => {}
            },
            QTok::Colon => match prev_token {
                Some(QTok::Ident(name))
                    if name.contains('.') || (!in_projection && brace_depth == 0) =>
                {
                    return Err(CliError::usage(&format!(
                        "readonly: blocked assignment ('{name}:'); \
                             unset --readonly / Q_CLI_READONLY to allow"
                    )));
                }
                Some(QTok::CloseBracket | QTok::CloseParen) => {
                    return Err(CliError::usage(
                        "readonly: blocked indexed/destructured assignment (':'); \
                             unset --readonly / Q_CLI_READONLY to allow",
                    ));
                }
                _ => {}
            },
            _ => {}
        }
        prev_token = Some(tok);
    }

    Ok(())
}

/// True if a `DENIED` word at `bytes[start..end]` is part of a symbol literal
/// (`\`word` or `\`a\`word`) that immediately follows a read-only boolean comparison
/// operator (`=`, `<>`, `~`, `in`, `within`) and is not followed by a function argument.
fn is_comparison_sym_literal(bytes: &[u8], start: usize, end: usize) -> bool {
    if start == 0 || bytes[start - 1] != b'`' {
        return false;
    }
    // Walk back to the start of the symbol or symbol vector (`a`b`c)
    let mut sym_start = start - 1;
    while sym_start > 0 {
        let mut p = sym_start;
        while p > 0 && (bytes[p - 1].is_ascii_alphanumeric() || bytes[p - 1] == b'_') {
            p -= 1;
        }
        if p > 0 && bytes[p - 1] == b'`' {
            sym_start = p - 1;
        } else {
            break;
        }
    }
    // Skip whitespace before the symbol vector
    let mut before = sym_start;
    while before > 0 && bytes[before - 1].is_ascii_whitespace() {
        before -= 1;
    }
    if before == 0 {
        return false;
    }
    let preceded_by_cmp = if matches!(bytes[before - 1], b'=' | b'~')
        || (before >= 2 && &bytes[before - 2..before] == b"<>")
    {
        true
    } else if bytes[before - 1].is_ascii_alphabetic() {
        let mut w_start = before - 1;
        while w_start > 0 && bytes[w_start - 1].is_ascii_alphabetic() {
            w_start -= 1;
        }
        let prev_word = &bytes[w_start..before];
        matches!(prev_word, b"in" | b"within")
    } else {
        false
    };
    if !preceded_by_cmp {
        return false;
    }

    // Walk forward to the end of the symbol vector (`a`b`c)
    let mut sym_end = end;
    while sym_end < bytes.len() && bytes[sym_end] == b'`' {
        sym_end += 1;
        while sym_end < bytes.len()
            && (bytes[sym_end].is_ascii_alphanumeric() || bytes[sym_end] == b'_')
        {
            sym_end += 1;
        }
    }
    while sym_end < bytes.len() && bytes[sym_end].is_ascii_whitespace() {
        sym_end += 1;
    }
    // After a comparison symbol literal, only end-of-input, `;`, `,`, `)`, `]`, `}`, or
    // query clauses (`from`, `by`, `where`, `and`, `or`) are safe.
    if sym_end >= bytes.len() || matches!(bytes[sym_end], b';' | b',' | b')' | b']' | b'}') {
        return true;
    }
    if bytes[sym_end].is_ascii_alphabetic() {
        let w_start = sym_end;
        let mut w_end = sym_end;
        while w_end < bytes.len() && bytes[w_end].is_ascii_alphabetic() {
            w_end += 1;
        }
        return matches!(
            &bytes[w_start..w_end],
            b"from" | b"by" | b"where" | b"and" | b"or"
        );
    }
    false
}

/// True if `ident` is a valid q identifier or dot-qualified identifier
/// (e.g. `trade`, `t_1`, `.ns.trade`). Rejects empty segments like `..` or `trade.`.
fn is_valid_q_ident(ident: &str) -> bool {
    let body = ident.strip_prefix('.').unwrap_or(ident);
    if body.is_empty() {
        return false;
    }
    body.split('.').all(|seg| {
        let mut chars = seg.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Validate a table argument is a plain q identifier (optionally namespaced)
/// before it's interpolated into a q expression, so a stray name like `foo bar`
/// gives a clean usage error (exit 2) instead of a confusing q parse error.
fn need_table(t: &str) -> Result<&str, CliError> {
    if t.is_empty() {
        return Err(CliError::usage("this command needs a table name"));
    }
    if is_valid_q_ident(t) {
        Ok(t)
    } else {
        Err(CliError::usage(&format!(
            "invalid table name '{t}' (expected a q identifier, e.g. trade or .ns.trade)"
        )))
    }
}

/// Validate an optional namespace argument (`""` / `"."` for root, or `.ns` / `ns`)
/// before interpolating it into `functions_q`.
fn need_namespace(ns: &str) -> Result<&str, CliError> {
    if ns.is_empty() || ns == "." {
        return Ok("");
    }
    if is_valid_q_ident(ns) {
        Ok(ns)
    } else {
        Err(CliError::usage(&format!(
            "invalid namespace '{ns}' (expected a q namespace, e.g. .q, .Q, or my_ns)"
        )))
    }
}

fn usage_string() -> String {
    let mut s = String::from("q-cli — native kdb+ IPC client\n\nUSAGE:\n");
    for c in CMDS {
        let out = matches!(c.spec, Spec::Eval { .. } | Spec::Run);
        let prefix = if out { "[OUT] " } else { "      " };
        let name = c.name;
        let help = c.help;
        let _ = writeln!(s, "  q-cli {prefix}{name:<9} {help}");
    }
    s.push_str(
        "\n\
         query/run/time accept '-' to read the q source from stdin.\n\
         CONN: [host]:port[:user:pass] | @name | name | @ (default)\n\
         \x20 config (merged, project overrides global):\n\
         \x20   global  $Q_CLI_CONFIG or ~/.config/q-cli/servers.conf\n\
         \x20   project nearest .q-cli.conf up from CWD  (config --project writes ./.q-cli.conf)\n\
         \n\
         OUT (default: aligned text, 50-row cap; or Q_CLI_OUT=text|json|csv|console):\n\
         \x20 --text          aligned text table/value output (default)\n\
         \x20 --json, -j      JSON (tables -> array of row objects)\n\
         \x20 --csv           CSV (tables only; uncapped)\n\
         \x20 --console       let the q server format via .Q.s\n\
         \x20 --max-rows N    row cap for text/json (0 = unlimited; or Q_CLI_MAX_ROWS)\n\
         \x20 --timeout MS    query wait in ms (default 30000; or Q_CLI_TIMEOUT)\n\
         \x20 Q_CLI_MAX_RESPONSE_MB  largest response accepted, in MB (default 256)\n\
         \x20 --readonly      reject mutating q (or set Q_CLI_READONLY=1)\n\
         \x20 --scoped        evaluate inside {[] ... }[] to isolate locals\n\
         \x20 --atomic        run .q script as a single atomic protected block\n\
         \x20 --dry-run       validate syntax on server with parse (zero mutations)\n\
         \x20 --tls           connect over TLS/SSL (CWE-319; or Q_CLI_TLS=1)\n\
         \x20 --tls-no-verify skip TLS cert validation (or --insecure / Q_CLI_TLS_NO_VERIFY=1)\n\
         \n\
         EXIT: 0 ok · 2 usage/policy · 3 connection · 4 q error · 5 timeout\n",
    );
    s
}

pub fn print_help() {
    print!("{}", usage_string());
}

pub fn print_usage() {
    eprint!("{}", usage_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readonly_opts() -> Opts {
        Opts {
            readonly: true,
            ..Opts::default()
        }
    }

    #[test]
    fn readonly_wrap_delegates_enforcement_to_reval() {
        assert_eq!(readonly_wrap("count t", false), "count t");
        assert_eq!(
            readonly_wrap("-6!-5!\"delete from `t\"", true),
            "reval parse \"-6!-5!\\\"delete from `t\\\"\""
        );
    }

    #[test]
    #[ignore = "requires Q_CLI_TEST_Q pointing to a licensed q executable"]
    fn trace_preserves_handlers_on_a_real_q_process() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let executable = std::env::var_os("Q_CLI_TEST_Q").expect("set Q_CLI_TEST_Q");
        let on = trace_expr("on").expect("valid action");
        let off = trace_expr("off").expect("valid action");
        let commands = [
            "system \"x .z.pg\";system \"x .z.ps\"",
            &off,
            &on,
            &on,
            "if[not 2~.z.pg[\"1+1\"]; '\"default handler changed\"]",
            &off,
            &off,
            "if[any {@[{value x;1b};x;{0b}]} each `.z.pg`.z.ps; \
              '\"absent handlers were not restored\"]",
            ".z.pg:{[request] 99};.z.ps:{[request] 98}",
            "originalPg:.z.pg;originalPs:.z.ps",
            &on,
            &on,
            "if[not 99~.z.pg[\"1+1\"]; '\"sync policy bypassed\"]",
            "if[not 98~.z.ps[\"1+1\"]; '\"async policy bypassed\"]",
            &off,
            &off,
            "if[not originalPg~.z.pg; '\"sync handler not restored\"]",
            "if[not originalPs~.z.ps; '\"async handler not restored\"]",
            &on,
            ".z.ps:{[request] 97}",
            &format!(
                "if[not 1b~@[{{{off};0b}};::;{{1b}}]; \
                 '\"changed async handler overwritten\"]"
            ),
            "if[not 97~.z.ps[\"1+1\"]; '\"changed async policy lost\"]",
            &format!(
                "if[not 1b~@[{{{on};0b}};::;{{1b}}]; \
                 '\"changed handler overwritten by repeated on\"]"
            ),
            "-1 \"Q_CLI_TRACE_TEST_OK\"",
        ];
        let script = format!(
            ".[{{[]\n{}\n}};();{{-2 x;exit 1}}];\nexit 0\n",
            commands.join(";\n")
        );
        let mut child = Command::new(executable)
            .arg("-q")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start q");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())
            .expect("send q assertions");
        let output = child.wait_with_output().expect("wait for q");
        assert!(
            output.status.success()
                && output
                    .stdout
                    .windows(b"Q_CLI_TRACE_TEST_OK".len())
                    .any(|part| part == b"Q_CLI_TRACE_TEST_OK"),
            "q trace assertions failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn strip_keeps_code_drops_string_contents() {
        assert_eq!(strip_q_code("a\"delete\"b").trim(), "a\"\"b");
        assert_eq!(strip_q_code("like \"*x\\\"y*\"").trim(), "like \"\"");
        assert_eq!(strip_q_code("no strings here").trim(), "no strings here");
    }

    #[test]
    fn prepare_stmt_for_block_converts_backslash() {
        assert_eq!(prepare_stmt_for_block("\\c 25 120"), "system \"c 25 120\"");
        assert_eq!(prepare_stmt_for_block("\\p 5000"), "system \"p 5000\"");
        assert_eq!(prepare_stmt_for_block("select from t;"), "select from t");
    }

    #[test]
    fn readonly_blocks_mutating_code() {
        assert!(check_readonly("delete from t", &readonly_opts()).is_err());
        assert!(check_readonly("`t insert (1;2)", &readonly_opts()).is_err());
        assert!(check_readonly("save `:x set til 3", &readonly_opts()).is_err());
        assert!(check_readonly("(`:f) 0: \"x\"", &readonly_opts()).is_err()); // file I/O
    }

    #[test]
    fn readonly_blocks_computed_and_lambda_handle_calls() {
        for expr in [
            "(0)[\"reviewX:1\"]",
            "((0))[\"reviewX:1\"]",
            "{x[\"reviewX:1\"]}[0]",
            "{x[y]}[0;\"reviewX:1\"]",
            "{x y}[0;\"reviewX:1\"]",
            "{(x)y}[0;\"reviewX:1\"]",
            "{[count]count[\"reviewX:1\"]}[0]",
            "{x(\"reviewX:1\")}[0]",
            "{[count]count \"reviewX:1\"}[0]",
            "(first key .z.W)[\"reviewX:1\"]",
            "{0}[][\"reviewX:1\"]",
            "{0}[] \"reviewX:1\"",
            "0@\"reviewA:1\"",
            "(0)@\"reviewA:1\"",
            "{x@y}[0;\"reviewA:1\"]",
            "0 . enlist \"reviewA:1\"",
            "(first key .z.W)@\"reviewA:1\"",
        ] {
            assert!(check_readonly(expr, &readonly_opts()).is_err(), "{expr}");
        }
        for expr in [
            "{x+1}[1]",
            "{[x] y:x+1;y*2}[10]",
            "like[\"abc\";\"a*\"]",
            "{sum x}[1 2]",
            "exec {count x} each sym from t",
        ] {
            assert!(check_readonly(expr, &readonly_opts()).is_ok(), "{expr}");
        }
    }

    #[test]
    fn readonly_function_candidates_are_case_sensitive() {
        assert!(is_builtin_or_keyword("count"));
        for name in ["COUNT", "Count", "SELECT", "EXEC", "FROM"] {
            assert!(!is_builtin_or_keyword(name), "{name}");
            assert_eq!(
                extract_candidate_func_names(&format!("{name}[]")),
                vec![name]
            );
        }
    }

    #[test]
    fn readonly_allows_reads_and_string_literal_keywords() {
        assert!(check_readonly("select from t", &readonly_opts()).is_ok());
        // the keyword appears only inside a string literal -> not a mutation
        assert!(check_readonly(
            "select from t where note like \"*delete*\"",
            &readonly_opts()
        )
        .is_ok());
        // symbol literals matching denied names are allowed in read queries
        assert!(check_readonly("select from log where action = `delete", &readonly_opts()).is_ok());
        assert!(check_readonly(
            "select from log where action = `delete, sym = `AAPL",
            &readonly_opts()
        )
        .is_ok());
        assert!(check_readonly(
            "select from log where action in `insert`update",
            &readonly_opts()
        )
        .is_ok());
        // Calling a symbol as a function (bracket, prefix, or infix) or handle symbols is blocked
        assert!(check_readonly("`insert[t; (1;2)]", &readonly_opts()).is_err());
        assert!(check_readonly("`set (`t; 1)", &readonly_opts()).is_err());
        assert!(check_readonly("`t `insert (1;2)", &readonly_opts()).is_err());
        assert!(check_readonly("`::localhost:5000 \"1+1\"", &readonly_opts()).is_err());
        assert!(check_readonly("(@)[`t;`a;:;1]", &readonly_opts()).is_err());
        assert!(check_readonly("(.)[`t;();:;1]", &readonly_opts()).is_err());
        assert!(check_readonly("{trade[0]: 1}[]", &readonly_opts()).is_err());
        assert!(check_readonly("{(a;.ns.b): (1;2)}[]", &readonly_opts()).is_err());
    }

    #[test]
    fn readonly_off_allows_everything() {
        let opts = Opts {
            readonly: false,
            ..readonly_opts()
        };
        assert!(check_readonly("delete from t", &opts).is_ok());
    }

    #[test]
    fn table_name_validation() {
        assert!(need_table("trade").is_ok());
        assert!(need_table(".ns.trade").is_ok());
        assert!(need_table("t_1").is_ok());
        assert!(need_table("foo bar").is_err());
        assert!(need_table("").is_err());
        assert!(need_table("1trade").is_err()); // must not start with a digit
        assert!(need_table(".").is_err()); // cannot be only dots
        assert!(need_table("..").is_err());
        assert!(need_table("._.").is_err());
        assert!(need_table("..trade").is_err());
        assert!(need_table("trade.").is_err());
        assert!(need_table(".ns..trade").is_err());
    }

    #[test]
    fn namespace_validation() {
        assert_eq!(need_namespace("").unwrap(), "");
        assert_eq!(need_namespace(".").unwrap(), "");
        assert_eq!(need_namespace(".Q").unwrap(), ".Q");
        assert_eq!(need_namespace("u").unwrap(), "u");
        assert!(need_namespace("q; delete from trade").is_err());
        assert!(need_namespace("..").is_err());
        assert!(need_namespace("..ns").is_err());
        assert!(need_namespace("ns.").is_err());
        assert!(need_namespace("1ns").is_err());
    }

    #[test]
    fn time_command_works_with_readonly_and_blocks_mutations() {
        let (expr, guard_raw, stmts) = prepare_eval(
            "select from trade",
            build_time,
            true,
            true,
            &readonly_opts(),
        )
        .unwrap();
        assert!(expr.contains(".z.p"));
        assert_eq!(guard_raw.as_deref(), Some("select from trade"));
        assert_eq!(stmts.len(), 1);

        assert!(prepare_eval(
            "delete from trade",
            build_time,
            true,
            true,
            &readonly_opts()
        )
        .is_err());
        assert!(prepare_eval("   ", build_time, true, true, &readonly_opts()).is_err());
        assert!(prepare_eval("/ only comment", build_query, true, true, &readonly_opts()).is_err());
        assert!(prepare_eval("", build_query, true, true, &readonly_opts()).is_err());
        assert!(prepare_eval("   ", build_query, true, true, &readonly_opts()).is_err());
        assert!(prepare_eval(
            "",
            |a| no_extra_arg(a, "gc").map(|()| ".Q.gc[]".to_string()),
            true,
            false,
            &readonly_opts()
        )
        .is_err());
        // `gc -` must fail immediately as an unexpected argument, not read stdin
        assert!(prepare_eval(
            "-",
            |a| no_extra_arg(a, "gc").map(|()| ".Q.gc[]".to_string()),
            true,
            false,
            &Opts::default()
        )
        .is_err());
        assert!(no_extra_arg("extra", "tables").is_err());
        assert!(check_readonly("(`:host:5000) 3: \"1+1\"", &readonly_opts()).is_err());
    }

    #[test]
    fn readonly_blocks_value_amend_and_namespaces() {
        assert!(check_readonly("value \"1+1\"", &readonly_opts()).is_err());
        assert!(check_readonly("@[`t;`a;:;1]", &readonly_opts()).is_err());
        assert!(check_readonly(".[t;(0;`a);:;1]", &readonly_opts()).is_err());
        assert!(check_readonly("![`t;();0b;`symbol$()]", &readonly_opts()).is_err());
        assert!(check_readonly(".z.pg:{x}", &readonly_opts()).is_err());
        assert!(check_readonly(".ps.f:{x}", &readonly_opts()).is_err());

        // Harmless read-only .z. variables must be allowed
        assert!(check_readonly(".z.p", &readonly_opts()).is_ok());
        assert!(check_readonly(".z.K", &readonly_opts()).is_ok());
        assert!(check_readonly("select from t where date = .z.d", &readonly_opts()).is_ok());
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

        let mismatched = "f:{[x] (1+2]}";
        assert!(split_q_script(mismatched).is_err());

        let unexpected_close = "1+2)";
        assert!(split_q_script(unexpected_close).is_err());
    }

    #[test]
    fn readonly_blocks_assignments_and_system_commands() {
        assert!(check_readonly("t: 0#t", &readonly_opts()).is_err());
        assert!(check_readonly("trade: 1", &readonly_opts()).is_err());
        assert!(check_readonly("trade::1", &readonly_opts()).is_err());
        assert!(check_readonly("t,: (1; 2)", &readonly_opts()).is_err());
        assert!(check_readonly("t +: 1", &readonly_opts()).is_err());
        assert!(check_readonly("t #: 10", &readonly_opts()).is_err());
        assert!(check_readonly("d !: 1", &readonly_opts()).is_err());
        assert!(check_readonly("trade: select from trade", &readonly_opts()).is_err());
        assert!(check_readonly("\"lib.so\" 2: (\"f\"; 1)", &readonly_opts()).is_err());
        assert!(check_readonly("\\rm -rf /", &readonly_opts()).is_err());
        assert!(check_readonly("1+1; \\cd /", &readonly_opts()).is_err());
        assert!(check_readonly("1+1\n\\rm -rf /", &readonly_opts()).is_err());
        assert!(check_readonly("select from t\n\\p 5000", &readonly_opts()).is_err());
        assert!(check_readonly("get \"delete from t\"", &readonly_opts()).is_err());
        assert!(check_readonly("eval (insert; `t; 1)", &readonly_opts()).is_err());
        assert!(check_readonly("trade[0]: 1", &readonly_opts()).is_err());
        assert!(check_readonly("trade[0]:: 1", &readonly_opts()).is_err());
        assert!(check_readonly("trade[0] +: 1", &readonly_opts()).is_err());
        assert!(check_readonly("(a;b): (1;2)", &readonly_opts()).is_err());
    }

    #[test]
    fn readonly_allows_select_aliases_comments_and_generic_null() {
        assert!(check_readonly(
            "select c: count i, max_p: max price from trade",
            &readonly_opts()
        )
        .is_ok());
        assert!(check_readonly("exec p: last price from trade", &readonly_opts()).is_ok());
        assert!(check_readonly("(::)", &readonly_opts()).is_ok());
        assert!(check_readonly("f[::; 1]", &readonly_opts()).is_ok());
        // Comments containing denied keywords must not block
        assert!(check_readonly("select from trade / delete from trade", &readonly_opts()).is_ok());
    }

    #[test]
    fn readonly_allows_lambda_local_variables_but_blocks_globals() {
        // Local variables inside { ... } should be allowed
        assert!(
            check_readonly("{tmp: 1; select from t where x > tmp}[]", &readonly_opts()).is_ok()
        );
        assert!(check_readonly("{[x] y: x + 1; y*2}[10]", &readonly_opts()).is_ok());
        assert!(check_readonly("{trade: 0#trade}[]", &readonly_opts()).is_ok());

        // Global assignment :: or namespaced assignment with . inside lambda must still be blocked
        assert!(check_readonly("{trade:: 0#trade}[]", &readonly_opts()).is_err());
        assert!(check_readonly("{x::1}[]", &readonly_opts()).is_err());
        assert!(check_readonly("{.t: 1}[]", &readonly_opts()).is_err());
        assert!(check_readonly("{.ns.x: 1}[]", &readonly_opts()).is_err());
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
        assert_eq!(f3, vec!["sym", "trade"]);

        let f4 = extract_candidate_func_names("cleanup 10; .ns.clean `trade; my_func arg; (f2)[1]");
        assert_eq!(f4, vec!["cleanup", ".ns.clean", "my_func", "arg", "f2"]);
    }

    #[test]
    fn readonly_blocks_advanced_bypasses() {
        assert!(check_readonly("k).\"delete from trade\"", &readonly_opts()).is_err());
        assert!(check_readonly("q)delete from trade", &readonly_opts()).is_err());
        assert!(check_readonly("`$\"delete\"", &readonly_opts()).is_err());
        assert!(check_readonly("`symbol$\"set\"", &readonly_opts()).is_err());
        assert!(check_readonly("0 \"delete from trade\"", &readonly_opts()).is_err());
        assert!(check_readonly("h \"delete from trade\"", &readonly_opts()).is_err());
        assert!(check_readonly("(`select; trade: 0#trade)", &readonly_opts()).is_err());
        assert!(check_readonly("select (trade: 0#trade) from trade", &readonly_opts()).is_err());
        assert!(check_readonly("select trade[0]: 1 from trade", &readonly_opts()).is_err());
        assert!(check_readonly("select .z.pg: {x} from trade", &readonly_opts()).is_err());
        assert!(check_readonly("{f: !; f[`t; (); 0b; `symbol$()]}[]", &readonly_opts()).is_err());
        assert!(check_readonly("{[f] f[`t; (); :; 1]}[.]", &readonly_opts()).is_err());
        assert!(check_readonly("(@/) (`t; `a; :; 1)", &readonly_opts()).is_err());
        assert!(check_readonly("(last (`a union `set))[`t; 1]", &readonly_opts()).is_err());
        assert!(check_readonly("1;\n  /\n  delete from trade", &readonly_opts()).is_err());
        assert!(check_readonly("{1;\n\\\ndelete from trade\n}[]", &readonly_opts()).is_err());
    }

    #[test]
    fn split_q_script_strips_inline_and_interleaved_comments() {
        let script = "select\n  sym, / symbol col\n/ price col\n  price\n  from trade";
        let stmts = split_q_script(script).unwrap();
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].text, "select\n  sym,\n  price\n  from trade");
    }
}
