//! Error model: a `Kind` -> exit code, a JSON-able message, and a helper that
//! classifies an I/O error as a timeout (exit 5) vs. a connection failure (3).

/// The common result of a command: a rendered string, or a structured error.
pub type R = Result<String, E>;

#[derive(Clone, Copy)]
pub enum Kind {
    Usage,
    Connect,
    Query,
    Timeout,
}

impl Kind {
    pub fn code(self) -> i32 {
        match self {
            Kind::Usage => 2,
            Kind::Connect => 3,
            Kind::Query => 4,
            Kind::Timeout => 5,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Usage => "usage",
            Kind::Connect => "connect",
            Kind::Query => "query",
            Kind::Timeout => "timeout",
        }
    }
}

pub struct E {
    pub kind: Kind,
    pub msg: String,
}

impl E {
    pub fn usage<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Usage, msg: m.into() }
    }
    pub fn connect<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Connect, msg: m.into() }
    }
    pub fn query<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Query, msg: m.into() }
    }
    pub fn timeout<S: Into<String>>(m: S) -> E {
        E { kind: Kind::Timeout, msg: m.into() }
    }
}

/// Classify an I/O error: a socket read/connect timeout becomes `Kind::Timeout`
/// (exit 5) so agents can tell "query never came back" apart from "refused"
/// (`Kind::Connect`, exit 3). Windows surfaces a read timeout as `TimedOut`,
/// most Unixes as `WouldBlock`; treat both as a timeout.
pub fn io_err(prefix: &str, e: &std::io::Error) -> E {
    if is_timeout(e) {
        E::timeout(format!("{}: timed out (raise --timeout to wait longer)", prefix))
    } else {
        E::connect(format!("{}: {}", prefix, e))
    }
}

/// True if an I/O error is a read/connect timeout (vs. a real connection error).
pub fn is_timeout(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    matches!(e.kind(), TimedOut | WouldBlock)
}
