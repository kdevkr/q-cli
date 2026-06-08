//! Named-server config: resolution + the `q-cli config` subcommands.
//!
//! Two layers, merged with **project overriding global**:
//!   - global:  `$Q_CLI_CONFIG`, else `~/.config/q-cli/servers.conf`
//!   - project: the nearest `.q-cli.conf` found walking up from the CWD
//!
//! If `$Q_CLI_CONFIG` is set it is used **alone** (explicit override, no merge).
//!
//! File format — `name = value` lines, `#` comments. `value` is
//! `host:port[:user:pass]`; `default` may name another entry. Example:
//! ```text
//! default = local
//! local   = localhost:5555
//! prod    = bigbox:5001:user:pass
//! ```
//! Connection token resolution:
//!   - contains `:`            -> literal host:port[:user:pass]
//!   - `@name` / `name`        -> looked up in the merged config
//!   - `@` / `default` / empty -> the `default` entry

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const PROJECT_FILE: &str = ".q-cli.conf";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The global config path (`$Q_CLI_CONFIG` or `~/.config/q-cli/servers.conf`).
pub fn global_path() -> PathBuf {
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

/// Nearest existing `.q-cli.conf` walking up from the current directory.
fn project_path_read() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let cand = dir.join(PROJECT_FILE);
        if cand.is_file() {
            return Some(cand);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Where `config --project` writes: `./.q-cli.conf` in the current directory.
fn project_path_write() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(PROJECT_FILE)
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn parse(text: &str) -> HashMap<String, String> {
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
    map
}

fn load_file(p: &Path) -> Result<HashMap<String, String>, String> {
    let text = fs::read_to_string(p).map_err(|_| format!("cannot read {}", p.display()))?;
    Ok(parse(&text))
}

/// Effective config: global, then project overlaid on top. With `$Q_CLI_CONFIG`
/// set, that single file is used alone.
fn load_merged() -> Result<HashMap<String, String>, String> {
    if std::env::var("Q_CLI_CONFIG").is_ok() {
        return load_file(&global_path());
    }
    let mut map = HashMap::new();
    if let Ok(g) = load_file(&global_path()) {
        map.extend(g);
    }
    if let Some(pp) = project_path_read() {
        if let Ok(p) = load_file(&pp) {
            map.extend(p); // project overrides global
        }
    }
    if map.is_empty() {
        return Err(format!(
            "no servers configured (global {} or a project {}); run `q-cli config init`",
            global_path().display(),
            PROJECT_FILE
        ));
    }
    Ok(map)
}

pub fn resolve_conn(token: &str) -> Result<String, String> {
    let name = token.strip_prefix('@').unwrap_or(token);
    if name.contains(':') {
        return Ok(name.to_string());
    }
    let key = if name.is_empty() { "default" } else { name };
    let servers = load_merged()?;

    let mut val = servers
        .get(key)
        .cloned()
        .ok_or_else(|| format!("unknown server '{}' (not in global/project config)", key))?;
    if !val.contains(':') {
        val = servers
            .get(&val)
            .cloned()
            .ok_or_else(|| format!("server '{}' points at undefined '{}'", key, val))?;
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// `q-cli config <action> [--project]`
// ---------------------------------------------------------------------------

const TEMPLATE: &str = "# q-cli server profiles\n\
# entries:  <name> = host:port[:user:pass]\n\
# `default` may name another entry, or be a literal host:port.\n\
\n\
default = local\n\
local   = localhost:5000\n";

pub fn run(args: &[String]) -> Result<String, String> {
    let project = args.iter().any(|a| a == "--project" || a == "-p");
    let rest: Vec<&str> = args
        .iter()
        .map(|s| s.as_str())
        .filter(|a| *a != "--project" && *a != "-p")
        .collect();
    let action = rest.first().copied().unwrap_or("path");
    let target = if project {
        project_path_write()
    } else {
        global_path()
    };

    match action {
        "init" => init(&target),
        "path" => Ok(path_summary()),
        "list" => list(),
        "add" => {
            let name = rest
                .get(1)
                .ok_or("usage: q-cli config add [--project] <name> <host:port[:user:pass]>")?;
            let conn = rest
                .get(2)
                .ok_or("usage: q-cli config add [--project] <name> <host:port[:user:pass]>")?;
            add(&target, name, conn)
        }
        other => Err(format!(
            "unknown config action '{}' (init | path | list | add) [--project]",
            other
        )),
    }
}

fn init(target: &Path) -> Result<String, String> {
    if target.exists() {
        return Ok(format!(
            "config already exists: {}\n(edit it, or `q-cli config add [--project] <name> <conn>`)",
            target.display()
        ));
    }
    if let Some(dir) = target.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
        }
    }
    fs::write(target, TEMPLATE).map_err(|e| format!("cannot write {}: {}", target.display(), e))?;
    Ok(format!(
        "created {}\nedit it so `default` points at your q process, then: q-cli tables @",
        target.display()
    ))
}

fn path_summary() -> String {
    let g = global_path();
    let mut s = format!(
        "global:  {}   ({})",
        g.display(),
        if g.exists() { "exists" } else { "none" }
    );
    match project_path_read() {
        Some(p) => s.push_str(&format!("\nproject: {}   (active)", p.display())),
        None => s.push_str(&format!(
            "\nproject: {}   (none here)",
            project_path_write().display()
        )),
    }
    if let Ok(e) = std::env::var("Q_CLI_CONFIG") {
        s.push_str(&format!("\nenv Q_CLI_CONFIG: {}   (used alone)", e));
    }
    s
}

/// Effective servers, annotated with their source (project overrides global).
fn list() -> Result<String, String> {
    let mut rows: BTreeMap<String, (String, &'static str)> = BTreeMap::new();
    if std::env::var("Q_CLI_CONFIG").is_ok() {
        if let Ok(m) = load_file(&global_path()) {
            for (k, v) in m {
                rows.insert(k, (v, "env"));
            }
        }
    } else {
        if let Ok(g) = load_file(&global_path()) {
            for (k, v) in g {
                rows.insert(k, (v, "global"));
            }
        }
        if let Some(pp) = project_path_read() {
            if let Ok(p) = load_file(&pp) {
                for (k, v) in p {
                    rows.insert(k, (v, "project"));
                }
            }
        }
    }
    if rows.is_empty() {
        return Ok("(no servers configured)".to_string());
    }
    Ok(rows
        .iter()
        .map(|(k, (v, src))| format!("{} = {}   [{}]", k, mask(v), src))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Add or update one entry in `target`, preserving existing lines and comments.
fn add(target: &Path, name: &str, conn: &str) -> Result<String, String> {
    if let Some(dir) = target.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
        }
    }
    let mut lines: Vec<String> = if target.exists() {
        fs::read_to_string(target)
            .map_err(|e| format!("cannot read {}: {}", target.display(), e))?
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
    fs::write(target, body).map_err(|e| format!("cannot write {}: {}", target.display(), e))?;
    Ok(format!(
        "{} {} = {}  ({})",
        if replaced { "updated" } else { "added" },
        name,
        mask(conn),
        target.display()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_blanks() {
        let m = parse("# comment\n\ndefault = local\nlocal = host:5000  \n");
        assert_eq!(m.get("default").map(String::as_str), Some("local"));
        assert_eq!(m.get("local").map(String::as_str), Some("host:5000"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_ignores_malformed_lines() {
        let m = parse("nokey\n= noval\nk =\n");
        assert!(m.is_empty());
    }

    #[test]
    fn mask_hides_only_the_password() {
        assert_eq!(mask("host:5000:user:secret"), "host:5000:user:****");
        assert_eq!(mask("host:5000"), "host:5000"); // nothing to hide
    }
}
