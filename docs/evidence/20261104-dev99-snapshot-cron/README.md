# .99 快照巡检 cron 的去留（t_c47bce71）—— 已移除

- 卡：`t_c47bce71`【收口D·遗留】.99 上另有一条快照巡检 cron，跑的是 `cp_lockorder_t`（非生产），口径待确认
- 上游：`t_485a84dd`（收口 D 验收，判「通过」）执行 ③ cron 核对时发现；该条 cron 由 `t_9df97272`（B1 落地）安装
- 执行机：**172.18.10.99（ubuntu-svr，开发/测试机）**　执行时间：2026-11-04 19:51–19:52 CST
- 结论：**选 (b) 移除**（注释停用，保留备份），并把本机与生产同名同格式的巡检日志改名，彻底断掉「拿开发库结果当生产结果」的可能。

## 1. 先确认来历与用途（要求 ①）

证据链三条，互相印证，结论一致：**它是 B1 阶段为了「验证 cron 通路能跑通」临时挂的，从来不是巡检本机的业务数据，也没有任何自动化消费方。**

| 证据 | 内容 |
|---|---|
| 安装者/时间 | `git log scripts/cron/snapshot_audit.sh` → `d537f62 2026-11-03 03:43:32 backend-developer feat(verify): B1 快照巡检落地 —— 16 条只读巡检 + 脚本 + 告警落盘 + 每日 02:00 定时（只报不修）`；同一提交即卡 `t_9df97272` |
| B1 证据文档自己写明 | `docs/evidence/20261103-b1-快照巡检落地.md:15`「crontab（user root）：追加 `0 2 * * * …/snapshot_audit.sh`」；同文件 §8 原文「env 里 `NIK_DATABASE_NAME` 现指向隔离库 `cp_lockorder_t`，**上线前必须改成生产库**，否则凌晨扫的是一个夹具库、看板永远『全绿』」 |
| env 文件头注释（原文） | `/root/.cloudpivot/snapshot-audit.env`：「⚠️ 上线前必须改成**生产库**。当前指向本机隔离库 `cp_lockorder_t`（全量迁移到 V0203、含合同表），**只用于验证 cron 通路**」 |

补充事实：

- 本机没有生产库 `cloudpivot`（`psql -l` 共 ~250 个库，全是 POC/测试/隔离库），`/etc/cloudpivot/` 在本机**不存在**（只有 `~/.cloudpivot/`，是 wrapper 的两级回退路径）。
- 该 cron 的自动执行记录只有**一次**：`2026-11-04 02:00:01 → 02:00:02 exit=0`（`cp_lockorder_t`，16 项全 CHECKED、BLOCKER 0/WARN 0/INFO 0）。11-03 03:42 才装上，所以 11-03 当天没有 02:00 轮次。
- **没有消费方**：全机 grep `snapshot-audit.latest.json` / `snapshot-audit.log` 的读取者，只有 `scripts/verify/snapshot_audit.py`、`scripts/cron/snapshot_audit.sh` 自身的默认路径与文档示例；`lsof` 无进程占用。`laoliu` 的 `deploy_check_v3.sh` / `auto_deploy_check.sh` 只是在提示文案里让人**手工**跑 `snapshot_audit.py`，不读这些文件、也不依赖 cron。
- 生产接线早已在 144 完成（`t_6156104c` 挂 `0 2 * * *` + `/etc/cloudpivot/snapshot-audit.env` + 飞书 BLOCKER 钩子，`t_485a84dd` 验收通过）。**所以 .99 这条今天起是纯冗余，且是风险源。**

## 2. 为什么选「移除」(b) 而不是「保留改造」(a)

1. **它扫的库没有意义**：`cp_lockorder_t` 是 2026-11-03 为「收货过账作废加锁顺序」临时建的隔离库（`docs/evidence/20261103-p2-收货过账作废加锁顺序.md:175`），随时可能被重建/清空。真要被重建，这条 cron 会**天天静默失败**（cron.log 全量落日志但无告警，且失败信息不会进飞书——飞书钩子只接在 144）。
2. **同名同格式是真实伤害，不是假设**：本卡发现的当天，`t_485a84dd` 的 p26 演练把 69 轮日志追加进了 `.99:/var/log/cloudpivot/snapshot-audit.log`——与 144 生产日志**同名、同格式、同为「云枢 · 快照一致性巡检」抬头**，只有 `db=` 字段能区分。取证时先看文件名就会读错口径。
3. **保留改造 (a) 的收益为零**：脚本能力本身有单元验证（B1 证据 §7：退出码 0/1/2/3 全测过），144 生产每天都会跑真正的巡检；.99 再挂一条开发库巡检，除了制造上面两条风险，不产生任何新信息。
4. 脚本**不删**：`scripts/cron/snapshot_audit.sh` 留在仓库里，手工随时能跑（本包 `03-manual-dryrun.out` 证明 `--dry-run` 仍 exit 0）。

> 若将来确实想在本机保留一条常驻巡检，正确做法是（不要复用默认日志名）：
> `0 2 * * * SNAPSHOT_AUDIT_LOG_DIR=/var/log/cloudpivot/dev99 /opt/cloudpivot/scripts/cron/snapshot_audit.sh >> /var/log/cloudpivot/dev99/snapshot-audit.cron.log 2>&1`
> 且必须在 env 里把 `NIK_DATABASE_NAME` 明确写成本机某个**长期存在**的库名。

## 3. 实际做了什么（逐条可回读）

| # | 动作 | 证据文件 |
|---|---|---|
| 1 | `crontab -l` 全量备份 → `/root/.cloudpivot/crontab.bak-20261104T1951.root.txt`（600 root:root），并复制一份进本目录 | `00-crontab.before.txt`、`01-crontab-before.out` |
| 2 | 把 2 行（`# B1 …` 注释行 + `0 2 * * * …` 调度行）用 `#DISABLED[2026-11-04 t_c47bce71 .99 不跑巡检，生产巡检在 144] ` 前缀注释掉，**不删行**；新表另存 `/root/.cloudpivot/crontab.new-20261104T1951.root.txt`（600） | diff 见下 |
| 3 | `crontab <新表>` 安装；`crontab -l` 回读：仍启用的 `snapshot_audit` 调度行 **0 行**，其余 6 条他人任务（`idle_unblock`×2、`disk_guard`、`deploy_check_v3`、`gen_test_report`、`zero_output_check`）与 3 行 `#ARK-PAUSED` 原样未动 | `02-crontab-after-and-schedulers.out` |
| 4 | 确认无第二条调度入口：`/etc/cron.d`、`/etc/cron.{hourly,daily,weekly,monthly}`、systemd timer、`atq` 均无 | 同上 |
| 5 | `cron` 服务 `active`，spool `/var/spool/cron/crontabs/root` mtime 19:51；`diff spool <(crontab -l)` 仅差 3 行 crontab 头部注释，内容一致 | 同上 |
| 6 | 本机日志改名（**只改名，不删**）：`snapshot-audit.log` → `snapshot-audit.dev99.log.archived-2026-11-04`；`snapshot-audit.log.2026-11-03` → `snapshot-audit.dev99.log.2026-11-03`；`snapshot-audit.latest.json` → `snapshot-audit.dev99.latest.json`；`snapshot-audit.cron.log` → `snapshot-audit.dev99.cron.log` | `04-last-cron-run-and-logdir.out` |
| 7 | 在 `/var/log/cloudpivot/README-snapshot-audit.txt` 写清「本机不是巡检机 / 生产日志在 144 / dev99.* 是本机历史文件」 | 同上（`ls` 可见） |
| 8 | `~/.cloudpivot/snapshot-audit.env` 末尾追加一行「本机 cron 已停用，仅手工验证」（600 保持） | 同上 |

改前 → 改后（crontab diff，仅这 2 行）：

```diff
-# B1 快照一致性巡检（只报不修）每日 02:00 —— scripts/cron/snapshot_audit.sh
-0 2 * * * /opt/cloudpivot/scripts/cron/snapshot_audit.sh >> /var/log/cloudpivot/snapshot-audit.cron.log 2>&1
+#DISABLED[2026-11-04 t_c47bce71 .99 不跑巡检，生产巡检在 144] # B1 快照一致性巡检（只报不修）每日 02:00 —— scripts/cron/snapshot_audit.sh
+#DISABLED[2026-11-04 t_c47bce71 .99 不跑巡检，生产巡检在 144] 0 2 * * * /opt/cloudpivot/scripts/cron/snapshot_audit.sh >> /var/log/cloudpivot/snapshot-audit.cron.log 2>&1
```

## 4. 怎么回滚（30 秒）

```bash
# 方式 A（推荐，原样恢复）：用备份覆盖回去
crontab /root/.cloudpivot/crontab.bak-20261104T1951.root.txt
crontab -l | grep snapshot_audit      # 应看到未带 #DISABLED 的两行

# 方式 B（只恢复调度行）：手改或 sed 去掉 #DISABLED[...]  前缀
sed -i 's/^#DISABLED\[[^]]*\] //' <(crontab -l)         # 生成后 crontab 安装，勿直接改 spool

# 日志文件名回滚（如需）
cd /var/log/cloudpivot
mv snapshot-audit.dev99.log.archived-2026-11-04 snapshot-audit.log
mv snapshot-audit.dev99.latest.json snapshot-audit.latest.json
```

回滚后要意识到风险照旧存在：凌晨扫的是开发库、日志与生产同名、无告警。若要长期保留，请按 §2 末尾的「独立日志目录 + 长期库名」方式挂。

## 5. 红线遵守声明

- **只动 .99**：改的是本机 crontab、本机 `/var/log/cloudpivot/` 下的文件名、本机 `~/.cloudpivot/snapshot-audit.env` 的一行注释。
- **144 零改动**：本卡**没有在 144 上做任何写操作**，没有碰它的 crontab / `/etc/cloudpivot/snapshot-audit.env` / 日志 / 库。只在最后做了一次**只读**核对（`ssh root@172.18.10.144 'crontab -l'`），证明生产仍是 2 行（`erp-health-daily.sh` 未丢 + `0 2 * * * snapshot_audit.sh`），`/etc/cloudpivot/snapshot-audit.env` 仍 `600 root:root`、mtime 18:04 未变 → 见 `05-prod-144-readonly-check.out`。
- **零数据库改动**：全程只跑了 `--dry-run`（不连库、不建视图、不产 JSON）；没有对 `cp_lockorder_t` 或任何库执行 DDL/DML，也没有删库/改名。
- 不删任何日志文件：一律 `mv` 改名留档。

## 6. 本目录文件清单

| 文件 | 内容 |
|---|---|
| `00-crontab.before.txt` | 改前 crontab 全量（= 备份副本） |
| `01-crontab-before.out` | 改前 `crontab -l` 原始输出（带 host/时间戳） |
| `02-crontab-after-and-schedulers.out` | 改后 `crontab -l`、启用行计数、其它调度入口排查、cron 服务与 spool 校验 |
| `03-manual-dryrun.out` | `SNAPSHOT_AUDIT_LOG_DIR=<证据目录> bash scripts/cron/snapshot_audit.sh --dry-run` → rc=0，证明脚本仍可用且日志可重定向 |
| `04-last-cron-run-and-logdir.out` | 最后一次 cron 自动执行（02:00 exit=0）证据、末轮 JSON 的 `db=cp_lockorder_t`、`/var/log/cloudpivot` 现状、env 键名（值遮蔽） |
| `05-prod-144-readonly-check.out` | 只读核对 144 生产：crontab 仍 2 行、`/etc/cloudpivot/snapshot-audit.env` 仍 600 root:root（证明本卡没碰生产） |
| `dryrun/snapshot-audit.log` | 上面 dry-run 产生的日志（写在证据目录内，未污染 `/var/log`） |

## 7. 已知遗留（不在本卡范围内改动）

- `scripts/verify/README.md:154`（`--out /var/log/cloudpivot/snapshot-audit.latest.json` 示例）与 `:161`「cron 档（每日 02:00，已在 crontab 里）」——该文案在 .99 读起来像是本机挂了 cron，实际指的是 144 生产。**本卡没改这个文件**：它当时在工作区里已有他人未提交的改动，按路径提交会把别人半成品一起带走；建议由持有该改动的卡顺带把这两行限定为「144 生产」。
- `scripts/cron/snapshot_audit.sh` 的默认 `LOG_DIR=/var/log/cloudpivot` 保持不变（生产在用）。本机手工跑时请显式传 `SNAPSHOT_AUDIT_LOG_DIR`，`/var/log/cloudpivot/README-snapshot-audit.txt` 已写明。
