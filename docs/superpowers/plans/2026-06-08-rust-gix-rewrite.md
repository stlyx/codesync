# Rust gix Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Python/git-subprocess CodeSync service with a Rust binary that uses `git2` / libgit2 for Git operations, keeps the current `config.json` shape parseable, and supports Linux + Windows with HTTPS environment-variable credentials.

**Architecture:** Build a small Rust application with focused modules: config parsing, credentials, locking, webhook authentication, HTTP serving, sync orchestration, and a `git2` backend. Keep sync orchestration independent of the concrete backend so tests can cover conflict behavior without a network server, then wire in the real `git2` backend. Preserve the current config file shape, but return clear config errors for unsupported SSH and credential-helper settings.

**Tech Stack:** Rust 2024-compatible binary crate, `git2`, `serde`, `serde_json`, `clap`, `tiny_http`, `fs4`, `hmac`, `sha2`, `subtle`, `thiserror`, `tracing`, `tracing-subscriber`, `tempfile` for tests.

---

## File Structure

Create and modify these files:

- Create: `Cargo.toml` — package metadata, runtime dependencies, dev dependencies.
- Create: `src/lib.rs` — module exports used by unit and integration tests.
- Create: `src/main.rs` — CLI entry point and application mode selection.
- Create: `src/error.rs` — `CodeSyncError`, `Result`, and HTTP status mapping.
- Create: `src/config.rs` — existing JSON shape parsing, validation, credential merge rules.
- Create: `src/credentials.rs` — environment variable credential resolution and URL redaction helpers.
- Create: `src/webhook.rs` — token and HMAC webhook authentication.
- Create: `src/lock.rs` — cross-platform file lock wrapper using `fs4`.
- Create: `src/sync.rs` — backend trait, sync orchestration, fast-forward target selection.
- Create: `src/git_backend.rs` — `git2` backend adapter for repository operations.
- Create: `src/http.rs` — blocking HTTP server using `tiny_http`.
- Modify: `.gitignore` — add Rust build artifacts.
- Modify: `README.md` — replace Python runtime instructions with Rust binary/Cargo instructions and document supported credentials.
- Modify: `config.example.json` — keep shape unchanged; adjust comments are not possible in JSON, so leave content unchanged unless tests require exact sample values.
- Remove or retire: `codesync_server.py` — final service implementation should not be Python. If kept temporarily for reference during development, remove it before final verification.

Do not call the `git` binary from Rust code or tests. Do not add Python files, Python fixtures, or Python scripts.

---

### Task 1: Scaffold the Rust crate

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Modify: `.gitignore`

- [ ] **Step 1: Write the manifest**

Create `Cargo.toml` with this content:

```toml
[package]
name = "codesync"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"

[dependencies]
clap = { version = "4.6.1", features = ["derive", "env"] }
fs4 = "1.1.0"
gix = { version = "0.84.0", features = ["blocking-network-client", "blocking-http-transport-reqwest-rust-tls"] }
hmac = "0.13.0"
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0"
sha2 = "0.11.0"
subtle = "2.6.1"
thiserror = "2.0.18"
tiny_http = "0.12.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

[dev-dependencies]
tempfile = "3.27.0"
```

- [ ] **Step 2: Create module exports**

Create `src/lib.rs` with this content:

```rust
pub mod config;
pub mod credentials;
pub mod error;
pub mod git_backend;
pub mod http;
pub mod lock;
pub mod sync;
pub mod webhook;
```

- [ ] **Step 3: Create a compiling entry point**

Create `src/main.rs` with this initial content:

```rust
fn main() {
    println!("codesync rust rewrite scaffold");
}
```

- [ ] **Step 4: Ignore Rust build output**

Append these lines to `.gitignore`:

```gitignore
target/
Cargo.lock
```

Then remove `Cargo.lock` from `.gitignore` before final verification because application crates should commit lockfiles. This temporary ignore prevents noisy intermediate changes while the crate is being scaffolded.

- [ ] **Step 5: Run the scaffold build**

Run:

```bash
cargo test
```

Expected: compilation succeeds with zero tests run or all tests passing.

- [ ] **Step 6: Checkpoint**

Do not commit unless the user explicitly asks for commits. If commits are authorized later, use:

```bash
git add Cargo.toml src/lib.rs src/main.rs .gitignore
git commit -m "chore: scaffold rust crate"
```

---

### Task 2: Add typed errors and configuration parsing

**Files:**
- Create: `src/error.rs`
- Create: `src/config.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write failing config tests first**

Create `src/config.rs` with the tests below first. The module will not compile until Step 3 adds the implementation.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn base_json() -> &'static str {
        r#"{
          "listen": { "host": "0.0.0.0", "port": 8080 },
          "webhook": { "path": "/webhook", "secret_env": "CODESYNC_WEBHOOK_SECRET", "max_body_bytes": 1048576 },
          "repo_dir": "/var/lib/codesync/repo.git",
          "state_dir": "/var/lib/codesync",
          "branch": "master",
          "git": { "timeout_seconds": 300 },
          "credential": {
            "username_env": "CODESYNC_GIT_USERNAME",
            "password_env": "CODESYNC_GIT_PASSWORD",
            "helper": "",
            "ssh_command_env": "",
            "use_http_path": true
          },
          "remotes": [
            { "name": "repo_a", "url": "https://example.com/group/project-a.git" },
            { "name": "repo_b", "url": "https://example.com/group/project-b.git" }
          ]
        }"#
    }

    #[test]
    fn parses_existing_config_shape() {
        let config = AppConfig::from_json_str(base_json()).expect("valid config");
        assert_eq!(config.listen_host, "0.0.0.0");
        assert_eq!(config.listen_port, 8080);
        assert_eq!(config.webhook.path, "/webhook");
        assert_eq!(config.branch, "master");
        assert_eq!(config.remotes.len(), 2);
        assert_eq!(config.remotes[0].name, "repo_a");
        assert_eq!(config.remotes[1].credential.username_env.as_deref(), Some("CODESYNC_GIT_USERNAME"));
    }

    #[test]
    fn rejects_credential_helper() {
        let text = base_json().replace("\"helper\": \"\"", "\"helper\": \"store --file /tmp/creds\"");
        let err = AppConfig::from_json_str(&text).unwrap_err().to_string();
        assert!(err.contains("credential.helper is not supported"), "{err}");
    }

    #[test]
    fn rejects_ssh_command_env() {
        let text = base_json().replace("\"ssh_command_env\": \"\"", "\"ssh_command_env\": \"CODESYNC_GIT_SSH_COMMAND\"");
        let err = AppConfig::from_json_str(&text).unwrap_err().to_string();
        assert!(err.contains("ssh_command_env is not supported"), "{err}");
    }

    #[test]
    fn rejects_non_https_remote_urls() {
        let text = base_json().replace("https://example.com/group/project-a.git", "git@example.com:group/project-a.git");
        let err = AppConfig::from_json_str(&text).unwrap_err().to_string();
        assert!(err.contains("only https remote URLs are supported"), "{err}");
    }

    #[test]
    fn rejects_partial_username_password_config() {
        let text = base_json().replace("\"password_env\": \"CODESYNC_GIT_PASSWORD\"", "\"password_env\": \"\"");
        let err = AppConfig::from_json_str(&text).unwrap_err().to_string();
        assert!(err.contains("credential.username_env and credential.password_env must be set together"), "{err}");
    }

    #[test]
    fn remote_credential_overrides_default() {
        let text = base_json().replace(
            "{ \"name\": \"repo_b\", \"url\": \"https://example.com/group/project-b.git\" }",
            "{ \"name\": \"repo_b\", \"url\": \"https://example.com/group/project-b.git\", \"credential\": { \"username_env\": \"B_USER\", \"password_env\": \"B_PASS\" } }",
        );
        let config = AppConfig::from_json_str(&text).expect("valid config");
        assert_eq!(config.remotes[1].credential.username_env.as_deref(), Some("B_USER"));
        assert_eq!(config.remotes[1].credential.password_env.as_deref(), Some("B_PASS"));
    }
}
```

- [ ] **Step 2: Run the failing tests**

Run:

```bash
cargo test config::tests -- --nocapture
```

Expected: FAIL because `AppConfig` and related types are not defined.

- [ ] **Step 3: Implement typed errors**

Create `src/error.rs` with this content:

```rust
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CodeSyncError>;

#[derive(Debug, thiserror::Error)]
pub enum CodeSyncError {
    #[error("config error: {0}")]
    Config(String),
    #[error("unsupported configuration: {0}")]
    Unsupported(String),
    #[error("credential error: {0}")]
    Credential(String),
    #[error("sync conflict: {0}")]
    Conflict(String),
    #[error("git backend error: {0}")]
    GitBackend(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl CodeSyncError {
    pub fn http_status(&self) -> u16 {
        match self {
            CodeSyncError::Conflict(_) => 409,
            CodeSyncError::Http(message) if message == "unauthorized" => 401,
            CodeSyncError::Http(message) if message == "payload_too_large" => 413,
            _ => 500,
        }
    }
}

pub fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> CodeSyncError {
    CodeSyncError::Io { path: path.into(), source }
}
```

- [ ] **Step 4: Implement config parsing**

Replace `src/config.rs` with complete implementation plus the tests from Step 1 at the bottom:

```rust
use crate::error::{CodeSyncError, Result, io_error};
use serde::Deserialize;
use std::{collections::HashSet, fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialConfig {
    pub username_env: Option<String>,
    pub password_env: Option<String>,
    pub use_http_path: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub name: String,
    pub url: String,
    pub credential: CredentialConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookConfig {
    pub path: String,
    pub secret: Option<String>,
    pub secret_env: Option<String>,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub listen_host: String,
    pub listen_port: u16,
    pub repo_dir: PathBuf,
    pub state_dir: PathBuf,
    pub branch: String,
    pub remotes: Vec<RemoteConfig>,
    pub webhook: WebhookConfig,
    pub git_timeout_seconds: u64,
}

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    listen: Option<RawListen>,
    listen_host: Option<String>,
    listen_port: Option<u16>,
    webhook: Option<RawWebhook>,
    webhook_path: Option<String>,
    repo_dir: Option<String>,
    state_dir: Option<String>,
    branch: Option<String>,
    git: Option<RawGit>,
    git_timeout_seconds: Option<u64>,
    credential: Option<RawCredential>,
    remotes: Option<Vec<RawRemote>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawListen { host: Option<String>, port: Option<u16> }

#[derive(Debug, Deserialize, Default)]
struct RawWebhook {
    path: Option<String>,
    secret: Option<String>,
    secret_env: Option<String>,
    max_body_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct RawCredential {
    username_env: Option<String>,
    password_env: Option<String>,
    helper: Option<String>,
    ssh_command_env: Option<String>,
    use_http_path: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct RawGit { timeout_seconds: Option<u64> }

#[derive(Debug, Deserialize, Default)]
struct RawRemote {
    name: Option<String>,
    url: Option<String>,
    credential: Option<RawCredential>,
    username_env: Option<String>,
    password_env: Option<String>,
    helper: Option<String>,
    ssh_command_env: Option<String>,
    use_http_path: Option<bool>,
}

impl AppConfig {
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|err| io_error(path, err))?;
        Self::from_json_str(&text)
    }

    pub fn from_json_str(text: &str) -> Result<Self> {
        let raw: RawConfig = serde_json::from_str(text)?;
        let listen = raw.listen.unwrap_or_default();
        let webhook = raw.webhook.unwrap_or_default();
        let credential = raw.credential.unwrap_or_default();

        let repo_dir = raw.repo_dir
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| CodeSyncError::Config("repo_dir is required".to_string()))?;
        let state_dir = raw.state_dir.map(PathBuf::from).unwrap_or_else(|| {
            repo_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
        });
        let branch = raw.branch.unwrap_or_else(|| "master".to_string());
        validate_branch_name(&branch)?;

        let raw_remotes = raw.remotes.ok_or_else(|| CodeSyncError::Config("remotes must contain exactly two remote objects".to_string()))?;
        if raw_remotes.len() != 2 {
            return Err(CodeSyncError::Config("remotes must contain exactly two remote objects".to_string()));
        }

        let mut seen = HashSet::new();
        let mut remotes = Vec::with_capacity(2);
        for (index, remote) in raw_remotes.into_iter().enumerate() {
            let name = remote.name.unwrap_or_default().trim().to_string();
            let url = remote.url.unwrap_or_default().trim().to_string();
            if name.is_empty() || url.is_empty() {
                return Err(CodeSyncError::Config(format!("remotes[{index}] requires name and url")));
            }
            validate_remote_name(&name)?;
            if !seen.insert(name.clone()) {
                return Err(CodeSyncError::Config(format!("duplicate remote name: {name}")));
            }
            validate_https_url(&url)?;
            let merged_credential = merge_credential(&credential, &remote)?;
            remotes.push(RemoteConfig { name, url, credential: merged_credential });
        }

        let webhook_path = raw.webhook_path.or(webhook.path).unwrap_or_else(|| "/webhook".to_string());
        if !webhook_path.starts_with('/') {
            return Err(CodeSyncError::Config("webhook.path must start with '/'".to_string()));
        }

        Ok(Self {
            listen_host: raw.listen_host.or(listen.host).unwrap_or_else(|| "0.0.0.0".to_string()),
            listen_port: raw.listen_port.or(listen.port).unwrap_or(8080),
            repo_dir,
            state_dir,
            branch,
            remotes,
            webhook: WebhookConfig {
                path: webhook_path,
                secret: webhook.secret.filter(|value| !value.is_empty()),
                secret_env: webhook.secret_env.filter(|value| !value.is_empty()),
                max_body_bytes: webhook.max_body_bytes.unwrap_or(1024 * 1024),
            },
            git_timeout_seconds: raw.git_timeout_seconds.or(raw.git.and_then(|git| git.timeout_seconds)).unwrap_or(300),
        })
    }
}

fn merge_credential(defaults: &RawCredential, remote: &RawRemote) -> Result<CredentialConfig> {
    let remote_nested = remote.credential.clone().unwrap_or_default();
    let helper = pick(remote.helper.as_ref(), remote_nested.helper.as_ref(), defaults.helper.as_ref());
    if helper.is_some_and(|value| !value.trim().is_empty()) {
        return Err(CodeSyncError::Unsupported("credential.helper is not supported by the Rust/gix rewrite".to_string()));
    }

    let ssh_command_env = pick(remote.ssh_command_env.as_ref(), remote_nested.ssh_command_env.as_ref(), defaults.ssh_command_env.as_ref());
    if ssh_command_env.is_some_and(|value| !value.trim().is_empty()) {
        return Err(CodeSyncError::Unsupported("credential.ssh_command_env is not supported by the Rust/gix rewrite".to_string()));
    }

    let username_env = pick(remote.username_env.as_ref(), remote_nested.username_env.as_ref(), defaults.username_env.as_ref())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    let password_env = pick(remote.password_env.as_ref(), remote_nested.password_env.as_ref(), defaults.password_env.as_ref())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);

    if username_env.is_some() != password_env.is_some() {
        return Err(CodeSyncError::Config("credential.username_env and credential.password_env must be set together".to_string()));
    }

    Ok(CredentialConfig {
        username_env,
        password_env,
        use_http_path: remote.use_http_path.or(remote_nested.use_http_path).or(defaults.use_http_path).unwrap_or(true),
    })
}

fn pick<'a>(first: Option<&'a String>, second: Option<&'a String>, third: Option<&'a String>) -> Option<&'a String> {
    first.or(second).or(third)
}

fn validate_https_url(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(CodeSyncError::Unsupported(format!("only https remote URLs are supported: {url}")))
    }
}

fn validate_remote_name(name: &str) -> Result<()> {
    if !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
        Ok(())
    } else {
        Err(CodeSyncError::Config(format!("remote name {name:?} is invalid; use letters, numbers, '.', '_' or '-'")))
    }
}

fn validate_branch_name(branch: &str) -> Result<()> {
    if branch.is_empty() || branch.starts_with('/') || branch.ends_with('/') || matches!(branch, "." | ".." | "@{" | "HEAD") {
        return Err(CodeSyncError::Config(format!("branch {branch:?} is invalid")));
    }
    let forbidden = ["..", "\\", " ", "~", "^", ":", "?", "*", "[", "//"];
    if forbidden.iter().any(|part| branch.contains(part)) {
        return Err(CodeSyncError::Config(format!("branch {branch:?} is invalid")));
    }
    Ok(())
}

// Keep the tests from Step 1 here unchanged.
```

- [ ] **Step 5: Run config tests**

Run:

```bash
cargo test config::tests -- --nocapture
```

Expected: all config tests PASS.

- [ ] **Step 6: Checkpoint**

If commits are authorized later:

```bash
git add src/error.rs src/config.rs src/lib.rs
git commit -m "feat: parse codesync config in rust"
```

---

### Task 3: Add credentials, URL redaction, and webhook verification

**Files:**
- Create: `src/credentials.rs`
- Create: `src/webhook.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write failing tests for credentials and webhook auth**

Create `src/credentials.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CredentialConfig;

    #[test]
    fn redacts_url_userinfo() {
        assert_eq!(redact_url("https://user:secret@example.com/a.git"), "https://***@example.com/a.git");
        assert_eq!(redact_url("https://example.com/a.git"), "https://example.com/a.git");
    }

    #[test]
    fn resolves_env_credentials() {
        unsafe {
            std::env::set_var("CODESYNC_TEST_USER", "alice");
            std::env::set_var("CODESYNC_TEST_PASS", "token");
        }
        let cfg = CredentialConfig {
            username_env: Some("CODESYNC_TEST_USER".to_string()),
            password_env: Some("CODESYNC_TEST_PASS".to_string()),
            use_http_path: true,
        };
        let resolved = ResolvedCredentials::from_config(&cfg).expect("credentials");
        assert_eq!(resolved.username, "alice");
        assert_eq!(resolved.password, "token");
    }

    #[test]
    fn missing_env_is_error() {
        unsafe {
            std::env::remove_var("CODESYNC_TEST_MISSING_USER");
            std::env::remove_var("CODESYNC_TEST_MISSING_PASS");
        }
        let cfg = CredentialConfig {
            username_env: Some("CODESYNC_TEST_MISSING_USER".to_string()),
            password_env: Some("CODESYNC_TEST_MISSING_PASS".to_string()),
            use_http_path: true,
        };
        let err = ResolvedCredentials::from_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("credential environment variables are missing"), "{err}");
    }
}
```

Create `src/webhook.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn headers(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn allows_when_no_secret() {
        assert!(verify_webhook_secret(None, &headers(&[]), b"body"));
    }

    #[test]
    fn accepts_token_header() {
        assert!(verify_webhook_secret(Some("secret"), &headers(&[("X-CodeSync-Token", "secret")]), b"body"));
    }

    #[test]
    fn accepts_bearer_header() {
        assert!(verify_webhook_secret(Some("secret"), &headers(&[("Authorization", "Bearer secret")]), b"body"));
    }

    #[test]
    fn accepts_github_hmac_header() {
        let signature = signature_for_test("secret", b"body");
        assert!(verify_webhook_secret(Some("secret"), &headers(&[("X-Hub-Signature-256", &signature)]), b"body"));
    }

    #[test]
    fn rejects_wrong_secret() {
        assert!(!verify_webhook_secret(Some("secret"), &headers(&[("X-CodeSync-Token", "wrong")]), b"body"));
    }
}
```

- [ ] **Step 2: Run failing tests**

Run:

```bash
cargo test credentials::tests webhook::tests -- --nocapture
```

Expected: FAIL because the production functions are not defined.

- [ ] **Step 3: Implement credentials**

Replace `src/credentials.rs` with this implementation and keep the tests from Step 1 at the bottom:

```rust
use crate::{config::CredentialConfig, error::{CodeSyncError, Result}};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCredentials {
    pub username: String,
    pub password: String,
}

impl ResolvedCredentials {
    pub fn from_config(config: &CredentialConfig) -> Result<Option<Self>> {
        match (&config.username_env, &config.password_env) {
            (Some(username_env), Some(password_env)) => {
                let username = std::env::var(username_env).ok();
                let password = std::env::var(password_env).ok();
                match (username, password) {
                    (Some(username), Some(password)) => Ok(Some(Self { username, password })),
                    _ => Err(CodeSyncError::Credential(format!(
                        "credential environment variables are missing: {username_env}, {password_env}"
                    ))),
                }
            }
            (None, None) => Ok(None),
            _ => Err(CodeSyncError::Credential(
                "credential.username_env and credential.password_env must be set together".to_string(),
            )),
        }
    }
}

pub fn redact_url(value: &str) -> String {
    if let Some(scheme_end) = value.find("://") {
        let authority_start = scheme_end + 3;
        if let Some(at_offset) = value[authority_start..].find('@') {
            let at_index = authority_start + at_offset;
            let mut out = String::with_capacity(value.len());
            out.push_str(&value[..authority_start]);
            out.push_str("***");
            out.push_str(&value[at_index..]);
            return out;
        }
    }
    value.to_string()
}

// Keep the tests from Step 1 here, but update the successful call to unwrap the Option:
// let resolved = ResolvedCredentials::from_config(&cfg).expect("credentials").expect("configured");
```

- [ ] **Step 4: Implement webhook verification**

Replace `src/webhook.rs` with this implementation and keep the tests from Step 1 at the bottom:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub fn verify_webhook_secret(secret: Option<&str>, headers: &HashMap<String, String>, body: &[u8]) -> bool {
    let Some(secret) = secret else { return true; };

    if let Some(token) = get_header(headers, "X-CodeSync-Token") {
        if constant_time_eq(token.as_bytes(), secret.as_bytes()) {
            return true;
        }
    }

    if let Some(auth) = get_header(headers, "Authorization") {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if constant_time_eq(token.as_bytes(), secret.as_bytes()) {
                return true;
            }
        }
    }

    if let Some(signature) = get_header(headers, "X-Hub-Signature-256") {
        if signature.starts_with("sha256=") {
            return constant_time_eq(signature.as_bytes(), signature_for(secret, body).as_bytes());
        }
    }

    false
}

fn get_header<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .or_else(|| headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value))
        .map(String::as_str)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.ct_eq(right).into()
}

pub fn signature_for(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    let digest = mac.finalize().into_bytes();
    format!("sha256={digest:x}")
}

#[cfg(test)]
fn signature_for_test(secret: &str, body: &[u8]) -> String {
    signature_for(secret, body)
}
```

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test credentials::tests webhook::tests -- --nocapture
```

Expected: all credentials and webhook tests PASS.

- [ ] **Step 6: Checkpoint**

If commits are authorized later:

```bash
git add src/credentials.rs src/webhook.rs src/lib.rs
git commit -m "feat: verify credentials and webhooks"
```

---

### Task 4: Add lock and sync orchestration with a fake backend

**Files:**
- Create: `src/lock.rs`
- Create: `src/sync.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write failing tests for fast-forward selection and fake sync**

Create `src/sync.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, CredentialConfig, RemoteConfig, WebhookConfig};
    use std::{collections::{HashMap, HashSet}, path::PathBuf};

    #[derive(Default)]
    struct FakeBackend {
        local_tip: Option<String>,
        remote_tips: HashMap<String, String>,
        ancestors: HashSet<(String, String)>,
        pushed: Vec<(String, String)>,
    }

    impl GitBackend for FakeBackend {
        fn ensure_repo(&mut self, _config: &AppConfig) -> crate::error::Result<()> { Ok(()) }
        fn fetch_remote(&mut self, remote: &RemoteConfig, _branch: &str) -> crate::error::Result<String> {
            Ok(self.remote_tips[&remote.name].clone())
        }
        fn local_branch_tip(&self, _branch: &str) -> crate::error::Result<Option<String>> { Ok(self.local_tip.clone()) }
        fn is_ancestor(&self, older: &str, newer: &str) -> crate::error::Result<bool> {
            Ok(older == newer || self.ancestors.contains(&(older.to_string(), newer.to_string())))
        }
        fn update_local_branch(&mut self, _branch: &str, target: &str) -> crate::error::Result<()> {
            self.local_tip = Some(target.to_string());
            Ok(())
        }
        fn push_remote(&mut self, remote: &RemoteConfig, branch: &str, target: &str) -> crate::error::Result<()> {
            self.pushed.push((remote.name.clone(), format!("{branch}:{target}")));
            Ok(())
        }
        fn push_tags(&mut self, remote: &RemoteConfig) -> crate::error::Result<()> {
            self.pushed.push((remote.name.clone(), "tags".to_string()));
            Ok(())
        }
    }

    fn config() -> AppConfig {
        let credential = CredentialConfig { username_env: None, password_env: None, use_http_path: true };
        AppConfig {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            repo_dir: PathBuf::from("repo.git"),
            state_dir: PathBuf::from("state"),
            branch: "master".to_string(),
            remotes: vec![
                RemoteConfig { name: "repo_a".to_string(), url: "https://example.com/a.git".to_string(), credential: credential.clone() },
                RemoteConfig { name: "repo_b".to_string(), url: "https://example.com/b.git".to_string(), credential },
            ],
            webhook: WebhookConfig { path: "/webhook".to_string(), secret: None, secret_env: None, max_body_bytes: 1024 },
            git_timeout_seconds: 300,
        }
    }

    #[test]
    fn selects_tip_that_all_others_can_fast_forward_to() {
        let backend = FakeBackend {
            ancestors: HashSet::from([("a".to_string(), "b".to_string()), ("local".to_string(), "b".to_string())]),
            ..FakeBackend::default()
        };
        let target = select_fast_forward_target(&backend, &[("local", "local"), ("repo_a", "a"), ("repo_b", "b")]).expect("target");
        assert_eq!(target, "b");
    }

    #[test]
    fn rejects_divergent_histories() {
        let backend = FakeBackend::default();
        let err = select_fast_forward_target(&backend, &[("repo_a", "a"), ("repo_b", "b")]).unwrap_err().to_string();
        assert!(err.contains("divergent histories"), "{err}");
    }

    #[test]
    fn sync_fetches_updates_and_pushes_branch_then_tags() {
        let cfg = config();
        let mut backend = FakeBackend {
            local_tip: Some("a".to_string()),
            remote_tips: HashMap::from([("repo_a".to_string(), "a".to_string()), ("repo_b".to_string(), "b".to_string())]),
            ancestors: HashSet::from([("a".to_string(), "b".to_string())]),
            pushed: Vec::new(),
        };
        let result = sync_once(&cfg, &mut backend, "test").expect("sync ok");
        assert_eq!(result.status, "ok");
        assert_eq!(result.target, "b");
        assert_eq!(backend.local_tip.as_deref(), Some("b"));
        assert_eq!(backend.pushed, vec![
            ("repo_a".to_string(), "master:b".to_string()),
            ("repo_a".to_string(), "tags".to_string()),
            ("repo_b".to_string(), "master:b".to_string()),
            ("repo_b".to_string(), "tags".to_string()),
        ]);
    }
}
```

- [ ] **Step 2: Run failing tests**

Run:

```bash
cargo test sync::tests -- --nocapture
```

Expected: FAIL because `GitBackend`, `sync_once`, `select_fast_forward_target`, and `SyncResult` are not defined.

- [ ] **Step 3: Implement sync orchestration**

Replace `src/sync.rs` with this implementation and keep tests from Step 1 at the bottom:

```rust
use crate::{config::{AppConfig, RemoteConfig}, error::{CodeSyncError, Result}};
use serde::Serialize;
use std::time::Instant;

pub trait GitBackend {
    fn ensure_repo(&mut self, config: &AppConfig) -> Result<()>;
    fn fetch_remote(&mut self, remote: &RemoteConfig, branch: &str) -> Result<String>;
    fn local_branch_tip(&self, branch: &str) -> Result<Option<String>>;
    fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool>;
    fn update_local_branch(&mut self, branch: &str, target: &str) -> Result<()>;
    fn push_remote(&mut self, remote: &RemoteConfig, branch: &str, target: &str) -> Result<()>;
    fn push_tags(&mut self, remote: &RemoteConfig) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncResult {
    pub id: String,
    pub status: String,
    pub branch: String,
    pub target: String,
    pub elapsed_ms: u128,
    pub remotes: Vec<String>,
}

pub fn sync_once(config: &AppConfig, backend: &mut impl GitBackend, reason: &str) -> Result<SyncResult> {
    let started = Instant::now();
    tracing::info!(reason, "sync started");
    backend.ensure_repo(config)?;

    let mut tips: Vec<(String, String)> = Vec::new();
    if let Some(local_tip) = backend.local_branch_tip(&config.branch)? {
        tips.push(("local".to_string(), local_tip));
    }

    for remote in &config.remotes {
        let tip = backend.fetch_remote(remote, &config.branch)?;
        tips.push((remote.name.clone(), tip));
    }

    let labeled: Vec<(&str, &str)> = tips.iter().map(|(label, tip)| (label.as_str(), tip.as_str())).collect();
    let target = select_fast_forward_target(backend, &labeled)?;
    backend.update_local_branch(&config.branch, &target)?;

    for remote in &config.remotes {
        backend.push_remote(remote, &config.branch, &target)?;
        backend.push_tags(remote)?;
    }

    Ok(SyncResult {
        id: sync_id(),
        status: "ok".to_string(),
        branch: config.branch.clone(),
        target,
        elapsed_ms: started.elapsed().as_millis(),
        remotes: config.remotes.iter().map(|remote| remote.name.clone()).collect(),
    })
}

pub fn select_fast_forward_target(backend: &impl GitBackend, labeled_tips: &[(&str, &str)]) -> Result<String> {
    let mut unique: Vec<&str> = Vec::new();
    for (_, tip) in labeled_tips {
        if !unique.contains(tip) {
            unique.push(tip);
        }
    }

    for candidate in &unique {
        let mut all_reachable = true;
        for other in &unique {
            if other != candidate && !backend.is_ancestor(other, candidate)? {
                all_reachable = false;
                break;
            }
        }
        if all_reachable {
            return Ok((*candidate).to_string());
        }
    }

    let detail = labeled_tips
        .iter()
        .map(|(label, tip)| format!("{label}={}", short_id(tip)))
        .collect::<Vec<_>>()
        .join(", ");
    Err(CodeSyncError::Conflict(format!("branch has divergent histories; refusing non-fast-forward sync: {detail}")))
}

fn short_id(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

fn sync_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    format!("{nanos:x}").chars().take(12).collect()
}
```

- [ ] **Step 4: Add file lock wrapper**

Create `src/lock.rs` with this implementation:

```rust
use crate::error::{Result, io_error};
use fs4::fs_std::FileExt;
use std::{fs::{self, File, OpenOptions}, path::{Path, PathBuf}};

pub struct FileLock {
    file: File,
    path: PathBuf,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .map_err(|err| io_error(path, err))?;
        file.lock_exclusive().map_err(|err| io_error(path, err))?;
        Ok(Self { file, path: path.to_path_buf() })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Err(err) = self.file.unlock() {
            tracing::warn!(path = %self.path.display(), error = %err, "failed to unlock file");
        }
    }
}
```

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test sync::tests -- --nocapture
```

Expected: all sync tests PASS.

- [ ] **Step 6: Checkpoint**

If commits are authorized later:

```bash
git add src/sync.rs src/lock.rs src/lib.rs
git commit -m "feat: orchestrate fast-forward sync"
```

---

### Task 5: Implement the real `gix` backend

**Files:**
- Create: `src/git_backend.rs`
- Modify: `src/sync.rs` only if trait signatures need a small adjustment discovered during compile.

- [ ] **Step 1: Write backend unit tests that do not need network**

Create `src/git_backend.rs` with these tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_ref_names_match_existing_service() {
        assert_eq!(branch_ref("master"), "refs/heads/master");
        assert_eq!(remote_branch_ref("repo_a", "master"), "refs/remotes/repo_a/master");
    }

    #[test]
    fn refspecs_match_existing_service() {
        assert_eq!(branch_fetch_refspec("repo_a", "master"), "+refs/heads/master:refs/remotes/repo_a/master");
        assert_eq!(branch_push_refspec("master"), "refs/heads/master:refs/heads/master");
        assert_eq!(tags_refspec(), "refs/tags/*:refs/tags/*");
    }
}
```

- [ ] **Step 2: Run failing backend tests**

Run:

```bash
cargo test git_backend::tests -- --nocapture
```

Expected: FAIL because helper functions and backend struct are undefined.

- [ ] **Step 3: Implement backend helpers and skeleton**

Add this production code above the tests in `src/git_backend.rs`:

```rust
use crate::{
    config::{AppConfig, RemoteConfig},
    credentials::ResolvedCredentials,
    error::{CodeSyncError, Result, io_error},
    sync::GitBackend,
};
use std::{fs, path::Path};

pub struct GixBackend {
    repo: Option<gix::Repository>,
}

impl GixBackend {
    pub fn new() -> Self {
        Self { repo: None }
    }

    fn repo(&self) -> Result<&gix::Repository> {
        self.repo.as_ref().ok_or_else(|| CodeSyncError::GitBackend("repository is not initialized".to_string()))
    }

    fn repo_mut(&mut self) -> Result<&mut gix::Repository> {
        self.repo.as_mut().ok_or_else(|| CodeSyncError::GitBackend("repository is not initialized".to_string()))
    }
}

impl Default for GixBackend {
    fn default() -> Self { Self::new() }
}

pub fn branch_ref(branch: &str) -> String { format!("refs/heads/{branch}") }
pub fn remote_branch_ref(remote: &str, branch: &str) -> String { format!("refs/remotes/{remote}/{branch}") }
pub fn branch_fetch_refspec(remote: &str, branch: &str) -> String { format!("+refs/heads/{branch}:{}", remote_branch_ref(remote, branch)) }
pub fn branch_push_refspec(branch: &str) -> String { format!("refs/heads/{branch}:refs/heads/{branch}") }
pub fn tags_refspec() -> &'static str { "refs/tags/*:refs/tags/*" }
```

- [ ] **Step 4: Implement repository initialization/opening**

Add this `GitBackend` implementation block. The exact `gix` calls may need compile-guided adjustment, but keep behavior and signatures unchanged:

```rust
impl GitBackend for GixBackend {
    fn ensure_repo(&mut self, config: &AppConfig) -> Result<()> {
        if self.repo.is_some() {
            return Ok(());
        }
        if let Some(parent) = config.repo_dir.parent() {
            fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        }
        let repo = if config.repo_dir.join("HEAD").exists() {
            gix::open(&config.repo_dir).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?
        } else {
            gix::init_bare(&config.repo_dir).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?
        };
        self.repo = Some(repo);
        Ok(())
    }

    fn fetch_remote(&mut self, remote: &RemoteConfig, branch: &str) -> Result<String> {
        fetch_remote_impl(self.repo_mut()?, remote, branch)
    }

    fn local_branch_tip(&self, branch: &str) -> Result<Option<String>> {
        let repo = self.repo()?;
        let name = branch_ref(branch);
        match repo.find_reference(name.as_str()) {
            Ok(mut reference) => Ok(Some(reference.peel_to_commit().map_err(|err| CodeSyncError::GitBackend(err.to_string()))?.id.to_string())),
            Err(_) => Ok(None),
        }
    }

    fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool> {
        let repo = self.repo()?;
        let older = gix::ObjectId::from_hex(older.as_bytes()).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
        let newer = gix::ObjectId::from_hex(newer.as_bytes()).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
        let base = repo.merge_base(older, newer).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
        Ok(base == older)
    }

    fn update_local_branch(&mut self, branch: &str, target: &str) -> Result<()> {
        update_ref_impl(self.repo_mut()?, &branch_ref(branch), target)
    }

    fn push_remote(&mut self, remote: &RemoteConfig, branch: &str, _target: &str) -> Result<()> {
        push_impl(self.repo_mut()?, remote, &[branch_push_refspec(branch)])
    }

    fn push_tags(&mut self, remote: &RemoteConfig) -> Result<()> {
        push_impl(self.repo_mut()?, remote, &[tags_refspec().to_string()])
    }
}
```

- [ ] **Step 5: Implement fetch/push/ref update helpers with `gix` only**

Add helper functions below the impl. Use this shape, then compile-adjust exact gix API names against `gix 0.84` docs/source:

```rust
fn fetch_remote_impl(repo: &mut gix::Repository, remote: &RemoteConfig, branch: &str) -> Result<String> {
    let mut remote_handle = repo
        .remote_at_without_url_rewrite(remote.url.as_str())
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?
        .with_refspecs([branch_fetch_refspec(&remote.name, branch).as_str()], gix::remote::Direction::Fetch)
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    remote_handle
        .replace_refspecs([tags_refspec()], gix::remote::Direction::Fetch)
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;

    let credentials = ResolvedCredentials::from_config(&remote.credential)?;
    let mut connection = remote_handle
        .connect(gix::remote::Direction::Fetch)
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    if let Some(credentials) = credentials {
        connection.set_credentials(move |action| credentials_for_action(action, &credentials));
    }
    connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?
        .receive(gix::progress::Discard, &std::sync::atomic::AtomicBool::new(false))
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;

    let remote_ref = remote_branch_ref(&remote.name, branch);
    let mut reference = repo.find_reference(remote_ref.as_str()).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    Ok(reference.peel_to_commit().map_err(|err| CodeSyncError::GitBackend(err.to_string()))?.id.to_string())
}

fn push_impl(repo: &mut gix::Repository, remote: &RemoteConfig, refspecs: &[String]) -> Result<()> {
    let remote_handle = repo
        .remote_at_without_url_rewrite(remote.url.as_str())
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?
        .with_refspecs(refspecs.iter().map(String::as_str), gix::remote::Direction::Push)
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    let credentials = ResolvedCredentials::from_config(&remote.credential)?;
    let mut connection = remote_handle
        .connect(gix::remote::Direction::Push)
        .map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    if let Some(credentials) = credentials {
        connection.set_credentials(move |action| credentials_for_action(action, &credentials));
    }
    // Use the gix push preparation/receive-pack API for 0.84 here. Keep non-force refspecs.
    // The finished implementation must return an error if the remote rejects a non-fast-forward branch or tag update.
    push_via_gix_connection(connection, refspecs)
}

fn update_ref_impl(repo: &mut gix::Repository, ref_name: &str, target: &str) -> Result<()> {
    let target = gix::ObjectId::from_hex(target.as_bytes()).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    let edit = gix::refs::transaction::RefEdit {
        change: gix::refs::transaction::Change::Update {
            log: gix::refs::transaction::LogChange::AndReference(gix::refs::transaction::RefLog::message("codesync fast-forward".into())),
            expected: gix::refs::transaction::PreviousValue::Any,
            new: gix::refs::Target::Object(target),
        },
        name: ref_name.try_into().map_err(|err| CodeSyncError::GitBackend(format!("invalid ref name {ref_name}: {err}")))?,
        deref: false,
    };
    repo.edit_reference(edit).map_err(|err| CodeSyncError::GitBackend(err.to_string()))?;
    Ok(())
}
```

The comments in this step are not permission to leave placeholders in final code. They identify the only compile-sensitive area: `gix` push API naming. Resolve it during implementation by consulting the local `gix-0.84.0` source and remove comments that imply unfinished work.

- [ ] **Step 6: Run backend tests and compile whole crate**

Run:

```bash
cargo test git_backend::tests -- --nocapture
cargo test
```

Expected: backend helper tests PASS; whole crate compiles. If gix API signatures differ, adjust only `src/git_backend.rs` while preserving the `GitBackend` trait contract.

- [ ] **Step 7: Checkpoint**

If commits are authorized later:

```bash
git add src/git_backend.rs src/sync.rs Cargo.toml Cargo.lock
git commit -m "feat: add gix git backend"
```

---

### Task 6: Add blocking HTTP server and CLI wiring

**Files:**
- Create: `src/http.rs`
- Modify: `src/main.rs`
- Modify: `src/config.rs` if loading `webhook.secret_env` needs to resolve at config-load time.

- [ ] **Step 1: Write tests for secret env resolution behavior**

Add this test to `src/config.rs` tests:

```rust
#[test]
fn secret_env_is_resolved_when_config_loads() {
    unsafe { std::env::set_var("CODESYNC_TEST_SECRET", "webhook-secret"); }
    let text = base_json().replace("CODESYNC_WEBHOOK_SECRET", "CODESYNC_TEST_SECRET");
    let config = AppConfig::from_json_str(&text).expect("valid config");
    assert_eq!(config.webhook.secret.as_deref(), Some("webhook-secret"));
}
```

- [ ] **Step 2: Make the test fail, then implement secret env resolution**

Run:

```bash
cargo test config::tests::secret_env_is_resolved_when_config_loads -- --nocapture
```

Expected before implementation: FAIL because `secret_env` is only stored.

Update config construction so `WebhookConfig.secret` resolves `secret_env`:

```rust
let secret_env = webhook.secret_env.filter(|value| !value.is_empty());
let secret = if let Some(secret_env_name) = &secret_env {
    Some(std::env::var(secret_env_name).map_err(|_| CodeSyncError::Config(format!("webhook.secret_env {secret_env_name:?} is not set")))?)
} else {
    webhook.secret.filter(|value| !value.is_empty())
};
```

Then set `WebhookConfig { secret, secret_env, ... }`.

- [ ] **Step 3: Implement HTTP server**

Create `src/http.rs` with this content:

```rust
use crate::{
    config::AppConfig,
    error::{CodeSyncError, Result},
    git_backend::GixBackend,
    lock::FileLock,
    sync::sync_once,
    webhook::verify_webhook_secret,
};
use serde_json::json;
use std::{collections::HashMap, io::Read, sync::{Arc, Mutex}};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

pub fn serve(config: AppConfig) -> Result<()> {
    let address = format!("{}:{}", config.listen_host, config.listen_port);
    let server = Server::http(&address).map_err(|err| CodeSyncError::Http(err.to_string()))?;
    let shared = Arc::new(ServerState { config, backend: Mutex::new(GixBackend::new()) });
    tracing::info!(address, "listening");

    for request in server.incoming_requests() {
        let state = Arc::clone(&shared);
        handle_request(request, state);
    }
    Ok(())
}

struct ServerState {
    config: AppConfig,
    backend: Mutex<GixBackend>,
}

fn handle_request(mut request: Request, state: Arc<ServerState>) {
    let response = match route_request(&mut request, &state) {
        Ok(payload) => json_response(200, payload),
        Err(err) => json_response(err.http_status(), json!({ "status": status_text(err.http_status()), "error": err.to_string() })),
    };
    if let Err(err) = request.respond(response) {
        tracing::warn!(error = %err, "failed to write HTTP response");
    }
}

fn route_request(request: &mut Request, state: &ServerState) -> Result<serde_json::Value> {
    match (request.method(), request.url()) {
        (&Method::Get, "/healthz") => Ok(json!({ "status": "ok" })),
        (&Method::Post, path) if path == state.config.webhook.path => handle_webhook(request, state),
        _ => Err(CodeSyncError::Http("not_found".to_string())),
    }
}

fn handle_webhook(request: &mut Request, state: &ServerState) -> Result<serde_json::Value> {
    let content_length = request.headers().iter()
        .find(|header| header.field.equiv("Content-Length"))
        .and_then(|header| header.value.as_str().parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > state.config.webhook.max_body_bytes {
        return Err(CodeSyncError::Http("payload_too_large".to_string()));
    }

    let mut body = Vec::with_capacity(content_length);
    request.as_reader().take(state.config.webhook.max_body_bytes as u64 + 1).read_to_end(&mut body)
        .map_err(|err| CodeSyncError::Http(err.to_string()))?;
    if body.len() > state.config.webhook.max_body_bytes {
        return Err(CodeSyncError::Http("payload_too_large".to_string()));
    }

    let headers = request.headers().iter()
        .map(|header| (header.field.as_str().to_string(), header.value.as_str().to_string()))
        .collect::<HashMap<_, _>>();
    if !verify_webhook_secret(state.config.webhook.secret.as_deref(), &headers, &body) {
        return Err(CodeSyncError::Http("unauthorized".to_string()));
    }

    let _file_lock = FileLock::acquire(&state.config.state_dir.join("sync.lock"))?;
    let mut backend = state.backend.lock().map_err(|_| CodeSyncError::Http("sync mutex poisoned".to_string()))?;
    let result = sync_once(&state.config, &mut *backend, "webhook")?;
    serde_json::to_value(result).map_err(CodeSyncError::from)
}

fn json_response(status: u16, payload: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&payload).unwrap_or_else(|_| b"{\"status\":\"error\"}".to_vec());
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).expect("static header is valid");
    Response::from_data(body).with_status_code(StatusCode(status)).with_header(header)
}

fn status_text(status: u16) -> &'static str {
    match status {
        401 => "unauthorized",
        404 => "not_found",
        409 => "conflict",
        413 => "payload_too_large",
        _ => "error",
    }
}
```

- [ ] **Step 4: Implement CLI wiring**

Replace `src/main.rs` with:

```rust
use clap::Parser;
use codesync::{config::AppConfig, error::Result, git_backend::GixBackend, http, lock::FileLock, sync::sync_once};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "codesync", about = "Fast-forward sync two Git repositories without invoking git")]
struct Args {
    #[arg(long, env = "CODESYNC_CONFIG", default_value = "config.json")]
    config: PathBuf,
    #[arg(long)]
    once: bool,
    #[arg(long, env = "CODESYNC_LOG_LEVEL", default_value = "info")]
    log_level: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    init_logging(&args.log_level);
    let config = AppConfig::from_path(&args.config)?;
    if args.once {
        let _file_lock = FileLock::acquire(&config.state_dir.join("sync.lock"))?;
        let mut backend = GixBackend::new();
        let result = sync_once(&config, &mut backend, "manual")?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    http::serve(config)
}

fn init_logging(level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(level).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
```

- [ ] **Step 5: Run tests and CLI help**

Run:

```bash
cargo test
cargo run -- --help
```

Expected: tests PASS; help output lists `--config`, `--once`, and `--log-level`.

- [ ] **Step 6: Checkpoint**

If commits are authorized later:

```bash
git add src/http.rs src/main.rs src/config.rs
git commit -m "feat: add webhook server and cli"
```

---

### Task 7: Update docs and remove Python service implementation

**Files:**
- Modify: `README.md`
- Modify: `.gitignore`
- Delete: `codesync_server.py`

- [ ] **Step 1: Update `.gitignore` for final Rust application state**

Ensure `.gitignore` contains `target/` and does not ignore `Cargo.lock`:

```gitignore
__pycache__/
*.py[cod]
config.json
data/
state/
.pytest_cache/
target/
```

Keeping Python cache ignores is harmless for users with old worktrees, but no Python code should remain in the service.

- [ ] **Step 2: Replace README runtime instructions**

Rewrite `README.md` to document:

```markdown
# CodeSync Webhook Server

CodeSync is a Rust webhook service that synchronizes two Git repositories without invoking the `git` binary. It uses the `gix` crate for Git operations.

## Supported scope

- exactly two HTTPS remotes
- one configured branch, default `master`
- all tags
- fast-forward-only branch convergence
- non-force tag synchronization
- Linux and Windows
- credentials from username/password/token environment variables

SSH remotes and Git credential helpers are not supported in this Rust rewrite yet. Existing `config.json` fields for those features remain parseable, but using them returns a clear configuration error.

## Configuration

Copy the example config:

```bash
cp config.example.json config.json
```

Set webhook and Git HTTPS credentials:

```bash
export CODESYNC_WEBHOOK_SECRET='webhook-secret'
export CODESYNC_GIT_USERNAME='git-user-or-token-name'
export CODESYNC_GIT_PASSWORD='token-or-password'
```

Windows PowerShell:

```powershell
$env:CODESYNC_WEBHOOK_SECRET = 'webhook-secret'
$env:CODESYNC_GIT_USERNAME = 'git-user-or-token-name'
$env:CODESYNC_GIT_PASSWORD = 'token-or-password'
```

## Build

```bash
cargo build --release
```

## Run once

```bash
cargo run --release -- --config config.json --once
```

Or with the built binary:

```bash
./target/release/codesync --config config.json --once
```

## Run webhook service

```bash
cargo run --release -- --config config.json
```

Health check:

```bash
curl http://127.0.0.1:8080/healthz
```

Trigger sync:

```bash
curl -X POST http://127.0.0.1:8080/webhook \
  -H "X-CodeSync-Token: $CODESYNC_WEBHOOK_SECRET" \
  -d '{}'
```

`Authorization: Bearer <secret>` and `X-Hub-Signature-256: sha256=<hmac>` are also accepted.

## Sync rules

The local repository uses a bare repo at `repo_dir`, defaulting in the example to `/var/lib/codesync/repo.git`. CodeSync maintains:

- `refs/remotes/<remote>/<branch>`
- `refs/heads/<branch>`
- `refs/tags/*`

The configured branch must converge by fast-forward. If the two remote branch tips and the local branch cannot all reach one common newest tip by ancestry, CodeSync returns a conflict and refuses to push.

Tags are never force-overwritten. If a remote has a tag with the same name pointing at a different object, sync fails rather than replacing it.
```

- [ ] **Step 3: Delete Python service**

Run:

```bash
rm /opt/codesync/codesync_server.py
```

Expected: `codesync_server.py` no longer exists. Do not add another Python file.

- [ ] **Step 4: Run docs-adjacent checks**

Run:

```bash
rg -n "python|git subprocess|git binary|codesync_server.py" README.md src Cargo.toml config.example.json
```

Expected: no stale instructions that tell users to run Python. Mentions of “without invoking git binary” are acceptable.

- [ ] **Step 5: Checkpoint**

If commits are authorized later:

```bash
git add README.md .gitignore Cargo.lock
git rm codesync_server.py
git commit -m "docs: document rust codesync service"
```

---

### Task 8: Final verification and cleanup

**Files:**
- Potentially modify any Rust file that fails formatting, clippy, or tests.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS. If it fails, run:

```bash
cargo fmt --all
```

Then repeat the `--check` command.

- [ ] **Step 2: Run all tests**

Run:

```bash
cargo test
```

Expected: PASS.

- [ ] **Step 3: Run clippy**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS. Fix warnings in the smallest relevant module.

- [ ] **Step 4: Build release binary**

Run:

```bash
cargo build --release
```

Expected: PASS and binary at `target/release/codesync` on Linux.

- [ ] **Step 5: Verify no runtime Python or git subprocess usage remains in source**

Run:

```bash
rg -n "std::process::Command|process::Command|Command::new|python|python3|subprocess|git-askpass|\bgit\b" src README.md Cargo.toml config.example.json
```

Expected: no Rust code that invokes `git` or Python. README may mention “without invoking the git binary” and unsupported Git credential helpers.

- [ ] **Step 6: Verify git status**

Run:

```bash
git status --short
```

Expected: only intentional Rust rewrite, docs, config, and deletion changes are present.

- [ ] **Step 7: Report completion honestly**

Report:

- design spec path;
- implementation plan path;
- tests run and their pass/fail status;
- any unsupported config fields documented;
- whether `codesync_server.py` was removed;
- any known limitations such as exact `git.timeout_seconds` parity if not implemented.

Do not say “complete” unless `cargo test`, `cargo clippy`, and `cargo build --release` pass.
