# 生产部署（144 · /opt/cloudpivot）

一键部署脚本，固化 DEPLOY-A / B / C 三次手工发布做对的流程。

## 用法

在**生产机**（172.18.10.144，`/opt/cloudpivot` + `systemctl cloudpivot` 所在主机）上：

```bash
cd /opt/cloudpivot
bash scripts/deploy/deploy.sh                      # 全流程
bash scripts/deploy/deploy.sh --dry-run            # 只体检 + 打印计划，不写不改
bash scripts/deploy/deploy.sh --skip-frontend      # 只发后端
bash scripts/deploy/deploy.sh --skip-migrate       # 已知不需要迁移时
bash scripts/deploy/deploy.sh --tag hotfix-1020    # 自定义备份/构建标签
```

从 99（开发机）远程跑：

```bash
ssh root@172.18.10.144 'bash /opt/cloudpivot/scripts/deploy/deploy.sh --dry-run'
```

## 它做了什么（8 步）

| 步 | 动作 | 关键判据 |
|---|---|---|
| 1 | `git fetch` + 列全量待上线提交；**待跑迁移按「磁盘 vs 库」比**（磁盘版本号 > `max(version::int)`） | HEAD 必须是 origin/main 的祖先（分叉直接停） |
| 2 | 备份三件套到 `/root/deploy-backup-<TAG>/`（`db.dump` / `cloudpivot` / `dist/`） | 每份 > 0 字节 |
| 3 | `git merge --ff-only` | 生产工作区有**已跟踪**改动时**拒绝**执行（不替你 stash 别人的在途任务） |
| 4 | **后端需重做时**（`backend/` 有 diff **或 二进制 mtime < 最近一次后端提交时间**）：`cargo build --release`（默认 `CARGO_BUILD_JOBS=1`）—— **只编译，不重启** | 编译失败即停（此时线上仍是旧二进制 + 旧 schema，一致状态） |
| 5 | 待跑迁移逐条应用（调 `scripts/verify/deploya_migrate.sh`，按 nik-db 引擎口径记账） | 失败即停；末尾核对 `max(version::int)` |
| 6 | 换二进制（`cp → .new` + `mv -f`）→ `systemctl restart` → 轮询 `/health` | 失败自动回滚二进制；`/proc/<pid>/exe` 的 md5 必须等于磁盘二进制 |
| 7 | **前端需重做时**（`frontend/` 有 diff **或 dist mtime < 最近一次前端提交时间**）：`pnpm typecheck`（0 错）→ `vite build --outDir dist-<TAG>` → 双 rename | 新 dist 入口 chunk 必须等于 `curl http://127.0.0.1/` 的入口 chunk；不等则自动回滚 dist |
| 8 | 收口核对 | 主仓 HEAD == origin/main、迁移最大版本、服务 active、`/health`、进程==磁盘、线上入口 |

> 为什么「先编译再迁移」而不是反过来：编译要 5–10 分钟，而删列类迁移（如 V0217 删
> `received_qty`）一落地，**仍在服务的旧二进制**就会因为引用已删列而 500。先编好再迁移，
> 能把「旧二进制 × 新 schema」的窗口从「编译耗时」压到「迁移耗时」（实测 ~0.2s/条），
> 换二进制重启只隔几秒。

日志：`/var/log/cloudpivot-deploy-<TAG>.log`（同时 tee 到终端）。

## 铁律（脚本已内置，别绕过）

1. **绝不先 `mv dist` 再 build**：先 `vite build --outDir dist-<TAG>`，成功后 `dist → dist-prev-<TAG>`、`dist-<TAG> → dist` 双 rename。
2. **换二进制用 `cp → .rollback/.new` + `mv -f`**，不要 `cp` 到正在运行的路径（ETXTBSY）。
3. **迁移逐条跑 + 按引擎口径记账**（`version` 不补零、`checksum=sha256`、`applied_via='imported'`）；重启日志必须出现「数据库迁移完成: 应用 0 个迁移」，否则说明记账不一致，引擎会重放。
4. **只在有改动时才动那一半**：后端没改不重启（避免无谓抖动），前端没改不 build。
5. **`cargo test` / Playwright 与 `cargo build --release` 不要并发**：7 GB 机器会 OOM（`signal: 9`）。

## 回滚

失败时脚本会在终端/日志尾部按「已经做到哪一步」打印精确命令。手工兜底：

```bash
TAG=<部署标签>; B=/root/deploy-backup-$TAG

# 后端二进制
systemctl stop cloudpivot
cp $B/cloudpivot /opt/cloudpivot/backend/target/release/cloudpivot.rollback
mv -f /opt/cloudpivot/backend/target/release/cloudpivot.rollback /opt/cloudpivot/backend/target/release/cloudpivot
systemctl start cloudpivot

# 前端 dist
mv /opt/cloudpivot/frontend/dist /opt/cloudpivot/frontend/dist-failed-$TAG
mv /opt/cloudpivot/frontend/dist-prev-$TAG /opt/cloudpivot/frontend/dist

# 数据库（会丢部署后写入，慎用）
systemctl stop cloudpivot
su - postgres -c 'dropdb cloudpivot && createdb -O cloudpivot cloudpivot'
su - postgres -c "pg_restore -d cloudpivot $B/db.dump"
systemctl start cloudpivot

# 代码
git -C /opt/cloudpivot reset --hard <部署前 HEAD>
```

迁移是不可逆的：要退回去必须跑对应 `backend/migrations/business/V****__*.down.sql`，
并在 `schema_migrations` 里删掉对应记录（`delete from schema_migrations where kind='versioned' and version='207'`）。

## 已知的坑（踩过的）

- **生产工作区脏了**（有未提交的已跟踪文件）→ 脚本直接停：生产上不该有人手改代码，先查清是谁。
- **`/health` 排不到 60 s**：先看 `journalctl -u cloudpivot -n 60`，最常见是迁移/种子失败或端口被占。
- **前端入口不一致**：nginx `root` 必须指向 `/opt/cloudpivot/frontend/dist`（`nginx -T | grep root`）；改过 nginx 配置记得 `nginx -s reload`。
- **发布后 5–10 分钟再核一次**：并行的卡可能覆盖二进制（`md5sum /proc/<pid>/exe` 与磁盘再比一次）。
- `erp_invariants.py` / `snapshot_audit.py` 必须在 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a` 下跑，否则连不上库、会被记成 BLOCKER。
