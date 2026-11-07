# P0-06 预算控制链演练：编制 → 占用 → 超支拦截 → 执行分析

- 演练环境：**145 测试机**（`http://172.18.10.145` nginx :80 → 后端 :8033，库 `cloudpivot_dev`），**全程未连 144 生产**
- 演练时间：2026-11-07 20:35–20:40（CST）
- 账号：admin（`NIK_ADMIN_PASSWORD`）、复用 P0-05 的 `emp_drill` / `lead_drill` / `fin_drill` / `demo_lead` / `gm_drill`
- 数据隔离：新建部门 **`DRILL_BDG 演练预算部`**（负责人 `lead_drill`），预算/报销全部挂在该部门，不触碰 P0-05 的 `DRILL_EXP` 数据
- 取证脚本：本目录 `00_env.sh` … `11_verify.sql`（令牌在仓外 `/root/.p006-tokens`，`raw/` 里只有响应体）

> **结论先行：预算控制不是摆设。** BLOCK 槽下一分钱超支都会被 409 挡住（`code=66037`），单据停在 DRAFT、0 占用行、0 流程实例、预算台账不变。
> 真正的坑不在「拦不拦」，而在「**预算没走到 ACTIVE 就等于没预算**」——编完不审批 = 零控制（见 ⑥-8 反证），这才是业务上容易误判成「预算控制形同虚设」的地方。

---

## 演练结果

### 链 1：预算编制 → 余额（① ②）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 1 | `POST /api/business/erp/budget`（2026 / DRILL_BDG / 5602.02 差旅费 / total 10000 / `control_mode=BLOCK` / 11 月 10000） | 200 | `biz_budget` BDG-20261107-000002 `DRAFT` v1 10000.0000，`biz_budget_period` 12 行 | ✅ |
| 2 | `POST /api/business/erp/budget/{id}/submit` | 200 | `SUBMITTED`，`biz_flow_instance` BUDGET_APPROVE 1 个 | ✅ |
| 3 | 审批：部门主管 2 票（`demo_lead` + `lead_drill`，DEPT_LEADER 全员）+ 财务经理 1 票（`fin_drill`） | 200×3 | `ACTIVE`，`approved_at` 有值，`effective_from=2026-12-01` | ✅ |
| 4 | `GET /api/business/erp/budget/{id}` 查余额 | 200 | `year_used=0`、`year_available=10000.0000`、11 月 `amount=10000 / used=0 / available=10000`；`biz_budget_usage` 0 行 | ✅ |

**② 的验证：剩余预算 = 10000 ✅，已用 = 0 ✅。**
（注意：`biz_budget` **没有 `used_amount` 列**，已用一律由 `biz_budget_usage` 现算 —— 单一事实源，见「问题 8」）

### 链 2：预算内 → 临界 → 超支（③ ④ ⑤）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 5 | ③ 报 3500（差旅费 2026-11-01）→ 建单 | 200 | `EXP-20261107-000009` `DRAFT` 3500 | ✅ |
| 6 | ③ 提交 | 200 | `SUBMITTED`，`over_budget=0`，占用行 `OCCUPIED 3500`（`budget_id` 已回填），1 个流程实例 | ✅ |
| 7 | ③ 预算台账 | 200 | 已用 **3500**、剩余 **6500**、11 月 used 3500 / available 6500 | ✅ 占用即时生效 |
| 8 | ③ 审批通过（直属上级 → 财务经理） | 200×2 | 单据 `APPROVED`；占用 `OCCUPIED → CONSUMED`；凭证 **PZ-20261107-000035 借 5602.02 3500 / 贷 2241.01 3500** | ✅ |
| 9 | ④ 报 6000（2026-11-05）→ 建单 → 提交 | 200 / 200 | `EXP-20261107-000010` `SUBMITTED`，`over_budget=0`，`OCCUPIED 6000` | ✅ |
| 10 | ④ 预算台账 | 200 | 已用 **9500**、剩余 **500**（11 月 used 9500 / available 500） | ✅ 临界未越界 |
| 11 | ④ 审批通过 | 200×2 | `APPROVED`，`CONSUMED`，凭证 **PZ-20261107-000036 借 5602.02 6000 / 贷 2241.01 6000**；已用仍 9500（口径不变） | ✅ |
| 12 | ⑤ 报 1000（2026-11-08，累计 10500 > 10000）→ 建单 | 200 | `EXP-20261107-000011` `DRAFT` 1000 | ✅ |
| 13 | ⑤ **提交（关键一步）** | **409** | **`code=66037`**；单据仍 `DRAFT`、`over_budget=0`、**0 占用行、0 流程实例**、槽已用仍 9500 | ✅ **被拦住，没有默默放过** |

⑤ 的原始报文（`raw/r1000_submit.json`）：

```
code=66037
msg=报销单 EXP-20261107-000011 超出预算额度（1/1 个预算槽超额度，超支额最大 500.0000（策略 标记并强制审批））：
    当前策略是 BLOCK，提交被拒绝。请联系财务调整预算，或让管理员把控制策略改为 WARN（标记 + 强制审批）。
```

- **是配置问题还是代码没实现？** 都不是 —— 拦截逻辑（`budget_guard::BudgetVerdict::blocks()` + `occupying` 同事务回滚）已实现且生效。
- 报文里「策略 标记并强制审批」是**文案 bug**（取全局 `WARN` 标签），真正的阻断来自槽级 `BLOCK`，见「问题 4」。

### 链 3：超预算的放行路径（⑥）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 14 | ⑥-a 编 1000 / `WARN` 预算（5602.03 办公费）→ 审批 → `ACTIVE` | 200 | BDG-20261107-000003 `ACTIVE` `WARN` 1000 | ✅ |
| 15 | ⑥-b 报 3500（预算 1000）→ 提交 | 200 | `EXP-20261107-000012` `SUBMITTED`，**`over_budget=1` / `over_amount=2500`**，`OCCUPIED 3500`（over=1） | ✅ WARN = 打标记 + 放行 |
| 16 | ⑥-c 审批链对比 | 200×2 | 超预算单走 **`n_sup → n_fin`**（与普通单完全一致）；流程定义 7 节点里**没有任何 over_budget 分支/节点** | ✅ **没有独立的「超预算审批」流** |
| 17 | ⑥-d 审批通过 | 200×2 | `APPROVED`（`over_budget=1` 保留），占用 `CONSUMED`，凭证 **PZ-20261107-000037 借 5602.03 3500 / 贷 2241.01 3500** | ✅ 能批完才会暴露给财务 |
| 18 | ⑥-e 执行报表 | 200 | 5602.03：额度 1000 / 已用 3500 / 可用 **−2500** / 执行率 **350%** / `over=true` | ✅ 超支在报表里现形 |
| 19 | ⑥-f BLOCK 下合法放行：`POST /budget/{id}/adjust` 10000 → 11000 | 200 | 新版本 BDG-20261107-000004 **v2 / ADJUST**，v1 转 `SUPERSEDED` | ✅ 调整 = 新版本走审批（不是改老版本） |
| 20 | ⑥-g 调整审批（财务复核 → 总经理） | 200×2 | v2 `ACTIVE` 11000，`effective_from=2026-12-01` | ✅ |
| 21 | ⑥-h 重提被拦的 1000 单 | **200** | `SUBMITTED`、`over_budget=0`、`OCCUPIED 1000`（`budget_id`=v2） | ✅ 追加额度后正常占用 |
| 22 | ⑥-i 台账/报表复核 | 200 | 11000 / 已用 **10500** / 剩余 **500** / 执行率 **95.45%** | ✅ |

⑥ 的三个答案：

1. **BLOCK 策略下没有「审批放行超支」**，只有两条合法路径：① 改控制策略为 `WARN`（打标记 + 强制审批）；② **调整预算**（新版本 + 审批）后重提。
2. **WARN 策略下也没有「超预算专审」**：`form_data.over_budget=true` 只是单据上的标记，审批链和普通单一模一样（实测 `n_sup → n_fin`），系统里没有任何「财务总监加签」之类的节点。
3. 反证（说明「零控制」怎么出现）：

| 步骤 | 场景 | HTTP | DB | 结论 |
|---|---|---|---|---|
| 23 | ⑥-8 5602.04 编了 500 `BLOCK` 预算但**只停在 DRAFT**，报 500 | **200 放行** | `over_budget=1 / over_amount=500`，占用行 `budget_id` 为空 | ⚠️ **预算没 ACTIVE = 零控制** |
| 24 | ⑥-9 5602.05 **完全没编预算**，报 800（全局 WARN） | **200 放行** | `over_budget=1 / over_amount=800`，占用行 `budget_id` 空 | ⚠️ 未编预算也放行（标超支=全额） |

### 链 4：预算执行分析（⑦）

卡面写的 `GET /erp/budget/analysis?year=2026` **不存在**：路径被 `/budget/{id}` 吃掉，返回 `400 code=10001「id 不是合法的 id：analysis」`。
真实接口是 **`GET /api/business/erp/budget/report?fiscal_year=2026`**（可加 `dept_id` / `account_id`）。

| 步骤 | 操作 | HTTP | DB 勾稽 | 结论 |
|---|---|---|---|---|
| 25 | ⑦-a `GET /budget/report?fiscal_year=2026`（调整前，额度 10000） | 200 | 5602.02：10000 / 已用 9500（占用 0 + 消耗 9500）/ 可用 500 / 执行率 **95.00%** / over=false；12 个月明细；11 月 10000-9500=500 | ✅ |
| 26 | ⑦-b 与 DB 逐行比对（`11_verify.sql` 第 7 段同源 SQL） | — | **MISMATCH_ROWS=0** | ✅ |
| 27 | ⑦-c 调整后 `GET /budget/report?fiscal_year=2026&dept_id=…` | 200 | 5602.02：11000 / 10500 / 500 / **95.45%**；5602.03：1000 / 3500 / **−2500** / 350% / over=true | ✅ 与 DB 完全一致 |

### 金额勾稽

| 勾稽项 | 结果 |
|---|---|
| 单据总额 = 明细合计之和？ | **是**（6/6 单 `header_eq_items=t`，每单单行） |
| 预算「可用」= 额度 −（占用 + 消耗）？ | **是**（5602.02：11000−10500=500；5602.03：1000−3500=−2500，负数是设计口径，报表要看得出超支） |
| 逐笔占用 = 单据金额？ | **是**（3500/6000/1000/3500/500/800 六行一一对应） |
| 凭证借贷平衡？ | **是**（PZ-…-000035 借 3500 = 贷 3500；000036 借 6000 = 贷 6000；000037 借 3500 = 贷 3500） |
| 报表口径 = 提交拦截口径？ | **是**（都是 `status IN ('OCCUPIED','CONSUMED')` + ACTIVE 版本，`MISMATCH_ROWS=0`） |

---

## 发现的问题

| # | 问题 | 严重度 | 复现步骤 | 建议 |
|---|---|---|---|---|
| 1 | **预算详情 `usages[]` 永远为空**：`budget_handler::detail_of` 里 `budget_guard::usages_of(db(), SOURCE_PURCHASE, row.id)` —— 拿预算单 id 当「源单据 id」，且写死 `SOURCE_PURCHASE`。报销产生的占用/消耗在预算详情里一条都看不到（`year_used` 却是对的） | P2 | `GET /budget/{id}` 返回 `usages: []`；DB 里 `biz_budget_usage` 该槽有 2 行 `budget_id` 指向它（见 `raw/b4_detail_after_approve.json` 与 `11_verify_output.txt` 第 2 段） | 改成按 `dept_id + account_id + fiscal_year` 查全部来源（或 `budget_id = ?`），并补一条用例；`UsageRes::of` 的第二个参数同理不能写死 |
| 2 | **预算没走到 `ACTIVE` 就等于没预算**：`active_control_mode` / `slot_snapshot` 只认 `status='ACTIVE'`。编完不送审（DRAFT）甚至槽级 `control_mode=BLOCK` 也不生效，超支直接放行 | P2（运营陷阱） | 见演练步骤 23：DRAFT 500 BLOCK 预算下报 500 → 200 放行 | 报销提交时若该槽存在 DRAFT/SUBMITTED 预算，返回 warning（「该槽预算未生效」）；前端在草稿预算上标黄提示。不建议改控制口径 |
| 3 | **`effective_from` 不参与控制判定**（复现 P0-05 问题 1）：v1/v2 的 `effective_from` 都是审批通过次月 1 日 `2026-12-01`，但 11 月的单据已按新额度占用与拦截 | P2 | 见 `11_verify.sql` 第 1 段 + 步骤 7/10/22 | 先定口径：控制/占用加 `effective_from <= 期间月末`，或把 `budget_bridge::effective_from_of` 改成当月 1 日 |
| 4 | **409 报文策略标签自相矛盾**（复现 P0-05 问题 2）：`check_for_expense` 的 `reason` 用全局 `global.label()`（「标记并强制审批」），实际阻断来自槽级 `BLOCK` | P3 | 步骤 13 的报文 | `reason` 用触发阻断的 `check.verdict.mode.label()`，并按槽列出策略 |
| 5 | **未编预算被判「超预算」**（复现 P0-05 问题 3）：`BudgetVerdict::evaluate` 中 `missing_budget=true` 与 `over=true` 同时成立，`over_amount=全额` | P3 | 步骤 24：未编预算报 800 → `over_budget=1 / over_amount=800` | `missing_budget` 时 `over=false`，用独立标记区分「未编预算」与「真超支」 |
| 6 | **`biz_expense_report.budget_mode` 记的是全局策略**：本例槽级是 `BLOCK`，单据上却写 `WARN`（`11_verify_output.txt` 第 3 段）→ 事后审计看不出这单被 BLOCK 控制过 | P3 | 步骤 6/9/11/13 的 `budget_mode` | 同时落 `slot_mode`（或把 `budget_mode` 改成真正生效的 `resolve(槽, 全局)`） |
| 7 | **状态机审计 `biz_status_log.operator_name` 全空**：只有 `operator_id`，没有姓名快照，预算/报销的审计回溯必须 join `sys_user`（用户改名/删号后失真） | P4 | `11_verify_output.txt` 第 8 段：15 行全部 `has_operator_name=f` | 写日志时一并落 `operator_name` 快照 |
| 8 | **卡面契约与实现不符**：`/erp/budget` → `/api/business/erp/budget`；`/budget/analysis` → `/budget/report?fiscal_year=`；入参 `year/dept_id/account_id/amount` → `fiscal_year/dept_id/account_id/total_amount` + `periods`（12 行；不传则平均分摊，**别用平均分摊做单月演练，月闸会先挡住**）；`biz_budget` **没有 `used_amount` 列**，已用统一由 `biz_budget_usage` 现算；`/erp/expense` 见 P0-05 问题 4 | P3 | 全流程 | 演练卡模板按 `budget_dto.rs` / `budget_handler.rs` 的接口注释改写 |

---

## 汇总

- 通过步骤：**7/7**（卡面 ①–⑦ 全过）+ 补充 6 个反证/放行场景
- 阻断性 bug：**0 个**（**P0 级功能缺失：无** —— 超支确实被拦截，不是「默默放过」）
- 新增问题：P2 × 3（详情页占用列表为空、未生效预算零控制、effective_from 不参与判定）、P3 × 4、P4 × 1；其中 3 条是 P0-05 已记问题的复现
- 结论：**链路通畅**（编制 → 审批 → 占用 → 拦截 → 调整放行 → 执行分析全通，报表与台账 0 差异）

## 本次造的数据（清理指引，未删）

| 对象 | 值 |
|---|---|
| 部门 | `DRILL_BDG 演练预算部`（leader `lead_drill`） |
| 预算 | `BDG-20261107-000002`（v1 10000 SUPERSEDED）、`BDG-20261107-000004`（v2 11000 ACTIVE）、`BDG-20261107-000003`（5602.03 1000 WARN ACTIVE）、`BDG-20261107-000005`（5602.04 500 DRAFT） |
| 报销单 | `EXP-20261107-000009`(3500 APPROVED/已计提)、`000010`(6000 APPROVED)、`000011`(1000 SUBMITTED)、`000012`(3500 APPROVED 超预算放行)、`000013`(500 SUBMITTED)、`000014`(800 SUBMITTED) |
| 凭证 | `PZ-20261107-000035..000037`（各 2 行、借贷相等、POSTED） |
| 占用台账 | 6 行（4 行 `budget_id` 回填，2 行未编/未生效预算为 NULL） |
| 流程实例 | BUDGET_APPROVE × 4 + EXPENSE × 6 |

清理顺序（如需要）：凭证 → `biz_expense_payment`/`biz_expense_item`/`biz_expense_report` → `biz_budget_usage` → `biz_budget_period`/`biz_budget` → `biz_flow_task`/`biz_flow_instance` → 部门。**本卡未删任何数据**，同库还有别的卡的演练数据。

## 复跑

```bash
bash 03_setup.sh        # 部门 + 账号登录（幂等：部门已存在则从 dept/tree 取 id）
bash 04_budget_a.sh     # ① 编制 + 审批到 ACTIVE + ② 余额
bash 05_expense_a.sh    # ③ 3500 / ④ 6000 / ⑤ 1000（期望 409）
bash 06_approve_a.sh    # ③④ 审批 → CONSUMED + 凭证
bash 07_analysis.sh     # ⑦ 执行分析 + DB 勾稽断言
bash 08_warn_over.sh    # ⑥ WARN 超预算放行 + 审批链对比
bash 09_adjust_release.sh # ⑥ 调整预算 10000→11000 → 重提放行
bash 10_not_controlled.sh # ⑥ 反证：DRAFT 预算 / 未编预算
bash run_11_verify.sh   # 全量 DB 复核（输出落 11_verify_output.txt）
```

> 复跑注意：`uk_biz_budget_slot_active` 让同一「部门 × 科目 × 年度」只能有一条 ACTIVE 预算 —— 重跑前要么换部门/科目，要么把旧版本 `CLOSED`。
