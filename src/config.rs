use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{CodeSyncError, Result};

const DEFAULT_LISTEN_HOST: &str = "0.0.0.0";
const DEFAULT_LISTEN_PORT: u16 = 8080;
const DEFAULT_BRANCH: &str = "master";
const DEFAULT_WEBHOOK_PATH: &str = "/webhook";
const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const DEFAULT_GIT_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialConfig {
    pub username_env: Option<String>,
    pub password_env: Option<String>,
    pub use_http_path: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub name: String,
    pub url: String,
    pub credential: CredentialConfig,
    pub role: Option<String>,
}

impl fmt::Debug for RemoteConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConfig")
            .field("name", &self.name)
            .field("url", &redact_url_userinfo(&self.url))
            .field("role", &self.role)
            .field("credential", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WebhookConfig {
    pub path: String,
    pub secret: Option<String>,
    pub secret_env: Option<String>,
    pub max_body_bytes: usize,
}

impl fmt::Debug for WebhookConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookConfig")
            .field("path", &self.path)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .field("secret_env", &self.secret_env)
            .field("max_body_bytes", &self.max_body_bytes)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
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

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("listen_host", &self.listen_host)
            .field("listen_port", &self.listen_port)
            .field("repo_dir", &self.repo_dir)
            .field("state_dir", &self.state_dir)
            .field("branch", &self.branch)
            .field("remotes", &self.remotes)
            .field("webhook", &self.webhook)
            .field("git_timeout_seconds", &self.git_timeout_seconds)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppConfig {
    listen: Option<RawListenConfig>,
    listen_host: Option<String>,
    listen_port: Option<u16>,
    repo_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    branch: Option<String>,
    git: Option<RawGitConfig>,
    git_timeout_seconds: Option<u64>,
    credential: Option<RawCredentialConfig>,
    remotes: Option<Vec<RawRemoteConfig>>,
    webhook: Option<RawWebhookConfig>,
    webhook_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawListenConfig {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGitConfig {
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum RawCredentialString {
    #[default]
    Missing,
    Null,
    Value(String),
}

impl RawCredentialString {
    fn is_present(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn into_normalized(self) -> Option<String> {
        match self {
            Self::Value(value) if !value.is_empty() => Some(value),
            Self::Missing | Self::Null | Self::Value(_) => None,
        }
    }
}

impl<'de> Deserialize<'de> for RawCredentialString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)
            .map(|value| value.map_or(Self::Null, Self::Value))
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawCredentialConfig {
    #[serde(default)]
    username_env: RawCredentialString,
    #[serde(default)]
    password_env: RawCredentialString,
    #[serde(default)]
    helper: RawCredentialString,
    #[serde(default)]
    ssh_command_env: RawCredentialString,
    use_http_path: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRemoteConfig {
    name: Option<String>,
    url: Option<String>,
    role: Option<String>,
    credential: Option<RawCredentialConfig>,
    #[serde(default)]
    username_env: RawCredentialString,
    #[serde(default)]
    password_env: RawCredentialString,
    #[serde(default)]
    helper: RawCredentialString,
    #[serde(default)]
    ssh_command_env: RawCredentialString,
    use_http_path: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWebhookConfig {
    path: Option<String>,
    secret: Option<String>,
    secret_env: Option<String>,
    max_body_bytes: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct MergedCredentialConfig {
    username_env: RawCredentialString,
    password_env: RawCredentialString,
    helper: RawCredentialString,
    ssh_command_env: RawCredentialString,
    use_http_path: Option<bool>,
}

impl AppConfig {
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|source| CodeSyncError::io_error(path.to_path_buf(), source))?;
        Self::from_json_str(&text)
    }

    pub fn from_json_str(text: &str) -> Result<Self> {
        let raw: RawAppConfig = serde_json::from_str(text)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawAppConfig) -> Result<Self> {
        let repo_dir = raw
            .repo_dir
            .ok_or_else(|| CodeSyncError::Config("repo_dir is required".to_string()))?;
        if repo_dir.as_os_str().to_string_lossy().trim().is_empty() {
            return Err(CodeSyncError::Config("repo_dir is required".to_string()));
        }
        let state_dir = raw
            .state_dir
            .unwrap_or_else(|| default_state_dir(&repo_dir));
        let listen_host = raw
            .listen_host
            .or_else(|| raw.listen.as_ref().and_then(|listen| listen.host.clone()))
            .unwrap_or_else(|| DEFAULT_LISTEN_HOST.to_string());
        let listen_port = raw
            .listen_port
            .or_else(|| raw.listen.as_ref().and_then(|listen| listen.port))
            .unwrap_or(DEFAULT_LISTEN_PORT);
        let branch = raw.branch.unwrap_or_else(|| DEFAULT_BRANCH.to_string());
        validate_branch(&branch)?;

        let webhook_path = raw
            .webhook_path
            .or_else(|| {
                raw.webhook
                    .as_ref()
                    .and_then(|webhook| webhook.path.clone())
            })
            .unwrap_or_else(|| DEFAULT_WEBHOOK_PATH.to_string());
        validate_webhook_path(&webhook_path)?;
        let webhook = WebhookConfig {
            path: webhook_path,
            secret: raw
                .webhook
                .as_ref()
                .and_then(|webhook| empty_to_none(webhook.secret.clone())),
            secret_env: raw
                .webhook
                .as_ref()
                .and_then(|webhook| empty_to_none(webhook.secret_env.clone())),
            max_body_bytes: raw
                .webhook
                .as_ref()
                .and_then(|webhook| webhook.max_body_bytes)
                .unwrap_or(DEFAULT_MAX_BODY_BYTES),
        };

        let git_timeout_seconds = raw
            .git_timeout_seconds
            .or_else(|| raw.git.as_ref().and_then(|git| git.timeout_seconds))
            .unwrap_or(DEFAULT_GIT_TIMEOUT_SECONDS);

        let raw_remotes = raw.remotes.ok_or_else(|| {
            CodeSyncError::Config("remotes must contain at least two remote objects".to_string())
        })?;
        if raw_remotes.len() < 2 {
            return Err(CodeSyncError::Config(
                "remotes must contain at least two remote objects".to_string(),
            ));
        }

        let global_credential = merged_from_raw(raw.credential.as_ref());
        let mut names = HashSet::new();
        let mut remotes = Vec::with_capacity(raw_remotes.len());
        for (index, remote) in raw_remotes.into_iter().enumerate() {
            let name = remote.name.clone().unwrap_or_default().trim().to_string();
            let url = remote.url.clone().unwrap_or_default().trim().to_string();
            if name.is_empty() || url.is_empty() {
                return Err(CodeSyncError::Config(format!(
                    "remotes[{index}] requires name and url"
                )));
            }
            validate_remote_name(&name)?;
            if !names.insert(name.clone()) {
                return Err(CodeSyncError::Config(format!(
                    "duplicate remote name: {}",
                    name
                )));
            }
            if !url.starts_with("https://") {
                return Err(CodeSyncError::Unsupported(
                    "only https remote URLs are supported".to_string(),
                ));
            }
            let role = validate_remote_role(remote.role.as_deref(), index)?;

            let credential =
                merge_credential(&global_credential, remote.credential.as_ref(), &remote)?;
            remotes.push(RemoteConfig {
                name,
                url,
                credential,
                role,
            });
        }

        Ok(Self {
            listen_host,
            listen_port,
            repo_dir,
            state_dir,
            branch,
            remotes,
            webhook,
            git_timeout_seconds,
        })
    }
}

fn redact_url_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map(|offset| authority_start + offset)
        .unwrap_or(url.len());
    let authority = &url[authority_start..authority_end];
    let Some(at_index) = authority.rfind('@') else {
        return url.to_string();
    };

    let mut redacted = String::with_capacity(url.len());
    redacted.push_str(&url[..authority_start]);
    redacted.push_str("***@");
    redacted.push_str(&authority[at_index + 1..]);
    redacted.push_str(&url[authority_end..]);
    redacted
}

fn default_state_dir(repo_dir: &Path) -> PathBuf {
    repo_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| if value.is_empty() { None } else { Some(value) })
}

fn validate_branch(branch: &str) -> Result<()> {
    if branch.is_empty()
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch == "."
        || branch == ".."
        || branch == "@{"
        || branch == "HEAD"
        || branch.contains("..")
        || branch.contains('\\')
        || branch.contains(' ')
        || branch.contains('~')
        || branch.contains('^')
        || branch.contains(':')
        || branch.contains('?')
        || branch.contains('*')
        || branch.contains('[')
        || branch.contains("//")
    {
        return Err(CodeSyncError::Config(format!("invalid branch: {branch}")));
    }
    Ok(())
}

fn validate_remote_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CodeSyncError::Config(format!(
            "invalid remote name: {name}"
        )));
    }
    Ok(())
}

fn validate_remote_role(role: Option<&str>, index: usize) -> Result<Option<String>> {
    let Some(role) = role else {
        return Ok(None);
    };
    let role = role.trim();
    if role.is_empty() {
        return Ok(None);
    }
    if role == "master" {
        return Ok(Some(role.to_string()));
    }
    Err(CodeSyncError::Config(format!(
        "remotes[{index}].role {role:?} is invalid; supported role is 'master'"
    )))
}

fn validate_webhook_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(CodeSyncError::Config(
            "webhook.path must start with '/'".to_string(),
        ));
    }
    Ok(())
}

fn merged_from_raw(raw: Option<&RawCredentialConfig>) -> MergedCredentialConfig {
    raw.map(|raw| MergedCredentialConfig {
        username_env: raw.username_env.clone(),
        password_env: raw.password_env.clone(),
        helper: raw.helper.clone(),
        ssh_command_env: raw.ssh_command_env.clone(),
        use_http_path: raw.use_http_path,
    })
    .unwrap_or_default()
}

fn merge_string(
    remote_top_level: RawCredentialString,
    remote_nested: RawCredentialString,
    global: RawCredentialString,
) -> Option<String> {
    let selected = if remote_nested.is_present() {
        remote_nested
    } else if remote_top_level.is_present() {
        remote_top_level
    } else {
        global
    };
    selected.into_normalized()
}

fn merge_bool(
    remote_top_level: Option<bool>,
    remote_nested: Option<bool>,
    global: Option<bool>,
) -> bool {
    remote_nested
        .or(remote_top_level)
        .or(global)
        .unwrap_or(true)
}

fn merge_credential(
    global: &MergedCredentialConfig,
    remote_nested: Option<&RawCredentialConfig>,
    remote: &RawRemoteConfig,
) -> Result<CredentialConfig> {
    let remote_nested = merged_from_raw(remote_nested);
    let username_env = merge_string(
        remote.username_env.clone(),
        remote_nested.username_env,
        global.username_env.clone(),
    );
    let password_env = merge_string(
        remote.password_env.clone(),
        remote_nested.password_env,
        global.password_env.clone(),
    );
    let helper = merge_string(
        remote.helper.clone(),
        remote_nested.helper,
        global.helper.clone(),
    );
    let ssh_command_env = merge_string(
        remote.ssh_command_env.clone(),
        remote_nested.ssh_command_env,
        global.ssh_command_env.clone(),
    );
    let use_http_path = merge_bool(
        remote.use_http_path,
        remote_nested.use_http_path,
        global.use_http_path,
    );

    if helper.is_some() {
        return Err(CodeSyncError::Unsupported(
            "credential.helper is not supported".to_string(),
        ));
    }
    if ssh_command_env.is_some() {
        return Err(CodeSyncError::Unsupported(
            "ssh_command_env is not supported".to_string(),
        ));
    }
    if username_env.is_some() != password_env.is_some() {
        return Err(CodeSyncError::Config(
            "credential.username_env and credential.password_env must be set together".to_string(),
        ));
    }

    Ok(CredentialConfig {
        username_env,
        password_env,
        use_http_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CodeSyncError;
    use std::{fs, path::PathBuf};

    fn base_json() -> &'static str {
        r#"
{
  "listen": {
    "host": "0.0.0.0",
    "port": 8080
  },
  "webhook": {
    "path": "/webhook",
    "secret_env": "CODESYNC_WEBHOOK_SECRET",
    "max_body_bytes": 1048576
  },
  "repo_dir": "/var/lib/codesync/repo.git",
  "state_dir": "/var/lib/codesync",
  "branch": "master",
  "git": {
    "timeout_seconds": 300
  },
  "credential": {
    "username_env": "CODESYNC_GIT_USERNAME",
    "password_env": "CODESYNC_GIT_PASSWORD",
    "helper": "",
    "ssh_command_env": "",
    "use_http_path": true
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/group/project-a.git"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/group/project-b.git"
    }
  ]
}
"#
    }

    fn parse(text: &str) -> AppConfig {
        AppConfig::from_json_str(text).expect("config should parse")
    }

    fn replace(text: &str, from: &str, to: &str) -> String {
        text.replace(from, to)
    }

    #[test]
    fn parses_existing_config_shape() {
        let config = parse(base_json());

        assert_eq!(config.listen_host, "0.0.0.0");
        assert_eq!(config.listen_port, 8080);
        assert_eq!(config.repo_dir, PathBuf::from("/var/lib/codesync/repo.git"));
        assert_eq!(config.state_dir, PathBuf::from("/var/lib/codesync"));
        assert_eq!(config.branch, "master");
        assert_eq!(config.git_timeout_seconds, 300);
        assert_eq!(config.webhook.path, "/webhook");
        assert_eq!(config.webhook.secret, None);
        assert_eq!(
            config.webhook.secret_env.as_deref(),
            Some("CODESYNC_WEBHOOK_SECRET")
        );
        assert_eq!(config.webhook.max_body_bytes, 1048576);
        assert_eq!(config.remotes.len(), 2);
        assert_eq!(config.remotes[0].name, "repo_a");
        assert_eq!(
            config.remotes[0].url,
            "https://example.com/group/project-a.git"
        );
        assert_eq!(
            config.remotes[0].credential.username_env.as_deref(),
            Some("CODESYNC_GIT_USERNAME")
        );
        assert_eq!(
            config.remotes[0].credential.password_env.as_deref(),
            Some("CODESYNC_GIT_PASSWORD")
        );
        assert!(config.remotes[0].credential.use_http_path);
    }

    #[test]
    fn config_example_parses_with_empty_ssh_command_env() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.json");
        let text = fs::read_to_string(&path).expect("example config should be readable");

        let config = parse(&text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn missing_ssh_command_env_is_allowed() {
        let text = replace(base_json(), "    \"ssh_command_env\": \"\",\n", "");

        let config = parse(&text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn remote_top_level_non_empty_ssh_command_env_is_unsupported() {
        let text = replace(
            base_json(),
            "\"url\": \"https://example.com/group/project-a.git\"",
            "\"url\": \"https://example.com/group/project-a.git\",\n      \"ssh_command_env\": \"REMOTE_SSH_COMMAND\"",
        );

        let err = AppConfig::from_json_str(&text)
            .expect_err("remote ssh command should be unsupported");

        assert!(
            matches!(err, CodeSyncError::Unsupported(message) if message.contains("ssh_command_env is not supported"))
        );
    }

    #[test]
    fn rejects_credential_helper() {
        let text = replace(
            base_json(),
            "\"helper\": \"\"",
            "\"helper\": \"store --file /tmp/creds\"",
        );

        let err = AppConfig::from_json_str(&text).expect_err("helper should be unsupported");

        assert!(
            matches!(err, CodeSyncError::Unsupported(message) if message.contains("credential.helper is not supported"))
        );
    }

    #[test]
    fn rejects_ssh_command_env() {
        let text = replace(
            base_json(),
            "\"ssh_command_env\": \"\"",
            "\"ssh_command_env\": \"CODESYNC_GIT_SSH_COMMAND\"",
        );

        let err = AppConfig::from_json_str(&text).expect_err("ssh command should be unsupported");

        assert!(
            matches!(err, CodeSyncError::Unsupported(message) if message.contains("ssh_command_env is not supported"))
        );
    }

    #[test]
    fn rejects_non_https_remote_urls() {
        let text = replace(
            base_json(),
            "https://example.com/group/project-a.git",
            "ssh://example.com/group/project-a.git",
        );

        let err = AppConfig::from_json_str(&text)
            .expect_err("non-https remote URL should be unsupported");

        assert!(
            matches!(err, CodeSyncError::Unsupported(message) if message.contains("only https remote URLs are supported"))
        );
    }

    #[test]
    fn rejects_partial_username_password_config() {
        let text = replace(
            base_json(),
            "\"password_env\": \"CODESYNC_GIT_PASSWORD\"",
            "\"password_env\": \"\"",
        );

        let err =
            AppConfig::from_json_str(&text).expect_err("partial credential env should be invalid");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("credential.username_env and credential.password_env must be set together"))
        );
    }

    #[test]
    fn remote_credential_overrides_default() {
        let text = replace(
            base_json(),
            "\"url\": \"https://example.com/group/project-a.git\"",
            "\"url\": \"https://example.com/group/project-a.git\",\n      \"credential\": {\n        \"username_env\": \"REMOTE_USER\",\n        \"password_env\": \"REMOTE_PASS\",\n        \"use_http_path\": false\n      }",
        );

        let config = parse(&text);

        assert_eq!(
            config.remotes[0].credential.username_env.as_deref(),
            Some("REMOTE_USER")
        );
        assert_eq!(
            config.remotes[0].credential.password_env.as_deref(),
            Some("REMOTE_PASS")
        );
        assert!(!config.remotes[0].credential.use_http_path);
        assert_eq!(
            config.remotes[1].credential.username_env.as_deref(),
            Some("CODESYNC_GIT_USERNAME")
        );
        assert_eq!(
            config.remotes[1].credential.password_env.as_deref(),
            Some("CODESYNC_GIT_PASSWORD")
        );
        assert!(config.remotes[1].credential.use_http_path);
    }

    #[test]
    fn top_level_aliases_work() {
        let text = r#"
{
  "listen_host": "127.0.0.1",
  "listen_port": 9090,
  "webhook_path": "/alias-hook",
  "git_timeout_seconds": 45,
  "repo_dir": "/srv/codesync/repo.git",
  "remotes": [
    { "name": "repo_a", "url": "https://example.com/a.git" },
    { "name": "repo_b", "url": "https://example.com/b.git" }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.listen_host, "127.0.0.1");
        assert_eq!(config.listen_port, 9090);
        assert_eq!(config.webhook.path, "/alias-hook");
        assert_eq!(config.git_timeout_seconds, 45);
        assert_eq!(config.branch, "master");
        assert_eq!(config.state_dir, PathBuf::from("/srv/codesync"));
    }

    #[test]
    fn parses_master_role_and_more_than_two_remotes() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": "repo_a", "url": "https://example.com/a.git", "role": "master" },
    { "name": "repo_b", "url": "https://example.com/b.git" },
    { "name": "repo_c", "url": "https://example.com/c.git" }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 3);
        assert_eq!(config.remotes[0].role.as_deref(), Some("master"));
        assert_eq!(config.remotes[1].role, None);
        assert_eq!(config.remotes[2].name, "repo_c");
    }

    #[test]
    fn rejects_invalid_remote_role() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": "repo_a", "url": "https://example.com/a.git", "role": "source" },
    { "name": "repo_b", "url": "https://example.com/b.git" }
  ]
}
"#;

        let err = AppConfig::from_json_str(text).expect_err("invalid role should be rejected");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("supported role is 'master'"))
        );
    }

    #[test]
    fn rejects_fewer_than_two_remotes() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": "repo_a", "url": "https://example.com/a.git" }
  ]
}
"#;

        let err = AppConfig::from_json_str(text).expect_err("single remote should be rejected");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("at least two remote objects"))
        );
    }

    #[test]
    fn duplicate_remote_names_rejected() {
        let text = replace(base_json(), "\"name\": \"repo_b\"", "\"name\": \"repo_a\"");

        let err =
            AppConfig::from_json_str(&text).expect_err("duplicate remote names should be invalid");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("duplicate remote name"))
        );
    }

    #[test]
    fn invalid_branch_rejected() {
        let text = replace(
            base_json(),
            "\"branch\": \"master\"",
            "\"branch\": \"feature bad\"",
        );

        let err = AppConfig::from_json_str(&text).expect_err("invalid branch should be rejected");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("invalid branch"))
        );
    }

    #[test]
    fn state_dir_defaults_to_repo_dir_parent() {
        let text = replace(base_json(), "  \"state_dir\": \"/var/lib/codesync\",\n", "");

        let config = parse(&text);

        assert_eq!(config.state_dir, PathBuf::from("/var/lib/codesync"));
    }

    #[test]
    fn empty_repo_dir_is_rejected() {
        for repo_dir in ["", "   "] {
            let text = replace(
                base_json(),
                "\"repo_dir\": \"/var/lib/codesync/repo.git\"",
                &format!("\"repo_dir\": \"{repo_dir}\""),
            );

            let err =
                AppConfig::from_json_str(&text).expect_err("blank repo_dir should be invalid");

            assert!(
                matches!(&err, CodeSyncError::Config(message) if message.contains("repo_dir is required")),
                "unexpected error for repo_dir {repo_dir:?}: {err}"
            );
        }
    }

    #[test]
    fn empty_webhook_secret_and_secret_env_become_none() {
        let text = replace(
            base_json(),
            "\"secret_env\": \"CODESYNC_WEBHOOK_SECRET\"",
            "\"secret\": \"\",\n    \"secret_env\": \"\"",
        );

        let config = parse(&text);

        assert_eq!(config.webhook.secret, None);
        assert_eq!(config.webhook.secret_env, None);
    }

    #[test]
    fn remote_nested_empty_helper_clears_global_helper() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "helper": "store --file /tmp/creds"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "helper": "" }
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": { "helper": "" }
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn remote_top_level_empty_helper_clears_global_helper() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "helper": "store --file /tmp/creds"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "helper": ""
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "helper": ""
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn remote_nested_empty_ssh_command_env_clears_global_ssh_command_env() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "ssh_command_env": "CODESYNC_GIT_SSH_COMMAND"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "ssh_command_env": "" }
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": { "ssh_command_env": "" }
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn remote_top_level_empty_ssh_command_env_clears_global_ssh_command_env() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "ssh_command_env": "CODESYNC_GIT_SSH_COMMAND"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "ssh_command_env": ""
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "ssh_command_env": ""
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn remote_name_and_url_are_trimmed() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": " repo_a ", "url": " https://example.com/a.git " },
    { "name": " repo_b ", "url": " https://example.com/b.git " }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes[0].name, "repo_a");
        assert_eq!(config.remotes[0].url, "https://example.com/a.git");
        assert_eq!(config.remotes[1].name, "repo_b");
        assert_eq!(config.remotes[1].url, "https://example.com/b.git");
    }

    #[test]
    fn missing_remote_name_returns_config_error() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "url": "https://example.com/a.git" },
    { "name": "repo_b", "url": "https://example.com/b.git" }
  ]
}
"#;

        let err =
            AppConfig::from_json_str(text).expect_err("missing remote name should be invalid");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("remotes[0] requires name and url"))
        );
    }

    #[test]
    fn missing_remote_url_returns_config_error() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": "repo_a" },
    { "name": "repo_b", "url": "https://example.com/b.git" }
  ]
}
"#;

        let err = AppConfig::from_json_str(text).expect_err("missing remote URL should be invalid");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("remotes[0] requires name and url"))
        );
    }

    #[test]
    fn blank_remote_url_returns_config_error() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": "repo_a", "url": "   " },
    { "name": "repo_b", "url": "https://example.com/b.git" }
  ]
}
"#;

        let err = AppConfig::from_json_str(text).expect_err("blank remote URL should be invalid");

        assert!(
            matches!(err, CodeSyncError::Config(message) if message.contains("remotes[0] requires name and url"))
        );
    }

    #[test]
    fn nested_empty_helper_ignores_top_level_helper_and_clears_global() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": { "helper": "store --file /tmp/global-creds" },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "helper": "" },
      "helper": "store --file /tmp/top-creds"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": { "helper": "" },
      "helper": "store --file /tmp/top-creds"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn nested_non_empty_helper_ignores_top_level_empty_helper_and_errors() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "helper": "store --file /tmp/nested-creds" },
      "helper": ""
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git"
    }
  ]
}
"#;

        let err = AppConfig::from_json_str(text).expect_err("nested helper should be unsupported");

        assert!(
            matches!(err, CodeSyncError::Unsupported(message) if message.contains("credential.helper is not supported"))
        );
    }

    #[test]
    fn nested_empty_ssh_command_env_ignores_top_level_ssh_command_env_and_clears_global() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": { "ssh_command_env": "GLOBAL_SSH_COMMAND" },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "ssh_command_env": "" },
      "ssh_command_env": "TOP_SSH_COMMAND"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": { "ssh_command_env": "" },
      "ssh_command_env": "TOP_SSH_COMMAND"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn nested_non_empty_ssh_command_env_ignores_top_level_empty_ssh_command_env_and_errors() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "ssh_command_env": "NESTED_SSH_COMMAND" },
      "ssh_command_env": ""
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git"
    }
  ]
}
"#;

        let err =
            AppConfig::from_json_str(text).expect_err("nested ssh command should be unsupported");

        assert!(
            matches!(err, CodeSyncError::Unsupported(message) if message.contains("ssh_command_env is not supported"))
        );
    }

    #[test]
    fn nested_username_password_wins_over_top_level_username_password() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "username_env": "GLOBAL_USER",
    "password_env": "GLOBAL_PASS"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": {
        "username_env": "NESTED_USER",
        "password_env": "NESTED_PASS"
      },
      "username_env": "TOP_USER",
      "password_env": "TOP_PASS"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(
            config.remotes[0].credential.username_env.as_deref(),
            Some("NESTED_USER")
        );
        assert_eq!(
            config.remotes[0].credential.password_env.as_deref(),
            Some("NESTED_PASS")
        );
    }

    #[test]
    fn top_level_username_password_fills_when_nested_missing() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "use_http_path": false },
      "username_env": "TOP_USER",
      "password_env": "TOP_PASS"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(
            config.remotes[0].credential.username_env.as_deref(),
            Some("TOP_USER")
        );
        assert_eq!(
            config.remotes[0].credential.password_env.as_deref(),
            Some("TOP_PASS")
        );
    }

    #[test]
    fn unknown_top_level_key_returns_error() {
        let text = replace(
            base_json(),
            "\"repo_dir\": \"/var/lib/codesync/repo.git\"",
            "\"repo_dir\": \"/var/lib/codesync/repo.git\",\n  \"surprise\": true",
        );

        let err = AppConfig::from_json_str(&text).expect_err("unknown top-level keys should fail");
        let message = err.to_string();

        assert!(
            matches!(err, CodeSyncError::Json(_) | CodeSyncError::Config(_)),
            "unexpected error type: {message}"
        );
        assert!(
            message.contains("unknown field") || message.contains("surprise"),
            "unexpected error message: {message}"
        );
    }

    #[test]
    fn webhook_config_debug_redacts_secret() {
        let webhook = WebhookConfig {
            path: "/webhook".to_string(),
            secret: Some("super-secret".to_string()),
            secret_env: Some("SECRET_ENV".to_string()),
            max_body_bytes: 42,
        };

        let debug = format!("{webhook:?}");

        assert!(
            !debug.contains("super-secret"),
            "secret leaked in Debug: {debug}"
        );
        assert!(
            debug.contains("<redacted>"),
            "secret should be redacted in Debug: {debug}"
        );
    }

    #[test]
    fn nested_null_username_password_clear_global_and_block_top_level_fallback() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "username_env": "GLOBAL_USER",
    "password_env": "GLOBAL_PASS"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": {
        "username_env": null,
        "password_env": null
      },
      "username_env": "TOP_USER",
      "password_env": "TOP_PASS"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": {
        "username_env": null,
        "password_env": null
      },
      "username_env": "TOP_USER",
      "password_env": "TOP_PASS"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes[0].credential.username_env, None);
        assert_eq!(config.remotes[0].credential.password_env, None);
        assert_eq!(config.remotes[1].credential.username_env, None);
        assert_eq!(config.remotes[1].credential.password_env, None);
    }

    #[test]
    fn top_level_null_username_password_clear_global_when_nested_missing() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": {
    "username_env": "GLOBAL_USER",
    "password_env": "GLOBAL_PASS"
  },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "username_env": null,
      "password_env": null
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "username_env": null,
      "password_env": null
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes[0].credential.username_env, None);
        assert_eq!(config.remotes[0].credential.password_env, None);
        assert_eq!(config.remotes[1].credential.username_env, None);
        assert_eq!(config.remotes[1].credential.password_env, None);
    }

    #[test]
    fn nested_null_helper_clears_global_helper_and_blocks_top_level_helper() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": { "helper": "store --file /tmp/global-creds" },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "helper": null },
      "helper": "store --file /tmp/top-creds"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": { "helper": null },
      "helper": "store --file /tmp/top-creds"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn nested_null_ssh_command_env_clears_global_and_blocks_top_level_ssh_command_env() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "credential": { "ssh_command_env": "GLOBAL_SSH_COMMAND" },
  "remotes": [
    {
      "name": "repo_a",
      "url": "https://example.com/a.git",
      "credential": { "ssh_command_env": null },
      "ssh_command_env": "TOP_SSH_COMMAND"
    },
    {
      "name": "repo_b",
      "url": "https://example.com/b.git",
      "credential": { "ssh_command_env": null },
      "ssh_command_env": "TOP_SSH_COMMAND"
    }
  ]
}
"#;

        let config = parse(text);

        assert_eq!(config.remotes.len(), 2);
    }

    #[test]
    fn remote_config_debug_redacts_url_userinfo() {
        let remote = RemoteConfig {
            name: "repo_a".to_string(),
            url: "https://user:token@example.com/repo.git".to_string(),
            credential: CredentialConfig {
                username_env: None,
                password_env: None,
                use_http_path: true,
            },
            role: Some("master".to_string()),
        };

        let debug = format!("{remote:?}");

        assert!(!debug.contains("user"), "username leaked in Debug: {debug}");
        assert!(!debug.contains("token"), "token leaked in Debug: {debug}");
        assert!(
            debug.contains("https://***@example.com/repo.git"),
            "redacted URL missing from Debug: {debug}"
        );
    }

    #[test]
    fn app_config_debug_redacts_remote_url_userinfo() {
        let text = r#"
{
  "repo_dir": "/var/lib/codesync/repo.git",
  "remotes": [
    { "name": "repo_a", "url": "https://user:token@example.com/a.git" },
    { "name": "repo_b", "url": "https://user:token@example.com/b.git" }
  ]
}
"#;

        let config = parse(text);
        let debug = format!("{config:?}");

        assert!(!debug.contains("user"), "username leaked in Debug: {debug}");
        assert!(!debug.contains("token"), "token leaked in Debug: {debug}");
        assert!(
            debug.contains("https://***@example.com/a.git"),
            "redacted repo_a URL missing from Debug: {debug}"
        );
        assert!(
            debug.contains("https://***@example.com/b.git"),
            "redacted repo_b URL missing from Debug: {debug}"
        );
    }
}
