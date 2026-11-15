# 云枢日志切割（P3-15）

按天切割 + 保留 30 天。**两套配置，缺一不可**，因为云枢的日志分两处落：

| 日志 | 落点 | 由谁管 | 配置 |
|:--|:--|:--|:--|
| 运维/告警/部署脚本日志 | `/var/log/cloudpivot/*.log`（cron `>>` 追加） | logrotate | 本目录 `cloudpivot` → `/etc/logrotate.d/cloudpivot` |
| **应用进程日志** | **journald**（三个单元都是 `StandardOutput=journal`） | journald | 本目录 `journald-cloudpivot-retention.conf` → `/etc/systemd/journald.conf.d/` |

> 只装 logrotate 是**不够的**：`logrotate` 匹配不到 journal 里的应用日志，
> 「保留 30 天」对应用日志会是空的。这就是第二份配置存在的原因。

## 安装 / 验证

```bash
# 安装 + 断言（本机）
bash scripts/ops/logrotate/install.sh

# 安装 + 断言（远端；145 测试机 / 144 生产机）
bash scripts/ops/logrotate/install.sh --host 172.18.10.145
bash scripts/ops/logrotate/install.sh --host 172.18.10.144

# 只检查不改动（适合巡检 / 上线前复核）
bash scripts/ops/logrotate/install.sh --host 172.18.10.144 --check

# 强制切一次（验收：能立刻看到 .gz 归档）
bash scripts/ops/logrotate/install.sh --host 172.18.10.144 --rotate

# 语义自证：临时目录里连切 32 天，证明「只留 30 份」（不碰线上日志，任何机器可跑）
bash scripts/ops/logrotate/install.sh --self-test

# 修 /etc/logrotate.d 里的重复 glob（仅当两份逐字节相同时才改名；不同则拒绝并打 diff）
bash scripts/ops/logrotate/install.sh --host 172.18.10.144 --fix-conflicts
```

退出码 0 = 断言全绿，非 0 = 有断言失败（可直接进 cron / CI）。

## 断言清单（`--check` / `--rotate`）

| # | 断言 |
|:--:|:--|
| ① | `/etc/logrotate.d/cloudpivot` 存在且 `644 root:root` |
| ② | 线上片段与仓库源逐字节一致（sha256） |
| ③ | 本片段 `logrotate -d` 解析通过（rc=0，输出无 `error:`） |
| ③b | **整份配置** `logrotate -d /etc/logrotate.conf` rc=0 —— 定时器每天跑的是这份，别人的片段坏了也会让整个 run rc=1 |
| ④ | `logrotate.timer` active+enabled（`OnCalendar=daily` → 每天真会跑） |
| ⑤ | 所有 `.gz` 归档 `gzip -t` 通过 |
| ⑥ | 每个基名归档数 ≤ 31 |
| ⑦ | journald 生效：`MaxFileSec=1day` + `MaxRetentionSec=30day`，且 `systemd-journald` active |

`--self-test` 另有 12 条语义断言（`rotate 30` 连切 32 天后恰好 30 份、`.1`=最新、`.30`=最老、
`notifempty` 不切空文件、`missingok` 无匹配不报错、`copytruncate` 清空基文件、重复执行幂等）。

## 必须知道的边界

1. **journald 的保留期是整机口径**，不是按服务隔离。`MaxRetentionSec=30day` 对 144/145 上
   sshd / nginx / cron / hermes 等所有写入 journal 的进程同样生效。journald 不提供 per-unit 保留期。
2. **`copytruncate` 有微秒级丢行窗口**（copy 与 truncate 之间的写入）。因此这里只切运维脚本日志，
   不切任何业务事务日志（云枢业务日志本来也不在文件里）。
3. **logrotate 只管 `.1..rotate` 号段**。若有人手工塞进 `xxx.log.31.gz` 这类超出 `rotate` 号的归档，
   logrotate 不会主动清理，要人工删。这正是 `--self-test` 用「连切 32 天」而不是「预置 35 份」验证的原因。
4. **应用日志按天切分依赖 journald 自身**（`MaxFileSec=1day`），首次自动切分发生在配置生效后的
   下一次轮转点（`journalctl --rotate` 可手工触发同样路径）。
5. **`.log` 之外的日志不在范围内**：`snapshot-audit.log.2026-11-12` 这种自管日期的归档
   （脚本自己 `mv`）、`/var/log/cloudpivot/hermes/*.log`（hermes 自管）都不匹配 `*.log` 通配，保持原样。
6. **`/etc/logrotate.d` 是按目录整体解析的**：任意一份片段坏了，整份配置 rc=1、`logrotate.service`
   天天红，而你自己的片段可能是好的（假绿的来源）。断言 ③b 因此跑**整份**配置。
   已知同类：`cloud-init` / `cloud-init-base` 重复 glob —— 用 `--fix-conflicts` 改名成 `.disabled`
   （logrotate 忽略该后缀；仅在两份逐字节相同时才动，不同则拒绝）。

## 改配置的正确姿势

改本目录两个配置文件 → 提交 → 在 145 跑 `install.sh --host 172.18.10.145 --check`
→ 确认断言②（sha256 一致）先红（线上还是旧的）→ 跑 install → `--check` 转绿 → 再上 144。
**不要在线手改 `/etc/logrotate.d/cloudpivot`**：下次 install 会按仓库覆盖，且断言②会一直红。
