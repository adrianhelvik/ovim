use super::InstallResult;
use serde::Deserialize;
use std::path::PathBuf;
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

    let target_path = expand_install_path(install_path);

    if is_archive_asset(&asset.name) {
        if let Err(e) = tokio::fs::create_dir_all(&target_path).await {
            return InstallResult::Failed(format!(
                "Failed to create install directory '{}': {e}",
                target_path.display()
            ));
        }

        if let Err(e) = extract_archive(&bytes, &asset.name, &target_path) {
            return InstallResult::Failed(format!(
                "Failed to extract archive '{}': {e}",
                asset.name
            ));
        }

        let bin = binary_name.unwrap_or(language_name);
        let binary_path = target_path.join(bin);
        if !binary_path.exists() {
            return InstallResult::Failed(format!(
                "Archive extracted but binary '{}' not found at '{}'",
                bin,
                binary_path.display()
            ));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            let _ = std::fs::set_permissions(&binary_path, perms);
        }

        InstallResult::Success(binary_path)
    } else {
        let parent = target_path.parent().map(PathBuf::from).unwrap_or_default();
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(&parent).await {
                return InstallResult::Failed(format!(
                    "Failed to create install directory '{}': {e}",
                    parent.display()
                ));
            }
        }

        if let Err(e) = tokio::fs::write(&target_path, bytes).await {
            return InstallResult::Failed(format!(
                "Failed to write binary to '{}': {e}",
                target_path.display()
            ));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            if let Err(e) = tokio::fs::set_permissions(&target_path, perms).await {
                return InstallResult::Failed(format!(
                    "Installed file but failed to set executable permissions on '{}': {e}",
                    target_path.display()
                ));
            }
        }

        InstallResult::Success(target_path)
    }
}

fn expand_install_path(raw: &str) -> PathBuf {
    let expanded = shellexpand::tilde(raw).into_owned();
    PathBuf::from(expanded)
}

/// Expand `{os}` and `{arch}` placeholders in asset patterns.
/// Returns multiple candidates to handle naming inconsistencies across projects.
fn expand_platform_patterns(pattern: &str) -> Vec<String> {
    if !pattern.contains("{os}") && !pattern.contains("{arch}") {
        return vec![pattern.to_string()];
    }

    let os_variants: &[&str] = match std::env::consts::OS {
        "macos" => &["darwin", "macos"],
        "linux" => &["linux"],
        "windows" => &["windows", "win64"],
        other => {
            return vec![pattern
                .replace("{os}", other)
                .replace("{arch}", std::env::consts::ARCH)]
        }
    };

    let arch_variants: &[&str] = match std::env::consts::ARCH {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        other => &[other],
    };

    let mut patterns = Vec::new();
    for os in os_variants {
        for arch in arch_variants {
            patterns.push(pattern.replace("{os}", os).replace("{arch}", arch));
        }
    }
    patterns
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
}
