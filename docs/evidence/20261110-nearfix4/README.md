# NEAR-FIX-4 证据：修账后巡检结论不刷新

- 卡：`t_4d341c78`（【NEAR-FIX-4】修账后巡检结论不刷新 → invariant 告警对着旧 FAIL 响到次日）
- 前序卡：`t_cfc10bad`（本次误报的处置，见 `attachments/t_cfc10bad/evidence-invariant-144.md`）
- 主机：144 = 172.18.10.144（生产 `/opt/cloudpivot`，`cloudpivot.service` :8023，库 `cloudpivot`）
- 采集时间：2026-11-10 03:33 ~ 03:36 CST
- 落地提交：`2a6918cf`（主修）、`d9bf3bcf`（收口文案）、`61cf6c60`（默认不试 admin 登录 + 演练脚本）

## 病

巡检结论是**快照**，内置 cron 一天只落一条（`0 20 2 * * *` 按 UTC 解释 = 每天 10:20 CST）。
「修账的迁移」与「告警读的结论」之间没有任何先后关系判断：

| 时间 | 事实 |
|---|---|
| 11-09 10:20 | SCHEDULED 巡检落库 FAIL（H6 BLOCKER + H5 WARN，V0331/V0332 种子漏写 `is_system`） |
| 11-09 22:23 | `91525642` 新增 V0366 修掉该缺陷并应用 ⇒ **账已经是对的** |
| 11-10 03:15 / 03:18 | 告警仍在喊「账已错」—— 它读的是 17 小时前那条结论 |

中间每个 5 分钟 tick 都会把「账已错」再扣一次（去重窗口只有 300s）。

## 两条修法（互为兜底）

| # | 落点 | 文件 | 行为 |
|---|------|------|------|
| ① | 发布/迁移后主动重跑 | `scripts/ops/invariant_rerun.py`（新）+ `scripts/deploy/deploy.sh` 第 `[7/9]` 步 + 144 `/root/prod_deploy.sh` 第 `⑥b` 步 | 迁移 + 重启之后重跑一次；**幂等**（结论不旧就跳过）、**非致命**（失败不回滚发版） |
| ② | 判定侧识别「结论过期」 | `scripts/ops/alert_check.sh`：`decide_invariant` 加第 5 段 `migration_newer_seconds`、`check_invariant` 一条 SQL 同取结论与迁移时间 | 结论早于最后一次迁移 ⇒ `blockers>0` 降 **WARN「待重跑」**、`OK` 降 **INFO**；「既早于迁移、又 >36h 没重跑」才 ALERT（那时确实无人复核） |

## 证据清单（全部为 144 上实跑输出）

| 文件 | 验的是什么 | 结论 |
|---|---|---|
| `01_drill_stale_conclusion_144.txt` | 干净库演练四场景（schema 从生产 `pg_dump -s` 搬，数据全造） | A 过期 FAIL → **WARN「待重跑」**；B 当前 FAIL → **ALERT「账已错」**（旧口径没被吞）；C 过期 OK → INFO；D 过期 40h → ALERT |
| `02_drill_metrics_branch_144.txt` | `/metrics` 分支（144 现在没有 `cloudpivot_invariant_*` gauge，故用假 `/metrics`）+ psql 不可用降级 | metric 分支同样取到 `migration_newer=54000` → WARN；psql 不可用时**不崩**，显式打「无法判断是否过期」并按旧口径判 |
| `03_step7_exact_144.txt` | `deploy.sh [7/9]` 步的**逐字命令**在「结论早于迁移」时真的主动重跑 | 落 `MANUAL` 行，`run_at` 晚于 `max(applied_at)`，`newer=0`；随后 alert_check 报的是**真实结论**而非「过期」 |
| `04_rerun_self_test_144.txt` | `invariant_rerun.py --self-test` | 13/13（含库内 SQL 归一实测） |
| `05_alert_self_test_144.txt` | `alert_check.sh --self-test` | 41/41（原 34 + 新增 7 条过期判定） |
| `06_prod_rerun_144.txt` | 生产 `--force` 重跑（落真结论） | `MANUAL` OK 80/80，`run_at=1794252974` > `max(applied_at)=1794244598`，`newer=0` |
| `07_prod_state_144.txt` | 生产库现状 + `applied_at` 单位分布 | 最近 3 条结论全 OK；`ms 行 4 / s 行 294`；naive `max()` 会把时间顶到 58826 年，归一后 2026-11-10 01:16:38 |
| `08_deploy_wiring_144.txt` | 真实部署流程里的 `[7/9]` 步（`--skip-migrate --skip-backend --skip-frontend`） | 步进正常：`已是最新：最后结论 OK@… 不早于最后一次迁移 → 无需重跑`，收口显示 `账务巡检结论 结论不旧，无需重跑` |

## 演练怎么复现（都在临时库上跑）

```bash
# 临时库（库名必须 cp_drill_*，否则脚本直接拒跑）
bash scripts/verify/nearfix4_stale_drill.sh          # 四场景
bash scripts/verify/nearfix4_metrics_branch_drill.sh # /metrics 分支 + psql 降级
bash scripts/verify/nearfix4_step7_exact_drill.sh    # deploy [7/9] 逐字命令
bash scripts/verify/nearfix4_collect_evidence.sh     # 重新采集本目录
```

演练库 `cp_drill_inv4` 的 schema 来自生产 `pg_dump -s -t public.schema_migrations -t public.sys_invariant_result`，
**数据全造**（生产库不接受任何演练写入）。

## 踩到的两个坑（都已写进代码注释）

1. **`schema_migrations.applied_at` 混单位**：契约是 epoch **秒**（`nik-db/src/migrate/history.rs`），
   但 144 有 4 行是手工 psql 补记账写成的**毫秒**。直接 `max(applied_at) > run_at` ⇒ 毫秒值永远
   压过秒值 ⇒ **每条结论都被判「过期」**，真正的错账会被永久降级成 WARN 而漏报。
   两处（alert_check 的 SQL、invariant_rerun 的新鲜度核对）都先归一：`> 1e11 视为毫秒`。
   场景 B 就是这个回归守门（库里有毫秒旧记账，仍必须报「账已错」）。
2. **别拿生产 admin 试口令**：最初版本默认先试 `POST /api/support/invariant/run`，144 的
   admin 口令漂移（t_c9c2cd5f）⇒ 每次探测都给 admin 记一次登录失败；
   03:33/03:34/03:35 三次探测把 admin 锁了（`auth.max_login_fail_count=5` / `auth.lock_minutes=30`，
   04:05 自动解锁；`login_fail_count` 归零要等一次**成功**登录）。
   已改成**默认直接走脚本 + 落库**，HTTP 路径要显式 `--http`。

## 顺带发现（未处置，不属本卡）

- 144 的生产工作树里 **V0368（receipt_refund_page_and_price_page）尚未记账**（库内最大 V0367，
  磁盘已有 V0368）。这是本卡合入前就存在的漂移（`9dfa91da` 的树里就带着它），
  `deploy.sh` 第 [9/9] 步因此显红；下一次正规部署会在第 [5/9] 步应用它。本卡只做了
  `--skip-migrate` 的发版连通性验证，没有代跑别人的迁移。
- Prometheus 规则（`scripts/ops/prometheus/cloudpivot.rules.yml` 的 `CloudpivotInvariantBlockers`）
  还没有这条 staleness 判断；当前没装 Prometheus，上之前要同步补。
