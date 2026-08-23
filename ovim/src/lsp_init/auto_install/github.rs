use super::InstallResult;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Install via GitHub release (download binary or archive).
pub(super) async fn install_via_github(
    language_name: &str,
    repo: &str,
    asset_pattern: &str,
    install_path: &str,
    binary_name: Option<&str>,
) -> InstallResult {
    #[derive(Debug, Deserialize)]
    struct GitHubRelease {
        assets: Vec<GitHubAsset>,
    }

    #[derive(Debug, Deserialize)]
    struct GitHubAsset {
        name: String,
        browser_download_url: String,
    }

    if repo.split('/').count() != 2 {
        return InstallResult::Failed(format!(
            "Invalid GitHub repo '{repo}'. Expected format: owner/repo"
        ));
    }

    let release_url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let client = match reqwest::Client::builder()
        .user_agent("ovim-auto-install")
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return InstallResult::Failed(format!("Failed to initialize HTTP client: {e}"));
        }
    };

    let release_res = match client
        .get(&release_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            return InstallResult::Failed(format!(
                "Failed to query GitHub releases for {repo}: {e}"
            ));
        }
    };

    if !release_res.status().is_success() {
        return InstallResult::Failed(format!(
            "GitHub API request failed for {repo}: HTTP {}",
            release_res.status()
        ));
    }

    let release = match release_res.json::<GitHubRelease>().await {
        Ok(release) => release,
        Err(e) => {
            return InstallResult::Failed(format!("Failed to parse GitHub release metadata: {e}"));
        }
    };

    let expanded_patterns = expand_platform_patterns(asset_pattern);
    let asset = expanded_patterns.iter().find_map(|pattern| {
        release
            .assets
            .iter()
            .find(|asset| asset_matches_pattern(&asset.name, pattern))
    });

    let Some(asset) = asset else {
        return InstallResult::Failed(format!(
            "No release asset matched pattern '{asset_pattern}' (expanded for {}/{}) in {repo}",
            std::env::consts::OS,
            std::env::consts::ARCH,
        ));
    };

    ovim_core::lsp_info!(
        "AutoInstall",
        "Installing {} via GitHub release: {}/{}",
        language_name,
        repo,
        asset.name
    );

    let download_res = match client
        .get(&asset.browser_download_url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            return InstallResult::Failed(format!(
                "Failed to download GitHub asset '{}': {e}",
                asset.name
            ));
        }
    };

    if !download_res.status().is_success() {
        return InstallResult::Failed(format!(
            "GitHub asset download failed for '{}': HTTP {}",
            asset.name,
            download_res.status()
        ));
    }

    let bytes = match download_res.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            return InstallResult::Failed(format!(
                "Failed to read downloaded bytes for '{}': {e}",
                asset.name
            ));
        }
    };

    match install_downloaded_asset(
        &bytes,
        &asset.name,
        &expand_install_path(install_path),
        binary_name,
        language_name,
    )
    .await
    {
        Ok(path) => InstallResult::Success(path),
        Err(error) => InstallResult::Failed(format!(
            "Failed to install GitHub asset '{}': {error}",
            asset.name
        )),
    }
}

fn expand_install_path(raw: &str) -> PathBuf {
    let expanded = shellexpand::tilde(raw).into_owned();
    PathBuf::from(expanded)
}

/// Expand `{os}`, `{arch}`, and `{ext}` placeholders in asset patterns.
/// Returns multiple candidates to handle naming inconsistencies across projects.
fn expand_platform_patterns(pattern: &str) -> Vec<String> {
    expand_platform_patterns_for(pattern, std::env::consts::OS, std::env::consts::ARCH)
}

fn expand_platform_patterns_for(pattern: &str, os: &str, arch: &str) -> Vec<String> {
    if !["{os}", "{arch}", "{ext}"]
        .iter()
        .any(|placeholder| pattern.contains(placeholder))
    {
        return vec![pattern.to_string()];
    }

    let os_variants: Vec<&str> = match os {
        "macos" => vec!["darwin", "macos"],
        "linux" => vec!["linux"],
        "windows" => vec!["windows", "win64"],
        other => vec![other],
    };

    let arch_variants: Vec<&str> = match arch {
        "x86_64" => vec!["x86_64", "amd64", "x64"],
        "aarch64" => vec!["aarch64", "arm64"],
        other => vec![other],
    };

    // Most projects ship Unix standalone binaries with gzip and Windows
    // binaries in zip archives. Configs that do not use {ext} are unaffected.
    let extension = if os == "windows" { "zip" } else { "gz" };
    let mut patterns = Vec::new();
    for os in os_variants {
        for arch in &arch_variants {
            patterns.push(
                pattern
                    .replace("{os}", os)
                    .replace("{arch}", arch)
                    .replace("{ext}", extension),
            );
        }
    }
    patterns
}

async fn install_downloaded_asset(
    bytes: &[u8],
    asset_name: &str,
    install_path: &Path,
    binary_name: Option<&str>,
    default_binary_name: &str,
) -> Result<PathBuf, String> {
    match asset_kind(asset_name) {
        AssetKind::Archive => {
            install_archive(
                bytes,
                asset_name,
                install_path,
                binary_name,
                default_binary_name,
            )
            .await
        }
        AssetKind::GzipBinary => {
            let output_path = binary_name
                .map(|name| install_path.join(platform_executable_name(name)))
                .unwrap_or_else(|| install_path.to_path_buf());
            let decompressed = decompress_gzip_binary(bytes)?;
            write_executable(&output_path, &decompressed).await?;
            Ok(output_path)
        }
        AssetKind::Binary => {
            write_executable(install_path, bytes).await?;
            Ok(install_path.to_path_buf())
        }
    }
}

async fn install_archive(
    bytes: &[u8],
    asset_name: &str,
    target_dir: &Path,
    binary_name: Option<&str>,
    default_binary_name: &str,
) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(target_dir)
        .await
        .map_err(|error| {
            format!(
                "failed to create install directory '{}': {error}",
                target_dir.display()
            )
        })?;

    extract_archive(bytes, asset_name, target_dir)?;

    let binary_name = binary_name.unwrap_or(default_binary_name);
    let binary_path = target_dir.join(platform_executable_name(binary_name));
    if !binary_path.is_file() {
        return Err(format!(
            "archive extracted but binary '{binary_name}' was not found at '{}'",
            binary_path.display()
        ));
    }

    set_executable_permissions(&binary_path).await?;
    Ok(binary_path)
}

fn decompress_gzip_binary(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|error| format!("gzip decompression failed: {error}"))?;
    Ok(decompressed)
}

async fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            format!(
                "failed to create install directory '{}': {error}",
                parent.display()
            )
        })?;
    }

    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| format!("failed to write binary to '{}': {error}", path.display()))?;
    set_executable_permissions(path).await
}

#[cfg(unix)]
async fn set_executable_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .await
        .map_err(|error| {
            format!(
                "failed to set executable permissions on '{}': {error}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
async fn set_executable_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn platform_executable_name(binary_name: &str) -> String {
    if std::env::consts::EXE_SUFFIX.is_empty()
        || binary_name.ends_with(std::env::consts::EXE_SUFFIX)
        || Path::new(binary_name).extension().is_some()
    {
        binary_name.to_string()
    } else {
        format!("{binary_name}{}", std::env::consts::EXE_SUFFIX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Archive,
    GzipBinary,
    Binary,
}

fn asset_kind(name: &str) -> AssetKind {
    let lower = name.to_ascii_lowercase();
    if is_archive_asset(&lower) {
        AssetKind::Archive
    } else if lower.ends_with(".gz") {
        AssetKind::GzipBinary
    } else {
        AssetKind::Binary
    }
}

/// Extract an archive (.tar.gz, .tgz, .zip) to a target directory.
fn extract_archive(
    bytes: &[u8],
    asset_name: &str,
    target_dir: &std::path::Path,
) -> Result<(), String> {
    let lower = asset_name.to_ascii_lowercase();

    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        extract_tar_gz(bytes, target_dir)
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        extract_via_shell_tar(bytes, asset_name, target_dir)
    } else if lower.ends_with(".zip") {
        extract_zip(bytes, target_dir)
    } else {
        Err(format!("Unsupported archive format: {asset_name}"))
    }
}

fn extract_tar_gz(bytes: &[u8], target_dir: &std::path::Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::io::Cursor;

    let cursor = Cursor::new(bytes);
    let decoder = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(decoder);

    archive
        .unpack(target_dir)
        .map_err(|e| format!("tar.gz extraction failed: {e}"))
}

fn extract_zip(bytes: &[u8], target_dir: &std::path::Path) -> Result<(), String> {
    use std::io::Cursor;

    let cursor = Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to read zip archive: {e}"))?;

    archive
        .extract(target_dir)
        .map_err(|e| format!("zip extraction failed: {e}"))
}

fn extract_via_shell_tar(
    bytes: &[u8],
    asset_name: &str,
    target_dir: &std::path::Path,
) -> Result<(), String> {
    let temp_file = target_dir.join(asset_name);
    std::fs::write(&temp_file, bytes).map_err(|e| format!("Failed to write temp archive: {e}"))?;

    let output = Command::new("tar")
        .args([
            "xf",
            &temp_file.to_string_lossy(),
            "-C",
            &target_dir.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("Failed to run tar: {e}"))?;

    let _ = std::fs::remove_file(&temp_file);

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("tar extraction failed: {stderr}"))
    }
}

fn is_archive_asset(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".zip")
        || lower.ends_with(".tar")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".txz")
}

fn asset_matches_pattern(asset_name: &str, pattern: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return asset_name == pattern;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let starts_anchored = !pattern.starts_with('*');
    let ends_anchored = !pattern.ends_with('*');
    let mut cursor = 0usize;

    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if idx == 0 && starts_anchored {
            if !asset_name[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
            continue;
        }

        if idx == parts.len() - 1 && ends_anchored {
            let remaining = &asset_name[cursor..];
            if !remaining.ends_with(part) {
                return false;
            }
            if let Some(pos) = remaining.rfind(part) {
                cursor += pos + part.len();
            }
            continue;
        }

        if let Some(found_at) = asset_name[cursor..].find(part) {
            cursor += found_at + part.len();
        } else {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn asset_patterns_support_wildcards() {
        assert!(asset_matches_pattern(
            "typescript-language-server-linux-x64",
            "typescript-language-server-*"
        ));
        assert!(asset_matches_pattern("gopls", "gopls"));
        assert!(!asset_matches_pattern(
            "clangd-arm64.zip",
            "clangd-*-linux*"
        ));
    }

    #[test]
    fn rust_analyzer_pattern_matches_supported_platform_assets() {
        let pattern = "rust-analyzer-{arch}-*{os}*.{ext}";
        for (os, arch, asset) in [
            (
                "linux",
                "x86_64",
                "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            ),
            ("macos", "aarch64", "rust-analyzer-aarch64-apple-darwin.gz"),
            (
                "windows",
                "x86_64",
                "rust-analyzer-x86_64-pc-windows-msvc.zip",
            ),
        ] {
            let patterns = expand_platform_patterns_for(pattern, os, arch);
            assert!(
                patterns
                    .iter()
                    .any(|pattern| asset_matches_pattern(asset, pattern)),
                "{asset} did not match any of {patterns:?}"
            );
        }
    }

    #[test]
    fn distinguishes_gzip_binaries_from_tar_archives() {
        assert_eq!(
            asset_kind("rust-analyzer-x86_64-unknown-linux-gnu.gz"),
            AssetKind::GzipBinary
        );
        assert_eq!(
            asset_kind("lua-language-server-linux-x64.tar.gz"),
            AssetKind::Archive
        );
        assert_eq!(asset_kind("zls"), AssetKind::Binary);
    }

    #[tokio::test]
    async fn installs_a_gzip_compressed_executable() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let install_dir = temp_dir.path().join("rust-analyzer/bin");
        let expected_contents = b"standalone executable";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(expected_contents)
            .expect("compress fixture");
        let compressed = encoder.finish().expect("finish fixture");

        let installed = install_downloaded_asset(
            &compressed,
            "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            &install_dir,
            Some("rust-analyzer"),
            "Rust",
        )
        .await
        .expect("install gzip binary");

        assert_eq!(
            std::fs::read(&installed).expect("read installed binary"),
            expected_contents
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = installed
                .metadata()
                .expect("binary metadata")
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "installed binary must be executable");
        }
    }

    #[tokio::test]
    async fn invalid_gzip_does_not_create_a_binary() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let install_dir = temp_dir.path().join("rust-analyzer/bin");

        let error = install_downloaded_asset(
            b"not gzip data",
            "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            &install_dir,
            Some("rust-analyzer"),
            "Rust",
        )
        .await
        .expect_err("invalid gzip must fail");

        assert!(error.contains("gzip decompression failed"));
        assert!(!install_dir.join("rust-analyzer").exists());
    }

    #[tokio::test]
    #[ignore = "downloads the latest Rust Analyzer release from GitHub"]
    async fn installs_rust_analyzer_release() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let result = install_via_github(
            "Rust",
            "rust-lang/rust-analyzer",
            "rust-analyzer-{arch}-*{os}*.{ext}",
            temp_dir.path().to_str().expect("UTF-8 temporary path"),
            Some("rust-analyzer"),
        )
        .await;
        let InstallResult::Success(binary) = result else {
            panic!("Rust Analyzer install failed: {result:?}");
        };

        let version = Command::new(binary)
            .arg("--version")
            .output()
            .expect("run installed Rust Analyzer");
        assert!(version.status.success());
        assert!(String::from_utf8_lossy(&version.stdout).contains("rust-analyzer"));
    }
}
