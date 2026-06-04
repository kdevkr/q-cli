//! q-cli — a native kdb+/q IPC client.
//!
//! Speaks the kdb+ IPC wire protocol directly over TCP (handshake -> sync
//! message -> deserialize the returned K object). No q.exe, no license.
//!
//! Exit codes: 0 ok · 2 usage/policy · 3 connection · 4 q error.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

mod config;
mod k;
mod render;

use k::{Reader, K};

// ----------------------------------------------------------------------------
// Errors with a kind -> exit code, and JSON-able output
// ----------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    Usage,
    Connect,
    Query,
}

impl Kind {
    fn code(self) -> i32 {
        match self {
            Kind::Usage => 2,
            Kind::Connect => 3,
            Kind::Query => 4,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Kind::Usage => "usage",
            Kind::Connect => "connect",
            Kind::Query => "query",
        }
    }
}

struct E {
    kind: Kind,
    msg: String,
}

impl E {
    fn usage<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Usage, msg: m.into() }
    }
    fn connect<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Connect, msg: m.into() }
    }
    fn query<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Query, msg: m.into() }
    }
}

type R = Result<String, E>;

#[derive(Clone, Copy, PartialEq)]
enum OutMode {
    Text,
    Json,
    Csv,
    Console,
}

struct Opts {
    out: OutMode,
    max_rows: usize, // 0 = unlimited
    readonly: bool,
}

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    let mut out = OutMode::Text;
    let mut max_rows: usize = 50;
    let mut readonly = std::env::var("Q_CLI_READONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

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
                max_rows = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(max_rows);
            }
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            s if s.starts_with("--max-rows=") => {
                max_rows = s[11..].parse().unwrap_or(max_rows);
            }
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    let opts = Opts { out, max_rows, readonly };

    if pos.is_empty() {
        print_usage();
        return 2;
    }
    let mode = pos[0].as_str();

    // commands that take no <conn>
    if mode == "config" {
        return finish(config::run(&pos[1..]).map_err(E::usage), &opts);
    }
    // commands shaped <action> <conn>
    if mode == "web" {
        return do_web(&pos[1..], &opts);
    }
    if mode == "trace" {
        return do_trace(&pos[1..], &opts);
    }

    if pos.len() < 2 {
        print_usage();
        return 2;
    }

    let conn = match config::resolve_conn(&pos[1]) {
        Ok(c) => c,
        Err(e) => return finish(Err(E::usage(e)), &opts),
    };
    let arg = pos.get(2).cloned().unwrap_or_default();

    let result: R = match mode {
        "ping" => do_ping(&conn),
        "query" => ro(&arg, &opts).and_then(|_| do_eval(&conn, &arg, &opts)),
        "run" => match std::fs::read_to_string(&arg) {
            Ok(src) => ro(&src, &opts).and_then(|_| do_eval(&conn, &src, &opts)),
            Err(e) => Err(E::usage(format!("cannot read file '{}': {}", arg, e))),
        },
        "tables" => do_eval(&conn, "tables[]", &opts),
        "meta" => need_table(&arg).and_then(|t| do_eval(&conn, &format!("meta {}", t), &opts)),
        "count" => need_table(&arg).and_then(|t| do_eval(&conn, &format!("count {}", t), &opts)),
        "describe" => need_table(&arg).and_then(|t| do_eval(&conn, &describe_q(t), &opts)),
        "info" => do_eval(&conn, INFO_Q, &opts),
        "gc" => do_eval(&conn, ".Q.gc[]", &opts),
        "time" => {
            if arg.is_empty() {
                Err(E::usage("usage: q-cli time <conn> <expr>"))
            } else {
                ro(&arg, &opts).and_then(|_| do_eval(&conn, &time_q(&arg), &opts))
            }
        }
        other => Err(E::usage(format!(
            "unknown mode '{}' (query|run|ping|tables|meta|count|describe|info|time|gc|web|trace|config)",
            other
        ))),
    };
    finish(result, &opts)
}

/// Print a result or a structured error, return the process exit code.
fn finish(result: R, opts: &Opts) -> i32 {
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

fn need_table(t: &str) -> Result<&str, E> {
    if t.is_empty() {
        Err(E::usage("this command needs a table name"))
    } else {
        Ok(t)
    }
}

/// Server health snapshot: version, pid, port, open handles, timer + memory.
const INFO_Q: &str = "(`version`pid`port`handles`timer!(.z.K;.z.i;first system\"p\";count .z.W;first system\"t\")),`used`heap`peak#.Q.w[]";

/// One-shot table profile: name, partitioned?, partition field, rows, columns
/// (meta), and a small sample. Sample is skipped for partitioned tables so we
/// never trigger a full multi-partition load. Locals stay inside the lambda.
fn describe_q(t: &str) -> String {
    format!(
        "{{[t] tt:value t; p:.Q.qp tt; `name`partitioned`partition`rows`columns`sample!(t;p;$[p~1b;.Q.pf;`];count tt;0!meta tt;$[p~1b;();3 sublist 0!tt])}}`{}",
        t
    )
}

/// Wrap an expression so the server times it and returns `ms` + result `count`.
fn time_q(expr: &str) -> String {
    format!(
        "t:.z.p;r:value\"{}\";`ms`count!(`float$(.z.p-t)%1000000;count r)",
        q_escape(expr)
    )
}

/// `q-cli web <off|get-ok|status> <conn>` — tune the q process' built-in HTTP
/// handlers (.z.ph GET / .z.pp POST) at runtime.
fn do_web(args: &[String], opts: &Opts) -> i32 {
    let expr = match args.first().map(|s| s.as_str()).unwrap_or("") {
        "off" => ".z.ph:{hclose .z.w};.z.pp:{hclose .z.w};\"web: off (all HTTP closed)\"",
        "get-ok" => {
            ".z.ph:{.h.hy[`txt;\"OK\"]};.z.pp:{hclose .z.w};\"web: get-ok (GET 200, POST closed)\""
        }
        "status" => "`ph`pp!(.z.ph;.z.pp)",
        _ => return finish(Err(E::usage("web action must be: off | get-ok | status")), opts),
    };
    run_on(args.get(1), expr, opts)
}

/// `q-cli trace <on|off|status> <conn>` — wrap .z.pg/.z.ps with .Q.trp so query
/// errors return a .Q.sbt backtrace (and print it to the server console).
fn do_trace(args: &[String], opts: &Opts) -> i32 {
    const H: &str = "{.Q.trp[value;x;{-2 .Q.sbt y;'x,\"\\n\",.Q.sbt y}]}";
    let on = format!(".z.pg:{H};.z.ps:{H};\"trace: on\"");
    let expr: String = match args.first().map(|s| s.as_str()).unwrap_or("") {
        "on" => on,
        "off" => ".z.pg:value;.z.ps:value;\"trace: off\"".to_string(),
        "status" => "`pg`ps!(@[{string value x};`.z.pg;\"(default)\"];@[{string value x};`.z.ps;\"(default)\"])".to_string(),
        _ => return finish(Err(E::usage("trace action must be: on | off | status")), opts),
    };
    run_on(args.get(1), &expr, opts)
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

fn print_usage() {
    eprintln!(
        "q-cli — native kdb+ IPC client\n\
         \n\
         USAGE:\n\
         \x20 q-cli [OUT] query    <conn> \"<q expression>\"\n\
         \x20 q-cli [OUT] run      <conn> <path.q>\n\
         \x20 q-cli [OUT] tables   <conn>\n\
         \x20 q-cli [OUT] meta     <conn> <table>\n\
         \x20 q-cli [OUT] count    <conn> <table>\n\
         \x20 q-cli [OUT] describe <conn> <table>   (partitioning+schema+rows+sample)\n\
         \x20 q-cli [OUT] info     <conn>            (version/pid/port/mem/handles)\n\
         \x20 q-cli [OUT] time     <conn> \"<q expr>\" (elapsed ms + result count)\n\
         \x20 q-cli       gc       <conn>            (.Q.gc[] -> bytes freed to OS)\n\
         \x20 q-cli       ping     <conn>\n\
         \x20 q-cli       web      <off|get-ok|status> <conn>\n\
         \x20 q-cli       trace    <on|off|status> <conn>\n\
         \x20 q-cli       config   <init|path|list|add>\n\
         \n\
         CONN: host:port[:user:pass] | @name | name | @ (default)\n\
         \x20 config: $Q_CLI_CONFIG or ~/.config/q-cli/servers.conf\n\
         \n\
         OUT (default: aligned text, 50-row cap):\n\
         \x20 --json, -j      JSON (tables -> array of row objects)\n\
         \x20 --csv           CSV (tables only; uncapped)\n\
         \x20 --console       let the q server format via .Q.s\n\
         \x20 --max-rows N    row cap for text/json (0 = unlimited)\n\
         \x20 --readonly      reject mutating q (or set Q_CLI_READONLY=1)\n\
         \n\
         EXIT: 0 ok · 2 usage/policy · 3 connection · 4 q error\n"
    );
}

fn do_ping(conn: &str) -> R {
    let mut c = Conn::open(conn)?;
    let _ = c.sync("1+1")?;
    Ok("pong".to_string())
}

fn do_eval(conn: &str, expr: &str, opts: &Opts) -> R {
    let mut c = Conn::open(conn)?;
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

/// Escape a q expression so it can be embedded inside a q string literal.
fn q_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o
}

/// A live IPC connection to a q process.
struct Conn {
    stream: TcpStream,
}

impl Conn {
    /// Parse `host:port[:user:pass]`, connect, and perform the login handshake.
    fn open(conn: &str) -> Result<Conn, E> {
        let parts: Vec<&str> = conn.split(':').collect();
        if parts.len() < 2 {
            return Err(E::usage(format!("bad connection '{}', expected host:port", conn)));
        }
        let host = parts[0];
        let port: u16 = parts[1]
            .parse()
            .map_err(|_| E::usage(format!("bad port '{}'", parts[1])))?;
        let creds = if parts.len() >= 4 {
            format!("{}:{}", parts[2], parts[3])
        } else if parts.len() == 3 {
            parts[2].to_string()
        } else {
            String::new()
        };

        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| E::connect(format!("resolve {}:{} failed: {}", host, port, e)))?
            .collect();
        if addrs.is_empty() {
            return Err(E::connect(format!("no address for {}:{}", host, port)));
        }
        // try every resolved address (localhost -> ::1 and 127.0.0.1).
        let mut stream = None;
        let mut last = String::new();
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, Duration::from_secs(5)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = e.to_string(),
            }
        }
        let stream = stream
            .ok_or_else(|| E::connect(format!("connect {}:{} failed: {}", host, port, last)))?;
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

        let mut c = Conn { stream };
        c.handshake(&creds)?;
        Ok(c)
    }

    /// kdb+ login: send credentials + capability byte + null, read 1-byte reply.
    fn handshake(&mut self, creds: &str) -> Result<(), E> {
        let mut msg = creds.as_bytes().to_vec();
        msg.push(3);
        msg.push(0);
        self.stream
            .write_all(&msg)
            .map_err(|e| E::connect(format!("handshake write failed: {}", e)))?;
        let mut resp = [0u8; 1];
        self.stream
            .read_exact(&mut resp)
            .map_err(|_| E::connect("authentication failed (server closed connection)"))?;
        Ok(())
    }

    /// Send a q expression as a sync request and return the deserialized result.
    fn sync(&mut self, expr: &str) -> Result<K, E> {
        let qb = expr.as_bytes();
        let mut body = Vec::with_capacity(6 + qb.len());
        body.push(10);
        body.push(0);
        body.extend_from_slice(&(qb.len() as u32).to_le_bytes());
        body.extend_from_slice(qb);

        let total = (8 + body.len()) as u32;
        let mut msg = Vec::with_capacity(total as usize);
        msg.push(1); // little-endian
        msg.push(1); // sync
        msg.push(0); // not compressed
        msg.push(0);
        msg.extend_from_slice(&total.to_le_bytes());
        msg.extend_from_slice(&body);
        self.stream
            .write_all(&msg)
            .map_err(|e| E::connect(format!("send failed: {}", e)))?;

        let mut hdr = [0u8; 8];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| E::connect(format!("no response header: {}", e)))?;
        let le = hdr[0] == 1;
        let compressed = hdr[2] == 1;
        let len = if le {
            u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])
        } else {
            u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])
        } as usize;
        if len < 8 {
            return Err(E::connect(format!("invalid response length {}", len)));
        }
        let mut payload = vec![0u8; len - 8];
        self.stream
            .read_exact(&mut payload)
            .map_err(|e| E::connect(format!("truncated response: {}", e)))?;

        if compressed {
            payload = k::decompress(&payload, le).map_err(E::query)?;
        }

        let mut r = Reader::new(&payload, le);
        r.read().map_err(E::query)
    }
}
