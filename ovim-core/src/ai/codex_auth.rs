//! Ovim-owned ChatGPT OAuth credentials for direct Codex inference.
//!
//! These credentials deliberately have their own refresh-token lineage. Never
//! import or refresh `~/.codex/auth.json`: refresh tokens rotate, so sharing a
//! lineage with the Codex CLI makes one client eventually invalidate the other.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use rand::RngCore;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use url::Url;

const AUTH_SCHEMA_VERSION: u8 = 2;
const AUTH_ORIGIN: &str = "ovim_pkce";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CALLBACK_PORT: u16 = 1455;
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const REFRESH_MARGIN_SECONDS: u64 = 60;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredCredentials {
    #[serde(default)]
    schema_version: u8,
    #[serde(default)]
    auth_origin: String,
    pub(crate) access_token: String,
    refresh_token: String,
    pub(crate) account_id: String,
    #[serde(default)]
    expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) enum CredentialReadiness {
    Ready,
    RefreshRequired,
    LoginRequired(String),
}

pub(crate) struct LoginAttempt {
    pub(crate) authorize_url: String,
    pub(crate) receiver: oneshot::Receiver<Result<()>>,
    pub(crate) task: tokio::task::JoinHandle<()>,
}

#[cfg(not(test))]
pub(crate) fn credential_readiness() -> CredentialReadiness {
    match read_credentials() {
        Ok(credentials)
            if credentials.expires_at > now().saturating_add(REFRESH_MARGIN_SECONDS) =>
        {
            CredentialReadiness::Ready
        }
        Ok(_) => CredentialReadiness::RefreshRequired,
        Err(error) => CredentialReadiness::LoginRequired(error.to_string()),
    }
}

#[cfg(test)]
pub(crate) fn credential_readiness() -> CredentialReadiness {
    // Editor unit tests must never discover the developer's real credentials.
    CredentialReadiness::LoginRequired("test credential store is isolated".to_string())
}

pub(crate) async fn load_credentials(client: &Client) -> Result<StoredCredentials> {
    let credentials = read_credentials()?;
    if credentials.expires_at > now().saturating_add(REFRESH_MARGIN_SECONDS) {
        return Ok(credentials);
    }
    refresh_serialized(client, None).await
}

/// Refresh after an inference 401. If another process already replaced the
/// rejected access token while this process waited for the lock, use that
/// newer credential instead of rotating the lineage a second time.
pub(crate) async fn refresh_after_unauthorized(
    client: &Client,
    rejected_access_token: &str,
) -> Result<StoredCredentials> {
    refresh_serialized(client, Some(rejected_access_token)).await
}

pub(crate) async fn refresh_for_ui() -> Result<()> {
    let client = Client::builder()
        .build()
        .context("failed to create Codex login HTTP client")?;
    refresh_serialized(&client, None).await?;
    Ok(())
}

pub(crate) fn begin_login() -> Result<LoginAttempt> {
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).with_context(|| {
            format!(
            "cannot listen on localhost:{CALLBACK_PORT}; another login or process may be using it"
        )
        })?;
    listener
        .set_nonblocking(true)
        .context("failed to configure the Codex login callback")?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .context("failed to start the Codex login callback")?;

    let verifier = random_urlsafe(32);
    let state = random_urlsafe(32);
    let authorize_url = build_authorize_url(&verifier, &state)?;
    let (tx, receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let result = tokio::time::timeout(LOGIN_TIMEOUT, complete_login(listener, verifier, state))
            .await
            .map_err(|_| anyhow!("sign-in timed out; press Enter to try again"))
            .and_then(|result| result);
        let _ = tx.send(result);
    });
    Ok(LoginAttempt {
        authorize_url,
        receiver,
        task,
    })
}

fn build_authorize_url(verifier: &str, state: &str) -> Result<String> {
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url = Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "ovim");
    Ok(url.into())
}

async fn complete_login(
    listener: tokio::net::TcpListener,
    verifier: String,
    expected_state: String,
) -> Result<()> {
    let (mut socket, _) = listener
        .accept()
        .await
        .context("failed to receive the browser callback")?;
    let mut request = vec![0_u8; 16 * 1024];
    let count = socket
        .read(&mut request)
        .await
        .context("failed to read the browser callback")?;
    let callback = parse_callback_request(&request[..count], &expected_state);
    let result = match callback {
        Ok(code) => exchange_code(&code, &verifier).await,
        Err(error) => Err(error),
    };
    let (status, page) = match &result {
        Ok(()) => (
            "200 OK",
            "<!doctype html><meta charset=\"utf-8\"><title>Ovim sign-in complete</title>\
             <style>body{font:16px system-ui;max-width:36rem;margin:12vh auto;padding:2rem;\
             color:#e7ebf2;background:#11151c}h1{color:#8ed7a1}</style>\
             <h1>Signed in to Ovim</h1><p>You can close this tab and return to Ovim.</p>",
        ),
        Err(_) => (
            "400 Bad Request",
            "<!doctype html><meta charset=\"utf-8\"><title>Ovim sign-in failed</title>\
             <style>body{font:16px system-ui;max-width:36rem;margin:12vh auto;padding:2rem;\
             color:#e7ebf2;background:#11151c}h1{color:#f0b432}</style>\
             <h1>Sign-in was not completed</h1><p>Return to Ovim for details and try again.</p>",
        ),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
    result
}

fn parse_callback_request(request: &[u8], expected_state: &str) -> Result<String> {
    let text = std::str::from_utf8(request).context("browser callback was not valid HTTP")?;
    let target = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| anyhow!("browser callback did not include a request target"))?;
    let url = Url::parse(&format!("http://localhost{target}"))
        .context("browser callback URL was invalid")?;
    if url.path() != "/auth/callback" {
        anyhow::bail!("unexpected browser callback path");
    }
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        let detail = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or(error);
        anyhow::bail!("OpenAI sign-in was not completed: {detail}");
    }
    if params.get("state").map(String::as_str) != Some(expected_state) {
        anyhow::bail!("OpenAI sign-in returned an invalid state; please try again");
    }
    params
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| anyhow!("OpenAI sign-in did not return an authorization code"))
}

async fn exchange_code(code: &str, verifier: &str) -> Result<()> {
    let client = Client::builder()
        .build()
        .context("failed to create Codex login HTTP client")?;
    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .context("failed to exchange the OpenAI sign-in code")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read the OpenAI sign-in response")?;
    let value: Value =
        serde_json::from_str(&body).context("OpenAI sign-in returned an invalid response")?;
    if !status.is_success() {
        anyhow::bail!(
            "OpenAI sign-in returned {status}: {}",
            oauth_error_detail(&value)
        );
    }
    let credentials = credentials_from_token_response(&value, None)?;
    let path = ovim_auth_path()?;
    let _lock = AuthFileLock::acquire(path.with_extension("lock")).await?;
    write_credentials(&path, &credentials)
}

async fn refresh_serialized(
    client: &Client,
    rejected_access_token: Option<&str>,
) -> Result<StoredCredentials> {
    let path = ovim_auth_path()?;
    let _lock = AuthFileLock::acquire(path.with_extension("lock")).await?;
    let current = read_credentials_from(&path)?;
    if rejected_access_token.is_some_and(|rejected| current.access_token != rejected) {
        return Ok(current);
    }
    if rejected_access_token.is_none()
        && current.expires_at > now().saturating_add(REFRESH_MARGIN_SECONDS)
    {
        return Ok(current);
    }

    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", current.refresh_token.as_str()),
            ("client_id", CLIENT_ID),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .context("failed to refresh Ovim's Codex sign-in")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read the Codex refresh response")?;
    let value: Value =
        serde_json::from_str(&body).context("Codex refresh returned an invalid response")?;
    if !status.is_success() {
        let detail = oauth_error_detail(&value);
        if status.is_client_error() {
            let mut invalidated = current.clone();
            invalidated.auth_origin = "ovim_pkce_reauth_required".to_string();
            write_credentials(&path, &invalidated)?;
        }
        let recovery = if detail.contains("refresh_token_reused") {
            "Ovim's refresh token was already used. Sign in to Ovim again."
        } else {
            "Sign in to Ovim again."
        };
        anyhow::bail!("Codex sign-in refresh returned {status}: {detail}. {recovery}");
    }
    let credentials = credentials_from_token_response(&value, Some(&current))?;
    write_credentials(&path, &credentials)?;
    Ok(credentials)
}

fn credentials_from_token_response(
    value: &Value,
    previous: Option<&StoredCredentials>,
) -> Result<StoredCredentials> {
    let access_token = required_string(value, "access_token")?;
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| previous.map(|credentials| credentials.refresh_token.clone()))
        .ok_or_else(|| anyhow!("OpenAI sign-in omitted refresh_token"))?;
    let account_id = value
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(jwt_account_id)
        .or_else(|| jwt_account_id(&access_token))
        .or_else(|| previous.map(|credentials| credentials.account_id.clone()))
        .ok_or_else(|| anyhow!("OpenAI sign-in did not identify a ChatGPT workspace"))?;
    let expires_at = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|seconds| now().saturating_add(seconds))
        .or_else(|| jwt_expiry(&access_token))
        .unwrap_or_default();
    Ok(StoredCredentials {
        schema_version: AUTH_SCHEMA_VERSION,
        auth_origin: AUTH_ORIGIN.to_string(),
        access_token,
        refresh_token,
        account_id,
        expires_at,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("OpenAI sign-in omitted {field}"))
}

fn oauth_error_detail(value: &Value) -> String {
    value
        .get("error_description")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("unknown OAuth error")
        .to_string()
}

fn ovim_auth_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .ok_or_else(|| anyhow!("cannot locate Ovim's configuration directory"))?
        .join("ovim/codex-auth.json"))
}

fn read_credentials() -> Result<StoredCredentials> {
    read_credentials_from(&ovim_auth_path()?)
}

fn read_credentials_from(path: &Path) -> Result<StoredCredentials> {
    let bytes = std::fs::read(path).with_context(|| {
        format!(
            "Ovim is not signed in to Codex (credential file not found at {})",
            path.display()
        )
    })?;
    parse_credentials(&bytes)
}

fn parse_credentials(bytes: &[u8]) -> Result<StoredCredentials> {
    let mut credentials: StoredCredentials =
        serde_json::from_slice(bytes).context("Ovim's Codex credentials are invalid")?;
    if credentials.auth_origin == "ovim_pkce_reauth_required" {
        anyhow::bail!("Ovim's Codex sign-in needs to be renewed");
    }
    if credentials.schema_version != AUTH_SCHEMA_VERSION || credentials.auth_origin != AUTH_ORIGIN {
        anyhow::bail!(
            "Ovim's legacy Codex credentials cannot be reused safely; sign in to Ovim once"
        );
    }
    if credentials.access_token.is_empty()
        || credentials.refresh_token.is_empty()
        || credentials.account_id.is_empty()
    {
        anyhow::bail!("Ovim's Codex credentials are incomplete; sign in again");
    }
    if credentials.expires_at == 0 {
        credentials.expires_at = jwt_expiry(&credentials.access_token).unwrap_or_default();
    }
    Ok(credentials)
}

fn write_credentials(path: &Path, credentials: &StoredCredentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(credentials)?;
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

struct AuthFileLock(PathBuf);

impl AuthFileLock {
    async fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > 60);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error).context("failed to lock Ovim Codex credentials"),
            }
        }
        Err(anyhow!(
            "timed out waiting for another Ovim process to update Codex credentials"
        ))
    }
}

impl Drop for AuthFileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn jwt_expiry(token: &str) -> Option<u64> {
    jwt_claims(token)?.get("exp")?.as_u64()
}

fn jwt_account_id(token: &str) -> Option<String> {
    let claims = jwt_claims(token)?;
    claims
        .get("chatgpt_account_id")
        .or_else(|| claims.get("https://api.openai.com/auth/chatgpt_account_id"))
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_uses_pkce_and_ovim_callback() {
        let url = Url::parse(&build_authorize_url("verifier", "state-value").unwrap()).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.as_str().split('?').next(), Some(AUTHORIZE_URL));
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some(REDIRECT_URI)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(params.get("state").map(String::as_str), Some("state-value"));
        assert!(params
            .get("scope")
            .is_some_and(|scope| scope.contains("offline_access")));
        assert_ne!(
            params.get("code_challenge").map(String::as_str),
            Some("verifier")
        );
    }

    #[test]
    fn callback_requires_matching_state() {
        let request = b"GET /auth/callback?code=abc&state=expected HTTP/1.1\r\n\r\n";
        assert_eq!(parse_callback_request(request, "expected").unwrap(), "abc");
        assert!(parse_callback_request(request, "different")
            .unwrap_err()
            .to_string()
            .contains("invalid state"));
    }

    #[test]
    fn legacy_credentials_are_rejected_instead_of_imported() {
        let legacy = br#"{"access_token":"a","refresh_token":"r","account_id":"acct"}"#;
        assert!(parse_credentials(legacy)
            .unwrap_err()
            .to_string()
            .contains("legacy"));
    }

    #[test]
    fn rejected_refresh_requires_a_fresh_ovim_login() {
        let invalidated = br#"{
            "schema_version":2,
            "auth_origin":"ovim_pkce_reauth_required",
            "access_token":"a",
            "refresh_token":"r",
            "account_id":"acct",
            "expires_at":1
        }"#;
        assert!(parse_credentials(invalidated)
            .unwrap_err()
            .to_string()
            .contains("needs to be renewed"));
    }

    #[test]
    fn token_claim_extracts_chatgpt_account() {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"},"exp":1234}"#,
        );
        let token = format!("header.{claims}.signature");
        assert_eq!(jwt_account_id(&token).as_deref(), Some("acct_123"));
        assert_eq!(jwt_expiry(&token), Some(1234));
    }
}
