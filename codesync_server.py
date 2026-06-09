#!/usr/bin/env python3
from __future__ import annotations

import argparse
import dataclasses
import hashlib
import hmac
import json
import logging
import os
import re
import subprocess
import sys
import threading
import time
import uuid
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit


LOG = logging.getLogger("codesync")
REMOTE_NAME_RE = re.compile(r"^[A-Za-z0-9._-]+$")
UNSAFE_LOCAL_CONFIG_RE = r"^(url\..*\.(insteadof|pushinsteadof)|include\..*|includeif\..*)$"


class ConfigError(Exception):
    pass


class GitError(Exception):
    def __init__(self, message: str, *, returncode: int | None = None) -> None:
        super().__init__(message)
        self.returncode = returncode


class SyncError(Exception):
    pass


@dataclasses.dataclass(frozen=True)
class CredentialConfig:
    username_env: str | None = None
    password_env: str | None = None
    helper: str | None = None
    ssh_command_env: str | None = None
    use_http_path: bool = True


@dataclasses.dataclass(frozen=True)
class RemoteConfig:
    name: str
    url: str
    credential: CredentialConfig
    role: str | None = None


@dataclasses.dataclass(frozen=True)
class WebhookConfig:
    path: str = "/webhook"
    secret: str | None = None
    max_body_bytes: int = 1024 * 1024


@dataclasses.dataclass(frozen=True)
class AppConfig:
    listen_host: str
    listen_port: int
    repo_dir: Path
    state_dir: Path
    branch: str
    remotes: tuple[RemoteConfig, ...]
    webhook: WebhookConfig
    git_timeout_seconds: int


@dataclasses.dataclass(frozen=True)
class GitResult:
    args: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str


def _as_mapping(value: Any, name: str) -> dict[str, Any]:
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise ConfigError(f"{name} must be an object")
    return value


def _optional_str(value: Any, name: str) -> str | None:
    if value is None or value == "":
        return None
    if not isinstance(value, str):
        raise ConfigError(f"{name} must be a string")
    return value


def _load_secret(webhook_obj: dict[str, Any]) -> str | None:
    secret = _optional_str(webhook_obj.get("secret"), "webhook.secret")
    secret_env = _optional_str(webhook_obj.get("secret_env"), "webhook.secret_env")
    if secret_env:
        secret = os.environ.get(secret_env)
        if not secret:
            raise ConfigError(f"webhook.secret_env {secret_env!r} is not set")
    return secret


def _merge_credential(default_obj: dict[str, Any], remote_obj: dict[str, Any]) -> CredentialConfig:
    remote_credential = _as_mapping(remote_obj.get("credential"), "remote.credential")
    merged = {**default_obj, **remote_credential}

    for key in ("username_env", "password_env", "helper", "ssh_command_env"):
        if key in remote_obj and key not in remote_credential:
            merged[key] = remote_obj[key]

    username_env = _optional_str(merged.get("username_env"), "credential.username_env")
    password_env = _optional_str(merged.get("password_env"), "credential.password_env")
    helper = _optional_str(merged.get("helper"), "credential.helper")
    ssh_command_env = _optional_str(merged.get("ssh_command_env"), "credential.ssh_command_env")
    use_http_path = bool(merged.get("use_http_path", True))

    if (username_env and not password_env) or (password_env and not username_env):
        raise ConfigError("credential.username_env and credential.password_env must be set together")

    return CredentialConfig(
        username_env=username_env,
        password_env=password_env,
        helper=helper,
        ssh_command_env=ssh_command_env,
        use_http_path=use_http_path,
    )


def _validate_remote_name(name: str) -> None:
    if not REMOTE_NAME_RE.match(name):
        raise ConfigError(
            f"remote name {name!r} is invalid; use letters, numbers, '.', '_' or '-'"
        )


def _validate_branch_name(branch: str) -> None:
    if not branch or branch.startswith("/") or branch.endswith("/"):
        raise ConfigError("branch must be a non-empty branch name")
    if branch in {".", "..", "@{", "HEAD"}:
        raise ConfigError(f"branch {branch!r} is invalid")
    forbidden = ["..", "\\", " ", "~", "^", ":", "?", "*", "[", "//"]
    if any(part in branch for part in forbidden):
        raise ConfigError(f"branch {branch!r} is invalid")


def _validate_remote_role(role: str | None, index: int) -> str | None:
    if role is None or role == "":
        return None
    if not isinstance(role, str):
        raise ConfigError(f"remotes[{index}].role must be a string")
    role = role.strip()
    if role in {"", "master"}:
        return role or None
    raise ConfigError(f"remotes[{index}].role {role!r} is invalid; supported role is 'master'")


def load_config(path: Path) -> AppConfig:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise ConfigError(f"config file not found: {path}") from exc
    except json.JSONDecodeError as exc:
        raise ConfigError(f"invalid JSON in {path}: {exc}") from exc

    if not isinstance(raw, dict):
        raise ConfigError("config root must be an object")

    listen_obj = _as_mapping(raw.get("listen"), "listen")
    webhook_obj = _as_mapping(raw.get("webhook"), "webhook")
    credential_obj = _as_mapping(raw.get("credential"), "credential")

    listen_host = str(raw.get("listen_host") or listen_obj.get("host") or "0.0.0.0")
    listen_port = int(raw.get("listen_port") or listen_obj.get("port") or 8080)

    repo_dir_raw = raw.get("repo_dir")
    if not repo_dir_raw:
        raise ConfigError("repo_dir is required")
    repo_dir = Path(str(repo_dir_raw)).expanduser().resolve()
    state_dir = Path(str(raw.get("state_dir") or repo_dir.parent)).expanduser().resolve()

    branch = str(raw.get("branch") or "master")
    _validate_branch_name(branch)

    remotes_raw = raw.get("remotes")
    if not isinstance(remotes_raw, list) or len(remotes_raw) < 2:
        raise ConfigError("remotes must contain at least two remote objects")

    remotes: list[RemoteConfig] = []
    seen_names: set[str] = set()
    for index, remote_raw in enumerate(remotes_raw):
        remote_obj = _as_mapping(remote_raw, f"remotes[{index}]")
        name = str(remote_obj.get("name") or "").strip()
        url = str(remote_obj.get("url") or "").strip()
        if not name or not url:
            raise ConfigError(f"remotes[{index}] requires name and url")
        _validate_remote_name(name)
        if name in seen_names:
            raise ConfigError(f"duplicate remote name: {name}")
        seen_names.add(name)
        role = _validate_remote_role(remote_obj.get("role"), index)
        remotes.append(
            RemoteConfig(
                name=name,
                url=url,
                credential=_merge_credential(credential_obj, remote_obj),
                role=role,
            )
        )

    webhook_path = str(raw.get("webhook_path") or webhook_obj.get("path") or "/webhook")
    if not webhook_path.startswith("/"):
        raise ConfigError("webhook.path must start with '/'")

    webhook = WebhookConfig(
        path=webhook_path,
        secret=_load_secret(webhook_obj),
        max_body_bytes=int(webhook_obj.get("max_body_bytes") or 1024 * 1024),
    )

    git_obj = _as_mapping(raw.get("git"), "git")
    git_timeout_seconds = int(git_obj.get("timeout_seconds") or raw.get("git_timeout_seconds") or 300)

    return AppConfig(
        listen_host=listen_host,
        listen_port=listen_port,
        repo_dir=repo_dir,
        state_dir=state_dir,
        branch=branch,
        remotes=tuple(remotes),
        webhook=webhook,
        git_timeout_seconds=git_timeout_seconds,
    )


def redact_url(value: str) -> str:
    try:
        parts = urlsplit(value)
    except ValueError:
        return value
    if not parts.scheme or "@" not in parts.netloc:
        return value
    host = parts.netloc.rsplit("@", 1)[1]
    return urlunsplit((parts.scheme, f"***@{host}", parts.path, parts.query, parts.fragment))


def short_sha(value: str) -> str:
    return value[:12]


@contextmanager
def exclusive_file_lock(path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+b") as lock_file:
        lock_file.seek(0, os.SEEK_END)
        if lock_file.tell() == 0:
            lock_file.write(b"\0")
            lock_file.flush()

        lock_file.seek(0)
        if os.name == "nt":
            import msvcrt

            while True:
                try:
                    lock_file.seek(0)
                    msvcrt.locking(lock_file.fileno(), msvcrt.LK_NBLCK, 1)
                    break
                except OSError:
                    time.sleep(0.1)
            try:
                yield
            finally:
                lock_file.seek(0)
                msvcrt.locking(lock_file.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


class GitRunner:
    def __init__(self, config: AppConfig) -> None:
        self.config = config
        self.askpass_script_path = config.state_dir / "git-askpass.py"
        self.askpass_path = config.state_dir / ("git-askpass.cmd" if os.name == "nt" else "git-askpass.py")
        self.isolated_config_path = config.state_dir / "isolated-gitconfig"

    def ensure_isolated_config(self) -> None:
        self.config.state_dir.mkdir(parents=True, exist_ok=True)
        content = "# CodeSync intentionally keeps this Git config empty.\n"
        current = self.isolated_config_path.read_text(encoding="utf-8") if self.isolated_config_path.exists() else None
        if current != content:
            self.isolated_config_path.write_text(content, encoding="utf-8")

    def ensure_askpass(self) -> None:
        self.config.state_dir.mkdir(parents=True, exist_ok=True)
        script_content = """#!/usr/bin/env python3
import os
import sys

prompt = sys.argv[1].lower() if len(sys.argv) > 1 else ""
if "username" in prompt:
    value = os.environ.get("CODESYNC_ASKPASS_USERNAME", "")
elif "password" in prompt:
    value = os.environ.get("CODESYNC_ASKPASS_PASSWORD", "")
else:
    value = ""
sys.stdout.write(value + "\\n")
"""
        current = self.askpass_script_path.read_text(encoding="utf-8") if self.askpass_script_path.exists() else None
        if current != script_content:
            self.askpass_script_path.write_text(script_content, encoding="utf-8")
            self.askpass_script_path.chmod(0o700)

        if os.name == "nt":
            wrapper_content = f'@echo off\n"{sys.executable}" "{self.askpass_script_path}" "%~1"\n'
            current_wrapper = self.askpass_path.read_text(encoding="utf-8") if self.askpass_path.exists() else None
            if current_wrapper != wrapper_content:
                self.askpass_path.write_text(wrapper_content, encoding="utf-8")
        else:
            self.askpass_path.chmod(0o700)

    def _env_for(self, credential: CredentialConfig | None) -> dict[str, str]:
        env = os.environ.copy()
        self.ensure_isolated_config()
        env["GIT_TERMINAL_PROMPT"] = "0"
        env["GIT_CONFIG_NOSYSTEM"] = "1"
        env["GIT_CONFIG_SYSTEM"] = str(self.isolated_config_path)
        env["GIT_CONFIG_GLOBAL"] = str(self.isolated_config_path)
        env["GIT_CONFIG_COUNT"] = "0"
        env.pop("GIT_CONFIG", None)
        env.pop("GIT_CONFIG_PARAMETERS", None)
        for key in list(env):
            if key.startswith(("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_")):
                env.pop(key, None)

        if not credential:
            return env

        if credential.username_env and credential.password_env:
            username = os.environ.get(credential.username_env)
            password = os.environ.get(credential.password_env)
            if username is None or password is None:
                raise ConfigError(
                    "credential environment variables are missing: "
                    f"{credential.username_env}, {credential.password_env}"
                )
            self.ensure_askpass()
            env["GIT_ASKPASS"] = str(self.askpass_path)
            env["SSH_ASKPASS"] = str(self.askpass_path)
            env["CODESYNC_ASKPASS_USERNAME"] = username
            env["CODESYNC_ASKPASS_PASSWORD"] = password

        if credential.ssh_command_env:
            ssh_command = os.environ.get(credential.ssh_command_env)
            if ssh_command:
                env["GIT_SSH_COMMAND"] = ssh_command

        return env

    def run(
        self,
        args: list[str],
        *,
        credential: CredentialConfig | None = None,
        check: bool = True,
    ) -> GitResult:
        full_args = ["git"]
        if credential:
            full_args.extend(["-c", "credential.helper="])
            full_args.extend(["-c", f"credential.useHttpPath={str(credential.use_http_path).lower()}"])
            if credential.helper:
                full_args.extend(["-c", f"credential.helper={credential.helper}"])
        full_args.extend(args)

        env = self._env_for(credential)
        log_args = " ".join(redact_url(arg) for arg in full_args)
        LOG.debug("running: %s", log_args)

        try:
            proc = subprocess.run(
                full_args,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=self.config.git_timeout_seconds,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:
            raise GitError(f"git command timed out after {self.config.git_timeout_seconds}s: {log_args}") from exc

        result = GitResult(
            args=tuple(full_args),
            returncode=proc.returncode,
            stdout=proc.stdout,
            stderr=proc.stderr,
        )
        if check and proc.returncode != 0:
            detail = (proc.stderr or proc.stdout or "").strip()
            raise GitError(
                f"git command failed ({proc.returncode}): {log_args}\n{detail}",
                returncode=proc.returncode,
            )
        return result


class SyncService:
    def __init__(self, config: AppConfig) -> None:
        self.config = config
        self.git_runner = GitRunner(config)
        self.memory_lock = threading.Lock()

    @contextmanager
    def _process_lock(self):
        with exclusive_file_lock(self.config.state_dir / "sync.lock"):
            yield

    def git(
        self,
        args: list[str],
        *,
        credential: CredentialConfig | None = None,
        check: bool = True,
    ) -> GitResult:
        return self.git_runner.run(["--git-dir", str(self.config.repo_dir), *args], credential=credential, check=check)

    def init_repo(self) -> None:
        if (self.config.repo_dir / "HEAD").exists():
            return
        self.config.repo_dir.parent.mkdir(parents=True, exist_ok=True)
        self.git_runner.run(["init", "--bare", str(self.config.repo_dir)])

    def configure_remotes(self) -> None:
        self.git(["symbolic-ref", "HEAD", f"refs/heads/{self.config.branch}"])
        for remote in self.config.remotes:
            existing = self.git(["remote", "get-url", remote.name], check=False)
            if existing.returncode == 0:
                if existing.stdout.strip() != remote.url:
                    self.git(["remote", "set-url", remote.name, remote.url])
            else:
                self.git(["remote", "add", remote.name, remote.url])

    def sanitize_local_config(self) -> None:
        result = self.git(
            [
                "config",
                "--local",
                "--no-includes",
                "--name-only",
                "--get-regexp",
                UNSAFE_LOCAL_CONFIG_RE,
            ],
            check=False,
        )
        if result.returncode == 1:
            return
        if result.returncode != 0:
            detail = (result.stderr or result.stdout or "").strip()
            raise GitError(f"failed to inspect local git config: {detail}", returncode=result.returncode)

        unsafe_keys = sorted(set(line.strip() for line in result.stdout.splitlines() if line.strip()))
        for key in unsafe_keys:
            LOG.warning("removing unsafe local git config: %s", key)
            unset = self.git(["config", "--local", "--unset-all", key], check=False)
            if unset.returncode != 0:
                detail = (unset.stderr or unset.stdout or "").strip()
                raise GitError(f"failed to remove unsafe local git config {key}: {detail}", returncode=unset.returncode)

    def fetch_remote(self, remote: RemoteConfig, *, force_tags: bool = False) -> str:
        branch = self.config.branch
        remote_ref = f"refs/remotes/{remote.name}/{branch}"
        branch_refspec = f"+refs/heads/{branch}:{remote_ref}"
        LOG.info("fetching %s/%s", remote.name, branch)
        self.git(
            ["fetch", "--prune", "--no-tags", remote.name, branch_refspec],
            credential=remote.credential,
        )
        LOG.info("fetching tags from %s", remote.name)
        tag_refspec = "refs/tags/*:refs/tags/*"
        fetch_tag_args = ["fetch"]
        if force_tags:
            tag_refspec = f"+{tag_refspec}"
            fetch_tag_args.append("--prune")
        fetch_tag_args.extend([remote.name, tag_refspec])
        self.git(fetch_tag_args, credential=remote.credential)
        return self.rev_parse_commit(remote_ref)

    def ref_exists(self, ref: str) -> bool:
        result = self.git(["rev-parse", "--verify", f"{ref}^{{commit}}"], check=False)
        return result.returncode == 0

    def rev_parse_commit(self, ref: str) -> str:
        result = self.git(["rev-parse", "--verify", f"{ref}^{{commit}}"])
        return result.stdout.strip()

    def is_ancestor(self, older: str, newer: str) -> bool:
        result = self.git(["merge-base", "--is-ancestor", older, newer], check=False)
        if result.returncode == 0:
            return True
        if result.returncode == 1:
            return False
        detail = (result.stderr or result.stdout or "").strip()
        raise GitError(f"merge-base failed: {detail}", returncode=result.returncode)

    def select_fast_forward_target(self, labeled_tips: list[tuple[str, str]]) -> str:
        unique_tips: list[str] = []
        for _, tip in labeled_tips:
            if tip not in unique_tips:
                unique_tips.append(tip)

        for candidate in unique_tips:
            if all(other == candidate or self.is_ancestor(other, candidate) for other in unique_tips):
                return candidate

        detail = ", ".join(f"{label}={short_sha(tip)}" for label, tip in labeled_tips)
        raise SyncError(f"{self.config.branch} has divergent histories; refusing non-fast-forward sync: {detail}")

    def update_local_branch(self, target: str) -> None:
        branch_ref = f"refs/heads/{self.config.branch}"
        LOG.info("updating local %s to %s", self.config.branch, short_sha(target))
        self.git(["update-ref", branch_ref, target])

    def push_remote(self, remote: RemoteConfig, target: str, *, force: bool = False) -> None:
        branch = self.config.branch
        action = "force-pushing" if force else "pushing"
        LOG.info("%s %s to %s/%s", action, short_sha(target), remote.name, branch)
        push_args = ["push"]
        if force:
            push_args.append("--force")
        push_args.extend([remote.name, f"refs/heads/{branch}:refs/heads/{branch}"])
        self.git(push_args, credential=remote.credential)

        LOG.info("%s tags to %s", "force-pushing" if force else "pushing", remote.name)
        tag_args = ["push"]
        if force:
            tag_args.extend(["--force", "--prune", remote.name, "refs/tags/*:refs/tags/*"])
        else:
            tag_args.extend([remote.name, "--tags"])
        self.git(tag_args, credential=remote.credential)

    def master_remote(self) -> RemoteConfig:
        masters = [remote for remote in self.config.remotes if remote.role == "master"]
        if len(masters) != 1:
            raise ConfigError("--once --force requires exactly one remote with role='master'")
        return masters[0]

    def sync(self, reason: str = "webhook") -> dict[str, Any]:
        sync_id = uuid.uuid4().hex[:12]
        started = time.time()
        LOG.info("sync %s started (%s)", sync_id, reason)

        with self.memory_lock:
            with self._process_lock():
                self.init_repo()
                self.sanitize_local_config()
                self.configure_remotes()

                tips: list[tuple[str, str]] = []
                local_ref = f"refs/heads/{self.config.branch}"
                if self.ref_exists(local_ref):
                    tips.append(("local", self.rev_parse_commit(local_ref)))

                for remote in self.config.remotes:
                    remote_tip = self.fetch_remote(remote)
                    tips.append((remote.name, remote_tip))

                target = self.select_fast_forward_target(tips)
                self.update_local_branch(target)

                for remote in self.config.remotes:
                    self.push_remote(remote, target)

        elapsed_ms = round((time.time() - started) * 1000)
        LOG.info("sync %s finished in %sms at %s", sync_id, elapsed_ms, short_sha(target))
        return {
            "id": sync_id,
            "status": "ok",
            "branch": self.config.branch,
            "target": target,
            "elapsed_ms": elapsed_ms,
            "remotes": [remote.name for remote in self.config.remotes],
        }

    def force_from_master(self, reason: str = "manual-force") -> dict[str, Any]:
        sync_id = uuid.uuid4().hex[:12]
        started = time.time()
        source = self.master_remote()
        targets = [remote for remote in self.config.remotes if remote.name != source.name]
        LOG.info("force sync %s started (%s) from %s", sync_id, reason, source.name)

        with self.memory_lock:
            with self._process_lock():
                self.init_repo()
                self.sanitize_local_config()
                self.configure_remotes()

                target = self.fetch_remote(source, force_tags=True)
                self.update_local_branch(target)

                for remote in targets:
                    self.push_remote(remote, target, force=True)

        elapsed_ms = round((time.time() - started) * 1000)
        LOG.info(
            "force sync %s finished in %sms from %s at %s",
            sync_id,
            elapsed_ms,
            source.name,
            short_sha(target),
        )
        return {
            "id": sync_id,
            "status": "ok",
            "mode": "force",
            "branch": self.config.branch,
            "source": source.name,
            "target": target,
            "elapsed_ms": elapsed_ms,
            "remotes": [remote.name for remote in targets],
        }


def verify_webhook_secret(config: WebhookConfig, headers: Any, body: bytes) -> bool:
    if not config.secret:
        return True

    token = headers.get("X-CodeSync-Token")
    if token and hmac.compare_digest(token, config.secret):
        return True

    authorization = headers.get("Authorization", "")
    prefix = "Bearer "
    if authorization.startswith(prefix) and hmac.compare_digest(authorization[len(prefix) :], config.secret):
        return True

    github_signature = headers.get("X-Hub-Signature-256", "")
    if github_signature.startswith("sha256="):
        digest = hmac.new(config.secret.encode("utf-8"), body, hashlib.sha256).hexdigest()
        return hmac.compare_digest(github_signature, f"sha256={digest}")

    return False


def write_json(handler: BaseHTTPRequestHandler, status: int, payload: dict[str, Any]) -> None:
    body = json.dumps(payload, ensure_ascii=False, indent=2).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)


def make_handler(config: AppConfig, service: SyncService) -> type[BaseHTTPRequestHandler]:
    class WebhookHandler(BaseHTTPRequestHandler):
        def log_message(self, fmt: str, *args: Any) -> None:
            LOG.info("http %s - %s", self.client_address[0], fmt % args)

        def do_GET(self) -> None:
            if self.path == "/healthz":
                write_json(self, 200, {"status": "ok"})
                return
            write_json(self, 404, {"status": "not_found"})

        def do_POST(self) -> None:
            if self.path != config.webhook.path:
                write_json(self, 404, {"status": "not_found"})
                return

            content_length = int(self.headers.get("Content-Length") or 0)
            if content_length > config.webhook.max_body_bytes:
                write_json(self, 413, {"status": "payload_too_large"})
                return

            body = self.rfile.read(content_length) if content_length else b""
            if not verify_webhook_secret(config.webhook, self.headers, body):
                write_json(self, 401, {"status": "unauthorized"})
                return

            try:
                result = service.sync(reason="webhook")
            except SyncError as exc:
                LOG.warning("sync conflict: %s", exc)
                write_json(self, 409, {"status": "conflict", "error": str(exc)})
            except (ConfigError, GitError) as exc:
                LOG.exception("sync failed")
                write_json(self, 500, {"status": "error", "error": str(exc)})
            except Exception as exc:
                LOG.exception("unexpected sync failure")
                write_json(self, 500, {"status": "error", "error": str(exc)})
            else:
                write_json(self, 200, result)

    return WebhookHandler


def configure_logging(level: str) -> None:
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Webhook server that fast-forward syncs Git repositories.")
    parser.add_argument(
        "--config",
        default=os.environ.get("CODESYNC_CONFIG", "config.json"),
        help="path to JSON config file (default: CODESYNC_CONFIG or config.json)",
    )
    parser.add_argument("--once", action="store_true", help="run one sync and exit")
    parser.add_argument(
        "--force",
        action="store_true",
        help="with --once, force-push from the remote whose role is master to all other remotes",
    )
    parser.add_argument("--log-level", default=os.environ.get("CODESYNC_LOG_LEVEL", "INFO"))
    args = parser.parse_args(argv)
    if args.force and not args.once:
        parser.error("--force can only be used with --once")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    configure_logging(args.log_level)

    try:
        config = load_config(Path(args.config))
        service = SyncService(config)
        if args.once:
            if args.force:
                result = service.force_from_master(reason="manual-force")
            else:
                result = service.sync(reason="manual")
            print(json.dumps(result, ensure_ascii=False, indent=2))
            return 0

        handler = make_handler(config, service)
        server = ThreadingHTTPServer((config.listen_host, config.listen_port), handler)
        LOG.info(
            "listening on http://%s:%s%s",
            config.listen_host,
            config.listen_port,
            config.webhook.path,
        )
        server.serve_forever()
    except KeyboardInterrupt:
        LOG.info("shutting down")
        return 0
    except (ConfigError, GitError, SyncError) as exc:
        LOG.error("%s", exc)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
