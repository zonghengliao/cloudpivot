# P2-6 收口 C · 144 生产挂 snapshot_audit cron（env + 通知钩子）证据包

> 卡：`t_6156104c`（【收口C·接线】） · 执行：backend-developer · 日期：2026-11-04 18:04–18:08 CST
> 上游决定：manong `t_3de61e4b` → `P2-6-生产首跑命中收口决定.md` **D-4：先降噪、再挂**
> 前置：卡 A `t_237f1caa` 已落地（DI-19 分级 + DI-18 精确豁免），生产只读复跑 BLOCKER 0 / WARN 0

## 一句话结论

144 生产机上：`/etc/cloudpivot/snapshot-audit.env` 已写（600 root:root）、手工首跑 **exit=0 / BLOCKER 0 / WARN 0 / INFO 80**、
通知钩子 dry-run + 真实投递双向验证通过、`crontab` 以**增量**方式加了 `0 2 * * *` 一行（既有任务一条没丢，装前有备份）。
**唯一未覆盖的分支**：生产零 BLOCKER，故「真实 BLOCKER → 触发通知」这条路径无法在不动数据的前提下现场跑通
（造 BLOCKER 需改业务数据 = 红线）；钩子本体已按 `notify()` 传入的同一组环境变量实测 exit=0。

## 各步原始证据

| 步 | 做了什么 | 证据文件 |
|---|---|---|
| 1 | 写 `/etc/cloudpivot/snapshot-audit.env`（`chmod 600 root:root`，口令从 `cloudpivot.env` 取值、全程不回显） | `01-env-file.txt` |
| 2 | 手工跑一轮（**不加 cron**）：`bash /opt/cloudpivot/scripts/cron/snapshot_audit.sh; echo exit=$?` → `exit=0` | `02-manual-run-first.txt` |
| 2b | 用 cron 的最小环境（`env -i PATH=/usr/bin:/bin HOME=/root`）复跑真正的 cron 命令行 → `exit=0`（证明 cron 里 python3 / env 文件都能找到） | `03-cron-style-run.txt` |
| 2c | `--install-views` 后 `di` schema 视图数量 / 11 列契约 / 豁免台账 | `04-views-and-waiver.txt` |
| 3 | 日志与 JSON 落位（`blocker/warn/info` 三计数） | `05-json-summary.txt` |
| 1b | 通知钩子 `--dry-run`（只打印正文与命令，不发消息） | `06-notify-dryrun.txt` |
| 1c | 通知钩子真实投递（2 条自测卡，标题均带「接线自测·非告警」）+ 只读回查频道消息 | `07-notify-realtest.txt` |
| 4 | `crontab` 增量安装（装前备份 `/etc/cloudpivot/crontab.bak.1793786757`）+ 回读 | `08-crontab.txt` |
| 5 | 红线自证：巡检零 DML、BLOCKER 0 轮次无 ALERT | `09-redlines.txt` |

## 关键数字（原始输出见对应文件）

```
[2026-11-04 18:04:43] 开始：db=cloudpivot@127.0.0.1 配置=/etc/cloudpivot/snapshot-audit.env
已幂等安装 di 巡检视图（25 → 25 个 c_di* 视图）
[2026-11-04T18:04:43+08:00] db=cloudpivot@127.0.0.1 BLOCKER=0 WARN=0 INFO=80 checks=25(passed=22 skipped=0 failed=0) exit=0
[2026-11-04 18:04:45] 结束：exit=0  日志保留 30 天  轮转归档：0 份
```

- `di` 视图 **25 个**（c_di01…c_di25，全部 11 列）—— 卡面写的「22（16 既有 + 6 新增）」是 P2-6b 之前的数，
  P2-6b 已先合入 DI-23/24/25 三条（父卡 `t_237f1caa` 的 deviations 里已说明）。
- `di.v_poly_ref_waiver` 1 行：`DI-18 / biz_inventory_txn / 01a19108-ee53-73c6-bdaa-0109614ea963 / 2026-11-04 → expires_on 2027-02-04`。
- 生产命中：DI-18 **2 条 INFO**（其中 1 条 `已核查豁免#2026-11-04：`，另一条是 `doc_type=ADJUST / NO_TARGET` —— 改之前就是 INFO）、
  DI-19 **47 条 INFO**（PENDING 0）、DI-20 31 条 INFO。卡面「DI-18 1 条」的口径见上。

## cron 实际形态（卡面的「既有 5 个任务」是 .99 的口径，不是 144）

144 `crontab -l`（装前）只有 **1** 条；`.99` 本机的 5 条（`idle_unblock.sh` / `disk_guard.sh` / `deploy_check_v3.sh` /
`gen_test_report.py` / `zero_output_check.py`）在 `.99` 上，**本卡未动 .99 一行**。

```
装前：0 2 * * * /opt/cloudpivot/scripts/erp-health-daily.sh >/dev/null 2>&1
装后：0 2 * * * /opt/cloudpivot/scripts/erp-health-daily.sh >/dev/null 2>&1
      0 2 * * * /opt/cloudpivot/scripts/cron/snapshot_audit.sh >> /var/log/cloudpivot/snapshot-audit.cron.log 2>&1
```

- 用 `crontab -l > tmp; 追加; crontab tmp` 增量方式，装前备份 `/etc/cloudpivot/crontab.bak.<ts>`；
- `cron` 服务 `active`，`journalctl -u cron` 有 `18:06:01 (root) RELOAD (crontabs/root)`；
- 全量落日志，**没有** `>/dev/null 2>&1`（红线）。

## 通知钩子怎么接的

```
snapshot_audit.py 有 BLOCKER（或巡检项执行失败）
  └─ notify(cmd)  ← cmd = $SNAPSHOT_AUDIT_NOTIFY_CMD（取自 /etc/cloudpivot/snapshot-audit.env）
       └─ /opt/cloudpivot/scripts/cron/snapshot_audit_notify.sh
            ├─ 读 SNAPSHOT_AUDIT_JSON / _BLOCKERS / _SUMMARY（由 notify() 注入）
            └─ ssh root@172.18.10.99 → python3 feishu_alert_send.py --stdin
                 └─ 飞书 Open API v1 卡片 → 团队告警频道（凭据在 99 的 laoliu .env，生产机不另存 bot secret）
```

- 只有 `ERROR/BLOCKER` 才打扰人；`WARN/INFO` 只写日志与 JSON（与设计文档 D-3 一致）。
- 验接线不发消息：`bash /opt/cloudpivot/scripts/cron/snapshot_audit_notify.sh --dry-run`（或 `SNAPSHOT_AUDIT_NOTIFY_DRYRUN=1`）。
- 发送器 `scripts/cron/feishu_alert_send.py` 在仓库留档；实际部署在
  `99:/root/.hermes/profiles/backend-developer/scripts/feishu_alert_send.py`（两份 md5 一致）。

## 偏差与坑（照实记）

1. **卡面「既有 5 个 cron 任务」是 .99 口径**，144 实际只有 1 条；增量安装后 144 = 2 条，1 条未丢。
2. **卡面「22 个 c_di\* 视图」**：实测 25（P2-6b 已加 DI-23/24/25）。
3. **卡面「DI-18 1 条 INFO」**：实测 2 条 INFO（另 1 条 ADJUST/NO_TARGET 本来就是 INFO）。
4. **飞书回读校验第一版过严**：卡片落库时 markdown `**` 会被吃掉，`body.splitlines()[0][:20]` 直接比对必然不命中
   （第一次自测因此误报「投递失败」，实际卡片已到达）。已改为「去 markdown 标记 + 把 body.content 这层 JSON 字符串解一层再比」，
   并对**已发送的那条消息**做只读复验（命中=True，未再补发）。
5. **通知链是 ssh 到 99 再发**：生产机没有 bot secret（有意）。代价是 99 不可达时告警发不出去——
   但那只影响「打扰人」，不影响巡检本身：`exit code` 与 `/var/log/cloudpivot/snapshot-audit.log` 仍在。

## 回滚（任一步不合意时）

```bash
# 1) 撤 cron（用装前备份整体恢复）
crontab /etc/cloudpivot/crontab.bak.1793786757
# 2) 撤钩子（只影响告警，不影响巡检）
sed -i 's#^SNAPSHOT_AUDIT_NOTIFY_CMD=.*#SNAPSHOT_AUDIT_NOTIFY_CMD=#' /etc/cloudpivot/snapshot-audit.env
# 3) 代码回滚
git revert e721a78          # 钩子 + 发送器
```

`di` 视图不需要回滚（`CREATE OR REPLACE VIEW` 幂等；真要清则 `DROP SCHEMA di CASCADE`，但那会连带 `v_poly_ref_map` 登记，
**不要**为了让巡检变绿去动它）。
