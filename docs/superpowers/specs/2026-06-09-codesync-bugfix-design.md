# CodeSync HTTPS-only Config and Git2 Backend Bugfix Design

Date: 2026-06-09

## Goal

Fix two runtime issues in the Rust CodeSync implementation:

1. Existing HTTPS-only deployments should not fail because `credential.ssh_command_env` is missing or explicitly empty.
2. One-shot and webhook sync should no longer fail with `git backend error: fetch_remote_impl not wired`.

The fix preserves the current Rust rewrite scope: HTTPS remotes are supported, SSH remotes are not supported, and the runtime must not invoke the `git` binary.

## Confirmed behavior

- `credential.ssh_command_env` remains part of the accepted JSON schema for compatibility with older config files.
- Missing, `null`, or empty-string `ssh_command_env` values are treated as disabled and must parse successfully.
- A non-empty `ssh_command_env` value is still rejected with a clear unsupported-configuration error because SSH support is out of scope for this Rust version.
- Remote URLs continue to require `https://`.
- Git operations use `git2`/libgit2 directly, not subprocesses.

## Changes

### Config compatibility

Keep the existing `RawCredentialString` merge semantics in `src/config.rs`: nested remote credential values override legacy top-level remote values, which override global credentials. Empty strings and `null` clear inherited values.

Add regression coverage for the selected HTTPS-only behavior:

- Base/example-style config with empty `ssh_command_env` parses.
- Missing `ssh_command_env` parses.
- Non-empty global or remote `ssh_command_env` still returns `CodeSyncError::Unsupported`.

If any example config still contains a non-empty `ssh_command_env`, change it to `""` so the documented default is runnable for HTTPS deployments.

### Git2 backend wiring

Replace the three placeholder helpers in `src/git_backend.rs`:

- `fetch_remote_impl`
- `fetch_tags_impl`
- `push_impl`

with real git2 operations using anonymous remotes against each configured URL.

Fetch behavior:

- Fetch `refs/heads/<branch>` into `refs/remotes/<remote>/<branch>` with a force-update local tracking refspec.
- Return the fetched tracking ref object id as a string.
- Fetch tags with `refs/tags/*:refs/tags/*` without force, preserving tag conflict safety.

Push behavior:

- Push `refs/heads/<branch>:refs/heads/<branch>` without force.
- Push `refs/tags/*:refs/tags/*` without force.
- Surface libgit2 rejection/errors through `CodeSyncError::GitBackend`.

Credential behavior:

- Reuse `ResolvedCredentials::from_config` for HTTPS username/password environment variables.
- Configure git2 remote callbacks for username/password and username credential requests.
- If credentials are not configured, allow local/path remotes used by tests and unauthenticated HTTPS remotes to proceed without forcing a credential callback failure.

## Testing plan

Follow TDD for the implementation:

1. Add local bare-repository integration tests in `src/git_backend.rs` that fail with the current stub messages:
   - fetching a branch writes the remote-tracking ref and returns its tip;
   - fetching tags transfers a tag without force;
   - pushing a branch updates the destination bare repo;
   - pushing tags transfers local tags.
2. Add config regression tests only where current coverage is missing for the approved HTTPS-only behavior.
3. Run the new tests and verify they fail for the expected reasons before implementation.
4. Implement the smallest backend and config/example changes to make those tests pass.
5. Run targeted backend/config tests, then full `cargo test` and clippy.

## Non-goals

- Do not add SSH URL support.
- Do not make `ssh_command_env` silently active or silently ignored when non-empty.
- Do not reintroduce Python or `git` subprocess execution.
- Do not add force-push or force-tag behavior.

## Self-review

- No placeholders or TODOs remain.
- Scope is limited to the two reported issues.
- The selected behavior matches the approved option B: HTTPS-only with clear rejection for actual SSH command configuration.
- Testing covers both the configuration compatibility boundary and the git2 backend wiring boundary.
