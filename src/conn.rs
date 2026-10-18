//! kdb+ IPC over TCP: connect, login handshake, sync request/response.
//!
//! This module knows nothing about the CLI — it speaks the wire protocol and
//! hands back a deserialized `K` object (or a structured `E`).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::{io_err, E};
use crate::k::{self, Reader, K};

/// A live IPC connection to a q process.
pub struct Conn {
    stream: TcpStream,
}

impl Conn {
    /// Parse `host:port[:user:pass]`, connect, and perform the login handshake.
    /// `timeout` bounds the query round-trip (socket read/write) and caps connect
    /// at 5s; `None` means no timeout (block until the server responds).
    pub fn open(conn: &str, timeout: Option<Duration>) -> Result<Conn, E> {
        let (host, port, creds) = parse_conn(conn)?;

        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| E::connect(format!("resolve {}:{} failed: {}", host, port, e)))?
            .collect();
        if addrs.is_empty() {
            return Err(E::connect(format!("no address for {}:{}", host, port)));
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
        let stream = match stream {
            Some(s) => s,
            None => {
                let prefix = format!("connect {}:{} failed", host, port);
                return Err(match last {
                    Some(e) => E::connect(format!("{}: {}", prefix, e)),
                    None => E::connect(prefix),
                });
            }
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
        let mut c = Conn { stream };
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
    fn handshake(&mut self, creds: &str) -> Result<(), E> {
        let mut msg = creds.as_bytes().to_vec();
        msg.push(3);
        msg.push(0);
        self.stream
            .write_all(&msg)
            .map_err(|e| E::connect(format!("handshake write failed: {}", e)))?;
        let mut resp = [0u8; 1];
        // Still part of establishment: any failure here (closed, reset, or a
        // stalled login that hit the read timeout) is a connection problem (3).
        self.stream
            .read_exact(&mut resp)
            .map_err(|_| E::connect("authentication failed (no login reply from server)"))?;

        validate_handshake_capability(resp[0])
    }

    /// Send a q expression as a sync request and return the deserialized result.
    pub fn sync(&mut self, expr: &str) -> Result<K, E> {
        let qb = expr.as_bytes();
        if qb.len() > i32::MAX as usize {
            return Err(E::usage("query length exceeds max 2GB IPC limit"));
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
            .map_err(|e| io_err("send failed", &e))?;

        let mut hdr = [0u8; 8];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| io_err("no response header", &e))?;
        validate_ipc_header(&hdr)?;
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
        const MAX_MSG_LEN: usize = 1024 * 1024 * 1024; // 1 GB cap prevents OOM on non-kdb ports
        if len > MAX_MSG_LEN {
            return Err(E::connect(format!(
                "response length {} exceeds 1GB limit (rogue port or invalid message)",
                len
            )));
        }
        let mut payload = vec![0u8; len - 8];
        self.stream
            .read_exact(&mut payload)
            .map_err(|e| io_err("truncated response", &e))?;

        if compressed {
            payload = k::decompress(&payload, le).map_err(E::query)?;
        }

        let mut r = Reader::new(&payload, le);
        r.read().map_err(E::query)
    }
}

/// Parse `host:port[:user:pass]` or `[ipv6]:port[:user:pass]`.
pub fn parse_conn(conn: &str) -> Result<(&str, u16, String), E> {
    if let Some(rest) = conn.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| E::usage(format!("bad connection '{}', missing closing ']'", conn)))?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let after = after.strip_prefix(':').ok_or_else(|| {
            E::usage(format!("bad connection '{}', expected port after ']'", conn))
        })?;
        let parts: Vec<&str> = after.split(':').collect();
        let port: u16 = parts[0]
            .parse()
            .map_err(|_| E::usage(format!("bad port '{}'", parts[0])))?;
        let creds = if parts.len() > 1 {
            parts[1..].join(":")
        } else {
            String::new()
        };
        Ok((host, port, creds))
    } else {
        let parts: Vec<&str> = conn.split(':').collect();
        if parts.len() < 2 {
            return Err(E::usage(format!(
                "bad connection '{}', expected host:port",
                conn
            )));
        }
        let host = if parts[0].is_empty() { "localhost" } else { parts[0] };
        let port: u16 = parts[1]
            .parse()
            .map_err(|_| E::usage(format!("bad port '{}'", parts[1])))?;
        let creds = if parts.len() > 2 {
            parts[2..].join(":")
        } else {
            String::new()
        };
        Ok((host, port, creds))
    }
}

pub fn validate_ipc_header(hdr: &[u8; 8]) -> Result<(), E> {
    if hdr[0] > 1 {
        return Err(E::connect(format!(
            "invalid endian byte 0x{:02x} in response header (expected 0x00 or 0x01)",
            hdr[0]
        )));
    }
    if hdr[1] != 2 {
        return Err(E::connect(format!(
            "unexpected message type 0x{:02x} in response header (expected 0x02 sync response)",
            hdr[1]
        )));
    }
    if hdr[2] > 1 {
        return Err(E::connect(format!(
            "unsupported compression flag 0x{:02x} in response header (expected 0x00 or 0x01)",
            hdr[2]
        )));
    }
    Ok(())
}

pub fn validate_handshake_capability(cap: u8) -> Result<(), E> {
    if cap > 6 {
        let hint = if cap.is_ascii_graphic() {
            format!(" '{}'", cap as char)
        } else {
            String::new()
        };
        return Err(E::connect(format!(
            "invalid capability byte 0x{:02x}{} from server; remote port is likely not a kdb+ process",
            cap, hint
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
    }
}
