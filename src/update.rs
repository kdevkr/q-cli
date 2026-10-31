//! Self-update command (`q-cli update`).
//!
//! Checks GitHub Releases for `kdevkr/q-cli`, downloads the matching pre-built
//! binary asset for the current OS/architecture via OS-native `curl`, and
//! replaces the running executable in-place using `self-replace`.

use std::env;
use std::fs::File;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::cmd::{Opts, OutMode};
use crate::error::{CliError, CmdResult};

const GITHUB_REPO: &str = "kdevkr/q-cli";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
}

impl SemVer {
    fn parse(s: &str) -> Option<Self> {
        let cleaned = s.trim().trim_start_matches('v');
        // Strip pre-release suffixes like `-beta.1` for comparison
        let core = cleaned.split('-').next().unwrap_or(cleaned);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

/// Run `q-cli update [flags]`.
pub fn run(args: &[String], opts: &Opts) -> CmdResult {
    let mut check_only = false;
    let mut force = false;

    for arg in args {
        match arg.as_str() {
            "--check" | "-c" => check_only = true,
            "--force" | "-f" => force = true,
            "-h" | "--help" => {
                return Ok(
                    "Usage: q-cli update [--check] [--force]\n\n\
                     Options:\n  \
                       -c, --check    Check if an update is available without installing\n  \
                       -f, --force    Reinstall / update even if already on the latest version"
                        .to_string(),
                );
            }
            other => {
                return Err(CliError::usage(&format!(
                    "unexpected argument '{other}' for 'update' (see 'q-cli update --help')"
                )));
            }
        }
    }

    let release = fetch_latest_release()?;
    let latest_tag = release.tag_name.trim();
    let current_ver = SemVer::parse(CURRENT_VERSION);
    let latest_ver = SemVer::parse(latest_tag);

    let is_newer = match (current_ver, latest_ver) {
        (Some(c), Some(l)) => l > c,
        _ => latest_tag.trim_start_matches('v') != CURRENT_VERSION,
    };

    if check_only {
        return format_check_result(latest_tag, is_newer, opts.out);
    }

    if !is_newer && !force {
        if opts.out == OutMode::Json {
            return Ok(format!(
                "{{\"current\":\"{CURRENT_VERSION}\",\"latest\":\"{latest_tag}\",\
                 \"up_to_date\":true}}"
            ));
        }
        return Ok(format!(
            "q-cli is already up to date (v{CURRENT_VERSION})."
        ));
    }

    let asset = find_matching_asset(&release.assets)?;
    let temp_bin_path = download_and_extract_asset(asset)?;

    self_replace::self_replace(&temp_bin_path).map_err(|e| {
        CliError::connect(&format!(
            "failed to replace binary with updated version: {e}"
        ))
    })?;

    // Best-effort cleanup of temp artifact
    let _ = std::fs::remove_file(&temp_bin_path);

    if opts.out == OutMode::Json {
        Ok(format!(
            "{{\"previous\":\"{CURRENT_VERSION}\",\"updated_to\":\"{latest_tag}\",\
             \"status\":\"success\"}}"
        ))
    } else {
        Ok(format!(
            "Successfully updated q-cli v{CURRENT_VERSION} -> {latest_tag}!"
        ))
    }
}

fn format_check_result(latest_tag: &str, is_newer: bool, out: OutMode) -> CmdResult {
    if out == OutMode::Json {
        Ok(format!(
            "{{\"current\":\"{CURRENT_VERSION}\",\"latest\":\"{latest_tag}\",\
             \"up_to_date\":{}}}",
            !is_newer
        ))
    } else if is_newer {
        Ok(format!(
            "A new version of q-cli is available: {latest_tag} (current: v{CURRENT_VERSION})\n\
             Run 'q-cli update' to install the latest release."
        ))
    } else {
        Ok(format!(
            "q-cli is up to date (v{CURRENT_VERSION})."
        ))
    }
}

fn fetch_latest_release() -> Result<GithubRelease, CliError> {
    let url = env::var("Q_CLI_UPDATE_URL").unwrap_or_else(|_| {
        format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest")
    });
    let output = Command::new("curl")
        .args([
            "-sSfL",
            "-H",
            "User-Agent: q-cli",
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CliError::connect(
                    "curl command not found; install curl or update manually via: \
                     cargo install --git https://github.com/kdevkr/q-cli --features tls --force",
                )
            } else {
                CliError::connect(&format!("failed to execute curl: {e}"))
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::connect(&format!(
            "GitHub release check failed (no published release found for {GITHUB_REPO} yet, \
             or network error: {})",
            stderr.trim()
        )));
    }

    serde_json::from_slice::<GithubRelease>(&output.stdout).map_err(|e| {
        CliError::connect(&format!("failed to parse GitHub release response: {e}"))
    })
}

fn find_matching_asset(assets: &[GithubAsset]) -> Result<&GithubAsset, CliError> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    for asset in assets {
        let name = asset.name.to_ascii_lowercase();
        let matches_os = match os {
            "windows" => name.contains("windows") || name.contains("pc-windows"),
            "linux" => name.contains("linux") || name.contains("unknown-linux"),
            "macos" => {
                name.contains("darwin") || name.contains("apple") || name.contains("macos")
            }
            _ => false,
        };

        let matches_arch = match arch {
            "x86_64" => name.contains("x86_64") || name.contains("amd64"),
            "aarch64" => name.contains("aarch64") || name.contains("arm64"),
            _ => false,
        };

        if matches_os && matches_arch {
            return Ok(asset);
        }
    }

    Err(CliError::connect(&format!(
        "no compatible release asset found for target {os}-{arch}"
    )))
}

fn download_and_extract_asset(asset: &GithubAsset) -> Result<PathBuf, CliError> {
    let output = Command::new("curl")
        .args([
            "-sSfL",
            "-H",
            "User-Agent: q-cli",
            &asset.browser_download_url,
        ])
        .output()
        .map_err(|e| CliError::connect(&format!("failed to download release asset: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::connect(&format!(
            "failed downloading {}: {}",
            asset.name,
            stderr.trim()
        )));
    }

    let raw_bytes = output.stdout;
    let asset_name = asset.name.to_ascii_lowercase();

    let temp_file = tempfile::Builder::new()
        .prefix("q-cli-update-")
        .tempfile()
        .map_err(|e| CliError::connect(&format!("failed to create temporary file: {e}")))?;
    let (file, temp_path) = temp_file
        .keep()
        .map_err(|e| CliError::connect(&format!("failed to persist temporary file: {e}")))?;
    drop(file);

    if asset_name.ends_with(".zip") {
        extract_zip(&raw_bytes, &temp_path)?;
    } else if asset_name.ends_with(".tar.gz") || asset_name.ends_with(".tgz") {
        extract_tar_gz(&raw_bytes, &temp_path)?;
    } else {
        // Assume direct uncompressed binary
        let mut file = File::create(&temp_path)
            .map_err(|e| CliError::connect(&format!("failed writing binary: {e}")))?;
        file.write_all(&raw_bytes)
            .map_err(|e| CliError::connect(&format!("failed writing binary: {e}")))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&temp_path)
            .map_err(|e| CliError::connect(&format!("failed reading permissions: {e}")))?
            .permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&temp_path, perms);
    }

    Ok(temp_path)
}

fn is_q_cli_bin(name: &str) -> bool {
    name == "q-cli"
        || name == "q-cli.exe"
        || name.ends_with("/q-cli")
        || name.ends_with("/q-cli.exe")
}

fn extract_zip(bytes: &[u8], target: &std::path::Path) -> Result<(), CliError> {
    let archive = rawzip::ZipArchive::from_slice(bytes)
        .map_err(|e| CliError::connect(&format!("invalid zip archive: {e:?}")))?;

    for entry_result in archive.entries() {
        let entry = entry_result
            .map_err(|e| CliError::connect(&format!("failed reading zip entry: {e:?}")))?;
        let name = String::from_utf8_lossy(entry.file_path().as_bytes()).to_ascii_lowercase();

        if is_q_cli_bin(&name) {
            let local_entry = archive
                .get_entry(entry.wayfinder())
                .map_err(|e| CliError::connect(&format!("failed locating zip entry: {e:?}")))?;

            let mut out = File::create(target)
                .map_err(|e| CliError::connect(&format!("failed to create target file: {e}")))?;

            match entry.compression_method() {
                rawzip::CompressionMethod::DEFLATE => {
                    let mut decoder = flate2::read::DeflateDecoder::new(local_entry.data());
                    std::io::copy(&mut decoder, &mut out).map_err(|e| {
                        CliError::connect(&format!("failed decompressing zip entry: {e}"))
                    })?;
                    return Ok(());
                }
                rawzip::CompressionMethod::STORE => {
                    out.write_all(local_entry.data()).map_err(|e| {
                        CliError::connect(&format!("failed writing uncompressed entry: {e}"))
                    })?;
                    return Ok(());
                }
                other => {
                    return Err(CliError::connect(&format!(
                        "unsupported zip compression method: {other:?}"
                    )));
                }
            }
        }
    }

    Err(CliError::connect(
        "q-cli binary not found inside downloaded zip archive",
    ))
}

fn extract_tar_gz(bytes: &[u8], target: &std::path::Path) -> Result<(), CliError> {
    let tar = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(tar);

    let entries = archive
        .entries()
        .map_err(|e| CliError::connect(&format!("invalid tar.gz archive: {e}")))?;

    for entry in entries {
        let mut entry =
            entry.map_err(|e| CliError::connect(&format!("failed reading tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CliError::connect(&format!("invalid path in tar entry: {e}")))?
            .to_string_lossy()
            .to_ascii_lowercase();

        if is_q_cli_bin(&path) {
            let mut out = File::create(target)
                .map_err(|e| CliError::connect(&format!("failed to create target file: {e}")))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| CliError::connect(&format!("failed to extract binary: {e}")))?;
            return Ok(());
        }
    }

    Err(CliError::connect(
        "q-cli binary not found inside downloaded tar.gz archive",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parsing_and_ordering() {
        let v1 = SemVer::parse("0.1.0").unwrap();
        let v2 = SemVer::parse("v0.2.0").unwrap();
        let v3 = SemVer::parse("v0.2.1-beta").unwrap();
        let v4 = SemVer::parse("1.0.0").unwrap();

        assert!(v2 > v1);
        assert!(v3 > v2);
        assert!(v4 > v3);
        assert_eq!(v1, SemVer { major: 0, minor: 1, patch: 0 });
    }

    #[test]
    fn asset_matching_logic() {
        let assets = vec![
            GithubAsset {
                name: "q-cli-x86_64-pc-windows-msvc.zip".to_string(),
                browser_download_url: "http://win".to_string(),
            },
            GithubAsset {
                name: "q-cli-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                browser_download_url: "http://linux".to_string(),
            },
            GithubAsset {
                name: "q-cli-aarch64-apple-darwin.tar.gz".to_string(),
                browser_download_url: "http://mac".to_string(),
            },
        ];

        let matched = find_matching_asset(&assets);
        assert!(matched.is_ok());
    }

    #[test]
    fn extract_zip_finds_and_unpacks_binary() {
        let mut buf = Vec::new();
        {
            let mut writer = rawzip::ZipArchiveWriter::new(&mut buf);
            let (entry, config) = writer
                .new_file("q-cli.exe")
                .compression_method(rawzip::CompressionMethod::STORE)
                .start()
                .unwrap();
            let mut data_writer = config.wrap(entry);
            data_writer.write_all(b"dummy-binary-bytes").unwrap();
            let (entry, desc) = data_writer.finish().unwrap();
            entry.finish(desc).unwrap();
            writer.finish().unwrap();
        }
        let temp = tempfile::NamedTempFile::new().unwrap();
        let target = temp.path();
        extract_zip(&buf, target).unwrap();
        let contents = std::fs::read(target).unwrap();
        assert_eq!(contents, b"dummy-binary-bytes");
    }

    #[test]
    fn extract_tar_gz_finds_and_unpacks_binary() {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(18);
            header.set_cksum();
            tar.append_data(&mut header, "q-cli", b"dummy-tar-gz-bytes" as &[u8]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let temp = tempfile::NamedTempFile::new().unwrap();
        let target = temp.path();
        extract_tar_gz(&buf, target).unwrap();
        let contents = std::fs::read(target).unwrap();
        assert_eq!(contents, b"dummy-tar-gz-bytes");
    }

    #[test]
    fn update_check_with_mock_server() {
        use std::io::Read;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let body = concat!(
                    "{\"tag_name\":\"v9.9.9\",",
                    "\"assets\":[{\"name\":\"test\",\"browser_download_url\":\"http://test\"}]}"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        let test_url = format!("http://127.0.0.1:{port}/releases/latest");
        env::set_var("Q_CLI_UPDATE_URL", &test_url);

        let res = run(&["--check".to_string()], &Opts::default()).unwrap();
        assert!(res.contains("v9.9.9"));
        assert!(res.contains("A new version of q-cli is available"));

        env::remove_var("Q_CLI_UPDATE_URL");
    }
}
