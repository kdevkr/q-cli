//! kdb+ IPC over TCP: connect, login handshake, sync request/response.
//!
//! This module knows nothing about the CLI — it speaks the wire protocol and
//! hands back a deserialized `K` object (or a structured `CliError`).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error;
use crate::error::CliError;
use crate::k;
use crate::k::{Endian, Reader, K};

#[derive(Debug)]
enum Stream {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Tls(Box<native_tls::TlsStream<TcpStream>>),
}

impl Stream {
    fn set_read_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.set_read_timeout(dur),
            #[cfg(feature = "tls")]
            Self::Tls(s) => s.get_ref().set_read_timeout(dur),
        }
    }

    fn set_write_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.set_write_timeout(dur),
            #[cfg(feature = "tls")]
            Self::Tls(s) => s.get_ref().set_write_timeout(dur),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            #[cfg(feature = "tls")]
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            #[cfg(feature = "tls")]
            Self::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            #[cfg(feature = "tls")]
            Self::Tls(s) => s.flush(),
        }
    }
}

/// A live IPC connection to a q process.
#[derive(Debug)]
pub struct Conn {
    stream: Stream,
}

impl Conn {
    /// Parse `[tcps://|tls://|tcp://]host:port[:user:pass]`, connect, and perform handshake.
    /// `timeout` bounds query round-trip (socket read/write) and caps connect at 5s.
    /// `tls` explicitly enables TLS encryption; `tcps://` or `tls://` scheme also enables it.
    pub fn open(
        conn: &str,
        timeout: Option<Duration>,
        tls: bool,
        tls_no_verify: bool,
    ) -> Result<Self, CliError> {
        let use_tls = resolve_tls(conn, tls);
        #[cfg(not(feature = "tls"))]
        {
            let _ = tls_no_verify;
            if use_tls {
                return Err(CliError::usage(
                    "TLS connection requested but q-cli was compiled without 'tls' feature \
                     (rebuild with --features tls)",
                ));
            }
        }

        let (host, port, creds) = parse_conn(conn)?;

        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| CliError::connect(&format!("resolve {host}:{port} failed: {e}")))?
            .collect();
        if addrs.is_empty() {
            return Err(CliError::connect(&format!("no address for {host}:{port}")));
        }

        // Establishment (connect + handshake) failures are always reported as
        // `connect` (exit 3), never `timeout` — an unreachable host is a
        // connection problem, not a slow query. Connect is capped at 5s when a
        // timeout is set, and blocks (OS default) when `--timeout 0` = no timeout.
        // Try every resolved address (localhost -> ::1 and 127.0.0.1).
        let mut stream = None;
        let mut last: Option<std::io::Error> = None;
        for addr in &addrs {
            let res = match timeout {
                Some(t) => TcpStream::connect_timeout(addr, t.min(Duration::from_secs(5))),
                None => TcpStream::connect(addr),
            };
            match res {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let Some(stream) = stream else {
            let prefix = format!("connect {host}:{port} failed");
            return Err(match last {
                Some(e) => CliError::connect(&format!("{prefix}: {e}")),
                None => CliError::connect(&prefix),
            });
        };
        // Establishment (connect + handshake) timeout: cap at 5 seconds so a dead or
        // stalled port fails fast (exit 3) instead of waiting for long query timeouts.
        let handshake_timeout = match timeout {
            Some(t) => t.min(Duration::from_secs(5)),
            None => Duration::from_secs(5),
        };
        stream.set_read_timeout(Some(handshake_timeout)).ok();
        stream.set_write_timeout(Some(handshake_timeout)).ok();
        stream.set_nodelay(true).ok();

        #[cfg(feature = "tls")]
        let stream = if use_tls {
            let mut builder = native_tls::TlsConnector::builder();
            if tls_no_verify {
                eprintln!("warning: TLS certificate verification is disabled");
                builder.danger_accept_invalid_certs(true);
                builder.danger_accept_invalid_hostnames(true);
            }
            let connector = builder
                .build()
                .map_err(|e| CliError::connect(&format!("TLS initialization failed: {e}")))?;
            let tls_stream = connector
                .connect(host, stream)
                .map_err(|e| {
                    CliError::connect(&format!("TLS handshake with {host}:{port} failed: {e}"))
                })?;
            Stream::Tls(Box::new(tls_stream))
        } else {
            Stream::Plain(stream)
        };

        #[cfg(not(feature = "tls"))]
        let stream = Stream::Plain(stream);

        let mut c = Self { stream };
        c.handshake(&creds)?;

        // Handshake succeeded: switch to the requested query round-trip timeout.
        // `None` = wait forever; a timeout here surfaces as `timeout` (exit 5) from `sync`.
        c.stream.set_read_timeout(timeout).ok();
        c.stream
            .set_write_timeout(timeout.map(|t| t.min(Duration::from_secs(10))))
            .ok();

        Ok(c)
    }

    /// kdb+ login: send credentials + capability byte + null, read 1-byte reply.
    fn handshake(&mut self, creds: &str) -> Result<(), CliError> {
        let mut msg = creds.as_bytes().to_vec();
        msg.push(3);
        msg.push(0);
        self.stream
            .write_all(&msg)
            .map_err(|e| CliError::connect(&format!("handshake write failed: {e}")))?;
        let mut resp = [0u8; 1];
        // Still part of establishment: any failure here (closed, reset, or a
        // stalled login that hit the read timeout) is a connection problem (3).
        self.stream
            .read_exact(&mut resp)
            .map_err(|_| CliError::connect("authentication failed (no login reply from server)"))?;

        validate_handshake_capability(resp[0])
    }

    /// Send a q expression as a sync request and return the deserialized result.
    pub fn sync(&mut self, expr: &str) -> Result<K, CliError> {
        const MAX_QUERY_LEN: usize = (i32::MAX as usize) - 14;
        let qb = expr.as_bytes();
        if qb.len() > MAX_QUERY_LEN {
            return Err(CliError::usage("query length exceeds max 2GB IPC limit"));
        }
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
        // Query round-trip: a write/read timeout here is `timeout` (exit 5),
        // matching the response-read path below.
        self.stream
            .write_all(&msg)
            .map_err(|e| error::io_err("send failed", &e))?;

        let mut hdr = [0u8; 8];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| error::io_err("no response header", &e))?;
        validate_ipc_header(&hdr)?;
        let endian = if hdr[0] == 1 {
            Endian::Little
        } else {
            Endian::Big
        };
        let compressed = hdr[2] == 1;
        let len_bytes = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let len = match endian {
            Endian::Little => u32::from_le_bytes(len_bytes),
            Endian::Big => u32::from_be_bytes(len_bytes),
        } as usize;
        if len < 8 {
            return Err(CliError::connect(&format!("invalid response length {len}")));
        }
        const MAX_MSG_LEN: usize = 1024 * 1024 * 1024; // 1 GB cap prevents OOM on non-kdb ports
        if len > MAX_MSG_LEN {
            return Err(CliError::connect(&format!(
                "response length {len} exceeds 1GB limit (rogue port or invalid message)"
            )));
        }
        let mut payload = vec![0u8; len - 8];
        self.stream
            .read_exact(&mut payload)
            .map_err(|e| error::io_err("truncated response", &e))?;

        if compressed {
            payload = k::decompress(&payload, endian).map_err(|e| CliError::query(&e))?;
        }

        let mut reader = Reader::new(&payload, endian);
        reader.read().map_err(|e| CliError::query(&e))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Tcp,
    Tcps,
}

fn strip_scheme(conn: &str) -> (&str, Option<Scheme>, &str) {
    if conn.len() >= 7 && conn[..7].eq_ignore_ascii_case("tcps://") {
        (&conn[7..], Some(Scheme::Tcps), &conn[..7])
    } else if conn.len() >= 6 && conn[..6].eq_ignore_ascii_case("tls://") {
        (&conn[6..], Some(Scheme::Tcps), &conn[..6])
    } else if conn.len() >= 6 && conn[..6].eq_ignore_ascii_case("tcp://") {
        (&conn[6..], Some(Scheme::Tcp), &conn[..6])
    } else {
        (conn, None, "")
    }
}

/// Returns whether a connection string explicitly specifies a TLS scheme (`tcps://` or `tls://`).
#[cfg(test)]
#[must_use]
pub fn has_tls_scheme(conn: &str) -> bool {
    matches!(strip_scheme(conn).1, Some(Scheme::Tcps))
}

/// Determine whether TLS should be used: explicit URL scheme takes precedence over `--tls` flag.
#[must_use]
pub fn resolve_tls(conn: &str, opts_tls: bool) -> bool {
    match strip_scheme(conn).1 {
        Some(Scheme::Tcps) => true,
        Some(Scheme::Tcp) => false,
        None => opts_tls,
    }
}

/// Mask sensitive credentials from a connection string before reporting in error messages.
fn mask_err_conn(conn: &str) -> String {
    let (conn_no_scheme, _, prefix) = strip_scheme(conn);
    let (target, rest) = if let Some(stripped) = conn_no_scheme.strip_prefix('[') {
        if let Some((bracketed, after)) = stripped.split_once(']') {
            if let Some(after_col) = after.strip_prefix(':') {
                (format!("[{bracketed}]"), after_col)
            } else {
                return format!("{prefix}<invalid connection>");
            }
        } else {
            return format!("{prefix}<invalid connection>");
        }
    } else if let Some((h, after)) = conn_no_scheme.split_once(':') {
        (h.to_string(), after)
    } else {
        return conn.to_string();
    };

    if let Some((_port, _creds)) = rest.split_once(':') {
        format!("{prefix}{target}:<masked>")
    } else {
        conn.to_string()
    }
}

/// Parse `host:port[:user:pass]` or `[ipv6]:port[:user:pass]`.
pub fn parse_conn(conn: &str) -> Result<(&str, u16, String), CliError> {
    if conn.chars().any(char::is_control) {
        return Err(CliError::usage(
            "bad connection: contains control characters",
        ));
    }
    let (conn_no_scheme, _, _) = strip_scheme(conn);
    let masked = mask_err_conn(conn);
    let (host, rest) = if let Some(bracketed) = conn_no_scheme.strip_prefix('[') {
        let (h, after) = bracketed.split_once(']').ok_or_else(|| {
            CliError::usage(&format!("bad connection '{masked}', missing closing ']'"))
        })?;
        if h.is_empty() {
            return Err(CliError::usage(&format!(
                "bad connection '{masked}', empty bracketed host"
            )));
        }
        let after = after.strip_prefix(':').ok_or_else(|| {
            CliError::usage(&format!("bad connection '{masked}', expected port after ']'"))
        })?;
        (h, after)
    } else {
        let (h, after) = conn_no_scheme.split_once(':').ok_or_else(|| {
            CliError::usage(&format!("bad connection '{masked}', expected host:port"))
        })?;
        let host = if h.is_empty() { "localhost" } else { h };
        (host, after)
    };

    let (port_str, creds) = rest.split_once(':').unwrap_or((rest, ""));
    let port: u16 = port_str
        .parse()
        .ok()
        .filter(|&p| p > 0)
        .ok_or_else(|| CliError::usage(&format!("bad port '{port_str}'")))?;
    Ok((host, port, creds.to_string()))
}

fn validate_ipc_header(hdr: &[u8; 8]) -> Result<(), CliError> {
    if hdr[0] > 1 {
        let b = hdr[0];
        return Err(CliError::connect(&format!(
            "invalid endian byte 0x{b:02x} in response header (expected 0x00 or 0x01)"
        )));
    }
    if hdr[1] != 2 {
        let b = hdr[1];
        return Err(CliError::connect(&format!(
            "unexpected message type 0x{b:02x} in response header (expected 0x02 sync response)"
        )));
    }
    if hdr[2] > 1 {
        let b = hdr[2];
        return Err(CliError::connect(&format!(
            "unsupported compression flag 0x{b:02x} in response header (expected 0x00 or 0x01)"
        )));
    }
    Ok(())
}

fn validate_handshake_capability(cap: u8) -> Result<(), CliError> {
    if cap > 6 {
        let hint = if cap.is_ascii_graphic() {
            format!(" '{}'", cap as char)
        } else {
            String::new()
        };
        return Err(CliError::connect(&format!(
            "invalid capability byte 0x{cap:02x}{hint} from server; \
             remote port is likely not a kdb+ process"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_header_validation() {
        // Valid little-endian sync response, uncompressed
        let valid_le = [1u8, 2, 0, 0, 8, 0, 0, 0];
        assert!(validate_ipc_header(&valid_le).is_ok());

        // Valid big-endian sync response, compressed
        let valid_be = [0u8, 2, 1, 0, 0, 0, 0, 8];
        assert!(validate_ipc_header(&valid_be).is_ok());

        // Invalid endian
        let invalid_endian = [2u8, 2, 0, 0, 8, 0, 0, 0];
        assert!(validate_ipc_header(&invalid_endian).is_err());

        // Async message (0) instead of sync response (2)
        let async_msg = [1u8, 0, 0, 0, 8, 0, 0, 0];
        assert!(validate_ipc_header(&async_msg).is_err());

        // Invalid compression flag (2)
        let invalid_comp = [1u8, 2, 2, 0, 8, 0, 0, 0];
        assert!(validate_ipc_header(&invalid_comp).is_err());
    }

    #[test]
    fn capability_byte_validation() {
        assert!(validate_handshake_capability(0).is_ok());
        assert!(validate_handshake_capability(3).is_ok());
        assert!(validate_handshake_capability(5).is_ok());
        assert!(validate_handshake_capability(6).is_ok());

        let err_http = validate_handshake_capability(b'H').unwrap_err();
        assert!(err_http.msg.contains("0x48 'H'"));
        assert!(err_http.msg.contains("remote port is likely not a kdb+ process"));

        let err_ssh = validate_handshake_capability(b'S').unwrap_err();
        assert!(err_ssh.msg.contains("0x53 'S'"));
    }

    #[test]
    fn parse_conn_ipv4_and_hostname() {
        let (host, port, creds) = parse_conn("localhost:5000").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5000);
        assert_eq!(creds, "");

        let (host, port, creds) = parse_conn(":5000").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5000);
        assert_eq!(creds, "");

        let (host, port, creds) = parse_conn("127.0.0.1:5001:user").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 5001);
        assert_eq!(creds, "user");

        let (host, port, creds) = parse_conn("127.0.0.1:5002:user:pass:extra").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 5002);
        assert_eq!(creds, "user:pass:extra");
    }

    #[test]
    fn parse_conn_ipv6() {
        let (host, port, creds) = parse_conn("[::1]:5000").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5000);
        assert_eq!(creds, "");

        let (host, port, creds) = parse_conn("[2001:db8::1]:5001:admin:secret").unwrap();
        assert_eq!(host, "2001:db8::1");
        assert_eq!(port, 5001);
        assert_eq!(creds, "admin:secret");

        assert!(parse_conn("[::1]").is_err());
        assert!(parse_conn("[::1:5000").is_err());
        assert!(parse_conn("[]:5000").is_err());
        assert!(parse_conn("localhost:0").is_err());

        // Verify credentials are never leaked in parse error messages
        let err1 = parse_conn("[::1]5000:alice:secret").unwrap_err();
        assert!(!err1.msg.contains("secret"));
        let err2 = parse_conn("[::1:5000:alice:secret").unwrap_err();
        assert!(!err2.msg.contains("secret"));
        let err3 = parse_conn("localhost:badport:alice:secret").unwrap_err();
        assert!(!err3.msg.contains("secret"));
        assert!(parse_conn("[::1]:0").is_err());
        assert!(parse_conn("localhost:5000:user:pass\ninjected=evil:5000").is_err());
        assert!(parse_conn("localhost:5000:user\0pass").is_err());
    }

    #[test]
    fn parse_conn_with_schemes() {
        let (host, port, creds) = parse_conn("tcps://localhost:5000").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5000);
        assert_eq!(creds, "");
        assert!(has_tls_scheme("tcps://localhost:5000"));

        let (host, port, creds) = parse_conn("tls://[::1]:5001:alice:secret").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5001);
        assert_eq!(creds, "alice:secret");
        assert!(has_tls_scheme("tls://[::1]:5001:alice:secret"));

        let (host, port, creds) = parse_conn("tcp://127.0.0.1:5002").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 5002);
        assert_eq!(creds, "");
        assert!(!has_tls_scheme("tcp://127.0.0.1:5002"));
        assert!(!has_tls_scheme("localhost:5000"));

        // Case-insensitivity check
        let (host, port, creds) = parse_conn("TCPS://localhost:5000").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5000);
        assert_eq!(creds, "");
        assert!(has_tls_scheme("TCPS://localhost:5000"));

        let (host, port, _) = parse_conn("TCP://localhost:5003").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5003);

        // Scheme precedence in resolve_tls
        assert!(!resolve_tls("tcp://localhost:5000", true));
        assert!(!resolve_tls("TCP://localhost:5000", true));
        assert!(resolve_tls("tcps://localhost:5000", false));
        assert!(resolve_tls("TCPS://localhost:5000", false));
        assert!(resolve_tls("tls://localhost:5000", false));
        assert!(resolve_tls("TLS://localhost:5000", false));
        assert!(resolve_tls("localhost:5000", true));
        assert!(!resolve_tls("localhost:5000", false));

        assert_eq!(
            mask_err_conn("tcps://localhost:5000:alice:secret"),
            "tcps://localhost:<masked>"
        );
        assert_eq!(
            mask_err_conn("TCPS://localhost:5000:alice:secret"),
            "TCPS://localhost:<masked>"
        );
        assert_eq!(
            mask_err_conn("tls://[::1]:5001:user:pass"),
            "tls://[::1]:<masked>"
        );
    }

    #[test]
    #[cfg(not(feature = "tls"))]
    fn open_tls_without_feature_fails_with_usage() {
        let err = Conn::open("tcps://localhost:5000", None, false, false).unwrap_err();
        assert_eq!(err.kind, error::Kind::Usage);
        assert!(err.msg.contains("rebuild with --features tls"));
    }
}
