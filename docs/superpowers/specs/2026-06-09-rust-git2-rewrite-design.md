# Rust + git2 Rewrite Design

Date: 2026-06-09

## Supersedes

This document supersedes `docs/superpowers/specs/2026-06-08-rust-gix-rewrite-design.md` for the Git backend choice.

During implementation planning we verified that the current `gix` / gitoxide stack supports fetch-oriented operations but does not provide a complete push implementation. The `gitoxide` feature list still marks `push` as not implemented. Because CodeSync's core behavior requires pushing the converged branch and tags back to both remotes, the rewrite will use `git2` / libgit2 for all Git operations instead of `gix`.

## Goal

Rewrite CodeSync from Python plus `git` subprocesses into a Rust application backed by `git2` / libgit2. The runtime must not invoke the `git` binary and must not generate or execute Python. The existing `config.json` shape remains the public configuration interface for now, but exact Python CLI/HTTP output compatibility is not required.

## Confirmed scope

- Keep both operating modes:
  - one-shot sync from the CLI;
  - long-running HTTP webhook service.
- Keep the existing `config.json` structure parseable.
- Support exactly two remotes.
- Support one configured branch and all tags.
- Keep fast-forward-only branch convergence.
- Keep tag conflict safety: do not force-overwrite conflicting tags.
- Support HTTPS remotes authenticated with username/password/token environment variables.
- Target Linux and Windows.
- Do not use Python in source, tests, generated helper scripts, or development helper scripts.
- Do not call the `git` binary at runtime.
- Do not use `gix` in the final implementation.

## Explicit non-goals

- SSH remote support.
- Git credential helper support.
- Exact compatibility with every CLI flag, HTTP response body, or log line from the Python service.
- Multiple branch synchronization.
- Force-push or conflict-resolution workflows.
- Using Python for tests or fixtures.

Unsupported config fields remain parseable when empty or absent. If a config actually requests an unsupported feature, the Rust service returns a clear configuration error rather than silently ignoring it.

## Architecture

The Rust application remains split into focused modules:

- `main`: parses CLI arguments, loads configuration, initializes logging, and selects one-shot sync or HTTP server mode.
- `config`: deserializes and validates the current JSON config shape.
- `credentials`: resolves HTTPS username/password values from configured environment variable names and redacts URLs for logs/errors.
- `git_backend`: implements the `sync::GitBackend` trait using `git2` / libgit2 only.
- `sync`: orchestrates locking, fetch, fast-forward selection, local ref update, and push through the backend trait.
- `http`: provides a small blocking HTTP server with `/healthz` and the configured webhook path.
- `lock`: provides a cross-platform file lock.
- `error`: defines typed user-facing errors and HTTP status mapping.

This preserves the backend abstraction already implemented in `sync.rs`, but the concrete backend is now `Git2Backend` rather than `GixBackend`.

## git2 backend behavior

The backend uses `git2` APIs for all Git operations:

- initialize or open a bare repository at `repo_dir`;
- create transient in-memory remotes for configured URLs;
- provide HTTPS credentials through `git2::RemoteCallbacks::credentials` using resolved environment variables;
- fetch `refs/heads/<branch>` into `refs/remotes/<remote>/<branch>`;
- fetch all tags into `refs/tags/*` without force-updating existing conflicting tags;
- read and peel local refs to commits;
- check ancestry with libgit2 graph APIs;
- update `refs/heads/<branch>` to the selected commit;
- push `refs/heads/<branch>:refs/heads/<branch>` to each remote;
- push `refs/tags/*:refs/tags/*` to each remote without force.

The backend must not read global Git credential helpers, write credential stores, or prompt interactively. If credentials are configured, they come only from the configured environment variable names. If credentials are absent, operations proceed unauthenticated.

## Synchronization data flow

For every sync request:

1. Acquire the cross-process file lock under `state_dir`.
2. Ensure `state_dir` and `repo_dir` exist.
3. Open or initialize the bare repository at `repo_dir`.
4. If local `refs/heads/<branch>` exists, include it as a candidate tip.
5. For each configured remote:
   - fetch `refs/heads/<branch>` into `refs/remotes/<remote>/<branch>`;
   - fetch all tags into `refs/tags/*`;
   - record the fetched branch tip commit.
6. Choose a single fast-forward target commit:
   - deduplicate tips;
   - find a candidate tip that every other tip can reach by ancestry;
   - if no such candidate exists, fail with a conflict error and do not push.
7. Update local `refs/heads/<branch>` to the selected target.
8. Push the branch to each remote without force.
9. Push tags to each remote without force.
10. Return a structured sync result.

## Error handling

Errors remain typed as:

- `Config`: invalid config, unsupported configured feature, or missing secret env var.
- `Credential`: missing configured credential env vars or invalid credential state.
- `GitBackend`: libgit2 repository/network/ref/push/fetch errors.
- `Conflict`: branch tips cannot fast-forward converge.
- `Http`: request parsing/body/secret errors.

User-facing errors and logs must redact URL userinfo and must not print credential values.

## Testing strategy

Use Rust tests only; no Python fixtures or scripts.

Existing unit tests for config, credentials, webhook, lock, and sync orchestration remain valid. Add git2 backend tests for:

- refname/refspec helper generation;
- credential callback behavior where practical without contacting a network;
- repository initialization/opening;
- local branch tip lookup;
- ancestry checks;
- local ref update;
- fetch/push behavior against local bare repositories using libgit2-supported local paths or `file://` URLs, if available without invoking the `git` binary.

If full HTTPS network tests are impractical locally, keep network operations behind the backend methods and cover local repository behavior plus sync orchestration with fake backends.

## Migration and repository changes

Expected repository changes from the current in-progress Rust branch:

- Replace the `gix` dependency with `git2` in `Cargo.toml` and `Cargo.lock`.
- Rename or reinterpret `src/git_backend.rs` as the git2 backend implementation.
- Ensure docs refer to `git2` / libgit2, not gix.
- Remove the Python service implementation before final verification.
- Keep `config.example.json` shape unchanged.

## Known trade-off

`git2` is a binding to libgit2, so it introduces a C library dependency through the crate build. This is acceptable because it still avoids the `git` binary and supports the required push functionality that gix currently lacks.
