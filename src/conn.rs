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
        // The read/write timeout bounds the query round-trip; `None` = wait
        // forever. A timeout here surfaces as `timeout` (exit 5) from `sync`.
        stream.set_read_timeout(timeout).ok();
        stream
            .set_write_timeout(timeout.map(|t| t.min(Duration::from_secs(10))))
            .ok();

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
        // Still part of establishment: any failure here (closed, reset, or a
        // stalled login that hit the read timeout) is a connection problem (3).
        self.stream
            .read_exact(&mut resp)
            .map_err(|_| E::connect("authentication failed (no login reply from server)"))?;
        Ok(())
    }

    /// Send a q expression as a sync request and return the deserialized result.
    pub fn sync(&mut self, expr: &str) -> Result<K, E> {
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
        // Query round-trip: a write/read timeout here is `timeout` (exit 5),
        // matching the response-read path below.
        self.stream
            .write_all(&msg)
            .map_err(|e| io_err("send failed", &e))?;

        let mut hdr = [0u8; 8];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| io_err("no response header", &e))?;
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
            .map_err(|e| io_err("truncated response", &e))?;

        if compressed {
            payload = k::decompress(&payload, le).map_err(E::query)?;
        }

        let mut r = Reader::new(&payload, le);
        r.read().map_err(E::query)
    }
}
