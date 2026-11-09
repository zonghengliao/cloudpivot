# NEAR-FIX-3 证据包 —— 告警接入（metrics + 3 条核心告警 + 通道 + 去重 + cron）

任务：`t_5317d520`（评审 5.8 运维就绪硬门槛 ②：告警）
落地提交：`de325030`（采样器 + 脚本 + 规则 + 文档）、`58e48d11`（去重判定修复）
本目录是 2026-11-10 03:10–03:30 CST 在 99 / 144 / 145 真机实跑的原始输出。

## 一、验收对照

| 验收 | 结果 | 证据文件 |
|------|------|----------|
| ① `config-prod.toml` / `config-test.toml` 里 `metrics.enabled = true` | ✅ `config-prod.toml`（已跟踪）本就有；`config-test.toml`/`config-dev.toml` 是**私有文件**（.gitignore），已给两台的私有配置补段，并把该段写进**模板** `config-test.example.toml` / `config-dev.example.toml` 防再犯 | `10_metrics_config.txt` |
| ② `curl /metrics` ≥ 20 个指标名 | ✅ **26** 个（采样器生效前 10 个） | `01_metrics_names_145.txt`、`02_metrics_gauges_145.txt` |
| ③ `scripts/ops/alert_check.sh` 6 类检查都能跑 | ✅ `--self-test` **34/34**（144 与 145 各一遍） | `03_self_test_144_145.txt` |
| ④ kanban 告警卡能建（实测造异常） | ✅ 真卡 `t_cfc10bad`（144 真实 BLOCKER）+ 演练卡 `t_6e586657`（造 /health 连不上，已归档） | `05_alert_real_144.txt`、`07_kanban_card_145_demo_dedup.txt` |
| ⑤ `ALERT.log` 有记录 | ✅ 144 / 145 都有，且含 **RECOVERED**（告警恢复也落日志） | `08_ALERT_log_144_145.txt`、`11_144_invariant_lifecycle.txt` |
| ⑥ 去重生效（同异常 5 分钟内只 1 张卡） | ✅ 第二次实跑输出「去重窗口内已告警过，本次抑制」，ALERT.log 行数不变；`--self-test` 端到端用例 3 条 | `07_...txt`、`03_...txt` |
| ⑦ cron 已装（144 + 145） | ✅ 两台 `*/5 * * * *`，**真跑过**（cron 最小环境实测） | `04_cron_144_145.txt`、`09_cron_runs.txt` |
| ⑧ push | ✅ `de325030` → `58e48d11` 已推 origin/main | `git log` |

## 二、每份文件是什么

- `01_metrics_names_145.txt` —— 145 隔离实例（端口 18095，profile=dev，连 `cloudpivot_dev`）的
  `/metrics` 指标家族清单与计数。**26 个**，其中 18 个是 `cloudpivot_*`：采样器新增 16 个
  （14 个业务 gauge + `cloudpivot_metrics_sampler_last_success_timestamp` /
  `runs_total` 两个自监控）。采样器生效前是 8 个框架指标 + 2 个 `cloudpivot_*` = 10 个。
- `02_metrics_gauges_145.txt` —— 同一实例的 `cloudpivot_*` 实际取值。注意 `cloudpivot_invariant_blockers 12`、
  `cloudpivot_invariant_ok 0`、`cloudpivot_impexp_import_failed_24h 4` 都是**真库真值**。
- `03_self_test_144_145.txt` —— 两台机器上 `alert_check.sh --self-test`，各 34/34 通过。
  覆盖 6 类检查的判定边界（含「ERROR 不算通过」「-1 未知不当 0」「证书边界 30 天」）
  与派发端到端（首派写 1 条 / 5 分钟内第二次仍 1 条 / dry-run 不写日志）。
- `04_cron_144_145.txt` —— 两台的 crontab 行。
- `05_alert_real_144.txt` —— **144 生产真数据**：`sys_invariant_result` 最近一行是
  `2026-11-09 10:20 | FAIL | failed=2 | blockers=1`，脚本据此告警并建卡 `t_cfc10bad`。
  这是本次接入**真实抓到的第一个生产问题**（H6：V0331/V0332 预置流程种子漏写 `is_system`）。
- `06_alert_metrics_path_145.txt` —— 145 隔离实例走 `/metrics` 数据源（不打库）判出 12 个 BLOCKER。
- `07_kanban_card_145_demo_dedup.txt` —— 145 造异常（BASE_URL 指向死端口 9）建卡 + 同 key 第二次被去重。
- `08_ALERT_log_144_145.txt` —— 两台 `ALERT.log` 与去重状态文件。
- `09_cron_runs.txt` —— cron 到点后的实际日志（证明 cron 的最小环境里脚本能跑：PATH 缺东西会立刻炸）。
- `10_metrics_config.txt` —— 各 profile 的 `[metrics]` 段现状（144 prod/test、145 dev/test）。
- `11_144_invariant_lifecycle.txt` —— **告警 → 恢复 的完整生命周期**（真数据）：
  03:18 建卡时最近一次巡检是 11-09 10:20 的 FAIL（1 个 BLOCKER，H6 种子漏写 `is_system`）；
  03:23:15 有人手动跑了一次巡检，80 项全过；03:25 前后 cron 判定 OK → `ALERT.log` 写
  **RECOVERED** 并清掉去重状态（下次再犯立刻报）。同一份文件里还记了仍未解决的真问题：
  11-10 02:20 的定时巡检没有运行（02:00–02:40 窗口内 0 条 invariant 日志）。

## 三、诚实说明（哪些行不要当真）

1. `08_ALERT_log_144_145.txt` 里 **145 的 03:08:05 两条**（`/health 返回体无法解析`、`依赖探测返回体无法解析`）
   是**修 bug 前的假告警**：`http_fetch` 用命令替换回传，body 落在子 shell 里丢了。
   已改成写临时文件（脚本内有注释）。这两条留着是为了说明「假告警长什么样」，不代表现状。
2. **144 的 03:15:36 一行**是旧版本 `--dry-run` 写的（那时 dry-run 还会落日志）；
   现在 dry-run 一个 sink 都不动。
3. `145-demo` 两行是**演练**（BASE_URL=`http://127.0.0.1:9`），对应两张卡已归档。
   板上现在只剩真卡 `t_cfc10bad`。
4. **144 的 release 二进制还没重编译**：`cloudpivot_*` 采样指标要等下次 `deploy.sh` 才出现在
   144 的 `/metrics` 上。当前 144 的 invariant / backlog 两项走 `psql` 只读回退（口令从
   `/etc/cloudpivot/cloudpivot.env` 自动探测），**告警照常生效**，只是数据源不同（脚本会打印
   `数据源 = ...` 明确告诉你走的是哪条路）。
5. 145 的 base 实例（8024，库 `cloudpivot_base`）**没有 `sys_invariant_result` 表**
   （该库没跑 V0235 巡检迁移），所以两项显示 `INFO 查不到`。这是如实上报，不是绿。

## 四、顺带发现（不在本卡范围）

- **144 生产巡检在 11-09 10:20 有 1 个 BLOCKER**（H6：V0331/V0332 预置流程种子漏写 `is_system`；
  H5 另有一条 WARN）→ 本次接入据此自动建卡 `t_cfc10bad`（assignee=laoliu）。
  **但 11-10 03:23:15 的手动巡检已经 80/80 全过**，说明这条 BLOCKER 期间已被别人修掉；
  cron 在 03:25 判定 OK 并写了 RECOVERED。卡里留了说明，别再去追一个已修的问题。
- **仍未解决**：144 的每日巡检 `0 20 2 * * *` 在 11-10 02:20 **没有运行**（既无日志也无落库行；
  11-09 的 02:20 同样缺失，唯一一条 SCHEDULED 行落在 10:20）。「没跑」和「跑挂」都会被
  `invariant` 的 36h 过期判据在 11-11 早上抓出来，但根因（调度器 / 脚本 / 落库）要人看。
- **99（本机）根盘 98%**：`alert_check.sh` 在 99 上跑会直接报磁盘 ALERT（阈值 85%）。
  99 未装 cron，这条只是提醒 —— 真满了 worker 会先炸（`disk_guard.sh` 阈值 75%）。
