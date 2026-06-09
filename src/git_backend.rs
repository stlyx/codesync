use std::fs;
use std::path::Path;

use crate::config::{AppConfig, RemoteConfig};
use crate::error::{CodeSyncError, Result};
use crate::sync::GitBackend;

pub struct Git2Backend {
    repo: Option<git2::Repository>,
}

impl Git2Backend {
    pub fn new() -> Self {
        Self { repo: None }
    }

    fn repo(&self) -> Result<&git2::Repository> {
        self.repo
            .as_ref()
            .ok_or_else(|| CodeSyncError::GitBackend("repository is not initialized".to_string()))
    }
}

impl Default for Git2Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl GitBackend for Git2Backend {
    fn ensure_repo(&mut self, config: &AppConfig) -> Result<()> {
        if self.repo.is_some() {
            return Ok(());
        }

        if let Some(parent) = repo_parent_dir(&config.repo_dir) {
            fs::create_dir_all(parent)
                .map_err(|err| CodeSyncError::io_error(parent.to_path_buf(), err))?;
        }

        let repo = if config.repo_dir.join("HEAD").exists() {
            git2::Repository::open_bare(&config.repo_dir).map_err(git_error)?
        } else {
            git2::Repository::init_bare(&config.repo_dir).map_err(git_error)?
        };

        self.repo = Some(repo);
        Ok(())
    }

    fn fetch_remote(&mut self, remote: &RemoteConfig, branch: &str) -> Result<String> {
        fetch_remote_impl(self.repo()?, remote, branch)
    }

    fn fetch_tags(&mut self, remote: &RemoteConfig) -> Result<()> {
        fetch_tags_impl(self.repo()?, remote)
    }

    fn local_branch_tip(&self, branch: &str) -> Result<Option<String>> {
        match self.repo()?.refname_to_id(&branch_ref(branch)) {
            Ok(oid) => Ok(Some(oid.to_string())),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(error) => Err(git_error(error)),
        }
    }

    fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool> {
        let older = git2::Oid::from_str(older).map_err(git_error)?;
        let newer = git2::Oid::from_str(newer).map_err(git_error)?;
        self.repo()?
            .graph_descendant_of(newer, older)
            .map_err(git_error)
    }

    fn update_local_branch(&mut self, branch: &str, target: &str) -> Result<()> {
        let target = git2::Oid::from_str(target).map_err(git_error)?;
        self.repo()?
            .reference(&branch_ref(branch), target, true, "codesync fast-forward")
            .map(|_| ())
            .map_err(git_error)
    }

    fn push_remote(&mut self, remote: &RemoteConfig, branch: &str, _target: &str) -> Result<()> {
        push_impl(self.repo()?, remote, &[branch_push_refspec(branch)])
    }

    fn push_tags(&mut self, remote: &RemoteConfig) -> Result<()> {
        push_impl(self.repo()?, remote, &[tag_push_refspec().to_string()])
    }
}

fn repo_parent_dir(repo_dir: &Path) -> Option<&Path> {
    repo_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

fn branch_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

#[cfg(test)]
fn remote_branch_ref(remote: &str, branch: &str) -> String {
    format!("refs/remotes/{remote}/{branch}")
}

#[cfg(test)]
fn branch_fetch_refspec(remote: &str, branch: &str) -> String {
    format!("+refs/heads/{branch}:{}", remote_branch_ref(remote, branch))
}

#[cfg(test)]
fn tag_fetch_refspec() -> &'static str {
    "refs/tags/*:refs/tags/*"
}

fn branch_push_refspec(branch: &str) -> String {
    format!("refs/heads/{branch}:refs/heads/{branch}")
}

fn tag_push_refspec() -> &'static str {
    "refs/tags/*:refs/tags/*"
}

fn git_error(error: git2::Error) -> CodeSyncError {
    CodeSyncError::GitBackend(error.message().to_string())
}

fn fetch_remote_impl(
    _repo: &git2::Repository,
    _remote: &RemoteConfig,
    _branch: &str,
) -> Result<String> {
    Err(CodeSyncError::GitBackend(
        "fetch_remote_impl not wired".to_string(),
    ))
}

fn fetch_tags_impl(_repo: &git2::Repository, _remote: &RemoteConfig) -> Result<()> {
    Err(CodeSyncError::GitBackend(
        "fetch_tags_impl not wired".to_string(),
    ))
}

fn push_impl(_repo: &git2::Repository, _remote: &RemoteConfig, _refspecs: &[String]) -> Result<()> {
    Err(CodeSyncError::GitBackend("push_impl not wired".to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use crate::config::{AppConfig, CredentialConfig, RemoteConfig, WebhookConfig};

    use super::*;

    fn test_config(repo_dir: PathBuf) -> AppConfig {
        let credential = CredentialConfig {
            username_env: None,
            password_env: None,
            use_http_path: true,
        };
        AppConfig {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            repo_dir: repo_dir.clone(),
            state_dir: repo_dir
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            branch: "main".to_string(),
            remotes: vec![
                RemoteConfig {
                    name: "repo_a".to_string(),
                    url: "https://example.invalid/a.git".to_string(),
                    credential: credential.clone(),
                    role: None,
                },
                RemoteConfig {
                    name: "repo_b".to_string(),
                    url: "https://example.invalid/b.git".to_string(),
                    credential,
                    role: None,
                },
            ],
            webhook: WebhookConfig {
                path: "/webhook".to_string(),
                secret: None,
                secret_env: None,
                max_body_bytes: 1024,
            },
            git_timeout_seconds: 300,
        }
    }

    fn write_commit(repo: &git2::Repository, message: &str, parent: Option<git2::Oid>) -> git2::Oid {
        let signature =
            git2::Signature::now("CodeSync Test", "codesync@example.invalid").unwrap();
        let tree_id = {
            let builder = repo.treebuilder(None).unwrap();
            builder.write().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        let parents = parent
            .map(|oid| vec![repo.find_commit(oid).unwrap()])
            .unwrap_or_default();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(None, &signature, &signature, message, &tree, &parent_refs)
            .unwrap()
    }

    #[test]
    fn branch_ref_names_match_existing_service() {
        assert_eq!(branch_ref("main"), "refs/heads/main");
        assert_eq!(remote_branch_ref("repo_a", "main"), "refs/remotes/repo_a/main");
    }

    #[test]
    fn refspecs_match_existing_service() {
        assert_eq!(
            branch_fetch_refspec("repo_a", "main"),
            "+refs/heads/main:refs/remotes/repo_a/main"
        );
        assert_eq!(tag_fetch_refspec(), "refs/tags/*:refs/tags/*");
        assert_eq!(branch_push_refspec("main"), "refs/heads/main:refs/heads/main");
        assert_eq!(tag_push_refspec(), "refs/tags/*:refs/tags/*");
    }

    #[test]
    fn ensure_repo_initializes_bare_repository() {
        let temp = tempdir().unwrap();
        let repo_dir = temp.path().join("repo.git");
        let config = test_config(repo_dir.clone());
        let mut backend = Git2Backend::new();

        backend.ensure_repo(&config).expect("repo initialized");

        let repo = git2::Repository::open_bare(&repo_dir).expect("bare repo opens");
        assert!(repo.is_bare());
    }

    #[test]
    fn repo_parent_dir_ignores_empty_relative_parent() {
        assert_eq!(repo_parent_dir(Path::new("repo.git")), None);
    }

    #[test]
    fn local_branch_tip_and_update_local_branch_work() {
        let temp = tempdir().unwrap();
        let repo_dir = temp.path().join("repo.git");
        let config = test_config(repo_dir.clone());
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).expect("repo initialized");
        let commit = write_commit(backend.repo().unwrap(), "initial", None);

        assert_eq!(backend.local_branch_tip("main").unwrap(), None);
        backend.update_local_branch("main", &commit.to_string()).unwrap();

        assert_eq!(
            backend.local_branch_tip("main").unwrap(),
            Some(commit.to_string())
        );
    }

    #[test]
    fn is_ancestor_reports_commit_relationships() {
        let temp = tempdir().unwrap();
        let repo_dir = temp.path().join("repo.git");
        let config = test_config(repo_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).expect("repo initialized");
        let first = write_commit(backend.repo().unwrap(), "first", None);
        let second = write_commit(backend.repo().unwrap(), "second", Some(first));

        assert!(backend
            .is_ancestor(&first.to_string(), &second.to_string())
            .unwrap());
        assert!(!backend
            .is_ancestor(&second.to_string(), &first.to_string())
            .unwrap());
    }
}
