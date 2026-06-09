use std::collections::HashSet;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::info;

use crate::config::{AppConfig, RemoteConfig};
use crate::error::{CodeSyncError, Result};
use crate::lock::FileLock;

pub trait GitBackend {
    fn ensure_repo(&mut self, config: &AppConfig) -> Result<()>;
    fn fetch_remote(&mut self, remote: &RemoteConfig, branch: &str) -> Result<String>;
    fn fetch_tags(&mut self, remote: &RemoteConfig) -> Result<()>;
    fn local_branch_tip(&self, branch: &str) -> Result<Option<String>>;
    fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool>;
    fn update_local_branch(&mut self, branch: &str, target: &str) -> Result<()>;
    fn push_remote(&mut self, remote: &RemoteConfig, branch: &str, target: &str) -> Result<()>;
    fn push_tags(&mut self, remote: &RemoteConfig) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncResult {
    pub id: String,
    pub status: String,
    pub branch: String,
    pub target: String,
    pub elapsed_ms: u128,
    pub remotes: Vec<String>,
}

pub fn sync_once(
    config: &AppConfig,
    backend: &mut impl GitBackend,
    reason: &str,
) -> Result<SyncResult> {
    let started_at = Instant::now();
    let sync_id = make_sync_id();
    info!(sync_id = %sync_id, branch = %config.branch, reason = reason, "sync started");

    let _lock = FileLock::acquire(&config.state_dir.join("sync.lock"))?;
    backend.ensure_repo(config)?;

    let mut labeled_tips: Vec<(&str, &str)> = Vec::with_capacity(config.remotes.len() + 1);
    let local_tip = backend.local_branch_tip(&config.branch)?;
    if let Some(ref local_tip) = local_tip {
        labeled_tips.push(("local", local_tip.as_str()));
    }

    let mut remote_tips = Vec::with_capacity(config.remotes.len());
    for remote in &config.remotes {
        let tip = backend.fetch_remote(remote, &config.branch)?;
        backend.fetch_tags(remote)?;
        remote_tips.push((remote.name.as_str(), tip));
    }
    for (name, tip) in &remote_tips {
        labeled_tips.push((name, tip.as_str()));
    }

    let target = select_fast_forward_target(backend, &labeled_tips)?;
    backend.update_local_branch(&config.branch, &target)?;

    for remote in &config.remotes {
        backend.push_remote(remote, &config.branch, &target)?;
        backend.push_tags(remote)?;
    }

    Ok(SyncResult {
        id: sync_id,
        status: "ok".to_string(),
        branch: config.branch.clone(),
        target,
        elapsed_ms: started_at.elapsed().as_millis(),
        remotes: config.remotes.iter().map(|remote| remote.name.clone()).collect(),
    })
}

pub fn select_fast_forward_target(
    backend: &impl GitBackend,
    labeled_tips: &[( &str, &str)],
) -> Result<String> {
    let mut seen = HashSet::new();
    let unique_tips: Vec<(&str, &str)> = labeled_tips
        .iter()
        .copied()
        .filter(|(_, tip)| seen.insert(*tip))
        .collect();

    for (_, candidate) in &unique_tips {
        let mut fits = true;
        for (_, other) in &unique_tips {
            if *other != *candidate && !backend.is_ancestor(other, candidate)? {
                fits = false;
                break;
            }
        }
        if fits {
            return Ok((*candidate).to_string());
        }
    }

    let short_ids: Vec<String> = unique_tips
        .iter()
        .map(|(label, tip)| format!("{label}={}", short_id(tip)))
        .collect();
    Err(CodeSyncError::Conflict(format!(
        "divergent histories across {}",
        short_ids.join(", ")
    )))
}

fn short_id(value: &str) -> String {
    value.chars().take(12).collect()
}

fn make_sync_id() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("sync-{}", duration.as_millis()),
        Err(_) => "sync-0".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::config::{AppConfig, CredentialConfig, RemoteConfig, WebhookConfig};
    use crate::error::{CodeSyncError, Result};

    use super::{GitBackend, select_fast_forward_target, sync_once};

    struct FakeBackend {
        ensure_repo_calls: usize,
        local_tip: Option<String>,
        fetch_results: HashMap<String, String>,
        ancestors: HashSet<(String, String)>,
        operations: RefCell<Vec<String>>,
        expected_lock_path: Option<PathBuf>,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                ensure_repo_calls: 0,
                local_tip: None,
                fetch_results: HashMap::new(),
                ancestors: HashSet::new(),
                operations: RefCell::new(Vec::new()),
                expected_lock_path: None,
            }
        }

        fn with_local_tip(mut self, tip: &str) -> Self {
            self.local_tip = Some(tip.to_string());
            self
        }

        fn with_remote_tip(mut self, remote: &str, tip: &str) -> Self {
            self.fetch_results
                .insert(remote.to_string(), tip.to_string());
            self
        }

        fn with_ancestor(mut self, older: &str, newer: &str) -> Self {
            self.ancestors
                .insert((older.to_string(), newer.to_string()));
            self
        }

        fn expect_lock_path(mut self, path: PathBuf) -> Self {
            self.expected_lock_path = Some(path);
            self
        }

        fn operations(&self) -> Vec<String> {
            self.operations.borrow().clone()
        }
    }

    impl GitBackend for FakeBackend {
        fn ensure_repo(&mut self, _config: &AppConfig) -> Result<()> {
            if let Some(path) = &self.expected_lock_path {
                assert!(path.exists(), "sync lock should exist before ensure_repo runs");
            }
            self.ensure_repo_calls += 1;
            self.operations.borrow_mut().push("ensure_repo".to_string());
            Ok(())
        }

        fn fetch_remote(&mut self, remote: &RemoteConfig, branch: &str) -> Result<String> {
            assert_eq!(branch, "main");
            assert_eq!(self.ensure_repo_calls, 1, "ensure_repo must run before fetch");
            self.operations
                .borrow_mut()
                .push(format!("fetch:{}", remote.name));
            self.fetch_results.get(&remote.name).cloned().ok_or_else(|| {
                CodeSyncError::GitBackend(format!("missing fetch result for {}", remote.name))
            })
        }

        fn fetch_tags(&mut self, remote: &RemoteConfig) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("fetch_tags:{}", remote.name));
            Ok(())
        }

        fn local_branch_tip(&self, branch: &str) -> Result<Option<String>> {
            assert_eq!(branch, "main");
            Ok(self.local_tip.clone())
        }

        fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool> {
            self.operations
                .borrow_mut()
                .push(format!("is_ancestor:{older}:{newer}"));
            Ok(older == newer || self.ancestors.contains(&(older.to_string(), newer.to_string())))
        }

        fn update_local_branch(&mut self, branch: &str, target: &str) -> Result<()> {
            assert_eq!(branch, "main");
            self.operations
                .borrow_mut()
                .push(format!("update:{branch}:{target}"));
            self.local_tip = Some(target.to_string());
            Ok(())
        }

        fn push_remote(
            &mut self,
            remote: &RemoteConfig,
            branch: &str,
            target: &str,
        ) -> Result<()> {
            assert_eq!(branch, "main");
            self.operations
                .borrow_mut()
                .push(format!("push_branch:{}:{target}", remote.name));
            Ok(())
        }

        fn push_tags(&mut self, remote: &RemoteConfig) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("push_tags:{}", remote.name));
            Ok(())
        }
    }

    #[test]
    fn selects_tip_that_all_others_can_fast_forward_to() {
        let backend = FakeBackend::new()
            .with_ancestor("1111111", "3333333")
            .with_ancestor("2222222", "3333333");

        let target = select_fast_forward_target(
            &backend,
            &[("origin", "1111111"), ("backup", "2222222"), ("local", "3333333")],
        )
        .expect("target should be selected");

        assert_eq!(target, "3333333");
    }

    #[test]
    fn rejects_divergent_histories() {
        let backend = FakeBackend::new();

        let error =
            select_fast_forward_target(&backend, &[("origin", "1111111"), ("backup", "2222222")])
                .expect_err("divergent tips should be rejected");

        match error {
            CodeSyncError::Conflict(message) => {
                assert!(
                    message.contains("divergent histories"),
                    "unexpected message: {message}"
                );
                assert!(message.contains("1111111"), "unexpected message: {message}");
                assert!(message.contains("2222222"), "unexpected message: {message}");
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    #[test]
    fn conflict_short_ids_do_not_panic_on_non_ascii_tip_strings() {
        let backend = FakeBackend::new();

        let error = select_fast_forward_target(&backend, &[("origin", "éééé"), ("backup", "bbbb")])
            .expect_err("divergent non-ASCII tips should return a conflict");

        match error {
            CodeSyncError::Conflict(message) => {
                assert!(
                    message.contains("divergent histories"),
                    "unexpected message: {message}"
                );
                assert!(message.contains("éééé"), "unexpected message: {message}");
                assert!(message.contains("bbbb"), "unexpected message: {message}");
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    #[test]
    fn selects_first_tip_when_all_tips_equal() {
        let backend = FakeBackend::new();

        let target = select_fast_forward_target(
            &backend,
            &[("origin", "abcdef0"), ("backup", "abcdef0"), ("local", "abcdef0")],
        )
        .expect("equal tips should succeed");

        assert_eq!(target, "abcdef0");
    }

    #[test]
    fn sync_fetches_updates_and_pushes_branch_then_tags() {
        let config = sample_config();
        let mut backend = FakeBackend::new()
            .with_local_tip("1111111")
            .with_remote_tip("origin", "2222222")
            .with_remote_tip("backup", "3333333")
            .with_ancestor("1111111", "3333333")
            .with_ancestor("2222222", "3333333");

        let result = sync_once(&config, &mut backend, "webhook").expect("sync should succeed");

        assert_eq!(result.status, "ok");
        assert_eq!(result.branch, "main");
        assert_eq!(result.target, "3333333");
        assert_eq!(result.remotes, vec!["origin".to_string(), "backup".to_string()]);
        assert!(!result.id.is_empty());
        assert_eq!(backend.ensure_repo_calls, 1);
        assert_eq!(
            backend.operations(),
            vec![
                "ensure_repo".to_string(),
                "fetch:origin".to_string(),
                "fetch_tags:origin".to_string(),
                "fetch:backup".to_string(),
                "fetch_tags:backup".to_string(),
                "is_ancestor:2222222:1111111".to_string(),
                "is_ancestor:1111111:2222222".to_string(),
                "is_ancestor:1111111:3333333".to_string(),
                "is_ancestor:2222222:3333333".to_string(),
                "update:main:3333333".to_string(),
                "push_branch:origin:3333333".to_string(),
                "push_tags:origin".to_string(),
                "push_branch:backup:3333333".to_string(),
                "push_tags:backup".to_string(),
            ]
        );
    }

    #[test]
    fn sync_fetches_tags_for_each_remote_before_selecting_target_or_pushing() {
        let config = sample_config();
        let mut backend = FakeBackend::new()
            .with_local_tip("1111111")
            .with_remote_tip("origin", "2222222")
            .with_remote_tip("backup", "3333333")
            .with_ancestor("1111111", "3333333")
            .with_ancestor("2222222", "3333333");

        sync_once(&config, &mut backend, "webhook").expect("sync should succeed");

        let operations = backend.operations();
        assert_eq!(
            &operations[..5],
            [
                "ensure_repo".to_string(),
                "fetch:origin".to_string(),
                "fetch_tags:origin".to_string(),
                "fetch:backup".to_string(),
                "fetch_tags:backup".to_string(),
            ]
        );
        let first_selection_or_write = operations
            .iter()
            .position(|operation| {
                operation.starts_with("is_ancestor:")
                    || operation.starts_with("update:")
                    || operation.starts_with("push_branch:")
                    || operation.starts_with("push_tags:")
            })
            .expect("sync should select, update, or push after fetching");
        assert_eq!(first_selection_or_write, 5);
    }

    #[test]
    fn sync_acquires_file_lock_before_backend_operations() {
        let tempdir = tempdir().expect("tempdir should be created");
        let lock_path = tempdir.path().join("sync.lock");
        let mut config = sample_config();
        config.state_dir = tempdir.path().to_path_buf();
        let mut backend = FakeBackend::new()
            .with_remote_tip("origin", "3333333")
            .with_remote_tip("backup", "3333333")
            .expect_lock_path(lock_path);

        sync_once(&config, &mut backend, "webhook").expect("sync should succeed");

        assert_eq!(backend.operations().first(), Some(&"ensure_repo".to_string()));
    }

    #[test]
    fn sync_handles_missing_local_branch_tip() {
        let config = sample_config();
        let mut backend = FakeBackend::new()
            .with_remote_tip("origin", "2222222")
            .with_remote_tip("backup", "3333333")
            .with_ancestor("2222222", "3333333");

        let result = sync_once(&config, &mut backend, "startup")
            .expect("sync should succeed without local tip");

        assert_eq!(result.target, "3333333");
        assert!(backend.operations().iter().all(|entry| entry != "fetch:local"));
    }

    fn sample_config() -> AppConfig {
        AppConfig {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 8080,
            repo_dir: PathBuf::from("/tmp/repo"),
            state_dir: PathBuf::from("/tmp/state"),
            branch: "main".to_string(),
            remotes: vec![
                RemoteConfig {
                    name: "origin".to_string(),
                    url: "https://example.com/origin.git".to_string(),
                    credential: CredentialConfig {
                        username_env: Some("ORIGIN_USER".to_string()),
                        password_env: Some("ORIGIN_PASS".to_string()),
                        use_http_path: false,
                    },
                    role: None,
                },
                RemoteConfig {
                    name: "backup".to_string(),
                    url: "https://example.com/backup.git".to_string(),
                    credential: CredentialConfig {
                        username_env: Some("BACKUP_USER".to_string()),
                        password_env: Some("BACKUP_PASS".to_string()),
                        use_http_path: false,
                    },
                    role: None,
                },
            ],
            webhook: WebhookConfig {
                path: "/webhook".to_string(),
                secret: None,
                secret_env: None,
                max_body_bytes: 1024,
            },
            git_timeout_seconds: 30,
        }
    }
}
