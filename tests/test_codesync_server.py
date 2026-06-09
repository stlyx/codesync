from __future__ import annotations

import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codesync_server import (  # noqa: E402
    AppConfig,
    ConfigError,
    CredentialConfig,
    GitResult,
    RemoteConfig,
    SyncService,
    WebhookConfig,
    load_config,
    parse_args,
)


def sample_config(state_dir: Path, remotes: tuple[RemoteConfig, ...]) -> AppConfig:
    return AppConfig(
        listen_host="127.0.0.1",
        listen_port=0,
        repo_dir=state_dir / "repo.git",
        state_dir=state_dir,
        branch="master",
        remotes=remotes,
        webhook=WebhookConfig(),
        git_timeout_seconds=30,
    )


def remote(name: str, *, role: str | None = None) -> RemoteConfig:
    return RemoteConfig(
        name=name,
        url=f"https://example.com/{name}.git",
        credential=CredentialConfig(),
        role=role,
    )


class RecordingSyncService(SyncService):
    def __init__(self, config: AppConfig) -> None:
        super().__init__(config)
        self.calls: list[tuple[object, ...]] = []

    def init_repo(self) -> None:
        self.calls.append(("init_repo",))

    def sanitize_local_config(self) -> None:
        self.calls.append(("sanitize_local_config",))

    def configure_remotes(self) -> None:
        self.calls.append(("configure_remotes",))

    def fetch_remote(self, remote: RemoteConfig, *, force_tags: bool = False) -> str:
        self.calls.append(("fetch_remote", remote.name, force_tags))
        return "abc123"

    def update_local_branch(self, target: str) -> None:
        self.calls.append(("update_local_branch", target))

    def push_remote(self, remote: RemoteConfig, target: str, *, force: bool = False) -> None:
        self.calls.append(("push_remote", remote.name, target, force))


class GitCallSyncService(SyncService):
    def __init__(self, config: AppConfig) -> None:
        super().__init__(config)
        self.calls: list[tuple[tuple[str, ...], str | None]] = []

    def git(
        self,
        args: list[str],
        *,
        credential: CredentialConfig | None = None,
        check: bool = True,
    ) -> GitResult:
        self.calls.append((tuple(args), credential.username_env if credential else None))
        return GitResult(args=tuple(args), returncode=0, stdout="", stderr="")

    def rev_parse_commit(self, ref: str) -> str:
        self.calls.append(((f"rev_parse_commit:{ref}",), None))
        return "abc123"


class ConfigTests(unittest.TestCase):
    def test_load_config_accepts_master_role_and_more_than_two_remotes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "repo_dir": str(Path(tmp) / "repo.git"),
                        "remotes": [
                            {
                                "name": "repo_a",
                                "url": "https://example.com/a.git",
                                "role": "master",
                            },
                            {"name": "repo_b", "url": "https://example.com/b.git"},
                            {"name": "repo_c", "url": "https://example.com/c.git"},
                        ],
                    }
                ),
                encoding="utf-8",
            )

            config = load_config(config_path)

        self.assertEqual(len(config.remotes), 3)
        self.assertEqual(config.remotes[0].role, "master")
        self.assertIsNone(config.remotes[1].role)

    def test_load_config_rejects_invalid_remote_role(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "repo_dir": str(Path(tmp) / "repo.git"),
                        "remotes": [
                            {
                                "name": "repo_a",
                                "url": "https://example.com/a.git",
                                "role": "source",
                            },
                            {"name": "repo_b", "url": "https://example.com/b.git"},
                        ],
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ConfigError, "supported role is 'master'"):
                load_config(config_path)

    def test_load_config_requires_at_least_two_remotes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "repo_dir": str(Path(tmp) / "repo.git"),
                        "remotes": [
                            {"name": "repo_a", "url": "https://example.com/a.git"},
                        ],
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ConfigError, "at least two remote objects"):
                load_config(config_path)


class ForceSyncTests(unittest.TestCase):
    def test_force_from_master_pushes_master_to_all_other_remotes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = RecordingSyncService(
                sample_config(
                    Path(tmp),
                    (remote("repo_a", role="master"), remote("repo_b"), remote("repo_c")),
                )
            )

            result = service.force_from_master()

        self.assertEqual(result["mode"], "force")
        self.assertEqual(result["source"], "repo_a")
        self.assertEqual(result["target"], "abc123")
        self.assertEqual(result["remotes"], ["repo_b", "repo_c"])
        self.assertEqual(
            service.calls,
            [
                ("init_repo",),
                ("sanitize_local_config",),
                ("configure_remotes",),
                ("fetch_remote", "repo_a", True),
                ("update_local_branch", "abc123"),
                ("push_remote", "repo_b", "abc123", True),
                ("push_remote", "repo_c", "abc123", True),
            ],
        )

    def test_force_from_master_requires_exactly_one_master_remote(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = RecordingSyncService(
                sample_config(Path(tmp), (remote("repo_a"), remote("repo_b")))
            )

            with self.assertRaisesRegex(ConfigError, "exactly one remote"):
                service.force_from_master()

    def test_force_from_master_rejects_multiple_master_remotes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = RecordingSyncService(
                sample_config(
                    Path(tmp),
                    (remote("repo_a", role="master"), remote("repo_b", role="master")),
                )
            )

            with self.assertRaisesRegex(ConfigError, "exactly one remote"):
                service.force_from_master()

    def test_force_fetch_uses_forced_tag_refspec(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = remote("repo_a", role="master")
            service = GitCallSyncService(sample_config(Path(tmp), (source, remote("repo_b"))))

            target = service.fetch_remote(source, force_tags=True)

        self.assertEqual(target, "abc123")
        self.assertEqual(
            service.calls,
            [
                (
                    (
                        "fetch",
                        "--prune",
                        "--no-tags",
                        "repo_a",
                        "+refs/heads/master:refs/remotes/repo_a/master",
                    ),
                    None,
                ),
                (("fetch", "--prune", "repo_a", "+refs/tags/*:refs/tags/*"), None),
                (("rev_parse_commit:refs/remotes/repo_a/master",), None),
            ],
        )

    def test_force_push_uses_force_for_branch_and_tags(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            target_remote = remote("repo_b")
            service = GitCallSyncService(
                sample_config(Path(tmp), (remote("repo_a", role="master"), target_remote))
            )

            service.push_remote(target_remote, "abc123", force=True)

        self.assertEqual(
            service.calls,
            [
                (("push", "--force", "repo_b", "refs/heads/master:refs/heads/master"), None),
                (
                    (
                        "push",
                        "--force",
                        "--prune",
                        "repo_b",
                        "refs/tags/*:refs/tags/*",
                    ),
                    None,
                ),
            ],
        )


class CliTests(unittest.TestCase):
    def test_force_requires_once(self) -> None:
        with redirect_stderr(StringIO()), self.assertRaises(SystemExit):
            parse_args(["--force"])

    def test_force_is_accepted_with_once(self) -> None:
        args = parse_args(["--once", "--force"])

        self.assertTrue(args.once)
        self.assertTrue(args.force)


if __name__ == "__main__":
    unittest.main()
