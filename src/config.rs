//! Named-server config: resolution + the `q-cli config` subcommands.
//!
//! Config file (`$Q_CLI_CONFIG`, else `~/.config/q-cli/servers.conf`) is a
//! simple `name = value` list; `#` starts a comment. Example:
//!
//! ```text
//! default = local
//! local   = localhost:5555
//! prod    = bigbox:5001:user:pass
//! tp      = localhost:5010
//! ```
//!
//! A connection token resolves as:
//!   - contains `:`            -> used literally (host:port[:user:pass])
//!   - `@name` / `name`        -> looked up in the config
//!   - `@` / `default` / empty -> the `default` entry (which may point at a name)

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Resolved path of the config file.
pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("Q_CLI_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("q-cli");
    p.push("servers.conf");
    p
}

fn load() -> Result<HashMap<String, String>, String> {
    let p = path();
    let text = fs::read_to_string(&p)
        .map_err(|_| format!("no config at {} (run `q-cli config init`)", p.display()))?;
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(map)
}

pub fn resolve_conn(token: &str) -> Result<String, String> {
    let name = token.strip_prefix('@').unwrap_or(token);

    // a literal connection string always contains a colon
    if name.contains(':') {
        return Ok(name.to_string());
    }

    let key = if name.is_empty() { "default" } else { name };
    let servers = load()?;

    let mut val = servers
        .get(key)
        .cloned()
        .ok_or_else(|| format!("unknown server '{}' in {}", key, path().display()))?;

    // a `default` (or any entry) that names another server resolves once more
    if !val.contains(':') {
        val = servers
            .get(&val)
            .cloned()
            .ok_or_else(|| format!("server '{}' points at undefined '{}'", key, val))?;
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// `q-cli config <action>`
// ---------------------------------------------------------------------------

const TEMPLATE: &str = "# q-cli server profiles\n\
# entries:  <name> = host:port[:user:pass]\n\
# `default` may name another entry, or be a literal host:port.\n\
\n\
default = local\n\
local   = localhost:5000\n";

pub fn run(args: &[String]) -> Result<String, String> {
    let action = args.first().map(|s| s.as_str()).unwrap_or("path");
    match action {
        "init" => init(),
        "path" => Ok(path().display().to_string()),
        "list" => list(),
        "add" => {
            let name = args
                .get(1)
                .ok_or("usage: q-cli config add <name> <host:port[:user:pass]>")?;
            let conn = args
                .get(2)
                .ok_or("usage: q-cli config add <name> <host:port[:user:pass]>")?;
            add(name, conn)
        }
        other => Err(format!(
            "unknown config action '{}' (use init | path | list | add)",
            other
        )),
    }
}

fn init() -> Result<String, String> {
    let p = path();
    if p.exists() {
        return Ok(format!(
            "config already exists: {}\n(edit it, or `q-cli config add <name> <conn>`)",
            p.display()
        ));
    }
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    }
    fs::write(&p, TEMPLATE).map_err(|e| format!("cannot write {}: {}", p.display(), e))?;
    Ok(format!(
        "created {}\nedit it so `default` -> `local` points at your q process,\nthen: q-cli tables @",
        p.display()
    ))
}

fn list() -> Result<String, String> {
    let map = load()?;
    if map.is_empty() {
        return Ok(format!("(no servers defined in {})", path().display()));
    }
    let mut lines: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{} = {}", k, mask(v)))
        .collect();
    lines.sort();
    Ok(lines.join("\n"))
}

/// Add or update one entry, preserving existing lines and comments.
fn add(name: &str, conn: &str) -> Result<String, String> {
    let p = path();
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    }
    let mut lines: Vec<String> = if p.exists() {
        fs::read_to_string(&p)
            .map_err(|e| format!("cannot read {}: {}", p.display(), e))?
            .lines()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec!["# q-cli server profiles".to_string()]
    };

    let mut replaced = false;
    for line in lines.iter_mut() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((k, _)) = t.split_once('=') {
            if k.trim() == name {
                *line = format!("{} = {}", name, conn);
                replaced = true;
                break;
            }
        }
    }
    if !replaced {
        lines.push(format!("{} = {}", name, conn));
    }

    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(&p, body).map_err(|e| format!("cannot write {}: {}", p.display(), e))?;
    Ok(format!(
        "{} {} = {}  ({})",
        if replaced { "updated" } else { "added" },
        name,
        mask(conn),
        p.display()
    ))
}

/// Hide the password component of a host:port:user:pass connection string.
fn mask(conn: &str) -> String {
    let parts: Vec<&str> = conn.split(':').collect();
    if parts.len() >= 4 {
        format!("{}:{}:{}:****", parts[0], parts[1], parts[2])
    } else {
        conn.to_string()
    }
}
