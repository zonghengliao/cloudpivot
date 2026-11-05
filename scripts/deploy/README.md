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
| 1 | `git fetch` + 列全量待上线提交；**待跑迁移按「磁盘 vs 库」做集合差**（`backend/migrations/` **递归全部子目录**：磁盘上每个 `V<n>__<name>.up.sql` 减去库里 `kind='versioned' and success` 的 `name`，按版本号升序） | HEAD 必须是 origin/main 的祖先（分叉直接停）；`applied_via` 取值、迁移文件名合法性、同名冲突三种预检不过直接停 |
| 2 | 备份三件套到 `/root/deploy-backup-<TAG>/`（`db.dump` / `cloudpivot` / `dist/`） | 每份 > 0 字节 |
| 3 | `git merge --ff-only` | 生产工作区有**已跟踪**改动时**拒绝**执行（不替你 stash 别人的在途任务） |
| 4 | **后端需重做时**（`backend/` 有 diff **或 二进制 mtime < 最近一次后端提交时间**）：`cargo build --release`（默认 `CARGO_BUILD_JOBS=1`）—— **只编译，不重启** | 编译失败即停（此时线上仍是旧二进制 + 旧 schema，一致状态） |
| 5 | 待跑迁移逐条应用（调 `scripts/verify/deploya_migrate.sh --assert-pending`，按 nik-db 引擎口径记账，`source` 记文件真实子目录） | 失败即停；**三道自检**：① 对账 `applied 增量 == 待跑条数`；② `applied_via` 全表 ∈ `executed/imported/baseline`；③ 集合差为空（磁盘上有、库里没记账的为 0） |
| 6 | 换二进制（`cp → .new` + `mv -f`）→ `systemctl restart` → 轮询 `/health` | 失败自动回滚二进制；`/proc/<pid>/exe` 的 md5 必须等于磁盘二进制 |
| 7 | **前端需重做时**（`frontend/` 有 diff **或 dist mtime < 最近一次前端提交时间**）：`pnpm typecheck`（0 错）→ `vite build --outDir dist-<TAG>` → 双 rename | 新 dist 入口 chunk 必须等于 `curl http://127.0.0.1/` 的入口 chunk；不等则自动回滚 dist |
| 8 | 收口核对 | 主仓 HEAD == origin/main、迁移最大版本、服务 active、`/health`、进程==磁盘、线上入口 |

> 为什么「先编译再迁移」而不是反过来：编译要 5–10 分钟，而删列类迁移（如 V0217 删
> `received_qty`）一落地，**仍在服务的旧二进制**就会因为引用已删列而 500。先编好再迁移，
> 能把「旧二进制 × 新 schema」的窗口从「编译耗时」压到「迁移耗时」（实测 ~0.2s/条），
> 换二进制重启只隔几秒。

日志：`/var/log/cloudpivot-deploy-<TAG>.log`（同时 tee 到终端）。**`--dry-run` 不落盘**：不写日志文件、不建备份目录、不动库（实测对比整棵树 + `/var/log` + `/root/deploy-backup-*` + 库内 `max(version)`/`applied_seq` 前后完全一致；只有 `git fetch` 会更新 `.git` 元数据）。

## 铁律（脚本已内置，别绕过）

1. **绝不先 `mv dist` 再 build**：先 `vite build --outDir dist-<TAG>`，成功后 `dist → dist-prev-<TAG>`、`dist-<TAG> → dist` 双 rename。
2. **换二进制用 `cp → .rollback/.new` + `mv -f`**，不要 `cp` 到正在运行的路径（ETXTBSY）。
3. **迁移逐条跑 + 按引擎口径记账**（`version` 不补零、`checksum=sha256`、`applied_via='imported'`、`source` 记文件真实子目录）。
   硬判据是**集合差为空**：磁盘上（`backend/migrations/` 递归全部分目录）的每个版本化迁移在库里都有 `success=true` 记录（第 5 步迁移后、第 6 步重启后各校一次）。
   重启日志「数据库迁移完成: 应用 0 个迁移」是**辅证**而非判据：改动过 `S__` 种子 / `R__` 可重复脚本时引擎按指纹变化会重跑，日志会出现 `应用 N 个迁移`（N>0）而版本化记账完全一致（2026-11-05 发版实测：`S__support_invariant_menu` 随 P2-7 改过，首启 N=1、次启 N=0），所以脚本对日志只做提示、对集合差才 `die`。
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

- **用 `max(version)` 比大小选迁移**（2026-11-05 发版中断的根因，卡 t_9467ce9f）：旧版第 1 步取 `max(version::int)` 当游标、只挑「版本号 > max」的脚本（旧 L184/L189，第 5 步再按 `-le APPLIED_MAX` 跳一次，旧 L237），于是**库内最大版本以下的 gap 永远挑不出来**。当时库里 max 是 V0235，而 V0220/V0221/V0226..V0234 十一条从未记账 ⇒ 只挑出 V0236..V0241 六条，跑到 V0240 时它依赖的 `biz_budget_subject_map`（V0227 建）不存在 → `ERROR: relation "biz_budget_subject_map" does not exist` → `[5/8]` 中断、V0241 也没跑。**引擎自己不是这么判的**：`needs_apply`（`backend/vendor/nik-db/src/migrate/mod.rs:1263-1275`）只看 `(kind,name)` 有没有 `success` 记录，低版本照样执行（只打 `out_of_order` WARN，`mod.rs:876-888`）。现在清单构造只在 `migrate_plan.sh:mp_build_pending` 一处实现（deploy.sh 与自测脚本共用），是**集合差**；gap 会在第 1 步用 `⚠ 其中 N 条版本号低于库内最大已应用` 显式列出来。
- **漏扫 system/support 目录**（2026-11-05 发版实测）：旧版第 1 步只 `git ls-tree backend/migrations/business/`，而引擎的 `path = "migrations"` 是**递归**装载 ⇒ `system/V0235__create_sys_invariant_result` 不在清单里，启动期被引擎按集合语义补跑（只打 `out_of_order` WARN，不炸启动），「应用 0 个迁移」自检失效。现在两处都改成递归全部分目录 + `source` 按真实子目录记；再加集合差硬校验兜底。
- **自造 `applied_via` 值**（2026-11-05 踩过）：引擎只认 `executed` / `baseline` / `imported`（`backend/vendor/nik-db/src/migrate/kind.rs:95-111`），手工补跑用 `'deploy-backfill'` 记一行 → 服务启动直接 `FATAL: unknown applied_via 'deploy-backfill' in record table`（`history.rs:219-221`）。现在 deploy.sh 第 1 步、迁移工具跑前、跑后、重启后各校一次，非法值当场停。
- **生产工作区脏了**（有未提交的已跟踪文件）→ 脚本直接停：生产上不该有人手改代码，先查清是谁。
- **`/health` 排不到 60 s**：先看 `journalctl -u cloudpivot -n 60`，最常见是迁移/种子失败或端口被占。
- **前端入口不一致**：nginx `root` 必须指向 `/opt/cloudpivot/frontend/dist`（`nginx -T | grep root`）；改过 nginx 配置记得 `nginx -s reload`。
- **发布后 5–10 分钟再核一次**：并行的卡可能覆盖二进制（`md5sum /proc/<pid>/exe` 与磁盘再比一次）。
- `erp_invariants.py` / `snapshot_audit.py` 必须在 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a` 下跑，否则连不上库、会被记成 BLOCKER。

## 改完清单逻辑怎么验（预演库回归，不碰生产）

```bash
# 1) 造一个预演库连接文件（**绝不能指向生产库**）
cat > /tmp/rehearsal.env <<'EOF'
NIK_DATABASE_NAME=cloudpivot_rehearsal_deploy
NIK_DATABASE_USER=<dev 账号>
NIK_DATABASE_PASSWORD=<dev 密码>
NIK_DATABASE_HOST=127.0.0.1
EOF

# 2) 一键回归：铺 V0219 基线 → 复现事故现场 → 旧法必失败 → 新法跑完 13 条 + 三道自检
DEPLOY_ENV_FILE=/tmp/rehearsal.env bash scripts/deploy/selftest_migrate_plan.sh

# 3) 想留库手工看：KEEP=1 再跑一次（默认跑完删库）
KEEP=1 DEPLOY_ENV_FILE=/tmp/rehearsal.env bash scripts/deploy/selftest_migrate_plan.sh
```

`selftest_migrate_plan.sh` 的断言清单（26 条，任何一条失败退出码非 0）：基线记账数、事故现场 max、旧法只挑到 2 条而新法把 11 条 gap 全纳入、卡面点名的 13 条一条不少、旧法跑 V0240 必失败且报错点名 `biz_budget_subject_map`、新法 `RECON_OK`/`VIA_OK`/`SET_OK`、清单少给一条被 `--assert-pending` 拒跑且库不变、非法 `applied_via` 被拦。

