//! q-cli — a native kdb+/q IPC client.
//!
//! Speaks the kdb+ IPC wire protocol directly over TCP (handshake -> sync
//! message -> deserialize the returned K object). No q.exe, no license.
//!
//! Layout:
//!   error.rs  — `Kind`/`CliError`/exit codes + timeout classification
//!   conn.rs   — the IPC protocol (`Conn`: open/handshake/sync)
//!   query.rs  — the canned q expressions the CLI sends + q-string escaping
//!   cmd.rs    — the `CMDS` command table, dispatch, output rendering, guards
//!   k.rs      — K type model, wire deserialization, decompress
//!   render.rs — text / JSON / CSV rendering
//!   config.rs — server-profile config (global + project layers)
//!
//! This file is just argument parsing -> `Opts` -> `cmd::dispatch`.
//!
//! Exit codes: 0 ok · 2 usage/policy · 3 connection · 4 q error · 5 timeout.

use std::time::Duration;

mod cmd;
mod config;
mod conn;
mod error;
mod k;
mod query;
mod render;
mod update;

use cmd::{Opts, OutMode};
use error::CliError;

fn main() {
    std::process::exit(real_main());
}

fn usage_err(msg: &str, out: OutMode) -> i32 {
    let opts = Opts {
        out,
        ..Opts::default()
    };
    cmd::finish(Err(CliError::usage(msg)), &opts)
}

fn real_main() -> i32 {
    let mut out: OutMode = std::env::var("Q_CLI_OUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_default();
    let mut max_rows: usize = std::env::var("Q_CLI_MAX_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let mut readonly = std::env::var("Q_CLI_READONLY")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut tls = std::env::var("Q_CLI_TLS")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut tls_no_verify = std::env::var("Q_CLI_TLS_NO_VERIFY")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut scoped = false;
    let mut atomic = false;
    let mut dry_run = false;
    let mut project = false;
    // default query wait: 30s, overridable per-call (--timeout) or via env.
    let mut timeout_ms: u64 = std::env::var("Q_CLI_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--" => {
                i += 1;
                while i < args.len() {
                    pos.push(args[i].clone());
                    i += 1;
                }
                break;
            }
            "--text" => out = OutMode::Text,
            "--json" | "-j" => out = OutMode::Json,
            "--csv" => out = OutMode::Csv,
            "--console" => out = OutMode::Console,
            "--readonly" => readonly = true,
            "--scoped" => scoped = true,
            "--atomic" => atomic = true,
            "--dry-run" => dry_run = true,
            "--tls" => tls = true,
            "--tls-no-verify" | "--insecure" => tls_no_verify = true,
            "--project" | "-p" => project = true,
            "--max-rows" => {
                i += 1;
                let parsed: Option<usize> = args.get(i).and_then(|v| v.parse().ok());
                match parsed {
                    Some(n) => max_rows = n,
                    None => return usage_err("--max-rows requires a numeric value", out),
                }
            }
            "--timeout" => {
                i += 1;
                let parsed: Option<u64> = args.get(i).and_then(|v| v.parse().ok());
                match parsed {
                    Some(n) => timeout_ms = n,
                    None => {
                        return usage_err(
                            "--timeout requires a numeric value (ms; 0 = no timeout)",
                            out,
                        );
                    }
                }
            }
            "-h" | "--help" => {
                cmd::print_help();
                return 0;
            }
            "-V" | "--version" => {
                println!("q-cli {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            s if s.starts_with("--max-rows=") => {
                let parsed: Result<usize, _> = s[11..].parse();
                match parsed {
                    Ok(n) => max_rows = n,
                    Err(_) => return usage_err("--max-rows requires a numeric value", out),
                }
            }
            s if s.starts_with("--timeout=") => {
                let parsed: Result<u64, _> = s[10..].parse();
                match parsed {
                    Ok(n) => timeout_ms = n,
                    Err(_) => {
                        return usage_err(
                            "--timeout requires a numeric value (ms; 0 = no timeout)",
                            out,
                        );
                    }
                }
            }
            s if s.starts_with('-') && s != "-" => {
                // Allow a leading `-` in `<expr>` position (`pos[2]`) for `query`/`time`
                // so negative literals or adverbs like `-1+2` or `-0W` work without `--`.
                let is_query_expr = pos.len() == 2 && matches!(pos[0].as_str(), "query" | "time");
                let is_update_flag = pos.first().map(String::as_str) == Some("update");
                if is_query_expr || is_update_flag {
                    pos.push(args[i].clone());
                } else {
                    return usage_err(&format!("unrecognized option '{s}'"), out);
                }
            }
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }

    if project {
        if pos.first().map(String::as_str) == Some("config") {
            if !pos.iter().any(|a| a == "--project" || a == "-p") {
                pos.push("--project".to_string());
            }
        } else {
            return usage_err("unrecognized option '--project'", out);
        }
    }
    let opts = Opts {
        out,
        max_rows,
        readonly,
        scoped,
        atomic,
        dry_run,
        tls,
        tls_no_verify,
        // 0 = no timeout (wait forever); otherwise a finite query-round-trip wait.
        timeout: if timeout_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(timeout_ms))
        },
    };

    if pos.is_empty() {
        cmd::print_usage();
        return 2;
    }
    cmd::dispatch(&pos, &opts)
}
