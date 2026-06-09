# Rust + gix Rewrite Design

Date: 2026-06-08

## Goal

Rewrite CodeSync from Python plus `git` subprocesses into a Rust application backed by the `gix` crate. The runtime must not invoke the `git` binary and must not generate or execute Python. The existing `config.json` shape remains the public configuration interface for now, but external CLI/HTTP behavior may follow Rust conventions instead of matching the Python implementation exactly.

## Confirmed scope

- Keep both operating modes:
  - one-shot sync from the CLI
  - long-running HTTP webhook service
- Keep the existing `config.json` structure parseable.
- Support exactly two remotes.
- Support one configured branch and all tags.
- Keep fast-forward-only branch convergence.
- Keep tag conflict safety: do not force-overwrite conflicting tags.
- Support HTTPS remotes authenticated with username/password/token environment variables.
- Target Linux and Windows.
- Do not use Python in source, tests, generated helper scripts, or development helper scripts.
- Do not call the `git` binary at runtime.

## Explicit non-goals for this rewrite

- SSH remote support.
- Git credential helper support.
- Exact compatibility with every CLI flag, HTTP response body, or log line from the Python service.
- Multiple branch synchronization.
- Force-push or conflict-resolution workflows.
- Using Python for tests or fixtures.

Existing config fields for unsupported features remain part of the parsed schema so existing config files do not need a shape change. If a config actually requests an unsupported feature, the Rust service returns a clear configuration error rather than silently doing the wrong thing.

## Architecture

The Rust application is split into focused modules:

- `main`: parses CLI arguments, loads configuration, initializes logging, and selects one-shot sync or HTTP server mode.
- `config`: deserializes and validates the current JSON config shape. It preserves the current nested fields and legacy top-level aliases where practical.
- `credentials`: resolves HTTPS username/password values from configured environment variable names and adapts them to `gix` authentication callbacks.
- `git_backend`: wraps `gix` operations behind a project-specific interface. It owns repository initialization/opening, remote fetch, push, reference lookup/update, tag transfer, and ancestry checks.
- `sync`: orchestrates the synchronization algorithm. It is independent of HTTP details and most `gix` API details.
- `http`: provides a small blocking HTTP server with `/healthz` and the configured webhook path.
- `lock`: provides an in-process mutex plus a cross-platform file lock for Linux and Windows.
- `errors`: defines typed user-facing errors and HTTP status mapping.

This keeps the HTTP server, config parsing, and Git backend independently testable. Future SSH or credential-helper support should mainly extend `credentials` and `git_backend`.

## Configuration behavior

The existing shape remains:

- `listen.host`, `listen.port`
- `webhook.path`, `webhook.secret`, `webhook.secret_env`, `webhook.max_body_bytes`
- `repo_dir`, `state_dir`
- `branch`
- `git.timeout_seconds`
- `credential.username_env`, `credential.password_env`, `credential.helper`, `credential.ssh_command_env`, `credential.use_http_path`
- per-remote `name`, `url`, and optional credential overrides
- legacy top-level aliases used by the Python version where they are straightforward: `listen_host`, `listen_port`, `webhook_path`, `git_timeout_seconds`

Validation rules:

- Config root must be an object.
- `repo_dir` is required.
- `remotes` must contain exactly two remotes.
- Remote names must match the existing safe-name rule: letters, numbers, `.`, `_`, `-`.
- Remote URLs must be HTTPS URLs for this rewrite.
- Branch names use the current conservative validation rule.
- Username and password env names must be configured together if either is configured.
- If `credential.helper` is non-empty, return unsupported configuration error.
- If `ssh_command_env` is set or a remote URL is SSH-like, return unsupported configuration error.
- `git.timeout_seconds` may be parsed for interface compatibility, but gix operations are not guaranteed to support identical timeout behavior in the first implementation. If no reliable timeout can be enforced, document this in README rather than implying parity.

## Synchronization data flow

For every sync request:

1. Acquire the process-local mutex.
2. Acquire the cross-process file lock under `state_dir`.
3. Ensure `state_dir` and `repo_dir` exist.
4. Open the bare repository at `repo_dir`, or initialize a new bare repository there.
5. For each configured remote:
   - fetch `refs/heads/<branch>` into `refs/remotes/<remote>/<branch>`;
   - fetch all tags into `refs/tags/*` without force-updating conflicting tags;
   - record the fetched branch tip commit.
6. If local `refs/heads/<branch>` exists, include it as a candidate tip.
7. Choose a single fast-forward target commit:
   - deduplicate tips;
   - find a candidate tip that every other tip can reach by ancestry;
   - if no such candidate exists, fail with a conflict error and do not push.
8. Update local `refs/heads/<branch>` to the selected target.
9. Push `refs/heads/<branch>` to each remote.
10. Push all tags to each remote without force.
11. Return a structured sync result containing status, branch, target commit, elapsed time, and remote names.

The branch refspec intentionally force-updates only the local tracking ref during fetch, matching the old service's behavior of making local remote-tracking refs reflect the remote source. Remote pushes remain non-force.

## HTTP behavior

The HTTP server is intentionally simple and blocking.

- `GET /healthz` returns a JSON health response.
- `POST <webhook.path>` checks body size, verifies the webhook secret, then runs one sync.
- Only one sync runs at a time because the sync layer holds the mutex and file lock.
- Requests arriving during another sync wait for the lock rather than running concurrently.
- Unsupported paths return 404 JSON.
- Invalid or missing secret returns 401 JSON.
- Oversized body returns 413 JSON.
- Divergent branch histories return 409 JSON.
- Configuration, repository, network, and push/fetch failures return 500 JSON.

Webhook secret compatibility:

- If no secret is configured, allow the request.
- Accept `X-CodeSync-Token: <secret>`.
- Accept `Authorization: Bearer <secret>`.
- Accept GitHub/Gitea style `X-Hub-Signature-256: sha256=<hmac>` computed over the raw request body.

## CLI behavior

The Rust CLI should support these minimum modes:

- `--config <path>` or `CODESYNC_CONFIG`, defaulting to `config.json`.
- `--once` to run one sync and print JSON.
- `--log-level <level>` or `CODESYNC_LOG_LEVEL` for logging.

Exact argparse wording from the Python implementation is not required.

## gix backend design

The backend must use `gix`/gitoxide APIs for all Git operations:

- create/open bare repositories;
- connect to HTTPS remotes;
- provide credentials through a `gix` credentials callback rather than `git-askpass`;
- fetch branch and tag refspecs;
- inspect refs and peel branch refs to commits;
- check ancestry/merge-base relationships;
- update local refs;
- push branch and tags.

The backend should hide concrete `gix` API shapes from `sync`. If a specific high-level `gix` operation proves awkward, it is acceptable to use lower-level gitoxide crates that are part of the same ecosystem, but still no `git` subprocesses.

For authentication, the backend supplies username/password values from environment variables directly to gix. It must not read global Git credential helpers, write credential stores, or prompt interactively.

## Error handling

Errors should be typed enough to map correctly:

- `ConfigError`: invalid config, unsupported configured feature, or missing credential environment variables.
- `AuthError`: authentication callback could not supply required HTTPS credentials.
- `GitBackendError`: gix repository/network/ref/push/fetch errors.
- `SyncConflict`: branch tips cannot fast-forward converge.
- `HttpError`: request parsing/body/secret errors.

User-facing errors should redact credentials embedded in URLs. Logs should also use redacted URLs.

## Testing strategy

Use Rust tests only; no Python fixtures or scripts.

Unit tests:

- config parsing and validation;
- credential merge behavior and unsupported-feature detection;
- branch and remote name validation;
- webhook secret verification, including HMAC;
- fast-forward target selection using controlled repository fixtures;
- URL redaction.

Integration tests:

- create temporary bare repositories with gix;
- seed commits/tags using gix or gitoxide APIs only;
- run sync against local repositories through file URLs only if the backend supports local transports without violating the HTTPS runtime scope, otherwise test local backend behavior through repository-level functions;
- verify fast-forward convergence updates both repositories;
- verify divergent histories fail;
- verify tag conflict fails;
- verify HTTP health and webhook paths.

If full push/fetch test coverage requires gix transport behavior that is difficult to exercise locally without a Git server, keep the backend interface thin and cover the orchestration with a fake backend plus targeted gix integration tests. Do not introduce Python test helpers.

## Migration and repository changes

Expected repository changes:

- Add `Cargo.toml` and `Cargo.lock`.
- Add Rust source under `src/`.
- Replace or retire `codesync_server.py`.
- Update `.gitignore` for Rust build artifacts.
- Update README commands from Python to Cargo/binary usage.
- Keep `config.example.json` shape unchanged, but document that only HTTPS environment-variable credentials are supported in this rewrite.

Whether to delete `codesync_server.py` immediately or leave it as a temporary reference is an implementation-plan decision. The final state should not ship Python as the service implementation.

## Open implementation risks

- `gix` push/fetch APIs and authentication callbacks may require feature flags and exact API adaptation.
- Enforcing `git.timeout_seconds` exactly may need additional cancellation plumbing or may be deferred with explicit documentation.
- Local integration tests for network fetch/push may need careful setup without using the `git` binary or Python.

These risks are implementation details, not reasons to change the requested direction.
