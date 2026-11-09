# 数据库备份 + 恢复演练 + 运维手册（REVIEW-FIX-1 / I-2 · R2 风险）

评审原文：*「生产没有数据库备份脚本 … 一次误操作/磁盘故障 = 账套丢失；客户数据无法承诺 RPO/RTO」*。
本证据包给出：脚本落地、144/145 真机备份实测、**跨机恢复演练（144 生产 dump → 145 临时库）**、恢复后真 token 登录与接口冒烟、cron 上线、安全护栏（生产库防护 / --yes / 磁盘余量 / 保留策略）、运维手册章节。

脚本：`scripts/ops/backup_db.sh`、`scripts/ops/restore_db.sh`
手册：`docs/运维规程-数据库备份与恢复.md`

---

## 验收对照

| # | 验收项 | 结果 | 证据文件 |
|---|---|---|---|
| ① | 144 生产手工跑一次 → dump + 校验 + `.meta` | **PASS** | `backup_144_prod.log`、`prod_144.meta`、`prod_144.dump.sha256` |
| ② | 145 测试同上 | **PASS** | `backup_145_test.log` |
| ③ | 把 144 的 dump 恢复到 145 临时库 → 表数/migrations/行数一致 | **PASS**（9 项基线全 OK） | `restore_drill_145.log` |
| ④ | 恢复后能登录（用恢复出来的账号） | **PASS**（登录 200 + 5 个接口 200；口令口径见下） | `login_smoke_145.log`、`login_smoke_145.sh` |
| ⑤ | cron 已装（144 + 145 各一条） | **PASS** | `cron_install_144.log`、`cron_install_145.log` |
| ⑥ | 运维手册章节 | **PASS** | `docs/运维规程-数据库备份与恢复.md` |
| ⑦ | 磁盘检查生效 | **PASS** | `diskcheck_block.log`（余量不足直接退出 1） |
| ⑧ | push | **PASS** | 见卡内汇报的 commit hash + `git ls-remote` 复核 |

补充证据（卡面未列但属 ① 的行为契约）：`retention_test_145.log`（`--keep 2` 连跑 4 次，只留最新 2 份、删最旧）、`guard1_no_yes.log`（非交互无 `--yes` 中止）、`guard2_prod_refused.log`（未 `--force-prod` 恢复生产库名被拒）。

---

## ① 144 生产备份（实测）

```
目标库 cloudpivot@127.0.0.1:5432（服务端 180006 / pg_dump 18，37508799 字节，176 张表）
磁盘余量检查：可用 234185478144 字节 / 需要 3016700 字节（2 × 预估 dump 1508350）→ 通过
dump 生成：cloudpivot-20261109-220753.dump（1509164 字节，耗时 1s）
完整性校验：pg_restore --list 可读，TOC 条目 2332 个（其中 TABLE DATA 175 个）
基线 .meta 已写入：迁移 295 条 / 最大版本 362 / 表 176 张
保留份数：3 / 14
```

`.meta` 内容见 `prod_144.meta`（DB 模板同目录；prod dump 本体**不进 git**，sha256 记在 `prod_144.dump.sha256`）。

## ② 145 测试备份（实测）

```
目标库 cloudpivot_dev@127.0.0.1:5432（服务端 180006 / pg_dump 18，50534079 字节，157 张表）
dump 生成：cloudpivot_dev-20261109-220755.dump（2847695 字节，耗时 0s）
完整性校验：TOC 2203 条（TABLE DATA 156 条）；基线：迁移 298 / 版本 362 / 表 157
保留份数：3 / 7
```

## ③ 跨机恢复演练（144 → 145）

```
源 dump : cloudpivot-20261109-220753.dump（从 144 /root/db-backup 拷到 145 /root/db-backup/from-144/）
目标库  : cloudpivot_drill3_20261109 @ 145:5432（不存在 → 新建 owner=cloudpivot）
防护    : 目标库非受保护库；给 --yes 跳过交互
恢复    : pg_restore --clean --if-exists --no-owner --no-privileges，耗时 1s，日志无 ERROR
```

基线比对（脚本自动打印）：

| 指标 | 基线(dump 时) | 恢复后 | 结果 |
|---|---|---|---|
| tables.count | 176 | 176 | OK |
| migrations.count | 295 | 295 | OK |
| migrations.max_version | 362 | 362 | OK |
| key_tables.sys_user | 3 | 3 | OK |
| key_tables.sys_role | 11 | 11 | OK |
| key_tables.sys_menu | 435 | 435 | OK |
| key_tables.biz_purchase_order | 0 | 0 | OK |
| key_tables.biz_voucher | 0 | 0 | OK |
| key_tables.biz_inventory | 0 | 0 | OK |

## ④ 恢复后登录 + 接口冒烟（真 token）

在 145 上起隔离实例 `127.0.0.1:8039`（库 = 恢复出来的 `cloudpivot_drill3_20261109`，8033/8034 在线服务不动）：

```
库内真实账号：admin(status=1), henry.liao(status=1), laoliu(status=1)
表 176 张 / 迁移 295 条 / 菜单 435 条
登录 admin → code=0，token_len=487
/system/user/page          200  （列表含 laoliu / henry.liao / admin）
/system/menu/tree          200  （顶级节点 19 个，来自 dump 的菜单）
/system/auth/menus         200
/business/erp/material/page 200
/system/role/page          200  （SUPER_ADMIN 等角色来自 dump）
RESULT: PASS
```

**口令口径（必须明说，别夸大）**：生产 admin 口令在 **2026-11-09 20:00:27 由用户本人**在 UI 里 SELF RESET 改过（证据：生产 `password_change_logs`，`operator_type=SELF`，`change_status=SUCCESS`）。演练方不持有该新口令，也不应为演练去猜/重置生产口令。因此演练只对**副本库**做一次口令哈希替换（换成 145 演练环境 dev 库 admin 的 argon2 哈希，同一产品同一参数）；账号行（id / 角色 / token_version）、菜单、业务数据**全部来自 dump** —— 用户列表里同时出现 `henry.liao`、`laoliu` 即证明数据来自恢复库而不是新建库。脚本见 `login_smoke_145.sh`（头部注释写明了这条口径）。

## ⑤ cron（两台真机已装）

```
144: 30 2 * * * bash /opt/cloudpivot/scripts/ops/backup_db.sh --keep 14 >> /var/log/cloudpivot/backup.log 2>&1
145: 30 3 * * * bash /opt/cloudpivot/scripts/ops/backup_db.sh --keep 7  >> /var/log/cloudpivot/backup.log 2>&1
```

安装用幂等脚本（先备份 root crontab 到 `/var/backups/crontab.bak-<date>`，再 grep -v 去重后追加），并在 **cron 等价环境**（`env -i HOME=/root PATH=/usr/bin:/bin`）跑 `--dry-run` 验证：env 文件探测、pg_dump 版本、连通性、磁盘余量全部通过（见 `cron_install_144.log` / `cron_install_145.log`）。

## ⑥ 运维手册

`docs/运维规程-数据库备份与恢复.md`：备份（自动/手工/文件布局/校验/行为契约/安全）、恢复（演练步骤/行为契约/基线不一致排查/演练库登录口径）、RPO=24h 与 RTO≈10min 的**实测依据与做不到的边界**（无 WAL 归档 → 无 PITR；备份与库同机 → 不抗整机故障）、季度演练清单、常见故障表。

## ⑦ 护栏实测

| 护栏 | 实测输出 | 文件 |
|---|---|---|
| 磁盘余量不足直接失败（退出 1，不留半截文件） | `磁盘余量不足：可用 144757903360 < 需要 999999999999 字节` → `RC=1` | `diskcheck_block.log` |
| 保留策略删最旧 | 4 次 `--keep 2`：第 3、4 次各删 1 份最旧，最终 2 dump + 2 meta | `retention_test_145.log` |
| 非交互缺 `--yes` 拒绝执行 | `非交互环境必须显式加 --yes 才会执行恢复` → `RC=1` | `guard1_no_yes.log` |
| 默认禁止恢复生产库 | `目标库 cloudpivot 属于「受保护库」… 拒绝恢复到受保护库 cloudpivot` → `RC=1` | `guard2_prod_refused.log` |
| 口令不进命令行/日志 | 口令只走 `PGPASSWORD`；全部日志中无口令字符串 | 上述所有日志 |

## 复现步骤

```bash
# 备份（144 生产 / 145 测试）
bash scripts/ops/backup_db.sh --keep 14
# 恢复到临时库（跨机演练：先把 144 的 dump 拷到目标机）
bash scripts/ops/restore_db.sh /root/db-backup/cloudpivot-20261109-220753.dump \
     --target-db cloudpivot_drill3_20261109 --yes
# 恢复后登录冒烟（145，隔离实例 :8039）
bash login_smoke_145.sh
```
