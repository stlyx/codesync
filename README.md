# CodeSync

CodeSync 是一个 Rust 编写的轻量级 Git 同步服务，用来把两个 HTTPS Git 远程仓库的同一个分支和全部 tags 保持 fast-forward 收敛。

当前 Rust 版本使用 `git2` / libgit2 执行 Git 操作：

- 不调用 `git` 二进制；
- 不使用 Python 服务、Python 测试或 Python helper；
- 支持一键同步和阻塞式 HTTP webhook 服务；
- 继续读取现有 `config.json` 结构；
- 只支持 HTTPS remote；
- 只从配置指定的 username/password 环境变量读取凭据；
- 不支持 SSH remote、Git credential helper、force push 或冲突自动解决。

## 构建

需要 Rust 1.85 或更高版本。

```bash
cargo build --release
```

生成的二进制位于：

```bash
./target/release/codesync
```

开发时也可以直接使用：

```bash
cargo run -- --help
```

## 配置

复制示例配置：

```bash
cp config.example.json config.json
```

默认配置路径是当前目录下的 `config.json`。也可以显式指定：

```bash
./target/release/codesync --config /path/to/config.json
```

HTTPS token 场景可以这样配置环境变量：

```bash
export CODESYNC_WEBHOOK_SECRET='webhook-secret'
export CODESYNC_GIT_USERNAME='git-user-or-token-name'
export CODESYNC_GIT_PASSWORD='git-token-or-password'
```

Windows PowerShell 示例：

```powershell
$env:CODESYNC_WEBHOOK_SECRET = 'webhook-secret'
$env:CODESYNC_GIT_USERNAME = 'git-user-or-token-name'
$env:CODESYNC_GIT_PASSWORD = 'git-token-or-password'
```

`credential.username_env` 和 `credential.password_env` 必须成对配置。Rust 版不会读取 Git credential helper，也不会把凭据写入 credential store。

## 运行

不带 `--once` 时默认启动 HTTP webhook 服务：

```bash
./target/release/codesync
```

等价于：

```bash
./target/release/codesync --config config.json
```

手动同步一次并退出：

```bash
./target/release/codesync --once
```

也可以在开发环境中直接运行：

```bash
cargo run -- --once
cargo run -- --config config.json
```

日志级别默认是 `info`，可以通过参数设置：

```bash
./target/release/codesync --log-level debug
```

## HTTP API

健康检查：

```bash
curl http://127.0.0.1:8080/healthz
```

返回：

```json
{
  "status": "ok"
}
```

触发同步：

```bash
curl -X POST http://127.0.0.1:8080/webhook \
  -H "X-CodeSync-Token: $CODESYNC_WEBHOOK_SECRET" \
  -d '{}'
```

也支持：

- `Authorization: Bearer <secret>`；
- GitHub/Gitea 常见的 `X-Hub-Signature-256: sha256=<hmac>`。

如果 `webhook.secret_env` 已配置，启动 HTTP 服务时必须能从该环境变量读取 secret。`--once` 手动同步不需要 webhook secret。

## 同步规则

每次同步会：

1. 获取 `state_dir/sync.lock` 跨进程文件锁。
2. 初始化或复用 `repo_dir` 下的 bare 仓库。
3. 从每个远程拉取 `refs/heads/<branch>` 到 `refs/remotes/<remote>/<branch>`。
4. 拉取每个远程的全部 tags。
5. 检查本地分支和所有远程分支是否能收敛到一个 fast-forward 目标。
6. 如果存在分叉冲突，返回 `409 conflict` 并拒绝推送。
7. 更新本地 `refs/heads/<branch>`。
8. 把分支和 tags 以非 force refspec 推送回所有远程。

普通同步不会强制覆盖 tag。如果任一远程存在同名但不同对象的 tag，同步会失败，不会做 force push。

## 配置兼容性说明

Rust 版保留当前 `config.json` 的主要结构，但下列功能不再支持：

- SSH remote；
- `credential.helper`；
- `credential.ssh_command_env`；
- `--force` / 从 `role: "master"` 远程强制覆盖其他远程。

这些字段如果为空或缺失可以继续解析；如果配置了实际值，程序会返回清晰的配置错误。
