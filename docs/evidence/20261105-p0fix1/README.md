# 【P0-FIX-1】4 个 ERP 自动提醒「全部停用 / 从没跑过」——根因、修复与实测证据

- 卡号：t_75048cfc
- 时间：2026-11-05 12:00–12:2x CST
- 结论一句话：**4 个提醒本身是活的（内置 cron 一直在跑），但 `sys_job` 4 行 `status=0`
  让「定时任务」页把它显示成停用，而内置那一轮不写 `sys_job_log`，于是「跑没跑过」
  在库里查不到任何证据 —— 业务方和评审看到的都是「停用 + 0 条日志」。本卡把 4 行改成
  启用（`sys_job` 驱动，每轮落日志）、补上「编辑这些行会 422」的白名单缺口、
  并加了一条「启用 + 近 7 天有成功记录」的巡检闸。**

---

## 1. 评审结论的校正（先摆事实）

评审卡面写「4 个自动提醒从建好至今一次没跑过」。**这句不准确**，证据如下：

```
$ journalctl -u cloudpivot --since 2026-10-25 | grep "业务提醒扫描完成"
Nov 04 15:10:00 cloudpivot[519242] 业务提醒扫描完成 erp_payable_due_scan: scanned=0 sent=0 ...
Nov 04 15:20:00 cloudpivot[519242] 业务提醒扫描完成 erp_po_overdue_scan: scanned=0 sent=0 ...
Nov 04 15:30:00 cloudpivot[519242] 业务提醒扫描完成 erp_contract_expire_scan: scanned=0 sent=0 ...
Nov 04 15:40:00 cloudpivot[519242] 业务提醒扫描完成 erp_low_stock_scan: scanned=0 sent=0 ...
```

它们**跑过**（11-04 各一次），只是：
1. 走的是 `reminder::register_builtin` 这条**内置注册**路径，而 `task::run_builtin`
   只打 tracing 日志、**不写 `sys_job_log`**（`src/task/mod.rs` 的 `register_builtin`
   doc 注释：「内置任务无 `sys_job` 记录，执行结果只写运行日志（不写 `sys_job_log`）」）；
2. `sys_job` 那 4 行是 `status=0`，`last_run_at` / `next_run_at` 全 NULL
   → 「定时任务」页显示 **停用**；

⇒ 「`sys_job` 4 行 status=0 + `sys_job_log` 0 行」这个组合**不能证明「没跑过」**，
但**足以证明「业务方无从知道它跑没跑、也不会相信它在跑」**。这仍然是必须修的 P0。

**另一个真问题（评审没提、我实测发现的）**：cron 按 **UTC** 解释，
`0 10 7 * * *` = 07:10 UTC = **北京时间 15:10**，而 V0210 的 remark 写「每天 07:10」。
所以这 4 个提醒实际是**下午 3 点**才响（见 §3 的 `next_run_at` 与 §6 的到点触发实测）。

---

## 2. 4 条任务的真实配置（改动前，生产库 172.18.10.144 / cloudpivot）

| job_name | job_group | cron_exp | invoke_target | status | last_run_at | next_run_at | remark(截断) |
|---|---|---|---|---|---|---|---|
| erp_payable_due_scan | ERP | `0 10 7 * * *` | erp_payable_due_scan | **0** | – | – | 应付到期提醒…每天 07:10 扫描「7/3/1 天后到期」 |
| erp_po_overdue_scan | ERP | `0 20 7 * * *` | erp_po_overdue_scan | **0** | – | – | 采购超期提醒…每天 07:20 |
| erp_contract_expire_scan | ERP | `0 30 7 * * *` | erp_contract_expire_scan | **0** | – | – | 合同到期提醒…每天 07:30 |
| erp_low_stock_scan | ERP | `0 40 7 * * *` | erp_low_stock_scan | **0** | – | – | 低库存提醒…每天 07:40 |

- 全表 6 行（另有 `erp_depreciation_run` 0、`invariant_audit` 0，见 §8 遗留项）
- `sys_job_log` 0 行；`biz_reminder_log` 0 行；`sys_notice`（4 类 ERP 提醒）0 行
- 逐字证据：`p0fix1-prod-before.txt`

---

## 3. `status` 语义确认：**1 = 启用，0 = 停用**（不是反过来）

四处独立证据：

| # | 位置 | 内容 |
|---|---|---|
| 1 | 迁移 `backend/migrations/support/V0012__create_sys_job.up.sql:27` | `COMMENT ON COLUMN sys_job.status IS '状态：1 启用 / 0 停用'` |
| 2 | `backend/src/shared/constants.rs:34,37,50-55` | `STATUS_DISABLED: i16 = 0` / `STATUS_ENABLED: i16 = 1`；`enum EnableStatus { disabled = 0, enabled = 1 }` |
| 3 | `backend/src/task/mod.rs` `load_db_jobs()` | 只装载 `WHERE deleted_at IS NULL AND status = ?`（绑 `constants::STATUS_ENABLED`）—— 即 **status=0 的行调度器根本不装** |
| 4 | `frontend/src/views/support/job/index.vue:14,108,402` | `<t-option :value="0" label="停用" />`、`<t-radio :value="0">停用</t-radio>`、`next === 1 ? '启用' : '停用'` |

以及 DTO 注释 `dto.rs:169`「状态（1 启用 / 0 暂停）」，单测
`handler::tests::enable_status_accepts_only_zero_and_one` 也把 1=启用 / 0=暂停写死。

⚠️ 但**不能直接把「0 改成 1」当修复的全部**：V0210 当初种 0 的理由是
「内置注册已在跑，再开动态注册会让同一 cron 注册两次」。这条理由**不成立**，见 §4.1。

---

## 4. 改了什么

### 4.1 迁移 `V0245__erp_reminder_jobs_enabled`（新增 up/down）

4 行 `status 0 → 1`，remark 改为真实触发时刻（cron 值一个字未动）。

为什么「1 + 内置注册」不会跑两遍：框架 `SchedulerPlugin` 的注册表是
`HashMap<String, ScheduledJob>`，`insert()` 是**按名字覆盖**
（`backend/vendor/nik-plugin/src/scheduler.rs:390`）；`Scheduler::reload()` 的顺序是
`register_builtin()` → `load_db_jobs()` → `register_job()`，同名时**动态那条覆盖内置那条**。
覆盖语义加了回归用例守着：
`backend/src/task/mod.rs::tests::dynamic_registration_overwrites_builtin_of_same_name`
（内置注册后注册表里 1 条；再用同 job_name 注册一次仍是 1 条，且描述被后者覆盖）。

内置注册**保留**当兜底：任务行被误删 / 被停用 / 迁移没跑时，提醒照跑（只是又不写日志）。

cron 的 UTC 语义在迁移头注释里写清，并给了「若要早上 7:10 就改成 `0 10 23 * * *`」的
操作说明 —— **本卡不改 cron**（那会把全系统 40+ 个 cron 的触发时刻口径一起改掉，
属框架层时区议题，另案）。

### 4.2 `backend/src/task/mod.rs`: `is_registered_target()`

原来只认 5 个日志清理 + 不变量巡检，**漏了 4 个提醒标识**。这个名单是
`POST/PUT /support/job` 校验 `invoke_target` 的唯一依据 ⇒ 对 4 行做**编辑（PUT）**
一律 422「未注册的任务处理器」，运维只能停在停用位（启停 PATCH 不查名单，所以能启停却不能编辑）。
现在补上 `RemindKind::from_task_name(target).is_some()`，并加用例
`task::tests::registered_targets_cover_reminder_jobs`。

### 4.3 `scripts/verify/erp_invariants.py`: 新增 J 组（2 条）

- **J1**（BLOCKER）4 行都在且 `status = 1`
- **J2**（BLOCKER）每条近 7 天至少一条 `sys_job_log.status = 1`（成功）记录
  - 前置条件：近 7 天**整库** 0 条执行记录 ⇒ J2 记 INFO 跳过（全新迁移、服务还没起过的
    CI / 演练库不该因此判红；那不是账错，也不是提醒坏了）。只要有任一任务在这库上跑过
    （调度器活着），4 个提醒就**必须**各有成功记录。

### 4.4 `scripts/verify/p12_business_reminder_e2e.py`

`①b` 断言从「停用」改为「启用」，并新增 `⑨ P0-FIX-1` 五条：
启停走 `PATCH /support/job/{id}/status`（0→1 真落库）/ `PUT` 编辑不再 422 /
4 个手动执行各落 1 条成功日志 / `last_run_at`+`next_run_at` 回写。

---

## 5. 验收对照

| 验收项 | 结果 | 证据 |
|---|---|---|
| 4 条任务真实配置贴进卡、逐条说明 | ✅ | §2 + `p0fix1-prod-before.txt` |
| `status` 语义确认（贴证据） | ✅ | §3（4 处独立证据） |
| 4 条全启用 | ✅ 生产库与隔离库都 `status=1` | `p0fix1-prod-enable.txt` §④、`snap.txt` §1 |
| 手动触发全部成功 | ✅ 生产 4/4 HTTP 200；隔离库 4/4 200 | `p0fix1-prod-enable.txt` §⑤、`p0fix1-trigger.txt` |
| `sys_job_log` 有 4 条新记录 | ✅ 生产 `0 → 4` 条，全 `status=1` | `p0fix1-prod-enable.txt` §⑥ |
| 如触发通知则 `sys_notice` 有记录 | ➖ **生产 0 条**：库里当前**没有任何到期单据**（应付 7 天内到期 0 / 采购超期 0 / 合同 30 天内到期 0 / 低库存 0），`scanned=0` 属正确行为。通知链路已在隔离库用真夹具验证（见下行） | `p0fix1_prod_vol2.sh` 输出：`0 \| 0 \| 0 \| 0`；`03_isolated_e2e_35.txt` ③b/④a/⑤b/⑤e |
| 新巡检 1 条能跑通、不变量全绿 | ✅ 隔离库 **63/63，BLOCKER 0，WARN 0**（连跑两轮）；生产 `--group 任务` **2/2** | `04_invariants_round1.txt`、`04_invariants_round2.txt`；生产见 §6 |
| git commit（`fix(system):`） | ✅ 见卡内 commit | `git log -1` |

补充两条硬证据：

- **真到点自触发**：把 `erp_low_stock_scan` 的 cron 用项目接口改到 70 秒后，等它在
  `12:13:15` 自己响了一次（期间不碰任何接口），`sys_job_log` **恰好 +1 条**（5 → 6），
  且服务端日志里「业务提醒扫描完成」（内置路径的日志）**0 次** ⇒ 只有 sys_job 驱动那一轮
  在跑，不存在「同一 cron 一天跑两遍」。见 `05_scheduled_fire_proof.txt`。
- **迁移往返**：`up → down → up → up` 四步全过，down 时 remark 逐字还原成 V0210 原文，
  重放无副作用。见 `07_migrate_drill.txt`。
- **巡检不是「怎么写都绿」**：三分支各跑一遍 —— 老库（V0245 未应用）J1 报红并给出 4 行定位；
  有 `sys_job` 但近 7 天无执行记录的库 J2 显式打印「新鲜度判定跳过」；修复后的库全绿。
  见 `09_invariant_branches.txt`。

### 生产库全量巡检（对照说明）

生产 `python3 erp_invariants.py` 全量 = **48/50**，2 条红**都不是本卡引入**：

- `[系统] group_l 执行异常 — relation "biz_expense_report" does not exist`
  → 生产还在 47043b6，FIN-3（V0242–V0244）尚未部署，缺表。
- `[系统] group_h 执行异常 — 2`
  → 同上（审批/报销相关表/列版本落后），需 FIN-3 部署后复核。

本卡新增的 J1/J2 在生产上**全绿**（见 `p0fix1-prod-inv-full.txt` 尾部与
`p0fix1-prod-task-group.txt`）。生产状态落后是发布节奏问题，不属本卡范围。

---

## 6. 生产实例操作（走项目接口，不直接改库）

```
① 登录（口令取部署 env 的 NIK_ADMIN_PASSWORD）
② PATCH /api/support/job/{id}/status {"status":1}   × 4  → 全部 HTTP 200
   （PATCH 内部触发 Scheduler::reload()，日志显示「定时任务插件: 32 个任务已启动」，
     4 个 erp_*_scan 各重新排程一次）
③ POST  /api/support/job/{id}/trigger                × 4  → 全部 HTTP 200
④ 库内交叉验证：status 4×1；sys_job_log 0 → 4 条全成功；
   last_run_at = 12:07:19；next_run_at = 15:10 / 15:20 / 15:30 / 15:40（今日）
```

`next_run_at` 落在**北京时间 15:1x** 正是 §1 说的 UTC cron 语义（07:10 UTC）。

---

## 7. 连跑两轮 / 重复性

- 隔离库不变量：`04_invariants_round1.txt`、`04_invariants_round2.txt` 均 63/63 全绿
- 隔离库 e2e：连跑 2 次（12:07 与 12:09）均 35/35
- 迁移：up/down/up/up 四步可重放
- 单测 + clippy：`08_unit_tests.txt`（25 passed，clippy -D warnings 干净）

---

## 8. 发现但**没在本卡做**的事（建议另开卡）

1. **`erp_depreciation_run` / `invariant_audit` 同样 `status=0`**：降级计提（每月 1 日）与
   账务巡检（每天 02:20）也是「内置注册跑、库里无日志」，同一个病根。要不要一起改成
   启用（能留执行日志）需产品/运维拍板 —— 改法与本卡完全一样。
2. **cron 全部按 UTC 解释**：`0 3 * * *` 的日志清理实际跑在北京时间 11:00。业务侧 remark
   都按「早上/凌晨」写，实际都晚 8 小时。属框架层（`SchedulerPlugin::new` 的 clock 是
   `Utc::now`）——要么统一在调度器侧加时区，要么全库 cron 平移 -8h，两条路都要评审。
3. **生产未部署 FIN-3**（`group_l` / `group_h` 巡检红）：发布节奏问题，不在本卡。
4. **编辑 cron 后 `sys_job.next_run_at` 不重算**（实测发现）：`PUT /support/job/{id}`
   改完 cron 会 reload 调度器（真实触发时刻按新 cron，正确），但回写触发时间只发生在
   **下一次执行时**（`Scheduler::touch_job`），于是「定时任务」页在下次跑之前显示的是旧
   `next_run_at`。见 `05_scheduled_fire_proof.txt` 第 ⑥ 步（还原成 `0 40 7 * * *` 后仍显示
   上一次临时 cron 的 `2026-11-06 12:13:15`）。属展示层小 bug，改动会碰 `update` handler
   与 `next_run_at` 语义（要不要在 PUT/PATCH 里用 `CronSchedule::next_after` 预填），
   建议单独开卡。

## 9. 证据文件索引

| 文件 | 内容 |
|---|---|
| `p0fix1-prod-before.txt` | 生产改动前体检（4 行 status / 日志 0 / 台账 0 / 迁移版本） |
| `p0fix1-prod-enable.txt` | 生产启用 + 触发全过程（HTTP 码、库内前后对照、日志 4 条） |
| `p0fix1-prod-inv-full.txt` | 生产全量巡检 48/50（2 条既存红 + J 组全绿） |
| `p0fix1-prod-task-group.txt` | 生产 `--group 任务` 2/2 |
| `03_isolated_e2e_35.txt` | 隔离实例 e2e 35/35（含通知落库、幂等、⑨ 段） |
| `04_invariants_round1/2.txt` | 隔离实例全量巡检 63/63 × 2 轮 |
| `04c_invariants_final.txt` | 迁移演练 + 到点触发之后复跑，仍 63/63 全绿 |
| `05_scheduled_fire_proof.txt` | 「到点真触发 + 只跑一次」铁证 |
| `07_migrate_drill.txt` | V0245 up/down/up/up 往返 |
| `08_unit_tests.txt` | clippy -D warnings + 25 条单测 |
| `09_invariant_branches.txt` | J 组三分支验证：老库 J1 红 / 无日志库 J2 显式跳过 / 修复后全绿 |
| `snap.txt` | 隔离库最终快照（sys_job / sys_job_log / 审计 / 迁移记录） |
| `p0fix1-trigger.txt` | 隔离库 4 个任务手动触发（各 +1 条成功日志） |
