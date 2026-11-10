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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::conn;

const PROJECT_FILE: &str = ".q-cli.conf";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The global config path (`$Q_CLI_CONFIG`, `$XDG_CONFIG_HOME/q-cli/servers.conf`,
/// or `~/.config/q-cli/servers.conf`).
#[must_use]
fn global_path() -> PathBuf {
    let q_cfg = std::env::var("Q_CLI_CONFIG").ok();
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    global_path_from(q_cfg.as_deref(), xdg.as_deref(), home.as_deref())
}

fn global_path_from(
    q_cli_config: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    if let Some(p) = q_cli_config {
        return PathBuf::from(p);
    }
    if let Some(xdg) = xdg_config_home {
        let mut p = PathBuf::from(xdg);
        p.push("q-cli");
        p.push("servers.conf");
        return p;
    }
    let mut p = PathBuf::from(home.unwrap_or_default());
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
        if let Some((k, mut v)) = line.split_once('=') {
            let k = k.trim();
            // Strip inline comments preceded by whitespace (` # comment` or `\t# comment`)
            if let Some((val_part, _)) = v.split_once(" #") {
                v = val_part;
            } else if let Some((val_part, _)) = v.split_once("\t#") {
                v = val_part;
            }
            let v = v.trim();
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
            let names = overridden(&map, &p);
            if !names.is_empty() {
                // Path and names come from an untrusted file: escape before printing.
                let msg = format!(
                    "warning: project {} overrides global server(s): {}",
                    pp.display(),
                    names.join(", ")
                );
                eprintln!("{}", crate::render::escape_control(&msg));
            }
            map.extend(p); // project overrides global
        }
    }
    if map.is_empty() {
        let g_disp = global_path().display().to_string();
        return Err(format!(
            "no servers configured (global {g_disp} or a project {PROJECT_FILE}); \
             run `q-cli config init`"
        ));
    }
    Ok(map)
}

/// Names a project file redefines to a different target than the global config.
/// A `.q-cli.conf` in a checked-out repo can silently repoint `prod`, so callers
/// surface these instead of trusting the overlay blindly.
fn overridden<'a>(
    global: &HashMap<String, String>,
    project: &'a HashMap<String, String>,
) -> Vec<&'a str> {
    let mut names: Vec<&str> = project
        .iter()
        .filter(|(k, v)| global.get(*k).is_some_and(|g| g != *v))
        .map(|(k, _)| k.as_str())
        .collect();
    names.sort_unstable();
    names
}

pub fn resolve_conn(token: &str) -> Result<String, String> {
    let name = token.strip_prefix('@').unwrap_or(token);
    if name.contains(':') {
        return Ok(name.to_string());
    }
    let key = if name.is_empty() { "default" } else { name };
    let servers = load_merged()?;
    resolve_alias(key, &servers)
}

fn resolve_alias(key: &str, servers: &HashMap<String, String>) -> Result<String, String> {
    let mut current = key.strip_prefix('@').unwrap_or(key);
    let mut visited: HashSet<&str> = HashSet::from([current]);

    let mut val = servers
        .get(current)
        .map(String::as_str)
        .ok_or_else(|| format!("unknown server '{current}' (not in global/project config)"))?;

    let mut hops = 0;
    while !val.contains(':') {
        hops += 1;
        let next_key = val.strip_prefix('@').unwrap_or(val);
        if hops > 16 || !visited.insert(next_key) {
            return Err(format!(
                "alias cycle or too many redirects starting at '{key}'"
            ));
        }
        current = next_key;
        val = servers
            .get(current)
            .map(String::as_str)
            .ok_or_else(|| format!("server '{current}' points at undefined '{val}'"))?;
    }
    Ok(val.to_string())
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
        .map(String::as_str)
        .filter(|a| *a != "--project" && *a != "-p")
        .collect();
    let action = rest.first().copied().unwrap_or("path");
    let target = if project {
        project_path_write()
    } else {
        global_path()
    };

    match action {
        "init" => {
            if let Some(extra) = rest.get(1) {
                return Err(format!("unexpected argument '{extra}' for 'config init'"));
            }
            init(&target)
        }
        "path" => {
            if let Some(extra) = rest.get(1) {
                return Err(format!("unexpected argument '{extra}' for 'config path'"));
            }
            Ok(path_summary())
        }
        "list" => {
            if let Some(extra) = rest.get(1) {
                return Err(format!("unexpected argument '{extra}' for 'config list'"));
            }
            Ok(list())
        }
        "add" => {
            let name = rest
                .get(1)
                .ok_or("usage: q-cli config add [--project] <name> <host:port[:user:pass]>")?;
            let conn_str = rest
                .get(2)
                .ok_or("usage: q-cli config add [--project] <name> <host:port[:user:pass]>")?;
            if let Some(extra) = rest.get(3) {
                return Err(format!("unexpected argument '{extra}' for 'config add'"));
            }
            add(&target, name, conn_str)
        }
        "-h" | "--help" | "help" => Ok(
            "Usage: q-cli config <init|path|list|add> [--project]\n\n\
             Subcommands:\n  \
               init                Initialize server configuration file\n  \
               path                Show path to active configuration file(s)\n  \
               list                List configured server profiles\n  \
               add <name> <conn>   Add or update a server profile\n\n\
             Options:\n  \
               -p, --project       Operate on project-local .q-cli.conf instead of global"
                .to_string(),
        ),
        other => Err(format!(
            "unknown config action '{other}' (init | path | list | add) [--project]"
        )),
    }
}

fn write_config_file(target: &Path, content: &str) -> Result<(), String> {
    if let Some(dir) = target.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
    }
    // Profiles can hold passwords: the file must be owner-only before any content
    // lands in it, whether it is created here or already existed with a looser mode.
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(target)
        .and_then(|mut f| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(content.as_bytes())
        })
        .map_err(|e| format!("cannot write {}: {e}", target.display()))
}

fn init(target: &Path) -> Result<String, String> {
    if target.exists() {
        return Ok(format!(
            "config already exists: {}\n(edit it, or `q-cli config add [--project] <name> <conn>`)",
            target.display()
        ));
    }
    write_config_file(target, TEMPLATE)?;
    Ok(format!(
        "created {}\nedit it so `default` points at your q process, then: q-cli tables @",
        target.display()
    ))
}

fn path_summary() -> String {
    let g = global_path();
    let status = if g.exists() { "exists" } else { "none" };
    let mut s = format!("global:  {}   ({status})", g.display());
    match project_path_read() {
        Some(p) => {
            let _ = write!(s, "\nproject: {}   (active)", p.display());
        }
        None => {
            let _ = write!(
                s,
                "\nproject: {}   (none here)",
                project_path_write().display()
            );
        }
    }
    if let Ok(e) = std::env::var("Q_CLI_CONFIG") {
        let _ = write!(s, "\nenv Q_CLI_CONFIG: {e}   (used alone)");
    }
    s
}

/// Effective servers, annotated with their source (project overrides global).
fn list() -> String {
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
                    let shadows = rows.get(&k).is_some_and(|(g, _)| *g != v);
                    let src = if shadows {
                        "project, overrides global"
                    } else {
                        "project"
                    };
                    rows.insert(k, (v, src));
                }
            }
        }
    }
    if rows.is_empty() {
        return "(no servers configured)".to_string();
    }
    let formatted: Vec<String> = rows
        .iter()
        .map(|(k, (v, src))| format!("{k} = {}   [{src}]", mask(v)))
        .collect();
    formatted.join("\n")
}

/// Add or update one entry in `target`, preserving existing lines and comments.
fn add(target: &Path, name: &str, conn_str: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("profile name cannot be empty".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid profile name '{name}' \
             (must contain only alphanumeric characters, '-', or '_')"
        ));
    }
    let conn_str = conn_str.trim();
    if conn_str.is_empty() {
        return Err("connection target cannot be empty".to_string());
    }
    if conn_str.chars().any(char::is_control) || conn_str.contains(" #") || conn_str.contains("\t#")
    {
        return Err(
            "invalid connection target (cannot contain control characters or inline comments)"
                .to_string(),
        );
    }
    if conn_str.contains(':') {
        conn::parse_conn(conn_str).map_err(|_| {
            format!(
                "invalid connection string '{}': expected host:port[:user:pass] with a valid port",
                mask(conn_str)
            )
        })?;
    } else {
        let alias = conn_str.strip_prefix('@').unwrap_or(conn_str);
        let valid_alias = !alias.is_empty()
            && alias
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid_alias {
            return Err(format!(
                "invalid alias target '{conn_str}' (must be a valid profile name or host:port)"
            ));
        }
    }

    let mut lines: Vec<String> = if target.exists() {
        fs::read_to_string(target)
            .map_err(|e| format!("cannot read {}: {e}", target.display()))?
            .lines()
            .map(str::to_string)
            .collect()
    } else {
        vec!["# q-cli server profiles".to_string()]
    };

    let mut replaced = false;
    for line in lines.iter_mut().rev() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((k, _)) = t.split_once('=') {
            if k.trim() == name {
                *line = format!("{name} = {conn_str}");
                replaced = true;
                break;
            }
        }
    }
    if !replaced {
        lines.push(format!("{name} = {conn_str}"));
    }

    let mut body = lines.join("\n");
    body.push('\n');
    write_config_file(target, &body)?;
    let verb = if replaced { "updated" } else { "added" };
    Ok(format!(
        "{verb} {name} = {}  ({})",
        mask(conn_str),
        target.display()
    ))
}

/// Hide the password/secret component of a connection string while preserving its exact format.
fn mask(conn_str: &str) -> String {
    if let Ok((_host, _port, creds)) = conn::parse_conn(conn_str) {
        if creds.is_empty() {
            return conn_str.to_string();
        }
        let secret = match creds.split_once(':') {
            Some((_user, pass)) => pass,
            None => &creds,
        };
        if let Some(prefix) = conn_str.strip_suffix(secret) {
            return format!("{prefix}****");
        }
    }
    if conn_str.contains(':') {
        "<invalid connection>".to_string()
    } else {
        conn_str.to_string()
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
    fn parse_strips_inline_comments() {
        let m = parse(
            "dev = localhost:5000 # primary dev kdb\nprod = [::1]:5001:user:pass#word\t# prod",
        );
        assert_eq!(m.get("dev").map(String::as_str), Some("localhost:5000"));
        // # without preceding whitespace in password is preserved
        assert_eq!(
            m.get("prod").map(String::as_str),
            Some("[::1]:5001:user:pass#word")
        );
    }

    #[test]
    fn parse_ignores_malformed_lines() {
        let m = parse("nokey\n= noval\nk =\n");
        assert!(m.is_empty());
    }

    #[test]
    fn mask_hides_only_the_password() {
        assert_eq!(mask("host:5000:user:secret"), "host:5000:user:****");
        assert_eq!(mask("host:5000:token_only"), "host:5000:****");
        assert_eq!(mask("host:5000"), "host:5000"); // nothing to hide
        assert_eq!(mask("[::1]:5000:user:secret"), "[::1]:5000:user:****");
        assert_eq!(
            mask("tcps://host:5000:user:secret"),
            "tcps://host:5000:user:****"
        );
        assert_eq!(
            mask("tls://[::1]:5000:token_only"),
            "tls://[::1]:5000:****"
        );
    }

    #[test]
    fn overridden_lists_only_changed_global_names() {
        let to_map = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect()
        };
        let global = to_map(&[("prod", "bigbox:5001"), ("local", "localhost:5000")]);
        let project = to_map(&[
            ("prod", "evil:5001"),
            ("local", "localhost:5000"),
            ("extra", "h:1"),
        ]);
        assert_eq!(overridden(&global, &project), vec!["prod"]);
    }

    #[test]
    fn multi_hop_alias_resolution() {
        let mut servers = HashMap::new();
        servers.insert("default".into(), "dev".into());
        servers.insert("dev".into(), "remote".into());
        servers.insert("remote".into(), "cluster:5000".into());

        assert_eq!(resolve_alias("default", &servers).unwrap(), "cluster:5000");
        assert_eq!(resolve_alias("dev", &servers).unwrap(), "cluster:5000");
    }

    #[test]
    fn alias_cycle_detection() {
        let mut servers = HashMap::new();
        servers.insert("a".into(), "b".into());
        servers.insert("b".into(), "c".into());
        servers.insert("c".into(), "a".into());

        assert!(resolve_alias("a", &servers).is_err());
    }

    #[test]
    fn alias_with_at_prefix_resolution() {
        let mut servers = HashMap::new();
        servers.insert("default".into(), "@local".into());
        servers.insert("local".into(), "@remote".into());
        servers.insert("remote".into(), "cluster:5000".into());

        assert_eq!(resolve_alias("default", &servers).unwrap(), "cluster:5000");
        assert_eq!(resolve_alias("@default", &servers).unwrap(), "cluster:5000");
    }

    #[test]
    fn add_validation_rejects_invalid_names_and_conns() {
        let dummy = std::env::temp_dir().join(format!("q-cli-test-{}.conf", std::process::id()));
        assert!(add(&dummy, "bad:name", "localhost:5000").is_err());
        assert!(add(&dummy, "bad name", "localhost:5000").is_err());
        assert!(add(&dummy, "bad=name", "localhost:5000").is_err());
        assert!(add(&dummy, "", "localhost:5000").is_err());
        assert!(add(&dummy, "good-name_1", "localhost:not_a_port").is_err());
        assert!(add(&dummy, "good-name_1", "bad alias with spaces").is_err());
        assert!(add(&dummy, "good-name_1", "localhost:5000:u:p # comment").is_err());
        assert!(add(&dummy, "good-name_1", "localhost:5000:u:p\nevil=1:2").is_err());
        assert!(add(&dummy, "good-name_1", "").is_err());
        assert!(add(&dummy, "good-name_1", "@target_alias").is_ok());
        assert!(add(&dummy, "good-name_1", "target_alias").is_ok());
        assert!(run(&["list".into(), "extra".into()]).is_err());
        assert!(run(&[
            "add".into(),
            "a".into(),
            "localhost:5000".into(),
            "extra".into()
        ])
        .is_err());
        let _ = fs::remove_file(&dummy);
    }

    #[test]
    fn invalid_connection_errors_do_not_expose_credentials() {
        let target = std::env::temp_dir().join("q-cli-invalid-connection-unused.conf");
        for conn in [
            "localhost:bad:alice:review-secret",
            "[::1:5000:alice:review-secret",
            "[]:5000:alice:review-secret",
            "localhost:review-secret",
        ] {
            let err = add(&target, "prod", conn).unwrap_err();
            assert!(!err.contains("review-secret"), "{err}");
            assert!(err.contains("<invalid connection>"), "{err}");
        }
        assert_eq!(mask("prod"), "prod");
    }

    #[cfg(unix)]
    #[test]
    fn config_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let target =
            std::env::temp_dir().join(format!("q-cli-mode-test-{}.conf", std::process::id()));
        // Start from an existing world-readable file: it must be tightened too.
        fs::write(&target, "old\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        write_config_file(&target, "prod = localhost:1:u:p\n").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        let _ = fs::remove_file(&target);
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn add_updates_last_duplicate_entry() {
        let target =
            std::env::temp_dir().join(format!("q-cli-duplicate-test-{}.conf", std::process::id()));
        fs::write(
            &target,
            "# profiles\nprod = localhost:1111\nother = localhost:4444\n prod = localhost:2222\n",
        )
        .unwrap();

        let result = add(&target, "prod", "localhost:3333");
        let content = fs::read_to_string(&target);
        fs::remove_file(&target).unwrap();

        assert!(result.is_ok());
        let content = content.unwrap();
        assert_eq!(parse(&content).get("prod").unwrap(), "localhost:3333");
        assert_eq!(
            content,
            "# profiles\nprod = localhost:1111\nother = localhost:4444\nprod = localhost:3333\n"
        );
    }

    #[test]
    fn global_path_resolution() {
        assert_eq!(
            global_path_from(
                Some("/custom/path/servers.conf"),
                Some("/xdg"),
                Some("/home")
            ),
            PathBuf::from("/custom/path/servers.conf")
        );
        assert_eq!(
            global_path_from(None, Some("/xdg/config"), Some("/home")),
            PathBuf::from("/xdg/config/q-cli/servers.conf")
        );
        assert_eq!(
            global_path_from(None, None, Some("/home/user")),
            PathBuf::from("/home/user/.config/q-cli/servers.conf")
        );
    }
}
