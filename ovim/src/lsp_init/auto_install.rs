// Auto-Install System for Language Servers
//
// Educational Note: Package Manager Integration
//
// This module handles automatic installation of language servers via package managers
// like npm, cargo, etc. The design principles here are:
//
// 1. User Consent First - Always prompt before installing anything
// 2. Graceful Degradation - If auto-install fails, show manual instructions
// 3. Network/Permission Resilience - Handle common failure modes with helpful messages
// 4. Progress Feedback - Users should know what's happening during long installs
//
// Why this matters:
// - Installing software is potentially dangerous (security, disk space, permissions)
// - Users should be in control, not surprised by automatic actions
// - Error messages should be actionable, not just "it failed"
//
// The pattern: Try → Fail gracefully → Guide user to success

use crate::language_config::{AutoInstallConfig, InstallMethod};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tokio::process::Command as TokioCommand;

/// Result of an auto-install attempt
#[derive(Debug)]
pub enum InstallResult {
    /// Installation succeeded, LSP server now available at this path
    Success(PathBuf),

    /// Installation failed with a user-facing error message
    Failed(String),

    /// Prerequisites not met (e.g., npm not installed)
    PrerequisitesMissing(String),
}

/// Attempt to auto-install a language server
///
/// Educational Note: Async for Network Operations
/// This function is async because package installations often involve:
/// - Network requests (downloading packages)
/// - Long-running processes (building from source)
/// - Multiple steps that could fail independently
///
/// Making it async allows the editor to remain responsive during installation.
pub async fn attempt_auto_install(
    language_name: &str,
    package_name: &str,
    config: &AutoInstallConfig,
) -> InstallResult {
    match &config.method {
        InstallMethod::Npm { global, bin, .. } => {
            let packages = config.method.npm_packages();
            // Verification prefers the explicit `bin`, then the server command
            // the caller is trying to resolve. The old npm_bin() fallback to
            // the package name breaks for packages whose binaries are named
            // differently (vscode-langservers-extracted has no such binary).
            let verify_bin = bin.clone().unwrap_or_else(|| package_name.to_string());
            install_via_npm(language_name, &packages, &verify_bin, *global).await
        }
        InstallMethod::Cargo {
            package,
            bin,
            features,
        } => install_via_cargo(language_name, package, bin.as_deref(), features).await,
        InstallMethod::Github {
            repo,
            asset_pattern,
            install_path,
            binary_name,
        } => {
            install_via_github(
                language_name,
                repo,
                asset_pattern,
                install_path,
                binary_name.as_deref(),
            )
            .await
        }
        InstallMethod::Shell { command } => {
            install_via_shell(language_name, package_name, command).await
        }
    }
}

/// Supply-chain guard: refuse to install node package versions published
/// less than this many days ago. Compromised releases are typically
/// discovered and yanked within hours; a 3-day quarantine covers that
/// window without meaningfully delaying legitimate updates. Enforced with
/// npm's `--before` flag, which restricts resolution to versions published
/// before the given timestamp.
const MINIMUM_RELEASE_AGE_DAYS: u64 = 3;

/// The `--before` cutoff: now minus the quarantine window, as an ISO-8601
/// UTC timestamp npm can parse.
fn release_age_cutoff() -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(MINIMUM_RELEASE_AGE_DAYS as i64);
    cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Install a node-ecosystem language server
///
/// By default packages install into an ovim-managed sandbox
/// (`~/.local/share/ovim/lsp/npm/<package>/`), mason-style: a stub
/// `package.json` is written there, `npm install` runs inside it, and
/// command resolution searches each sandbox's `node_modules/.bin`. The
/// user's global package namespace is never touched, and pinned
/// dependencies (e.g. astro-ls's typescript@6) can't conflict with the
/// user's versions.
///
/// Two supply-chain defenses, both free with npm (which ships with node):
/// `--before` quarantines freshly published versions (a hijacked package
/// ships malware for the few hours before it's caught), and
/// `--ignore-scripts` refuses to run install scripts, the usual malware
/// entry point. Language servers are plain JS and don't need build scripts.
///
/// `global = true` in a user's language override keeps the old
/// `npm install -g` behavior (no sandbox, no supply-chain guards).
async fn install_via_npm(
    _language_name: &str,
    packages: &[String],
    verify_bin: &str,
    global: bool,
) -> InstallResult {
    if packages.is_empty() {
        return InstallResult::Failed("No npm packages configured for auto-install.".to_string());
    }

    let package_list = packages.join(" ");
    if verify_bin.is_empty() {
        return InstallResult::Failed(
            "No npm binary configured for auto-install verification.".to_string(),
        );
    }

    // Step 1: Check npm is available
    let npm_ok = Command::new("npm")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !npm_ok {
        return InstallResult::PrerequisitesMissing(
            "npm not found. Install Node.js first:\n  \
             - macOS: brew install node\n  \
             - Linux: sudo apt install nodejs npm\n  \
             - Windows: Download from https://nodejs.org"
                .to_string(),
        );
    }

    // Step 2: Prepare the sandbox (sandboxed by default)
    let sandbox = if global {
        None
    } else {
        let primary = npm_package_base_name(&packages[0]);
        let Some(dir) = crate::language_config::managed_lsp_package_dir("npm", &primary) else {
            return InstallResult::Failed(
                "Could not determine home directory for sandboxed install.".to_string(),
            );
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return InstallResult::Failed(format!(
                "Failed to create install directory '{}': {}",
                dir.display(),
                e
            ));
        }
        // Convert sandboxes laid out by the interim pnpm-based installer:
        // npm can't reuse pnpm's symlinked node_modules.
        let pnpm_lock = dir.join("pnpm-lock.yaml");
        if pnpm_lock.exists() {
            let _ = std::fs::remove_file(&pnpm_lock);
            let _ = std::fs::remove_file(dir.join("pnpm-workspace.yaml"));
            let _ = std::fs::remove_dir_all(dir.join("node_modules"));
        }
        // A stub manifest pins npm to this directory; without one it walks
        // up looking for a project root and could install somewhere else.
        let manifest = dir.join("package.json");
        if !manifest.exists() {
            if let Err(e) = std::fs::write(&manifest, "{\n  \"private\": true\n}\n") {
                return InstallResult::Failed(format!(
                    "Failed to write '{}': {}",
                    manifest.display(),
                    e
                ));
            }
        }
        Some(dir)
    };

    let mut args = vec!["install".to_string()];
    if global {
        args.push("-g".to_string());
    } else {
        // The supply-chain guards. Only for sandboxed installs: global mode
        // is the user's explicit legacy escape hatch.
        args.push(format!("--before={}", release_age_cutoff()));
        args.push("--ignore-scripts".to_string());
    }
    args.extend(packages.iter().cloned());

    ovim_core::lsp_info!(
        "AutoInstall",
        "Installing {} via npm: npm {}{}",
        package_list,
        args.join(" "),
        sandbox
            .as_ref()
            .map(|d| format!(" (sandbox: {})", d.display()))
            .unwrap_or_default()
    );

    // Step 3: Run npm install with output streaming
    let mut command = TokioCommand::new("npm");
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &sandbox {
        command.current_dir(dir);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return InstallResult::Failed(format!("Failed to spawn npm process: {}", e));
        }
    };

    // Step 4: Wait for completion
    let output = match child.wait_with_output().await {
        Ok(output) => output,
        Err(e) => {
            return InstallResult::Failed(format!("npm install process failed: {}", e));
        }
    };

    // Step 5: Check exit status and parse errors
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Parse common npm error patterns
        if stderr.contains("EACCES") || stderr.contains("permission denied") {
            return InstallResult::Failed(format!(
                "Permission denied installing '{}'. Check that {} is writable.",
                package_list,
                sandbox
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| "the npm global prefix".to_string())
            ));
        }

        if stderr.contains("ENOTFOUND") || stderr.contains("ETIMEDOUT") {
            return InstallResult::Failed(
                "Network error. Check internet connection and try again.".to_string(),
            );
        }

        if stderr.contains("404") || stderr.contains("not found") || stderr.contains("ETARGET") {
            return InstallResult::Failed(format!(
                "One or more npm packages were not found: '{}'. Check package names. \
                 Note: versions published in the last {} days are quarantined \
                 (supply-chain guard).",
                package_list, MINIMUM_RELEASE_AGE_DAYS
            ));
        }

        // Generic failure with stderr output
        return InstallResult::Failed(format!(
            "npm install failed:\n{}",
            stderr.lines().take(10).collect::<Vec<_>>().join("\n")
        ));
    }

    // Step 6: Verify the executable exists in the sandbox. It is invoked at
    // its real location so .bin entries that resolve paths relative to $0
    // keep working.
    let install_path = match &sandbox {
        Some(dir) => {
            let candidate = dir.join("node_modules").join(".bin").join(verify_bin);
            candidate.is_file().then_some(candidate)
        }
        None => verify_npm_installation(verify_bin, global).await,
    };

    match install_path {
        Some(path) => {
            ovim_core::lsp_info!(
                "AutoInstall",
                "Successfully installed {} (binary: {}) at {}",
                package_list,
                verify_bin,
                path.display()
            );
            InstallResult::Success(path)
        }
        None => InstallResult::Failed(format!(
            "Installation appeared to succeed, but '{}' was not found. \
             Check the package actually provides that executable.",
            verify_bin
        )),
    }
}

/// Strip a version specifier from an npm package spec, preserving scopes:
/// `typescript@6` → `typescript`, `@astrojs/language-server@2` →
/// `@astrojs/language-server`.
fn npm_package_base_name(spec: &str) -> String {
    let search_from = if spec.starts_with('@') { 1 } else { 0 };
    match spec[search_from..].find('@') {
        Some(pos) => spec[..search_from + pos].to_string(),
        None => spec.to_string(),
    }
}

/// Verify npm package installation by finding the binary
///
/// Educational Note: PATH Resolution
/// After `npm install -g`, the binary should be in PATH. But there are edge cases:
/// - npm might install to a directory not in PATH
/// - Shell hasn't refreshed PATH yet
/// - User's npm prefix is misconfigured
///
/// We check common locations as fallback.
async fn verify_npm_installation(binary: &str, global: bool) -> Option<PathBuf> {
    // Try `which <binary>` first (checks PATH)
    if let Ok(output) = Command::new("which").arg(binary).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    // Fallback: Check common npm global install locations
    if global {
        let candidates = vec![
            dirs::home_dir().map(|h| h.join(".npm-global/bin").join(binary)),
            dirs::home_dir().map(|h| h.join(".nvm/current/bin").join(binary)),
            Some(PathBuf::from(format!("/usr/local/bin/{}", binary))),
            Some(PathBuf::from(format!("/opt/homebrew/bin/{}", binary))),
        ];

        for candidate in candidates.into_iter().flatten() {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Install via cargo (Rust's package manager)
///
/// Installs into an ovim-managed sandbox via `cargo install --root`
/// (binaries land in `<sandbox>/bin/`, which command resolution searches)
/// and the user's `~/.cargo/bin` is left alone.
async fn install_via_cargo(
    _language_name: &str,
    package: &str,
    bin: Option<&str>,
    features: &[String],
) -> InstallResult {
    // Check if cargo is available
    let cargo_ok = Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());

    if !cargo_ok {
        return InstallResult::PrerequisitesMissing(
            "cargo not found. Install Rust from https://rustup.rs".to_string(),
        );
    }

    let Some(sandbox) = crate::language_config::managed_lsp_package_dir("cargo", package) else {
        return InstallResult::Failed(
            "Could not determine home directory for sandboxed install.".to_string(),
        );
    };

    ovim_core::lsp_info!(
        "AutoInstall",
        "Installing {} via cargo install (sandbox: {})",
        package,
        sandbox.display()
    );

    // Build cargo install args
    let mut args = vec![
        "install".to_string(),
        package.to_string(),
        "--root".to_string(),
        sandbox.to_string_lossy().into_owned(),
    ];
    if !features.is_empty() {
        args.push("--features".to_string());
        args.push(features.join(","));
    }

    // Run cargo install
    let child = match TokioCommand::new("cargo")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return InstallResult::Failed(format!("Failed to spawn cargo process: {}", e));
        }
    };

    let output = match child.wait_with_output().await {
        Ok(output) => output,
        Err(e) => {
            return InstallResult::Failed(format!("cargo install failed: {}", e));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return InstallResult::Failed(format!(
            "cargo install failed:\n{}",
            stderr.lines().take(10).collect::<Vec<_>>().join("\n")
        ));
    }

    // Verify the binary landed in the sandbox: explicit bin name if
    // provided, otherwise package name
    let verify_bin = bin.unwrap_or(package);
    let candidate = sandbox.join("bin").join(verify_bin);
    if candidate.is_file() {
        return InstallResult::Success(candidate);
    }

    InstallResult::Failed(format!(
        "Installation appeared to succeed, but '{}' was not found in the \
         install sandbox. Check the crate actually provides that binary.",
        verify_bin
    ))
}

/// Install via GitHub release (download binary or archive)
async fn install_via_github(
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

    // Expand {os} and {arch} placeholders in the asset pattern
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
        // Extract archive to target directory
        if let Err(e) = tokio::fs::create_dir_all(&target_path).await {
            return InstallResult::Failed(format!(
                "Failed to create install directory '{}': {e}",
                target_path.display()
            ));
        }

        let extract_result = extract_archive(&bytes, &asset.name, &target_path);
        if let Err(e) = extract_result {
            return InstallResult::Failed(format!(
                "Failed to extract archive '{}': {e}",
                asset.name
            ));
        }

        // Find the binary within the extracted archive
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
        // Direct binary download (non-archive)
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
        // tar.xz: shell out to tar since xz decompression crates are heavy
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
    // Write to temp file and extract with system tar
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

    // Clean up temp file
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

    // Supports a simple `*` wildcard in any position.
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

/// Install via custom shell command
///
/// Educational Note: Why verify after a shell install?
/// Shell installers (`go install`, `dotnet tool install`, `gem install`, ...)
/// each drop their binary in a tool-specific directory that is often NOT in
/// PATH (e.g. `~/go/bin`, `~/.dotnet/tools`). If we returned a placeholder
/// "success" path without locating the real binary, the caller's post-install
/// re-detection (`find_lsp_command`) would fail and re-trigger the install
/// consent prompt forever. So we resolve the actual binary here and report a
/// clear failure if it can't be found, rather than a bogus success.
async fn install_via_shell(_language_name: &str, verify_bin: &str, command: &str) -> InstallResult {
    ovim_core::lsp_info!("AutoInstall", "Running custom install command: {}", command);

    // Parse command (simple split on spaces - doesn't handle quotes)
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return InstallResult::Failed("Empty install command".to_string());
    }

    let child = match TokioCommand::new(parts[0])
        .args(&parts[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return InstallResult::Failed(format!("Failed to run install command: {}", e));
        }
    };

    let output = match child.wait_with_output().await {
        Ok(output) => output,
        Err(e) => {
            return InstallResult::Failed(format!("Install command failed: {}", e));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return InstallResult::Failed(format!(
            "Install command failed:\n{}",
            stderr.lines().take(10).collect::<Vec<_>>().join("\n")
        ));
    }

    // The command exited 0 — now locate the binary it was supposed to install.
    match verify_shell_installation(verify_bin) {
        Some(path) => {
            ovim_core::lsp_info!(
                "AutoInstall",
                "Custom install succeeded; resolved '{}' at {}",
                verify_bin,
                path.display()
            );
            InstallResult::Success(path)
        }
        None => InstallResult::Failed(format!(
            "Install command succeeded, but '{}' was not found in PATH or any \
             well-known install directory (~/go/bin, ~/.dotnet/tools, etc.). \
             Add its install directory to PATH and reopen the file.",
            verify_bin
        )),
    }
}

/// Verify a shell-installed binary by checking PATH then well-known locations.
fn verify_shell_installation(binary: &str) -> Option<PathBuf> {
    if binary.is_empty() {
        return None;
    }

    // Try `which <binary>` first (checks PATH)
    if let Ok(output) = Command::new("which").arg(binary).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    // Fallback: the same well-known directories find_lsp_command searches,
    // so a successful verification here implies re-detection will also succeed.
    crate::language_config::find_in_well_known_locations(binary).map(PathBuf::from)
}

#[cfg(test)]
#[allow(clippy::print_stderr)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_verify_npm_installation_with_which() {
        // This test assumes `node` is installed (which is likely if npm is installed)
        // Note: This is a brittle test - depends on system state
        let result = verify_npm_installation("node", true).await;
        // We can't assert it's Some because CI might not have node
        // Just ensure it doesn't panic
        eprintln!("node path: {:?}", result);
    }

    #[test]
    fn test_release_age_cutoff_format() {
        let cutoff = release_age_cutoff();
        // ISO-8601 UTC, e.g. 2026-08-01T15:45:00Z
        assert_eq!(cutoff.len(), 20);
        assert!(cutoff.ends_with('Z'));
        assert_eq!(&cutoff[4..5], "-");
        assert_eq!(&cutoff[10..11], "T");
    }

    #[test]
    fn test_npm_package_base_name() {
        assert_eq!(npm_package_base_name("typescript"), "typescript");
        assert_eq!(npm_package_base_name("typescript@6"), "typescript");
        assert_eq!(
            npm_package_base_name("@astrojs/language-server"),
            "@astrojs/language-server"
        );
        assert_eq!(
            npm_package_base_name("@astrojs/language-server@2.16"),
            "@astrojs/language-server"
        );
    }

    // Real npm install into the managed sandbox; network + filesystem side
    // effects, so ignored by default. Run manually with:
    //   cargo test -p ovim sandbox_npm_install -- --ignored
    #[tokio::test]
    #[ignore]
    async fn sandbox_npm_install_links_binaries() {
        let result = install_via_npm(
            "Astro",
            &[
                "@astrojs/language-server".to_string(),
                "typescript@6".to_string(),
            ],
            "astro-ls",
            false,
        )
        .await;
        let InstallResult::Success(path) = result else {
            panic!("sandboxed npm install failed: {:?}", result);
        };
        // The resolved executable must live inside the sandbox, not in a
        // global prefix. No bin-dir linking: .bin entries may be shims that
        // resolve paths relative to $0 and only work at their real location.
        let sandbox =
            crate::language_config::managed_lsp_package_dir("npm", "@astrojs/language-server")
                .unwrap();
        assert!(
            path.starts_with(sandbox.join("node_modules/.bin")),
            "unexpected path: {:?}",
            path
        );
        // Command resolution must find the same executable by bare name.
        let resolved = crate::language_config::find_in_well_known_locations("astro-ls")
            .expect("resolution should find sandboxed astro-ls");
        assert_eq!(PathBuf::from(resolved), path);
        // typescript@6 must be present in the same sandbox so tsdk
        // resolution finds it next to the server.
        assert!(sandbox
            .join("node_modules/typescript/lib/tsserverlibrary.js")
            .exists());
        // npm layout, not a leftover pnpm one.
        assert!(sandbox.join("package-lock.json").exists());
        assert!(!sandbox.join("pnpm-workspace.yaml").exists());
    }

    #[test]
    fn test_install_result_display() {
        let success = InstallResult::Success(PathBuf::from("/usr/bin/test"));
        assert!(matches!(success, InstallResult::Success(_)));

        let failed = InstallResult::Failed("test error".to_string());
        assert!(matches!(failed, InstallResult::Failed(_)));
    }

    #[test]
    fn test_asset_matches_pattern() {
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
    fn test_verify_shell_installation_empty_binary() {
        // An empty verify name must never resolve to a path, otherwise a shell
        // install with no known binary would report a bogus success.
        assert!(verify_shell_installation("").is_none());
    }

    #[test]
    fn test_verify_shell_installation_resolves_path_binary() {
        // `sh` is on PATH on every unix system this runs on; verification must
        // resolve it so a successful shell install isn't reported as missing.
        #[cfg(unix)]
        {
            let resolved = verify_shell_installation("sh");
            assert!(resolved.is_some(), "expected to resolve `sh` via PATH");
        }
    }

    #[test]
    fn test_verify_shell_installation_missing_binary_is_none() {
        // A binary that exists nowhere must return None (→ actionable failure),
        // not a placeholder success that would re-trigger the install prompt.
        assert!(verify_shell_installation("ovim-definitely-not-a-real-binary-xyz").is_none());
    }
}
