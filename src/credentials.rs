use crate::config::CredentialConfig;
use crate::error::{CodeSyncError, Result};

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ResolvedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl ResolvedCredentials {
    pub fn from_config(config: &CredentialConfig) -> Result<Option<Self>> {
        match (
            config.username_env.as_deref(),
            config.password_env.as_deref(),
        ) {
            (Some(username_env), Some(password_env)) => {
                let username = std::env::var(username_env);
                let password = std::env::var(password_env);
                match (username, password) {
                    (Ok(username), Ok(password)) => Ok(Some(Self { username, password })),
                    _ => Err(CodeSyncError::Credential(format!(
                        "credential environment variables are missing: {username_env}, {password_env}"
                    ))),
                }
            }
            (None, None) => Ok(None),
            _ => Err(CodeSyncError::Credential(
                "credential.username_env and credential.password_env must be set together"
                    .to_string(),
            )),
        }
    }
}

pub fn redact_url(value: &str) -> String {
    let Some(scheme_end) = value.find("://") else {
        return value.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = value[authority_start..]
        .find(['/', '?', '#'])
        .map(|offset| authority_start + offset)
        .unwrap_or(value.len());
    let authority = &value[authority_start..authority_end];
    let Some(at_index) = authority.rfind('@') else {
        return value.to_string();
    };

    let mut redacted = String::with_capacity(value.len());
    redacted.push_str(&value[..authority_start]);
    redacted.push_str("***@");
    redacted.push_str(&authority[at_index + 1..]);
    redacted.push_str(&value[authority_end..]);
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn credential_config(
        username_env: Option<&str>,
        password_env: Option<&str>,
    ) -> CredentialConfig {
        CredentialConfig {
            username_env: username_env.map(str::to_string),
            password_env: password_env.map(str::to_string),
            use_http_path: true,
        }
    }

    #[test]
    fn redacts_url_userinfo() {
        assert_eq!(
            redact_url("https://user:secret@example.com/a.git"),
            "https://***@example.com/a.git"
        );
        assert_eq!(
            redact_url("https://example.com/a.git"),
            "https://example.com/a.git"
        );
    }

    #[test]
    fn resolves_env_credentials() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let username_env = "CODESYNC_TEST_USERNAME";
        let password_env = "CODESYNC_TEST_PASSWORD";
        unsafe {
            std::env::set_var(username_env, "alice");
            std::env::set_var(password_env, "secret-token");
        }

        let resolved = ResolvedCredentials::from_config(&credential_config(
            Some(username_env),
            Some(password_env),
        ))
        .expect("credentials should resolve");

        assert_eq!(
            resolved,
            Some(ResolvedCredentials {
                username: "alice".to_string(),
                password: "secret-token".to_string(),
            })
        );

        unsafe {
            std::env::remove_var(username_env);
            std::env::remove_var(password_env);
        }
    }

    #[test]
    fn missing_env_is_error() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let username_env = "CODESYNC_TEST_MISSING_USERNAME";
        let password_env = "CODESYNC_TEST_MISSING_PASSWORD";
        unsafe {
            std::env::remove_var(username_env);
            std::env::remove_var(password_env);
        }

        let error = ResolvedCredentials::from_config(&credential_config(
            Some(username_env),
            Some(password_env),
        ))
        .expect_err("missing env vars should error");

        assert!(matches!(
            error,
            CodeSyncError::Credential(message)
                if message.contains(
                    "credential environment variables are missing: CODESYNC_TEST_MISSING_USERNAME, CODESYNC_TEST_MISSING_PASSWORD"
                )
        ));
    }

    #[test]
    fn no_configured_credentials_returns_none() {
        let resolved =
            ResolvedCredentials::from_config(&credential_config(None, None)).expect("no env should be allowed");

        assert_eq!(resolved, None);
    }

    #[test]
    fn partial_configured_credentials_is_error() {
        let error = ResolvedCredentials::from_config(&credential_config(Some("ONLY_USER"), None))
            .expect_err("partial env configuration should error");

        assert!(matches!(
            error,
            CodeSyncError::Credential(message)
                if message.contains(
                    "credential.username_env and credential.password_env must be set together"
                )
        ));
    }
}
