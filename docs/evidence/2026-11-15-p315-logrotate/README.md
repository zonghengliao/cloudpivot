# P3-15 日志按天切割 + 保留 30 天 — 证据（2026-11-15/16）

执行人：manong（开发团队总管）· 机器：145（测试）、144（生产）

## 结论

两套配置双双落地并验收全绿：logrotate 管 `/var/log/cloudpivot/*.log`（cron 脚本日志），
journald 保留期管**应用进程日志**（三个单元都是 `StandardOutput=journal`，logrotate 碰不到）。
`--check` 在两台机器上各 **9/9 PASS**，`--self-test` **12/12 PASS**，
并且等到了 `logrotate.timer` **自己触发**（不是靠手工 `-f`）。

## 文件

| 文件 | 内容 |
|:--|:--|
| `01_archives_and_config.txt` | 两机现场：片段权限+sha256、冲突修复后目录清单（`cloud-init-base.disabled`）、`.gz` 归档、目录大小、journald 生效配置与容量 |
| `02_check_145.txt` | 145 `install.sh --check` 全量断言（9/9 PASS，rc=0） |
| `03_check_144.txt` | 144 `install.sh --check` 全量断言（9/9 PASS，rc=0） |
| `04_selftest.txt` | `install.sh --self-test` 语义自证 12/12（连切 32 天恰好留 30 份 / notifempty / missingok / 幂等） |
| `05_timer_and_unit.txt` | 定时器自动触发前后（状态日期翻到 11-16、`.2.gz` 出现）、**同一份日志里修前的 Failed 与修后的 Finished**、整份配置 `-d` rc=0、云枢服务健康 |

复现命令：

```bash
bash scripts/ops/logrotate/install.sh --host 172.18.10.145 --check
bash scripts/ops/logrotate/install.sh --host 172.18.10.144 --check
bash scripts/ops/logrotate/install.sh --self-test
```

## 关键实测数字

| 机器 | 切前 | 切后 | 归档 | 目录大小 |
|:--|:--|:--|:--:|:--|
| 145 | 18 个非空 `*.log` | copytruncate 清零 | 20 个 `.gz`（0 损坏） | 3.0M → 200K |
| 144 | 17 个非空 `*.log` | copytruncate 清零 | 18 个 `.gz`（0 损坏） | 4.0M → 384K |

归档数比基文件多，是因为定时器次日又切了一轮（`.2.gz` = 前一天内容），正是「按天」生效的表现。

## 定时器自己跑了一次（最硬的证据）

| 机器 | 触发时刻 | 结果 |
|:--|:--|:--|
| 144 | `2026-11-16 00:01:38` | `alert-check.cron.log.1.gz`（691B，新一天）+ `.2.gz`（74949B，前一天）；状态文件日期翻到 `2026-11-16` |
| 145 | `2026-11-16 00:08:14` | 同上形态（`.1.gz` 838B / `.2.gz` 61240B） |

空文件（`backup.log` / `snapshot-audit.log`）按 `notifempty` 被跳过，状态日期停在前一天 —— 与语义一致。

## 顺手修掉的既存缺陷（不属本卡引入）

`/etc/logrotate.d/cloud-init` 与 `cloud-init-base` 逐字节相同（glob 都是 `/var/log/cloud-init*.log`），
重复条目让 logrotate 整份配置 **rc=1**，`logrotate.service` 自 **2026-11-12** 起每天 Failed。
按「两份逐字节相同才动」的守卫改名成 `cloud-init-base.disabled`（logrotate 忽略该后缀，99 上早已是这个形态）。

`05_timer_and_unit.txt` 里 144 的同一份 journal 可以看到前后对照：

```
00:01:39  logrotate.service: Main process exited, code=exited, status=1/FAILURE   ← 修前
00:11:00  logrotate.service: Deactivated successfully / Finished                  ← 修后
```

修后 `systemctl show -p Result -p ExecMainStatus` = `success 0`，`logrotate -d /etc/logrotate.conf` = `rc=0`、`error` 行 0。

## 生产副作用核查

`cloudpivot.service` 全程**未重启**（MainPID 633825 自 2026-11-14 16:45 未变）、`GET :8023/health` → 200、
切割后应用日志继续进 journal、`systemd-journald` 重启后 active、`journalctl --rotate/--sync` 正常。

规程文档：`docs/运维规程-日志切割与保留.md`
