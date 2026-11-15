# P3-15 日志按天切割 + 保留 30 天 — 证据（2026-11-15）

执行人：manong（开发团队总管）· 机器：145（测试）、144（生产）

## 结论

两套配置双双落地并验收全绿：logrotate 管 `/var/log/cloudpivot/*.log`（cron 脚本日志），
journald 保留期管**应用进程日志**（三个单元都是 `StandardOutput=journal`，logrotate 碰不到）。
`--check` 在两台机器上各 8/8 PASS，`--self-test` 12/12 PASS。

## 文件

| 文件 | 内容 |
|:--|:--|
| `01_archives_and_config.txt` | 两机现场：`/etc/logrotate.d/cloudpivot` 权限+sha256、`logrotate -d` 输出、`.gz` 归档清单、目录大小、journald 生效配置与容量、服务状态 |
| `02_check_145.txt` | 145 `install.sh --check` 全量断言输出（8/8 PASS，rc=0） |
| `03_check_144.txt` | 144 `install.sh --check` 全量断言输出（8/8 PASS，rc=0） |
| `04_selftest.txt` | `install.sh --self-test` 语义自证（12/12 PASS：连切 32 天恰好留 30 份、notifempty、missingok、幂等） |

复现命令：

```bash
bash scripts/ops/logrotate/install.sh --host 172.18.10.145 --check
bash scripts/ops/logrotate/install.sh --host 172.18.10.144 --check
bash scripts/ops/logrotate/install.sh --self-test
```

## 关键实测数字

| 机器 | 切前 | 切后 | 归档 | 目录大小 |
|:--|:--|:--|:--:|:--|
| 145 | 18 个非空 `*.log` | copytruncate 清零 | 18 个 `.gz`（0 损坏） | 3.0M → 188K |
| 144 | 17 个非空 `*.log` | copytruncate 清零 | 17 个 `.gz`（0 损坏） | 4.0M → 380K |

生产侧副作用核查：`cloudpivot.service` 未重启（MainPID 633825 自 2026-11-14 16:45 未变）、
`GET :8023/health` → 200、切割后新日志继续进 journal、
`journalctl --rotate --sync` 在两机成功且查询正常、`systemd-journald` 在重启后 active。

规程文档：`docs/运维规程-日志切割与保留.md`
