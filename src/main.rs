//! q-cli — a native kdb+/q IPC client.
//!
//! Speaks the kdb+ IPC wire protocol directly over TCP (handshake -> sync
//! message -> deserialize the returned K object). No q.exe, no license, no
//! wrapper scripts.
//!
//! Usage:
//!   q-cli [OUT] query  <conn> "<q expression>"
//!   q-cli [OUT] run    <conn> <path-to.q>
//!   q-cli [OUT] tables <conn>
//!   q-cli [OUT] meta   <conn> <table>
//!   q-cli [OUT] count  <conn> <table>
//!   q-cli       ping   <conn>
//!
//!   <conn> = host:port[:user:pass]  |  @name / name (from config)  |  @ (default)
//!   OUT    = --json | --csv | --console   (default: aligned text)

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

mod config;
mod k;
mod render;

use k::{Reader, K};

#[derive(Clone, Copy, PartialEq)]
enum OutMode {
    Text,
    Json,
    Csv,
    Console,
}

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    let mut out = OutMode::Text;
    let mut pos: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--json" | "-j" => out = OutMode::Json,
            "--csv" => out = OutMode::Csv,
            "--console" => out = OutMode::Console,
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            _ => pos.push(a),
        }
    }
    if pos.len() < 2 {
        print_usage();
        return 2;
    }
    let mode = pos[0].as_str();

    let conn = match config::resolve_conn(&pos[1]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERR {}", e);
            return 2;
        }
    };
    let arg = pos.get(2).cloned().unwrap_or_default();

    let result = match mode {
        "ping" => do_ping(&conn),
        "query" => do_eval(&conn, &arg, out),
        "run" => match std::fs::read_to_string(&arg) {
            Ok(src) => do_eval(&conn, &src, out),
            Err(e) => Err(format!("cannot read file '{}': {}", arg, e)),
        },
        "tables" => do_eval(&conn, "tables[]", out),
        "meta" => need_table(&arg).and_then(|t| do_eval(&conn, &format!("meta {}", t), out)),
        "count" => need_table(&arg).and_then(|t| do_eval(&conn, &format!("count {}", t), out)),
        other => Err(format!("unknown mode '{}' (use query|run|ping|tables|meta|count)", other)),
    };

    match result {
        Ok(out) => {
            println!("{}", out);
            0
        }
        Err(e) => {
            eprintln!("ERR {}", e);
            1
        }
    }
}

fn need_table(t: &str) -> Result<&str, String> {
    if t.is_empty() {
        Err("this command needs a table name".to_string())
    } else {
        Ok(t)
    }
}

fn print_usage() {
    eprintln!(
        "q-cli — native kdb+ IPC client\n\
         \n\
         USAGE:\n\
         \x20 q-cli [OUT] query  <conn> \"<q expression>\"\n\
         \x20 q-cli [OUT] run    <conn> <path.q>\n\
         \x20 q-cli [OUT] tables <conn>\n\
         \x20 q-cli [OUT] meta   <conn> <table>\n\
         \x20 q-cli [OUT] count  <conn> <table>\n\
         \x20 q-cli       ping   <conn>\n\
         \n\
         CONN:\n\
         \x20 host:port[:user:pass]   literal connection\n\
         \x20 @name | name            named server from config\n\
         \x20 @ | default             the configured default server\n\
         \x20 config: $Q_CLI_CONFIG or ~/.config/q-cli/servers.conf\n\
         \n\
         OUT (default: aligned text):\n\
         \x20 --json, -j   JSON (tables -> array of row objects)\n\
         \x20 --csv        CSV (tables only; uncapped)\n\
         \x20 --console    let the q server format via .Q.s (max fidelity)\n"
    );
}

fn do_ping(conn: &str) -> Result<String, String> {
    let mut c = Conn::open(conn)?;
    let _ = c.sync("1+1")?;
    Ok("pong".to_string())
}

fn do_eval(conn: &str, expr: &str, out: OutMode) -> Result<String, String> {
    let mut c = Conn::open(conn)?;
    // --console: ask q itself to render the result string (.Q.s), so even
    // exotic/nested types come back exactly as the q console shows them.
    let to_send = if out == OutMode::Console {
        format!(".Q.s value \"{}\"", q_escape(expr))
    } else {
        expr.to_string()
    };
    let k = c.sync(&to_send)?;
    Ok(match out {
        OutMode::Json => render::to_json(&k),
        OutMode::Csv => render::to_csv(&k),
        OutMode::Text | OutMode::Console => render::to_text(&k),
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
    fn open(conn: &str) -> Result<Conn, String> {
        let parts: Vec<&str> = conn.split(':').collect();
        if parts.len() < 2 {
            return Err(format!("bad connection '{}', expected host:port", conn));
        }
        let host = parts[0];
        let port: u16 = parts[1]
            .parse()
            .map_err(|_| format!("bad port '{}'", parts[1]))?;
        let creds = if parts.len() >= 4 {
            format!("{}:{}", parts[2], parts[3])
        } else if parts.len() == 3 {
            parts[2].to_string()
        } else {
            String::new()
        };

        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {}:{} failed: {}", host, port, e))?
            .collect();
        if addrs.is_empty() {
            return Err(format!("no address for {}:{}", host, port));
        }
        // try every resolved address (e.g. localhost -> ::1 and 127.0.0.1);
        // a q server bound to IPv4 0.0.0.0 won't answer the IPv6 address.
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
        let stream =
            stream.ok_or_else(|| format!("connect {}:{} failed: {}", host, port, last))?;
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

        let mut c = Conn { stream };
        c.handshake(&creds)?;
        Ok(c)
    }

    /// kdb+ login: send credentials + capability byte + null, read 1-byte reply.
    fn handshake(&mut self, creds: &str) -> Result<(), String> {
        let mut msg = creds.as_bytes().to_vec();
        msg.push(3); // capability: support protocol up to v3
        msg.push(0);
        self.stream
            .write_all(&msg)
            .map_err(|e| format!("handshake write failed: {}", e))?;
        let mut resp = [0u8; 1];
        self.stream
            .read_exact(&mut resp)
            .map_err(|_| "authentication failed (server closed connection)".to_string())?;
        Ok(())
    }

    /// Send a q expression as a sync request and return the deserialized result.
    fn sync(&mut self, expr: &str) -> Result<K, String> {
        let qb = expr.as_bytes();
        let mut body = Vec::with_capacity(6 + qb.len());
        body.push(10); // char vector
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
            .map_err(|e| format!("send failed: {}", e))?;

        let mut hdr = [0u8; 8];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| format!("no response header: {}", e))?;
        let le = hdr[0] == 1;
        let compressed = hdr[2] == 1;
        let len = if le {
            u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])
        } else {
            u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])
        } as usize;
        if len < 8 {
            return Err(format!("invalid response length {}", len));
        }
        let mut payload = vec![0u8; len - 8];
        self.stream
            .read_exact(&mut payload)
            .map_err(|e| format!("truncated response: {}", e))?;

        if compressed {
            payload = k::decompress(&payload, le)?;
        }

        let mut r = Reader::new(&payload, le);
        r.read()
    }
}
