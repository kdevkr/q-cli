//! Named-server config resolution.
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

pub fn resolve_conn(token: &str) -> Result<String, String> {
    let name = token.strip_prefix('@').unwrap_or(token);

    // a literal connection string always contains a colon
    if name.contains(':') {
        return Ok(name.to_string());
    }

    let key = if name.is_empty() { "default" } else { name };
    let path = config_path();
    let servers = load(&path).map_err(|e| {
        format!(
            "server '{}' needs config but {} ({}); use host:port instead",
            key,
            e,
            path.display_lossy()
        )
    })?;

    let mut val = servers
        .get(key)
        .cloned()
        .ok_or_else(|| format!("unknown server '{}' in {}", key, path.display_lossy()))?;

    // a `default` (or any entry) that names another server resolves once more
    if !val.contains(':') {
        val = servers
            .get(&val)
            .cloned()
            .ok_or_else(|| format!("server '{}' points at undefined '{}'", key, val))?;
    }
    Ok(val)
}

struct PathBufLike(std::path::PathBuf);
impl PathBufLike {
    fn display_lossy(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

fn config_path() -> PathBufLike {
    if let Ok(p) = std::env::var("Q_CLI_CONFIG") {
        return PathBufLike(std::path::PathBuf::from(p));
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let mut p = std::path::PathBuf::from(home);
    p.push(".config");
    p.push("q-cli");
    p.push("servers.conf");
    PathBufLike(p)
}

fn load(path: &PathBufLike) -> Result<HashMap<String, String>, String> {
    let text = std::fs::read_to_string(&path.0).map_err(|_| "is missing".to_string())?;
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if !k.is_empty() && !v.is_empty() {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(map)
}
