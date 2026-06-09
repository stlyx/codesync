# CodeSync HTTPS-only Config and Git2 Backend Bugfix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve HTTPS-only `ssh_command_env` compatibility and replace the git2 backend network-operation stubs with real fetch/tag/push implementations.

**Architecture:** Keep config validation in `src/config.rs` and Git operations behind the existing `sync::GitBackend` trait. `src/git_backend.rs` will continue to own repository initialization, refs, ancestry, and remote operations; the placeholder remote helpers become small git2/libgit2 wrappers using anonymous remotes and existing `ResolvedCredentials`.

**Tech Stack:** Rust 2024, `git2` 0.21/libgit2, `tempfile`, Cargo tests, Cargo clippy.

---

## File Structure

- `Cargo.toml`
  - Change `git2 = "0.21.0"` to enable the `https` feature so real HTTPS remotes work at runtime.
- `config.example.json`
  - Keep `credential.ssh_command_env` as an empty string. If it is non-empty in the working tree, change it to `""`.
- `src/config.rs`
  - Add characterization/regression tests for the approved HTTPS-only config behavior. The existing implementation should already satisfy these tests; if so, do not change production config code.
- `src/git_backend.rs`
  - Add local bare-repository integration tests for branch fetch, tag fetch, branch push, and tag push.
  - Make `remote_branch_ref`, `branch_fetch_refspec`, and `tag_fetch_refspec` available to production code, not only tests.
  - Add git2 callbacks/options helpers and implement `fetch_remote_impl`, `fetch_tags_impl`, and `push_impl`.

---

### Task 1: Lock in HTTPS-only config compatibility

**Files:**
- Modify: `src/config.rs`
- Inspect/modify if needed: `config.example.json`

- [ ] **Step 1: Add config regression imports**

In `src/config.rs`, replace the test module imports near `src/config.rs:510`:

```rust
use super::*;
use crate::error::CodeSyncError;
use std::path::PathBuf;
```

with:

```rust
use super::*;
use crate::error::CodeSyncError;
use std::{fs, path::PathBuf};
```

- [ ] **Step 2: Add regression tests for empty, missing, and non-empty `ssh_command_env`**

In `src/config.rs`, insert these tests after `parses_existing_config_shape` and before `rejects_credential_helper`:

```rust
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
```

- [ ] **Step 3: Run config regression tests**

Run:

```bash
cargo test config::tests::config_example_parses_with_empty_ssh_command_env -- --nocapture
cargo test config::tests::missing_ssh_command_env_is_allowed -- --nocapture
cargo test config::tests::remote_top_level_non_empty_ssh_command_env_is_unsupported -- --nocapture
```

Expected:

- `config_example_parses_with_empty_ssh_command_env`: PASS if `config.example.json` has `"ssh_command_env": ""`; FAIL with `unsupported configuration: ssh_command_env is not supported` if the example still has a non-empty value.
- `missing_ssh_command_env_is_allowed`: PASS.
- `remote_top_level_non_empty_ssh_command_env_is_unsupported`: PASS.

If all pass, this task only adds regression coverage and production config code remains unchanged.

- [ ] **Step 4: Fix example config only if the example test fails**

If `config.example.json` contains a non-empty SSH command value, replace that value with an empty string:

```json
"ssh_command_env": "",
```

Do not remove the field. The field remains in the example to show the compatibility shape, but empty means disabled.

- [ ] **Step 5: Re-run config regression tests**

Run:

```bash
cargo test config::tests::config_example_parses_with_empty_ssh_command_env -- --nocapture
cargo test config::tests::missing_ssh_command_env_is_allowed -- --nocapture
cargo test config::tests::remote_top_level_non_empty_ssh_command_env_is_unsupported -- --nocapture
```

Expected: all PASS.

---

### Task 2: Add failing git2 backend wiring tests

**Files:**
- Modify: `src/git_backend.rs`

- [ ] **Step 1: Add local remote test helpers**

In `src/git_backend.rs`, inside `#[cfg(test)] mod tests`, insert these helpers after `write_commit` and before `branch_ref_names_match_existing_service`:

```rust
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
```

- [ ] **Step 2: Add branch fetch red test**

In `src/git_backend.rs`, inside the same test module, append this test after `is_ancestor_reports_commit_relationships`:

```rust
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
```

- [ ] **Step 3: Run branch fetch red test**

Run:

```bash
cargo test git_backend::tests::fetch_remote_fetches_branch_tip_to_tracking_ref -- --nocapture
```

Expected: FAIL with a panic containing `git backend error: fetch_remote_impl not wired`.

- [ ] **Step 4: Add tag fetch red test**

Append this test after the branch fetch test:

```rust
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
```

- [ ] **Step 5: Run tag fetch red test**

Run:

```bash
cargo test git_backend::tests::fetch_tags_fetches_remote_tags_without_force -- --nocapture
```

Expected: FAIL with a panic containing `git backend error: fetch_tags_impl not wired`.

- [ ] **Step 6: Add branch push red test**

Append this test after the tag fetch test:

```rust
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
```

- [ ] **Step 7: Run branch push red test**

Run:

```bash
cargo test git_backend::tests::push_remote_pushes_branch_without_force -- --nocapture
```

Expected: FAIL with a panic containing `git backend error: push_impl not wired`.

- [ ] **Step 8: Add tag push red test**

Append this test after the branch push test:

```rust
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
    let object = backend.repo().unwrap().find_object(commit, None).unwrap();
    backend
        .repo()
        .unwrap()
        .tag_lightweight("v1", &object, false)
        .unwrap();
    let remote = remote_config("origin", path_url(&remote_dir));

    backend.push_tags(&remote).unwrap();

    let remote_repo = git2::Repository::open_bare(&remote_dir).unwrap();
    assert_eq!(remote_repo.refname_to_id("refs/tags/v1").unwrap(), commit);
}
```

- [ ] **Step 9: Run tag push red test**

Run:

```bash
cargo test git_backend::tests::push_tags_pushes_local_tags_without_force -- --nocapture
```

Expected: FAIL with a panic containing `git backend error: push_impl not wired`.

---

### Task 3: Implement git2 fetch, tag fetch, and push helpers

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/git_backend.rs`

- [ ] **Step 1: Enable git2 HTTPS support**

In `Cargo.toml`, replace:

```toml
git2 = "0.21.0"
```

with:

```toml
git2 = { version = "0.21.0", features = ["https"] }
```

- [ ] **Step 2: Import `ResolvedCredentials`**

At the top of `src/git_backend.rs`, replace:

```rust
use crate::config::{AppConfig, RemoteConfig};
use crate::error::{CodeSyncError, Result};
use crate::sync::GitBackend;
```

with:

```rust
use crate::config::{AppConfig, RemoteConfig};
use crate::credentials::ResolvedCredentials;
use crate::error::{CodeSyncError, Result};
use crate::sync::GitBackend;
```

- [ ] **Step 3: Make fetch ref helpers available to production code**

In `src/git_backend.rs`, replace this block:

```rust
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
```

with:

```rust
fn remote_branch_ref(remote: &str, branch: &str) -> String {
    format!("refs/remotes/{remote}/{branch}")
}

fn branch_fetch_refspec(remote: &str, branch: &str) -> String {
    format!("+refs/heads/{branch}:{}", remote_branch_ref(remote, branch))
}

fn tag_fetch_refspec() -> &'static str {
    "refs/tags/*:refs/tags/*"
}
```

- [ ] **Step 4: Replace placeholder helpers with real git2 helpers**

In `src/git_backend.rs`, replace the placeholder functions from `fetch_remote_impl` through `push_impl`:

```rust
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
```

with:

```rust
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
    Ok(callbacks)
}

fn fetch_options(remote: &RemoteConfig) -> Result<git2::FetchOptions<'static>> {
    let mut options = git2::FetchOptions::new();
    options.remote_callbacks(callbacks(remote)?);
    options.download_tags(git2::AutotagOption::None);
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
    let mut options = fetch_options(remote)?;
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
    let mut options = fetch_options(remote)?;
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
}

fn push_impl(repo: &git2::Repository, remote: &RemoteConfig, refspecs: &[String]) -> Result<()> {
    let mut options = push_options(remote)?;
    with_anonymous_remote(repo, remote, |handle| {
        let refs = refspecs.iter().map(String::as_str).collect::<Vec<_>>();
        handle.push(&refs, Some(&mut options)).map_err(git_error)?;
        Ok(())
    })
}
```

- [ ] **Step 5: Run the new backend tests**

Run:

```bash
cargo test git_backend::tests::fetch_remote_fetches_branch_tip_to_tracking_ref -- --nocapture
cargo test git_backend::tests::fetch_tags_fetches_remote_tags_without_force -- --nocapture
cargo test git_backend::tests::push_remote_pushes_branch_without_force -- --nocapture
cargo test git_backend::tests::push_tags_pushes_local_tags_without_force -- --nocapture
```

Expected: all PASS.

- [ ] **Step 6: If local path remotes invoke the credential callback, adjust unauthenticated callback behavior**

Only if one of the local path tests fails with `credentials are not configured`, change the no-credential branch in `callbacks` from:

```rust
let Some(credentials) = credentials.as_ref() else {
    return Err(git2::Error::from_str("credentials are not configured"));
};
```

to:

```rust
let Some(credentials) = credentials.as_ref() else {
    if allowed.contains(git2::CredentialType::DEFAULT) {
        return git2::Cred::default();
    }
    return Err(git2::Error::from_str("credentials are not configured"));
};
```

Then re-run the four backend tests from Step 5. Expected: all PASS.

Do not make this adjustment unless the tests prove local remotes need it; prefer not to consult system/default credentials for normal HTTPS authentication.

---

### Task 4: Verify backend safety and whole crate

**Files:**
- No new files.
- Verify: `Cargo.lock` may change because `git2` HTTPS enables transitive dependencies.

- [ ] **Step 1: Run all backend tests**

Run:

```bash
cargo test git_backend::tests -- --nocapture
```

Expected: all backend tests PASS.

- [ ] **Step 2: Run all config tests**

Run:

```bash
cargo test config::tests -- --nocapture
```

Expected: all config tests PASS, including:

- `config_example_parses_with_empty_ssh_command_env`
- `missing_ssh_command_env_is_allowed`
- `rejects_ssh_command_env`
- `remote_top_level_non_empty_ssh_command_env_is_unsupported`

- [ ] **Step 3: Run full test suite**

Run:

```bash
cargo test
```

Expected: all tests PASS.

- [ ] **Step 4: Run clippy**

Run:

```bash
cargo clippy --lib --tests -- -D warnings
```

Expected: PASS with no warnings.

- [ ] **Step 5: Check no placeholder stub messages remain in production backend code**

Run:

```bash
rg -n "fetch_remote_impl not wired|fetch_tags_impl not wired|push_impl not wired" src
```

Expected: no matches.

- [ ] **Step 6: Review changed files**

Run:

```bash
git status --short
git diff -- Cargo.toml config.example.json src/config.rs src/git_backend.rs
```

Expected changed files:

- `Cargo.toml`
- `Cargo.lock` if Cargo resolved new feature dependencies
- `src/config.rs`
- `src/git_backend.rs`
- `config.example.json` only if it needed an empty `ssh_command_env` correction

Do not commit unless the user explicitly asks for a commit.

---

## Self-review

- Spec coverage: Task 1 covers HTTPS-only `ssh_command_env` compatibility and clear rejection for non-empty values. Tasks 2 and 3 cover git2 branch fetch, tag fetch, branch push, tag push, credentials callbacks, anonymous remotes, and HTTPS feature enablement. Task 4 covers final verification.
- Placeholder scan: The plan has no TBD/TODO/later placeholders. The conditional callback adjustment is fully specified and tied to a concrete observed failure.
- Type consistency: All code uses existing `Git2Backend`, `RemoteConfig { name, url, credential, role }`, `CredentialConfig { username_env, password_env, use_http_path }`, `Result`, and `CodeSyncError` names.
