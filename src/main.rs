//! q-cli — a native kdb+/q IPC client.
//!
//! Speaks the kdb+ IPC wire protocol directly over TCP (handshake -> sync
//! message -> deserialize the returned K object). No q.exe, no license.
//!
//! Layout:
//!   error.rs  — `Kind`/`E`/exit codes + timeout classification
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

use cmd::{Opts, OutMode};

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    let mut out = OutMode::Text;
    let mut max_rows: usize = 50;
    let mut readonly = std::env::var("Q_CLI_READONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
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
            "--json" | "-j" => out = OutMode::Json,
            "--csv" => out = OutMode::Csv,
            "--console" => out = OutMode::Console,
            "--readonly" => readonly = true,
            "--max-rows" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) => max_rows = n,
                    None => {
                        eprintln!("ERR --max-rows requires a numeric value");
                        return 2;
                    }
                }
            }
            "--timeout" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<u64>().ok()) {
                    Some(n) => timeout_ms = n,
                    None => {
                        eprintln!("ERR --timeout requires a numeric value (ms; 0 = no timeout)");
                        return 2;
                    }
                }
            }
            "-h" | "--help" => {
                cmd::print_usage();
                return 0;
            }
            s if s.starts_with("--max-rows=") => match s[11..].parse::<usize>() {
                Ok(n) => max_rows = n,
                Err(_) => {
                    eprintln!("ERR --max-rows requires a numeric value");
                    return 2;
                }
            },
            s if s.starts_with("--timeout=") => match s[10..].parse::<u64>() {
                Ok(n) => timeout_ms = n,
                Err(_) => {
                    eprintln!("ERR --timeout requires a numeric value (ms; 0 = no timeout)");
                    return 2;
                }
            },
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    let opts = Opts {
        out,
        max_rows,
        readonly,
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
