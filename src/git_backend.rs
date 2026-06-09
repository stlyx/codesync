use std::fs;
use std::path::Path;

use crate::config::{AppConfig, RemoteConfig};
use crate::credentials::ResolvedCredentials;
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
        let repo = self.repo()?;
        let refspecs = tag_push_refspecs(repo)?;
        if refspecs.is_empty() {
            return Ok(());
        }
        push_impl(repo, remote, &refspecs)
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

fn remote_branch_ref(remote: &str, branch: &str) -> String {
    format!("refs/remotes/{remote}/{branch}")
}

fn branch_fetch_refspec(remote: &str, branch: &str) -> String {
    format!("+refs/heads/{branch}:{}", remote_branch_ref(remote, branch))
}

fn tag_fetch_refspec() -> &'static str {
    "refs/tags/*:refs/tags/*"
}

fn branch_push_refspec(branch: &str) -> String {
    format!("refs/heads/{branch}:refs/heads/{branch}")
}

#[cfg(test)]
fn tag_push_refspec() -> &'static str {
    "refs/tags/*:refs/tags/*"
}

fn tag_push_refspecs(repo: &git2::Repository) -> Result<Vec<String>> {
    let tag_names = repo.tag_names(None).map_err(git_error)?;
    let mut refspecs = Vec::new();
    for name in tag_names.iter() {
        let name = name.map_err(git_error)?.ok_or_else(|| {
            CodeSyncError::GitBackend("tag name is not valid UTF-8".to_string())
        })?;
        let refname = format!("refs/tags/{name}");
        refspecs.push(format!("{refname}:{refname}"));
    }
    Ok(refspecs)
}

fn git_error(error: git2::Error) -> CodeSyncError {
    CodeSyncError::GitBackend(error.message().to_string())
}

fn callbacks(remote: &RemoteConfig) -> Result<git2::RemoteCallbacks<'static>> {
    let credentials = ResolvedCredentials::from_config(&remote.credential)?;
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(move |_url, username_from_url, allowed| {
        let Some(credentials) = credentials.as_ref() else {
            return Err(git2::Error::from_str("credentials are not configured"));
        };
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            git2::Cred::userpass_plaintext(&credentials.username, &credentials.password)
        } else if allowed.contains(git2::CredentialType::USERNAME) {
            git2::Cred::username(username_from_url.unwrap_or(&credentials.username))
        } else {
            Err(git2::Error::from_str(
                "unsupported credential type requested by remote",
            ))
        }
    });
    callbacks.push_update_reference(|refname, status| {
        if let Some(status) = status {
            Err(git2::Error::from_str(&format!(
                "push rejected for {refname}: {status}"
            )))
        } else {
            Ok(())
        }
    });
    Ok(callbacks)
}

fn fetch_options(
    remote: &RemoteConfig,
    prune: git2::FetchPrune,
) -> Result<git2::FetchOptions<'static>> {
    let mut options = git2::FetchOptions::new();
    options.remote_callbacks(callbacks(remote)?);
    options.download_tags(git2::AutotagOption::None);
    options.prune(prune);
    Ok(options)
}

fn push_options(remote: &RemoteConfig) -> Result<git2::PushOptions<'static>> {
    let mut options = git2::PushOptions::new();
    options.remote_callbacks(callbacks(remote)?);
    Ok(options)
}

fn with_anonymous_remote<T>(
    repo: &git2::Repository,
    remote: &RemoteConfig,
    f: impl FnOnce(&mut git2::Remote<'_>) -> Result<T>,
) -> Result<T> {
    let mut handle = repo.remote_anonymous(&remote.url).map_err(git_error)?;
    f(&mut handle)
}

fn fetch_remote_impl(
    repo: &git2::Repository,
    remote: &RemoteConfig,
    branch: &str,
) -> Result<String> {
    let refspec = branch_fetch_refspec(&remote.name, branch);
    let mut options = fetch_options(remote, git2::FetchPrune::On)?;
    with_anonymous_remote(repo, remote, |handle| {
        handle
            .fetch(
                &[refspec.as_str()],
                Some(&mut options),
                Some("codesync fetch branch"),
            )
            .map_err(git_error)?;
        let oid = repo
            .refname_to_id(&remote_branch_ref(&remote.name, branch))
            .map_err(git_error)?;
        Ok(oid.to_string())
    })
}

fn fetch_tags_impl(repo: &git2::Repository, remote: &RemoteConfig) -> Result<()> {
    let mut options = fetch_options(remote, git2::FetchPrune::Off)?;
    preflight_tag_conflicts(repo, remote)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_anonymous_remote(repo, remote, |handle| {
            handle
                .fetch(
                    &[tag_fetch_refspec()],
                    Some(&mut options),
                    Some("codesync fetch tags"),
                )
                .map_err(git_error)?;
            Ok(())
        })
    }));

    match result {
        Ok(result) => result,
        Err(_) => Err(CodeSyncError::GitBackend(
            "remote ref name is not valid UTF-8".to_string(),
        )),
    }
}

fn preflight_tag_conflicts(repo: &git2::Repository, remote: &RemoteConfig) -> Result<()> {
    with_anonymous_remote(repo, remote, |handle| {
        let connection = handle
            .connect_auth(git2::Direction::Fetch, Some(callbacks(remote)?), None)
            .map_err(git_error)?;
        for head in connection.list().map_err(git_error)? {
            let name = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| head.name()))
                .map_err(|_| {
                    CodeSyncError::GitBackend("remote ref name is not valid UTF-8".to_string())
                })?;
            if !name.starts_with("refs/tags/") || name.ends_with("^{}") {
                continue;
            }
            match repo.refname_to_id(name) {
                Ok(local) if local != head.oid() => {
                    return Err(CodeSyncError::GitBackend(format!(
                        "tag conflict for {name}: local {local} differs from remote {}",
                        head.oid()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.code() == git2::ErrorCode::NotFound => {}
                Err(error) => return Err(git_error(error)),
            }
        }
        Ok(())
    })
}

fn push_impl(repo: &git2::Repository, remote: &RemoteConfig, refspecs: &[String]) -> Result<()> {
    let mut options = push_options(remote)?;
    with_anonymous_remote(repo, remote, |handle| {
        let refs = refspecs.iter().map(String::as_str).collect::<Vec<_>>();
        handle.push(&refs, Some(&mut options)).map_err(git_error)?;
        Ok(())
    })
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

    fn write_commit(
        repo: &git2::Repository,
        message: &str,
        parent: Option<git2::Oid>,
    ) -> git2::Oid {
        let signature = git2::Signature::now("CodeSync Test", "codesync@example.invalid").unwrap();
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

    fn path_url(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn remote_config(name: &str, url: String) -> RemoteConfig {
        RemoteConfig {
            name: name.to_string(),
            url,
            credential: CredentialConfig {
                username_env: None,
                password_env: None,
                use_http_path: true,
            },
            role: None,
        }
    }

    fn init_bare_with_branch(path: &Path, branch: &str) -> (git2::Repository, git2::Oid) {
        let repo = git2::Repository::init_bare(path).expect("bare repo initialized");
        let commit = write_commit(&repo, "initial", None);
        repo.reference(&branch_ref(branch), commit, true, "seed branch")
            .expect("branch seeded");
        (repo, commit)
    }

    #[test]
    fn branch_ref_names_match_existing_service() {
        assert_eq!(branch_ref("main"), "refs/heads/main");
        assert_eq!(
            remote_branch_ref("repo_a", "main"),
            "refs/remotes/repo_a/main"
        );
    }

    #[test]
    fn refspecs_match_existing_service() {
        assert_eq!(
            branch_fetch_refspec("repo_a", "main"),
            "+refs/heads/main:refs/remotes/repo_a/main"
        );
        assert_eq!(tag_fetch_refspec(), "refs/tags/*:refs/tags/*");
        assert_eq!(
            branch_push_refspec("main"),
            "refs/heads/main:refs/heads/main"
        );
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
        backend
            .update_local_branch("main", &commit.to_string())
            .unwrap();

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

        assert!(
            backend
                .is_ancestor(&first.to_string(), &second.to_string())
                .unwrap()
        );
        assert!(
            !backend
                .is_ancestor(&second.to_string(), &first.to_string())
                .unwrap()
        );
    }

    #[test]
    fn fetch_remote_fetches_branch_tip_to_tracking_ref() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("source.git");
        let (_source, source_tip) = init_bare_with_branch(&source_dir, "main");
        let target_dir = temp.path().join("target.git");
        let config = test_config(target_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let remote = remote_config("origin", path_url(&source_dir));

        let fetched = backend.fetch_remote(&remote, "main").unwrap();

        assert_eq!(fetched, source_tip.to_string());
        let tracking = backend
            .repo()
            .unwrap()
            .refname_to_id("refs/remotes/origin/main")
            .unwrap();
        assert_eq!(tracking, source_tip);
    }

    #[test]
    fn fetch_remote_errors_when_remote_branch_is_missing_after_previous_fetch() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("source.git");
        let (source, _) = init_bare_with_branch(&source_dir, "main");
        let target_dir = temp.path().join("target.git");
        let config = test_config(target_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let remote = remote_config("origin", path_url(&source_dir));
        backend.fetch_remote(&remote, "main").unwrap();
        source.find_reference(&branch_ref("main")).unwrap().delete().unwrap();

        let err = backend
            .fetch_remote(&remote, "main")
            .expect_err("missing remote branch should not return stale tracking ref");

        assert!(
            err.to_string().contains("main") || err.to_string().contains("reference"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn fetch_tags_fetches_remote_tags_without_force() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("source.git");
        let (source, source_tip) = init_bare_with_branch(&source_dir, "main");
        source
            .tag_lightweight("v1", &source.find_object(source_tip, None).unwrap(), false)
            .unwrap();
        let target_dir = temp.path().join("target.git");
        let config = test_config(target_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let remote = remote_config("origin", path_url(&source_dir));

        backend.fetch_tags(&remote).unwrap();

        let tag = backend.repo().unwrap().refname_to_id("refs/tags/v1").unwrap();
        assert_eq!(tag, source_tip);
    }

    #[test]
    fn push_remote_pushes_branch_without_force() {
        let temp = tempdir().unwrap();
        let remote_dir = temp.path().join("remote.git");
        git2::Repository::init_bare(&remote_dir).unwrap();
        let local_dir = temp.path().join("local.git");
        let config = test_config(local_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let commit = write_commit(backend.repo().unwrap(), "local", None);
        backend.update_local_branch("main", &commit.to_string()).unwrap();
        let remote = remote_config("origin", path_url(&remote_dir));

        backend
            .push_remote(&remote, "main", &commit.to_string())
            .unwrap();

        let remote_repo = git2::Repository::open_bare(&remote_dir).unwrap();
        assert_eq!(remote_repo.refname_to_id("refs/heads/main").unwrap(), commit);
    }

    #[test]
    fn fetch_tags_fetches_annotated_tags() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("source.git");
        let (source, source_tip) = init_bare_with_branch(&source_dir, "main");
        let signature = git2::Signature::now("CodeSync Test", "codesync@example.invalid").unwrap();
        source
            .tag(
                "v1",
                &source.find_object(source_tip, None).unwrap(),
                &signature,
                "annotated tag",
                false,
            )
            .unwrap();
        let target_dir = temp.path().join("target.git");
        let config = test_config(target_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let remote = remote_config("origin", path_url(&source_dir));

        backend.fetch_tags(&remote).unwrap();

        backend.repo().unwrap().refname_to_id("refs/tags/v1").unwrap();
    }

    #[test]
    fn fetch_tags_rejects_conflicting_existing_tag() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("source.git");
        let source = git2::Repository::init_bare(&source_dir).unwrap();
        let first = write_commit(&source, "first", None);
        let second = write_commit(&source, "second", Some(first));
        source
            .reference(&branch_ref("main"), second, true, "seed branch")
            .unwrap();
        source
            .tag_lightweight("v1", &source.find_object(second, None).unwrap(), false)
            .unwrap();
        let target_dir = temp.path().join("target.git");
        let config = test_config(target_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let remote = remote_config("origin", path_url(&source_dir));
        backend.fetch_remote(&remote, "main").unwrap();
        {
            let first_object = backend.repo().unwrap().find_object(first, None).unwrap();
            backend
                .repo()
                .unwrap()
                .tag_lightweight("v1", &first_object, false)
                .unwrap();
        }
        let before = backend.repo().unwrap().refname_to_id("refs/tags/v1").unwrap();

        let err = backend
            .fetch_tags(&remote)
            .expect_err("conflicting tag should not be overwritten");

        assert!(
            err.to_string().contains("tag conflict for refs/tags/v1"),
            "unexpected error: {err}"
        );
        let after = backend.repo().unwrap().refname_to_id("refs/tags/v1").unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn push_remote_rejects_non_fast_forward_branch_update() {
        let temp = tempdir().unwrap();
        let remote_dir = temp.path().join("remote.git");
        let remote_repo = git2::Repository::init_bare(&remote_dir).unwrap();
        let remote_commit = write_commit(&remote_repo, "remote", None);
        remote_repo
            .reference(&branch_ref("main"), remote_commit, true, "seed branch")
            .unwrap();
        let local_dir = temp.path().join("local.git");
        let config = test_config(local_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let local_commit = write_commit(backend.repo().unwrap(), "local", None);
        backend
            .update_local_branch("main", &local_commit.to_string())
            .unwrap();
        let remote = remote_config("origin", path_url(&remote_dir));

        let err = backend
            .push_remote(&remote, "main", &local_commit.to_string())
            .expect_err("non-fast-forward push should fail");

        assert!(
            err.to_string().contains("non-fast-forward")
                || err.to_string().contains("rejected")
                || err.to_string().contains("failed")
                || err.to_string().contains("commits that are not present locally"),
            "unexpected error: {err}"
        );
        assert_eq!(
            remote_repo.refname_to_id("refs/heads/main").unwrap(),
            remote_commit
        );
    }

    #[test]
    fn push_tags_pushes_local_tags_without_force() {
        let temp = tempdir().unwrap();
        let remote_dir = temp.path().join("remote.git");
        git2::Repository::init_bare(&remote_dir).unwrap();
        let local_dir = temp.path().join("local.git");
        let config = test_config(local_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let commit = write_commit(backend.repo().unwrap(), "local", None);
        {
            let object = backend.repo().unwrap().find_object(commit, None).unwrap();
            backend
                .repo()
                .unwrap()
                .tag_lightweight("v1", &object, false)
                .unwrap();
        }
        let remote = remote_config("origin", path_url(&remote_dir));

        backend.push_tags(&remote).unwrap();

        let remote_repo = git2::Repository::open_bare(&remote_dir).unwrap();
        assert_eq!(remote_repo.refname_to_id("refs/tags/v1").unwrap(), commit);
    }

    #[test]
    fn push_tags_without_local_tags_is_noop() {
        let temp = tempdir().unwrap();
        let local_dir = temp.path().join("local.git");
        let config = test_config(local_dir);
        let mut backend = Git2Backend::new();
        backend.ensure_repo(&config).unwrap();
        let remote = remote_config("origin", path_url(&temp.path().join("missing.git")));

        backend.push_tags(&remote).unwrap();
    }
}
