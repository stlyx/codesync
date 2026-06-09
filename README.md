# CodeSync Webhook Server

一个无第三方依赖的 Python webhook 服务，用来同步多个 Git 仓库的 `master` 分支和全部 tag。

每次收到 webhook 后，服务会：

1. 初始化或复用一个本地 bare 仓库。
2. 用配置里的专用 Git credential 拉取所有远程的 `master` 分支，默认不会读取或写入本机全局 Git credential helper，也不会使用系统/全局 `url.*.insteadOf` URL 重写规则。
3. 拉取所有远程的全部 tag。
4. 检查本地分支和所有远程分支是否能收敛到同一个 fast-forward 目标。
5. 没有分叉冲突时，把 `master` 和全部 tag 推送回所有远程。

如果任意远程的 `master` 分叉，或者 tag 名相同但对象不同，服务会拒绝同步并返回错误，不会做非 fast-forward 推送。

## 配置

复制示例配置：

```bash
cp config.example.json config.json
```

按需修改 `config.json` 里的远程仓库地址、监听端口和本地目录。

HTTPS token 场景可以这样配置 credential：

```bash
export CODESYNC_WEBHOOK_SECRET='webhook-secret'
export CODESYNC_GIT_USERNAME='git-user-or-token-name'
export CODESYNC_GIT_PASSWORD='git-password-or-token'
python3 codesync_server.py --config config.json
```

这种方式通过临时 askpass 进程环境传递账号密码，不会调用 `git credential approve`，也不会把凭据写进 `~/.git-credentials`、Windows Credential Manager 或系统 credential store。

如果你已有专用 credential helper，也可以在配置里显式设置。服务会先清空 Git 的全局 credential helper 列表，再加载这里配置的 helper：

```json
{
  "credential": {
    "helper": "store --file /etc/codesync/git-credentials",
    "use_http_path": true
  }
}
```

SSH 场景可以使用独立 key：

```bash
export CODESYNC_GIT_SSH_COMMAND='ssh -i /etc/codesync/deploy_key -o IdentitiesOnly=yes'
```

Windows PowerShell 示例：

```powershell
$env:CODESYNC_WEBHOOK_SECRET = 'webhook-secret'
$env:CODESYNC_GIT_USERNAME = 'git-user-or-token-name'
$env:CODESYNC_GIT_PASSWORD = 'git-password-or-token'
python codesync_server.py --config config.json
```

服务运行 Git 子进程时会隔离 system/global/env Git config，防止 `url.*.insteadOf` 或 `url.*.pushInsteadOf` 生效。对于服务自己的 bare repo，本地 config 中的 `url.*.insteadOf`、`url.*.pushInsteadOf`、`include.*` 和 `includeIf.*` 会在同步前被移除。

## 运行

手动同步一次：

```bash
python3 codesync_server.py --config config.json --once
```

以 `role: "master"` 的仓库为主，强制覆盖推送到所有其他仓库：

```bash
python3 codesync_server.py --config config.json --once --force
```

`--force` 只能和 `--once` 一起使用。配置中必须且只能有一个远程仓库设置 `"role": "master"`；该模式只拉取 master 仓库，然后把配置的分支和 tags 强制同步到其他所有远程仓库。

启动 webhook 服务：

```bash
python3 codesync_server.py --config config.json
```

健康检查：

```bash
curl http://127.0.0.1:8080/healthz
```

触发同步：

```bash
curl -X POST http://127.0.0.1:8080/webhook \
  -H "X-CodeSync-Token: $CODESYNC_WEBHOOK_SECRET" \
  -d '{}'
```

也支持 `Authorization: Bearer <secret>`，以及 GitHub/Gitea 常见的 `X-Hub-Signature-256` HMAC 头。

## 同步规则

本地仓库使用 bare repo，默认路径是 `/var/lib/codesync/repo.git`。服务会维护：

- `refs/remotes/<remote>/master`
- `refs/heads/master`
- `refs/tags/*`

普通同步只接受 fast-forward 收敛。如果任意远程 `master` 和本地 `master` 里存在不能互相祖先化的提交历史，返回 `409 conflict`。

普通同步不会强制覆盖 tag。如果任一远程存在同名但不同对象的 tag，`git fetch` 或 `git push --tags` 会失败，服务返回错误。`--once --force` 会以 master 仓库的 tag 集合为准强制覆盖并剪枝其他远程。

进程锁使用跨平台文件锁，Linux/macOS 使用 `fcntl`，Windows 使用 `msvcrt`。
