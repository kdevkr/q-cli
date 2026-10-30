//! Error model: a `Kind` -> exit code, a JSON-able message, and a helper that
//! classifies an I/O error as a timeout (exit 5) vs. a connection failure (3).

use std::fmt;

/// The common result of a command: a rendered string, or a structured error.
pub type CmdResult = Result<String, CliError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Usage,
    Connect,
    Query,
    Timeout,
}

impl Kind {
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Kind::Usage => 2,
            Kind::Connect => 3,
            Kind::Query => 4,
            Kind::Timeout => 5,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Kind::Usage => "usage",
            Kind::Connect => "connect",
            Kind::Query => "query",
            Kind::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub kind: Kind,
    pub msg: String,
}

impl CliError {
    #[must_use]
    pub fn usage(msg: &str) -> Self {
        Self {
            kind: Kind::Usage,
            msg: msg.to_string(),
        }
    }

    #[must_use]
    pub fn connect(msg: &str) -> Self {
        Self {
            kind: Kind::Connect,
            msg: msg.to_string(),
        }
    }

    #[must_use]
    pub fn query(msg: &str) -> Self {
        Self {
            kind: Kind::Query,
            msg: msg.to_string(),
        }
    }

    #[must_use]
    pub fn timeout(msg: &str) -> Self {
        Self {
            kind: Kind::Timeout,
            msg: msg.to_string(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for CliError {}

/// Classify an I/O error: a socket read/connect timeout becomes `Kind::Timeout`
/// (exit 5) so agents can tell "query never came back" apart from "refused"
/// (`Kind::Connect`, exit 3). Windows surfaces a read timeout as `TimedOut`,
/// most Unixes as `WouldBlock`; treat both as a timeout.
#[must_use]
pub fn io_err(prefix: &str, err: &std::io::Error) -> CliError {
    if is_timeout(err) {
        CliError::timeout(&format!(
            "{prefix}: timed out (raise --timeout to wait longer)"
        ))
    } else {
        CliError::connect(&format!("{prefix}: {err}"))
    }
}

/// True if an I/O error is a read/connect timeout (vs. a real connection error).
#[must_use]
fn is_timeout(err: &std::io::Error) -> bool {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    matches!(err.kind(), TimedOut | WouldBlock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn io_err_classifies_timeouts_vs_connect_errors() {
        let timed_out = Error::new(ErrorKind::TimedOut, "socket timed out");
        let err1 = io_err("read failed", &timed_out);
        assert_eq!(err1.kind, Kind::Timeout);
        assert_eq!(err1.kind.code(), 5);
        assert_eq!(err1.kind.name(), "timeout");

        let would_block = Error::new(ErrorKind::WouldBlock, "would block");
        let err2 = io_err("read failed", &would_block);
        assert_eq!(err2.kind, Kind::Timeout);

        let refused = Error::new(ErrorKind::ConnectionRefused, "refused");
        let err3 = io_err("connect failed", &refused);
        assert_eq!(err3.kind, Kind::Connect);
        assert_eq!(err3.kind.code(), 3);
        assert_eq!(err3.kind.name(), "connect");
    }

    #[test]
    fn cli_error_constructors_and_display() {
        let u = CliError::usage("bad flag");
        assert_eq!(u.kind, Kind::Usage);
        assert_eq!(u.kind.code(), 2);
        assert_eq!(u.to_string(), "bad flag");

        let q = CliError::query("q error 'type'");
        assert_eq!(q.kind, Kind::Query);
        assert_eq!(q.kind.code(), 4);
    }
}
