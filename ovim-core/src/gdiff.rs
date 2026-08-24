use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

const INSTANCE_DIR: &str = "/tmp/gdiff/instances";
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_API_BYTES: usize = 1024 * 1024;
const MAX_INSTANCES: usize = 512;
const MAX_FILES: usize = 20_000;
const MAX_COMMENTS: usize = 20_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_COMMENT_BYTES: usize = 64 * 1024;
const MAX_SPEC_BYTES: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GdiffReview {
    pub installed: bool,
    pub running: bool,
    #[serde(skip_serializing)]
    pub pid: Option<u32>,
    #[serde(skip_serializing)]
    pub port: Option<u16>,
    pub repo: String,
    pub spec: String,
    pub display_spec: String,
    pub files: Vec<String>,
    pub comments: Vec<GdiffComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GdiffComment {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryEntry {
    pid: u32,
    port: u16,
    repo: PathBuf,
    #[serde(default)]
    started_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiInfo {
    pid: u32,
    repo: PathBuf,
    #[serde(default)]
    spec: String,
    #[serde(default)]
    display_spec: String,
    #[serde(default)]
    files: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CommentResponse {
    #[serde(default)]
    comments: Vec<GdiffComment>,
}

fn client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
        .context("failed to initialize gdiff client")
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn repo_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
        .to_string()
}

/// Resolve a path inside a non-bare Git worktree to that worktree's root.
pub fn worktree_root(start: &Path) -> Result<PathBuf> {
    let probe = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };
    let repository = git2::Repository::discover(probe)
        .with_context(|| format!("{} is not inside a Git worktree", start.display()))?;
    let root = repository
        .workdir()
        .ok_or_else(|| anyhow!("Gdiff collaboration does not support bare repositories"))?;
    Ok(canonical(root))
}

fn entries_for_repo_in(instance_dir: &Path, repo: &Path) -> Vec<RegistryEntry> {
    let expected = canonical(repo);
    let Ok(entries) = fs::read_dir(instance_dir) else {
        return Vec::new();
    };
    let mut matches = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .take(MAX_INSTANCES)
        .filter(|entry| {
            entry
                .metadata()
                .is_ok_and(|metadata| metadata.len() <= MAX_REGISTRY_BYTES)
        })
        .filter_map(|entry| {
            let mut bytes = Vec::new();
            fs::File::open(entry.path())
                .ok()?
                .take(MAX_REGISTRY_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() as u64 <= MAX_REGISTRY_BYTES).then_some(bytes)
        })
        .filter_map(|bytes| serde_json::from_slice::<RegistryEntry>(&bytes).ok())
        .filter(|entry| entry.pid > 0 && entry.repo.to_string_lossy().len() <= MAX_PATH_BYTES)
        .filter(|entry| canonical(&entry.repo) == expected)
        .collect::<Vec<_>>();
    matches.sort_by_key(|entry| std::cmp::Reverse(entry.started_at));
    matches
}

fn entries_for_repo(repo: &Path) -> Vec<RegistryEntry> {
    entries_for_repo_in(Path::new(INSTANCE_DIR), repo)
}

fn get_info(client: &Client, entry: &RegistryEntry, repo: &Path) -> Result<ApiInfo> {
    let url = format!("http://127.0.0.1:{}/api/info", entry.port);
    let response = client
        .get(url)
        .send()
        .context("gdiff instance did not respond")?
        .error_for_status()
        .context("gdiff rejected the info request")?;
    let response: ApiInfo = decode_response(response, "instance metadata")?;
    validate_info(&response)?;
    if response.pid != entry.pid || canonical(&response.repo) != canonical(repo) {
        bail!("gdiff instance metadata does not match its registry entry");
    }
    Ok(response)
}

fn decode_response<T: serde::de::DeserializeOwned>(
    mut response: reqwest::blocking::Response,
    label: &str,
) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_API_BYTES as u64)
    {
        bail!("gdiff {label} exceeds the {} byte limit", MAX_API_BYTES);
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_API_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read gdiff {label}"))?;
    if bytes.len() > MAX_API_BYTES {
        bail!("gdiff {label} exceeds the {} byte limit", MAX_API_BYTES);
    }
    serde_json::from_slice(&bytes).with_context(|| format!("gdiff returned invalid {label}"))
}

fn valid_review_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return false;
    }
    let path = Path::new(path);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn validate_info(info: &ApiInfo) -> Result<()> {
    if info.pid == 0
        || info.repo.to_string_lossy().len() > MAX_PATH_BYTES
        || info.spec.len() > MAX_SPEC_BYTES
        || info.display_spec.len() > MAX_SPEC_BYTES
    {
        bail!("gdiff comparison metadata is too large");
    }
    let files = info.files.as_deref().unwrap_or_default();
    if files.len() > MAX_FILES {
        bail!("gdiff review has too many changed files");
    }
    if files.iter().any(|path| !valid_review_path(path)) {
        bail!("gdiff returned an invalid changed-file path");
    }
    Ok(())
}

fn validate_comments(comments: &[GdiffComment], files: &[String]) -> Result<()> {
    if comments.len() > MAX_COMMENTS {
        bail!("gdiff review has too many comments");
    }
    let changed_files = files
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    if comments.iter().any(|comment| {
        comment.line == 0
            || comment.line > u32::MAX.into()
            || !valid_review_path(&comment.path)
            || !changed_files.contains(comment.path.as_str())
            || comment.text.trim().is_empty()
            || comment.text.len() > MAX_COMMENT_BYTES
    }) {
        bail!("gdiff returned an invalid review comment");
    }
    Ok(())
}

pub fn review(repo: &Path) -> Result<GdiffReview> {
    let installed = which::which("gdiff").is_ok();
    let repo = canonical(repo);
    let http = client()?;
    for entry in entries_for_repo(&repo) {
        let Ok(info) = get_info(&http, &entry, &repo) else {
            continue;
        };
        let comments_url = format!("http://127.0.0.1:{}/api/comments", entry.port);
        let files = info.files.clone().unwrap_or_default();
        let comments = if info.files.is_some() {
            let response = http
                .get(comments_url)
                .send()
                .context("failed to read gdiff comments")?
                .error_for_status()
                .context("gdiff rejected the comments request")?;
            decode_response::<CommentResponse>(response, "comments")?.comments
        } else {
            Vec::new()
        };
        validate_comments(&comments, &files)?;
        return Ok(GdiffReview {
            installed: true,
            running: true,
            pid: Some(info.pid),
            port: Some(entry.port),
            repo: repo_label(&repo),
            spec: info.spec,
            display_spec: info.display_spec,
            files,
            comments,
        });
    }
    Ok(GdiffReview {
        installed,
        repo: repo_label(&repo),
        ..GdiffReview::default()
    })
}

pub fn start(repo: &Path) -> Result<()> {
    let repo = worktree_root(repo)?;
    let executable = which::which("gdiff").context("gdiff is not installed or not on PATH")?;
    let mut child = Command::new(executable)
        .arg("start")
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch gdiff")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn comment_request(
    repo: &Path,
    method: reqwest::Method,
    path: &str,
    line: u64,
    text: Option<&str>,
) -> Result<Vec<GdiffComment>> {
    if !valid_review_path(path) || line == 0 || line > u32::MAX.into() {
        bail!("gdiff comments require a changed file and a one-based line number");
    }
    let state = review(repo)?;
    let port = state
        .port
        .ok_or_else(|| anyhow!("no gdiff review is running for this workspace"))?;
    if !state.files.iter().any(|file| file == path) {
        bail!("path is not in the active gdiff comparison: {path}");
    }
    let body = match text {
        Some(text) if !text.trim().is_empty() && text.len() <= MAX_COMMENT_BYTES => {
            serde_json::json!({ "path": path, "line": line, "text": text.trim() })
        }
        Some(_) => {
            bail!("gdiff comment text must be non-empty and at most {MAX_COMMENT_BYTES} bytes")
        }
        None => serde_json::json!({ "path": path, "line": line }),
    };
    let response = client()?
        .request(method, format!("http://127.0.0.1:{port}/api/comments"))
        .json(&body)
        .send()
        .context("failed to update the gdiff review")?
        .error_for_status()
        .context("gdiff rejected the comment update")?;
    let response: CommentResponse = decode_response(response, "comments")?;
    validate_comments(&response.comments, &state.files)?;
    Ok(response.comments)
}

pub fn add_comment(repo: &Path, path: &str, line: u64, text: &str) -> Result<Vec<GdiffComment>> {
    comment_request(repo, reqwest::Method::POST, path, line, Some(text))
}

pub fn remove_comment(repo: &Path, path: &str, line: u64) -> Result<Vec<GdiffComment>> {
    comment_request(repo, reqwest::Method::DELETE, path, line, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn review_projection_uses_gui_field_names() {
        let value = serde_json::to_value(GdiffReview {
            installed: true,
            running: true,
            pid: Some(42),
            port: Some(32123),
            repo: "project".into(),
            display_spec: "main...WORKTREE".into(),
            ..GdiffReview::default()
        })
        .unwrap();
        assert_eq!(value["displaySpec"], "main...WORKTREE");
        assert_eq!(value["repo"], "project");
        assert!(value.get("display_spec").is_none());
        assert!(value.get("pid").is_none());
        assert!(value.get("port").is_none());
        assert_eq!(repo_label(Path::new("/work/project")), "project");
    }

    #[test]
    fn registry_entry_matches_gdiff_protocol() {
        let entry: RegistryEntry = serde_json::from_value(serde_json::json!({
            "pid": 42,
            "port": 32123,
            "repo": "/work/project",
            "startedAt": 99
        }))
        .unwrap();
        assert_eq!(entry.started_at, 99);
        assert_eq!(entry.repo, PathBuf::from("/work/project"));
    }

    #[test]
    fn registry_discovery_is_scoped_to_the_canonical_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let instances = temp.path().join("instances");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&instances).unwrap();
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(
            instances.join("1.json"),
            serde_json::to_vec(&serde_json::json!({
                "pid": 1, "port": 31001, "repo": first, "startedAt": 10
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            instances.join("2.json"),
            serde_json::to_vec(&serde_json::json!({
                "pid": 2, "port": 31002, "repo": second, "startedAt": 20
            }))
            .unwrap(),
        )
        .unwrap();

        let matches = entries_for_repo_in(&instances, &first);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pid, 1);
    }

    #[test]
    fn worktree_root_resolves_nested_workspace_paths() {
        let temp = tempfile::tempdir().unwrap();
        git2::Repository::init(temp.path()).unwrap();
        let nested = temp.path().join("src/editor");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(worktree_root(&nested).unwrap(), canonical(temp.path()));
    }

    #[test]
    fn loopback_info_must_match_the_registry_identity() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let impostor = temp.path().join("impostor");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&impostor).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let impostor_json = serde_json::to_string(&impostor).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body =
                format!("{{\"pid\":7,\"repo\":{impostor_json},\"spec\":\"main\",\"files\":[]}}");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let entry = RegistryEntry {
            pid: 7,
            port,
            repo: repo.clone(),
            started_at: 1,
        };
        let error = get_info(&client().unwrap(), &entry, &repo).unwrap_err();
        assert!(error.to_string().contains("does not match"));
        server.join().unwrap();
    }

    #[test]
    fn review_paths_and_comments_are_bounded_and_workspace_relative() {
        assert!(valid_review_path("src/main.rs"));
        assert!(!valid_review_path("/etc/passwd"));
        assert!(!valid_review_path("../other/review.rs"));
        assert!(!valid_review_path(&"a".repeat(MAX_PATH_BYTES + 1)));

        let files = vec!["src/main.rs".to_string()];
        assert!(validate_comments(
            &[GdiffComment {
                path: "src/main.rs".into(),
                line: 7,
                text: "Please revisit this branch".into(),
            }],
            &files,
        )
        .is_ok());
        assert!(validate_comments(
            &[GdiffComment {
                path: "src/not-in-review.rs".into(),
                line: 7,
                text: "Ambiguous target".into(),
            }],
            &files,
        )
        .is_err());
        assert!(validate_comments(
            &[GdiffComment {
                path: "src/main.rs".into(),
                line: 0,
                text: "Invalid line".into(),
            }],
            &files,
        )
        .is_err());
    }

    #[test]
    fn api_response_size_is_rejected_before_deserialization() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_API_BYTES + 1
            )
            .unwrap();
        });
        let response = client()
            .unwrap()
            .get(format!("http://127.0.0.1:{port}/api/info"))
            .send()
            .unwrap();
        let error = decode_response::<ApiInfo>(response, "instance metadata").unwrap_err();
        assert!(error.to_string().contains("exceeds"));
        server.join().unwrap();
    }
}
