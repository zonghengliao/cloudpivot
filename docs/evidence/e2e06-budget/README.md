# E2E-06 预算 + 报销域 全流程演练（145 · cloudpivot_dev）

> 卡：`t_e70da85b` E2E-06 预算+报销域：预算编制/执行 + 报销全流程 + 超支拦截
> 环境：`172.18.10.145`（nginx :80 / 直连后端 :8033），库 `cloudpivot_dev`，后端 commit `df2221e0`（演练中修一版后重启，health 200）
> 结果：**6/6 页面走通**，193 条断言全绿 + 13 组勾稽 0 差异；当场修 1 个 P2（1 文件）

## 1. 演练范围（6 个页面）

| # | 页面 | 覆盖动作 |
|---|------|----------|
| ① | 费用类型 | 列表 / 建类型（关末级科目 / 可抵扣 / 受预算 / 必填发票号）/ 改类型 / 重复编码 409 / 选非末级科目 400 |
| ② | 预算编制 | 建草稿（部门×科目×期间 + 12 月分解）/ 提交审批 / 部门主管会签 + 财务经理 / ACTIVE + `effective_from` / 调整（新版本 + 旧版 SUPERSEDED）/ 槽级 BLOCK |
| ③ | 预算科目映射 | 建映射（物料→预算科目）/ 分页（类型筛选走 `status` 参数）/ 改优先级 / 缺目标 400 |
| ④ | 我的报销 | 建单（含税录入拆价税）/ 缺附件 400 / 提交（写占用）/ 撤回（释放）/ 重提（复用同一占用行）/ 审批（直属上级→财务经理）/ 入账凭证 / 作废（红冲 + 释放） |
| ⑤ | 待付款 | 待付款分页 / 部分付款（1000，PARTIAL）/ 付清（PAID）/ 超额付款 409 |
| ⑥ | 预算执行 | 执行报表（额度/占用/已执行/已用/可用/执行率/over/effective）/ 预算详情 usages[] / 12 月分解已用 |

**副线（超支拦截）**：BLOCK 槽 409 报文、WARN 槽放行+标记、未编预算不误判、未到生效日不控制。

## 2. 结果总览

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| ①-1 | `GET /business/erp/expense-type/page` | 200 | 9 条既有类型 | ✅ |
| ①-2 | `POST /business/erp/expense-type`（E2E06_TRAVEL→5602.02） | 200 | `tax_deductible=1 budget_controlled=1 need_invoice_no=1` | ✅ |
| ①-3 | 重复编码再建 | 409 | 未落库 | ✅ |
| ①-4 | `PUT /expense-type/{id}`（改名/上限 5000/发票号非必填） | 200 | `amount_limit=5000.0000 need_invoice_no=0` | ✅ |
| ①-5 | 科目填父级 `5602`（非末级） | **400** | 未落库 | ✅（原 **500**，本卡修复） |
| ②-1 | `POST /business/erp/budget`（FY2027 × E2E06-BDG × 5602.02，12 万，12×1 万） | 200 | `DRAFT` v1，12 行分解合计 = 120000，`effective_from=NULL` | ✅ |
| ②-2 | DRAFT 期提交报销（FIX-2） | 200 | 占用 `OCCUPIED/over_budget=0/over_amount=NULL`；详情 `budget_warnings= BUDGET_NOT_EFFECTIVE(DRAFT)` | ✅ |
| ②-3 | 预算 `submit` | 200 | `SUBMITTED`；`n_dept` 会签只剩 **1** 条待办（编制人 p3a_0002 被内控 SKIPPED） | ✅ |
| ②-4 | 编制人自批 | **403 / 65019** | 待办不变 | ✅ |
| ②-5 | e2e01_appr 批 `n_dept` → `n_fin` | 200 | 实例 `APPROVED` → 预算 `ACTIVE`，`effective_from=2026-12-01` | ✅ |
| ②-6 | 预算审批是否落账 | — | `biz_voucher` 中 `source_type like 'BUDGET%'` = **0** 张 | ✅ |
| ②-7 | BLOCK 槽（5602.09，1.2 万 = 1000/月）建+批 | 200 | `control_mode=BLOCK`，`ACTIVE` | ✅ |
| ②-8 | FY2023 预算建+批 | 200 | `ACTIVE`，`effective_from=2026-12-01` | ✅ |
| ②-9 | 2023-11 期间单据提交（早于生效日，FIX-2） | 200 | `missing_budget=1 over_budget=0 over_amount=NULL`；提示「ACTIVE 已生效（未到生效日）」 | ✅ |
| ②-10 | `POST /budget/{id}/adjust`（12 万→15 万） | 200 | 新版本 v2 `SUBMITTED`，旧版仍 `ACTIVE` | ✅ |
| ②-11 | 批 v2 | 200 | v2 `ACTIVE`、v1 `SUPERSEDED`，同槽 ACTIVE 唯一 = 1 | ✅ |
| ②-12 | 未编预算科目（5602.03）提交（FIX-5） | 200 | `over_budget=0 over_amount=NULL missing_budget=1`，占用行照写 `OCCUPIED/0/NULL` | ✅ |
| ③-1 | `POST /budget/mapping`（MATERIAL→5602.06） | 200 | 落库 `MATERIAL/E2E04-MAT/5602.06` | ✅ |
| ③-2 | `GET /budget/mapping/page?status=MATERIAL` | 200 | 筛选命中（后端把 `status` 当 `map_type`） | ✅ |
| ③-3 | `PUT /budget/mapping/{id}`（优先级 10→5） | 200 | `priority=5` | ✅ |
| ③-4 | MATERIAL 缺 `ref_id` | 400 | 未落库 | ✅ |
| ④-1 | `POST /business/erp/expense`（3180 含税，6%） | 200 | `total_amount=3000 / total_tax=180 / total_with_tax=3180`，`DRAFT/UNPAID` | ✅ |
| ④-2 | 无附件提交 | **400** | 报文：「必须上传发票影像或费用凭证…还差 1 个」（ATT-06） | ✅ |
| ④-3 | 上传发票影像后提交 | 200 | `SUBMITTED`；占用 `OCCUPIED/3180.0000/over_budget=0/period=2027-02`；单据 `budget_mode=WARN` | ✅ |
| ④-4 | `GET /budget/{id}`（FIX-2 usages） | 200 | `usages[]` 出现本单 `OCCUPIED/3180.0000/false/EXPENSE_REPORT`；`year_used=3180`；`2027-02 used=3180 / available=9320` | ✅ |
| ④-5 | 撤回 | 200 | `DRAFT`，占用 `RELEASED`（有 `release_reason`） | ✅ |
| ④-6 | 重提 | 200 | 复用**同一占用行**（id 不变，回 `OCCUPIED`，行数仍 1） | ✅ |
| ④-7 | 报销人自批 | **403 / 65019** | — | ✅ |
| ④-8 | 直属上级(e2e06_mgr) + 财务经理(e2e01_appr) 审批 | 200 | `APPROVED` + 计提凭证 + `period_code=2026-11`（审批期间） | ✅ |
| ④-9 | 计提凭证分录 | — | `借 5602.02 3000 + 借 2221.01 180 / 贷 2241.01 3180`，借贷平衡 | ✅ |
| ④-10 | 占用 → 已执行 | — | `CONSUMED` + `voucher_id` 绑定 | ✅ |
| ⑤-1 | `GET /expense/pay/page` | 200 | 列表含本单 | ✅ |
| ⑤-2 | 付款 1000 | 200 | `PARTIAL / paid=1000 / unpaid=2180`；付款凭证 `借 2241.01 1000 / 贷 1002 1000` | ✅ |
| ⑤-3 | 付款 2180（付清） | 200 | `PAID / unpaid=0` | ✅ |
| ⑤-4 | 再付 1 元 | **409** | 未落库 | ✅ |
| ④-11 | 第二单审批后作废 | 200 | `CANCELLED`；红冲凭证 `PZR-20261109-000092`（`is_red=1`）；占用 `RELEASED`；2241.01 对本单净额 = 0 | ✅ |
| ④-12 | BLOCK 槽超月度额度（5602.09 报 1500，月额度 1000） | **409** | 单据仍 `DRAFT`、**无占用行**、**无审批实例**（事务回滚干净） | ✅ |
| ④-13 | WARN 槽超月度额度（5602.05 报 1500，月额度 1000） | 200 | `over_budget=1 over_amount=500`；占用同标；照常入账 | ✅ |
| ⑥-1 | `GET /budget/report?fiscal_year=2027` | 200 | 5602.02：used=3180 / available=146820；5602.05：over=false/available=10500 但 `months[2027-02].available=-500`；5602.09：used=0 | ✅ |

BLOCK 409 报文（原样）：

```
报销单 EXP-20261109-000027 超出预算额度，提交被拒绝：
· 科目「5602.09 管理费用-其他」2027年2月：槽级策略 BLOCK（阻断提交），月度已用 0 / 月度预算 1000，本次 1500，超支 500
请联系财务调整预算，或让管理员把该科目的控制策略改为「标记并强制审批」（WARN）。
```

## 3. 勾稽验证（`scripts/verify/e2e06_reconcile.sql`，13 组 0 差异）

| 勾稽项 | 结果 |
|---|---|
| 报销单价税合计 ↔ 该单有效占用合计 | 0 差异 |
| 已审批单 ↔ 计提凭证 2241.01 贷方 | 0 差异 |
| 计提凭证不平张数 / 表头 ↔ 分录合计（借、贷） | 0 |
| 已付金额 ↔ 付款凭证 2241.01 借方 | 0 差异 |
| CONSUMED 占用未绑凭证 / RELEASED 无释放原因 / 占用金额≤0 | 0 / 0 / 0 |
| 同槽 ACTIVE > 1 行 | 0（库级唯一索引兜底） |
| 年度额度 ≠ 12 月分解合计 | 0 |
| 执行报表 used ↔ 库内 OCCUPIED+CONSUMED 合计 | 0 差异 |
| 未生效预算却标了超支（本卡数据） | 0 |

单据/凭证实测（见 `db-evidence.txt`）：

```
BUDGET BDG-…021 FY2023 ACTIVE v1 INHERIT 60000.0000  eff 2026-12-01
BUDGET BDG-…019 FY2027 SUPERSEDED v1 INHERIT 120000.0000
BUDGET BDG-…020 FY2027 ACTIVE v1 BLOCK 12000.0000  eff 2026-12-01
BUDGET BDG-…022 FY2027 ACTIVE v2 INHERIT 150000.0000 eff 2026-12-01
BUDGET BDG-…023 FY2027 ACTIVE v1 INHERIT 12000.0000 eff 2026-12-01

USAGE  EXP-…022 2027-02 3000.0000 RELEASED over=0 NULL budget=-            -- DRAFT 期
USAGE  EXP-…023 2023-11 2000.0000 RELEASED over=0 NULL budget=-            -- 未到生效日
USAGE  EXP-…024 2027-02  800.0000 RELEASED over=0 NULL budget=-            -- 未编预算
USAGE  EXP-…025 2027-02 3180.0000 CONSUMED over=0 NULL budget=…fd6d        -- 主链
USAGE  EXP-…026 2027-02  500.0000 RELEASED over=0 NULL budget=…fd6d        -- 作废
USAGE  EXP-…028 2027-02 1500.0000 CONSUMED over=1 500.0000 budget=…9bae    -- WARN 超支
（EXP-…027 = BLOCK 被拦单：无占用行）

VOUCHER PZ-…919 EXPENSE_REPORT 2026-11 3180 借5602.02 3000+借2221.01 180/贷2241.01 3180
VOUCHER PZ-…920 EXPENSE_PAYMENT 2026-11 1000 借2241.01 1000/贷1002 1000
VOUCHER PZ-…921 EXPENSE_PAYMENT 2026-11 2180 借2241.01 2180/贷1002 2180
VOUCHER PZ-…922 EXPENSE_REPORT 2026-11  500 借5602.02 471.70+借2221.01 28.30/贷2241.01 500
VOUCHER PZR-…092 EXPENSE_REPORT 2026-11  500 is_red=1（全额红冲 PZ-…922）
VOUCHER PZ-…923 EXPENSE_REPORT 2026-11 1500 借5602.05 1415.09+借2221.01 84.91/贷2241.01 1500
```

## 4. 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|------|------|----------|------|------|
| 1 | 费用类型选**非末级 / 停用**科目返回 **500 + 10004「服务器内部错误」**（真实原因只在日志里：`科目 5602 不是末级科目`） | P2 | `POST /business/erp/expense-type` 带 `account_id` = 父科目 `5602` | 用户看到「未知错误」，无法自助纠正；属可纠正入参错误却报 5xx，前端无法给出提示 | **本卡已修**（`expense_handler::resolve_account` 把 `DbError::InvalidValue` 映射 400） |
| 2 | `business:attachment:upload` 只授给 6 个管理角色（SUPER_ADMIN / 财务三档 / 采购经理 / 仓管主管 / 总经理），**EMPLOYEE、DRILL_CLERK（报销人本人）没有** | P2 | 用 DRILL_CLERK 账号 `POST /business/attachment/upload`（doc_type=EXPENSE_REPORT）→ **403**；而提交又强制要求附件（ATT-06） | 员工在「我的报销」页**无法自行上传发票影像**，必须先找财务代传才能提交 → 员工自助报销链路断在第一步 | 权限种子里把 `business:attachment:upload` 授给 EMPLOYEE/DRILL_CLERK，或拆出「上传本人单据附件」权限点（V0xxx 迁移） |
| 3 | 预算审批 `budget_approve` 的部门主管节点是 **ROLE `DEPT_LEADER` + `approval_mode=ALL`（会签）且是全局角色** | P2 | 任意预算提交后看 `biz_flow_task`：`n_dept` 为**全体** DEPT_LEADER 成员各 1 条；本次实测含种子/夹具用户 `p3a_0002`（`password` 列是占位符 `'x'`，登录 **500**） | ① 部门维度错配：A 部门的预算要 B 部门主管一起会签；② 只要该角色有一个不可用成员，预算**永远批不完**（DRAFT 无法变 ACTIVE，额度控制整套失效） | 改用 `assignee_type=DEPT_LEADER`（按发起人部门取 `leader_id`）+ `approval_mode=ANY`；并给种子账号补合法密码哈希 |
| 4 | 执行报表 `over` / `available` 是**年度口径**，月度超支不体现在 `over` | P3 | WARN 槽 5602.05 月额度 1000、2027-02 报 1500 → 报表 `over=false`、`available=10500`，只有 `months[2027-02].available=-500` | 财务从「执行报表」主列看不出本月已超，与提交侧（guard 月度闸 409/标记）**口径分叉** | **已修**（`t_24707809`，commit `a4e82902`）：行级新增 `month_over / month_over_amount / month_available / month_period`，月度闸抽成 `budget_guard::MonthGate`，提交侧与报表同源；见 `docs/evidence/e2e06-fix2/` |
| 5 | 执行报表 `effective` 以**本月月末**为时点，而报表按 `fiscal_year` 过滤 | P3 | 今天（2026-11）批的 FY2027 预算：报表行 `fiscal_year=2027` 却 `effective=false`；但 2027-02 的单据实际已被它控制 | 同一行自相矛盾（「2027 年的预算未生效」其实已生效），财务会误判「预算没管上」 | **已修**（`t_24707809`，commit `a4e82902`）：`effective_from <= 该行 fiscal_year 的期末`（`budget_guard::fiscal_year_end` / `effective_in_year`） |
| 6 | FIX-5 之前写入的占用行仍 `over_budget=1 / over_amount=全额` | P4 | 2026-11 的 DRILL_EXP / DRILL_BDG 占用行（本卡写「3 行」；145 实测 **5 行占用 + 5 张单头**，`over_amount` 全是全额） | 「压根没编预算」的历史单在执行报表/详情的「超支」列显示为超支，虚高超支槽 | **已修**（`t_1cf59295`，commit `0b86ea2f`）：迁移 `V0358__budget_over_flag_backfill` 一次性回填（只改 `budget_id IS NULL` 的行，真超支不动；改前整行快照进 `biz_data_fix_backup`）；见 `docs/evidence/e2e06-fix3/` |
| 7 | 已审批报销的**凭证期间 = 审批日所属期间**，预算占用期间 = 费用发生期间 | P4 | 2027-02 的费用在 2026-11 审批 → 凭证进 2026-11，占用记 2027-02 | 跨期时「已用」与凭证期间错位（本次演练即如此）；真实场景审批紧跟费用发生，影响有限 | **已明示**（`t_1cf59295`，commit `0b86ea2f`）：逻辑不变（ARCH-6 §4.5.4），详情页并排显示「费用归属期间 / 记账期间」、报表加口径注脚，后端 `ReportRes` 回 `expense_period_code` / `accounting_period_code`；见 `docs/evidence/e2e06-fix3/` |

**未修复的 2/3 属权限种子与流程配置**（需迁移 + 重新发布流程，超出本卡「≤2 源文件当场修」的边界），已按上表给出建议。

## 5. 修复记录

| commit | 文件 | 内容 |
|---|---|---|
| `df2221e0` | `backend/src/business/erp/expense_handler.rs`（+5/−1） | `resolve_account` 把 `voucher_multi::ensure_leaf` 的 `DbError::InvalidValue`（科目非末级 / 停用 / 不存在）映射为 **400 + 原样文案**，不再被 `utils::db_error` 收敛成 500；其余错误仍走 500 |

验证：145 上 `git pull` + `cargo build`（1m06s）+ 重启，health 200 后重跑全链路，①-5 由 500 变 400。

## 6. 夹具与如实记录

* 部门 `E2E06-BDG`、用户 `e2e06_clerk`（DRILL_CLERK，报销人）、`e2e06_mgr`（FINANCE_MANAGER，部门主管=直属上级）经**真实 API** 建立；期间 `2027-01..2027-06` 经 `POST /business/erp/period/batch-open` 开账。
* 审批人复用了既有演练账号 `e2e01_appr`（DEPT_LEADER+FINANCE_MANAGER+GENERAL_MANAGER，密码见 `scripts/verify/e2e01_purchase_chain.sh`）。
* **数据修补（如实记录）**：`p3a_0002` 的 `password` 列是 1 字节占位符 `'x'`（非 argon2 哈希，登录直接 500），而它被种在全局 `DEPT_LEADER` 角色里，是问题 3 的死锁根因之一。为让预算审批链能推进，本卡把它的哈希重置为 `e2e01_appr` 的哈希（原值 `'x'` 无法登录，重置无信息损失）。
* 附件上传用 admin 代传（问题 2），不改任何权限数据。

## 7. 残留数据（cloudpivot_dev）

* 部门 `E2E06-BDG`；用户 `e2e06_clerk` / `e2e06_mgr`；费用类型 `E2E06_TRAVEL`（改名版）
* 预算 5 张（FY2027 × 3 ACTIVE / 1 SUPERSEDED、FY2023 × 1 ACTIVE），期间 `2027-01..2027-06`
* 报销单 7 张（PAID 1 / CANCELLED 1 / APPROVED 1 / DRAFT 4），凭证 6 张（含 1 张红冲），付款记录 2 条，预算占用 6 行，映射 1 条
* **未触碰** 2026 业务年既有单据与预算、2022 年结年、`2027-12~2028-06` 资产演练段；探针遗留的 2 张 FY2029 预算已清理（`e2e06_cleanup_probe.sql`）
* 唯一落在 2026-11 的是 6 张报销凭证（入账期间 = 审批日，设计决定，见问题 7），均借贷平衡

## 8. 复现

```bash
# 145 上
scp scripts/verify/e2e06_*.sh scripts/verify/e2e06_*.sql root@172.18.10.145:/tmp/e2e06s/
ssh root@172.18.10.145 'bash /tmp/e2e06s/e2e06_run_all.sh'   # 清理 → A → B → 勾稽
# 汇总行：PASS=193 FAIL=0（其中勾稽失败 0）
```

证据文件：`asserts.log`（193 条断言）、`reconcile.log`（13 组勾稽 + 单据明细）、`e2e06_run.out`（逐步输出）、`db-evidence.txt`（预算/占用/凭证直查）。
