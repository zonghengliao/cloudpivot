# P0-05 费用报销链端到端演练（报销单 → 审批 → 付款 → 凭证 → 预算控制）

- 卡片：`t_7bc619ea`　演练时间：2026-11-07 20:26–20:36（CST）
- 目标环境：**145 测试机**（`http://172.18.10.145` → nginx 80 → 后端 `127.0.0.1:8033`，库 `cloudpivot_dev`）
- 生产 144 **全程未连接**
- 本卡**未改任何代码**（卡面允许 ≤2 文件，但本次发现的都是口径/文案问题，见「发现的问题」，需要先定口径）

## 0. 演练账号与数据（全部新建，带 DRILL_ 前缀）

| 对象 | 值 |
|---|---|
| 部门 | `DRILL_EXP` 演练费用部（`01a1b5f9-ec65-766c-9242-4c49c98e14b8`），负责人 = lead_drill |
| 角色 | `DRILL_CLERK` 演练报销员（只授 expense-type:list / expense:list,create,update,detail,submit，**不含** `business:flow:approve`，避免报销人自己被派成审批人） |
| 账号 | `lead_drill`（DEPT_LEADER，演练直属上级）/ `fin_drill`（FINANCE_MANAGER，演练财务经理）/ `emp_drill`（DRILL_CLERK，报销人），口令 `Drill@123456`；`gm_drill`（GENERAL_MANAGER，P0-04 建的）复用为总经理 |
| 费用类型 | `DRILL_TRAVEL` 差旅费(演练) → 借方科目 **5602.02 管理费用-差旅费**（`8c368d40-4c97-4f24-aa91-5562899014c3`，末级），tax_deductible=1，budget_controlled=1 |
| 预算 | `BDG-20261107-000001` 2026 年 / DRILL_EXP / 5602.02 / 8000 / 槽级策略 **BLOCK** |

> 复现脚本：本目录 `00_env.sh` … `09_tiers.sh`；原始响应在 `raw/`，收口核对输出在 `08_verify_output.txt`。

## 演练结果

### 链 1：报销单 → 审批 → 付款 → 凭证（卡面 ①–⑥）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| ① 建报销类型 | `POST /api/business/erp/expense-type` `DRILL_TRAVEL` | 200 | `biz_expense_type` 1 行，`account_id` = 5602.02（`is_leaf=1`，与出参 `account_code/account_name` 一致） | ✅ |
| ② 建报销单（2 明细） | `POST /api/business/erp/expense`（机票 2000 + 住宿 1500） | 200 | `biz_expense_report` `EXP-20261107-000002` DRAFT，`total_with_tax=3500.0000`、`total_amount=3500.0000`、`item_count=2`；`biz_expense_item` 2 行（2000/1500），Σ明细=3500 | ✅ |
| ③ 提交审批 | `POST /api/business/erp/expense/{id}/submit` | 200 | 单据 `SUBMITTED`；`biz_flow_instance` `FLW-20261107-59D662F1`（EXPENSE / PENDING）；**`biz_flow_task` 生成 1 条待办**（n_sup 直属上级审批 / INITIATOR_SUPERIOR / 演练直属上级 / PENDING）；lead_drill 的 `GET /business/flow/task/todo` total=1 | ✅ |
| ③b 审批任务落在哪张表 | — | — | 审批待办在 **`biz_flow_task`**（13 行全在），`biz_task` 里 0 行 —— `biz_task` 是任务/项目模块的表（有 project_id / parent_id / progress），**不是审批待办表** | ✅（卡面表名需订正） |
| ④ 审批通过（完整流） | lead_drill `POST /business/flow/task/{id}/approve` → fin_drill 同款 | 200 / 200 | `n_sup` + `n_fin` 两条待办均 APPROVED；单据 `APPROVED`；实例 `FLW-…-59D662F1` = **`APPROVED`**（引擎终态词是 APPROVED，不是 completed）；计提凭证 `PZ-20261107-000030` 生成 | ✅ |
| ⑤ 付款 3500 | `POST /api/business/erp/expense/{id}/pay`（BANK_TRANSFER / 1002） | 200 | `biz_expense_payment` `PAY-EXP-20261107-000001` 3500 PAID；单据 `status=PAID`、`pay_status=PAID`、paid 3500 / unpaid 0；付款凭证 `PZ-20261107-000031` | ✅ |
| ⑥ 凭证 | — | — | 见下「金额勾稽」：2 张凭证，逐张借贷平衡，借方科目 = 报销类型关联的 5602.02 | ✅ |

### 金额勾稽

- 主表金额 = 明细合计？ **是**（3500.0000 = 2000 + 1500，逐单核对见 `08_verify_output.txt` §2：7 张单 `header_eq_items` 全为 `t`，`item_count = item_rows` 也全为 `t`）
- 付款金额 = 单据金额？ **是**（3500 = 3500，付清后 unpaid 0）
- 凭证借贷平衡？ **是**（全部 34 张凭证 `total_debit = total_credit = 431152.0000`；本链 5 张 `EXPENSE_*` 凭证（4 张计提 + 1 张付款）逐张平衡，且 `biz_voucher_entry` 分录合计 = 表头合计）

### 凭证科目归集（卡面 ⑥）

卡面预期「借 管理费用-差旅费 3500 / 贷 银行存款 3500」是一条**合并视图**；实际系统按 ARCH-6 §4.5.1 拆成两张凭证：

| 凭证 | 来源 | 借方 | 贷方 |
|---|---|---|---|
| `PZ-20261107-000030` | `EXPENSE_REPORT` / EXP-20261107-000002（审批通过即计提） | 5602.02 管理费用-差旅费 3500 | 2241.01 其他应付款-员工垫付 3500 |
| `PZ-20261107-000031` | `EXPENSE_PAYMENT` / PAY-EXP-20261107-000001（付款时） | 2241.01 其他应付款-员工垫付 3500 | 1002 银行存款 3500 |

净效果 = 借 5602.02 3500 / 贷 1002 3500，与卡面预期一致；**借方科目就是报销类型关联的那个科目**（SQL 断言 `is_type_account=yes`，比对 `biz_expense_type.account_id`）。

本次演练凭证范围科目余额：5602.02 借 6000（3500+2000+500）、2241.01 借 3500 / 贷 18000（净贷 14500 = 已批未付 2000 + 500 + 12000）、1002 贷 3500、5711 借 12000（会签档那单，类型 `PENALTY`/`budget_controlled=0`）—— 借方合计 18000 = 贷方合计 18000，逐科目恒等。

### 链 2：预算控制（卡面 ⑦，与 P0-06 配合）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 编预算 | `POST /business/erp/budget` 8000 / BLOCK / 2026-11 月分解 8000 | 200 | `biz_budget` DRAFT，`biz_budget_period` 12 行（Σ=8000） | ✅ |
| 预算审批 | `POST /budget/{id}/submit` → 部门主管（**2 个成员都要签**：demo_lead + lead_drill）→ 财务经理 fin_drill | 200×4 | 实例 `FLW-20261107-11544424` APPROVED，3 条待办全 APPROVED；预算 `ACTIVE`、`control_mode=BLOCK` | ✅ |
| 预算内 1000 单 | 提交 | 200 | `over_budget=false`、`budget_mode=null`、`biz_budget_usage` OCCUPIED 1000（`budget_id` 已回填）；**撤回后** RELEASED（`release_reason=WITHDRAWN`） | ✅ 占用/释放正确 |
| 超预算 3000 单 | 提交 | **409** | 业务码 `66037`「超支额最大 500.0000」；单据仍 `DRAFT`、`biz_budget_usage` 0 行、`biz_flow_instance` 0 条 | ✅ 拦截生效 |
| 槽台账 | — | — | 额度 8000 / 已用（OCCUPIED+CONSUMED）5500→6000 / 可用 2500→2000，与逐行台账一致 | ✅ |

> 说明：3500 与 2000 两单是在预算还是 SUBMITTED（未 ACTIVE）时提交的，因此它们的占用行 `budget_id` 为空、`over_budget=1`；预算转 ACTIVE 后这两笔历史用量被**追溯计入口径**（used 5500 里含它们），这也是「超预算 3000」被拦（5500+3000 > 8000，超 500）的算账基础。

### 链 3：金额三档（「流程全配置化」的额外验证）

| 金额（含税） | 流程路由（实例/待办实测） | 待办数 | 结论 |
|---|---|---|---|
| 500（< 1000） | `n_sup` 直属上级审批 → END | 1 | ✅ `tier_label=直属上级` |
| 3500（1000–10000） | `n_sup` → `n_fin` 财务经理审批 | 2 | ✅ `tier_label=直属上级 + 财务经理` |
| 12000（≥ 10000） | `n_sup` → `n_board` 财务经理 + 总经理**会签**（gm_drill + fin_drill 各签一票才推进） | 3 | ✅ `tier_label=直属上级 + 财务经理 + 总经理会签` |

12000 那单用 `PENALTY`（5711 营业外支出，`budget_controlled=0`）类型，`budget_usages=0` —— 顺带验证了「费用类型上的预算开关」真的生效；凭证 借 5711 12000 / 贷 2241.01 12000，平衡。

**审批流是否真的产生了待办（本卡核心）**：是。每一次提交都在 `biz_flow_task` 落真实待办（`assignee_source` 分别 INITIATOR_SUPERIOR / ROLE），审批人能在 `GET /business/flow/task/todo` 看到（lead_drill 实测 total=1），必须逐节点签完单据才变 APPROVED 并出凭证；驳回/撤回/作废时待办统一变 CANCELLED、预算占用 RELEASED。

## 发现的问题

| # | 问题 | 严重度 | 复现步骤 | 建议 |
|---|---|---|---|---|
| 1 | 预算 `effective_from`（审批通过次月 1 日，本次 = 2026-12-01）**不参与控制判定**：`budget_guard::active_control_mode` / `slot_snapshot` 只按 `status='ACTIVE' + fiscal_year` 取版本，不看生效日 | P2（口径） | 编 8000 预算并审批到 ACTIVE（`effective_from=2026-12-01`）→ 立刻提交 2026-11 的报销 → 已按该预算占用/拦截 | 先定口径：要么控制/占用都加 `effective_from <= 期间月末`，要么把 `budget_bridge::effective_from_of` 改成当月 1 日。改之前 P0-06 的报表口径也要对齐 |
| 2 | 409 拦截报文自相矛盾：`check_for_expense` 的 reason 用**全局策略**标签（`global.label()` = 「标记并强制审批」），而阻断判定用**槽级策略** BLOCK → 同一条报文里既说「策略 标记并强制审批」又说「当前策略是 BLOCK」 | P3（文案） | 全局 `erp.budget.control_mode=WARN` + 槽级 BLOCK + 超预算提交 | `budget_guard.rs::check_for_expense` 的 reason 里改用「触发阻断的那个 check 的 verdict.mode.label()」，并按槽列出策略 |
| 3 | 未编预算时（全局 WARN）单据被判 `over_budget=1 / over_amount=3500` 并写占用行（`budget_id` 为空）：`BudgetVerdict::evaluate` 里 `missing_budget` 与 `over` 同时为真 | P3（口径） | 部门+科目没有任何 ACTIVE 预算 → 提交 3500 报销 | 建议 `missing_budget` 时 `over=false`（或在单据上分量标记「未编预算」），否则列表页一片「超预算」无法区分「真超支」与「没编预算」 |
| 4 | 卡面契约与实现不符（照抄必然踩空） | P3 | 见下 | 下次演练卡按实现改写 |
| 5 | 角色类审批节点（`APPROVAL` + `ROLE`）在角色有多个成员时**要求全员签**（实测预算「部门主管审批」节点：demo_lead + lead_drill 都签才推进） | P4（设计） | 给 DEPT_LEADER 加第二个成员，提交预算审批 | 若期望「任一即可」需在节点上显式 `approval_mode=ANY`；建议在流程设计器里把默认行为写进帮助文案 |

问题 4 的契约差异（本卡实际按实现跑通，卡面写法会失败）：

| 卡面写法 | 实现 |
|---|---|
| `POST .../erp/expense-type {"name","code","account_id"}` | 字段是 `type_code` / `type_name` / `account_id` |
| `POST .../erp/expense {"expense_type_id","applicant","amount","items":[{"description","amount"}]}` | 单头没有 `expense_type_id`/`amount`/`applicant`；单头是 `title`（必填）+ `expense_date` + `reporter_id` + `dept_id`；**每行**要 `expense_type_id` + `with_tax_amount`（含税）+ `tax_rate` + `summary`（`description` 应为 `summary`）；金额由明细汇总 |
| `POST .../erp/payment {"type":"expense","ref_id","amount"}` | 付款挂在报销单下：`POST /business/erp/expense/{id}/pay {"amount","pay_date","pay_method","pay_account_code"}` |
| `biz_expense_report_item` | 实际表名 `biz_expense_item`；付款表 `biz_expense_payment` |
| `status='submitted'` / 实例 `completed` | 状态是大写 `SUBMITTED / APPROVED / PAID`；实例终态是 `APPROVED`（还有 `CANCELLED / WITHDRAWN`），没有 `completed` |
| 审批任务查 `biz_task` | 审批待办在 `biz_flow_task`（`biz_task` 是任务/项目模块） |

## 汇总

- 通过步骤：**7/7**（①建类型 ②建单 ③提交+待办 ④完整审批 ⑤付款 ⑥凭证归集 ⑦预算控制），另加跑 3 档金额路由验证（500 / 3500 / 12000 全部命中预期节点）
- 阻断性 bug：**0 个**
- 非阻断问题：4 类（P2 ×1 口径、P3 ×3、P4 ×1）
- 结论：**链路通畅**。费用报销主链（建单→审批→入账→付款→凭证）全部打通，审批流是真实产生待办的配置化流程（三档路由器），凭证科目归集到报销类型关联的末级科目、逐张借贷平衡；预算控制在 BLOCK 策略下能占用、能释放、能拦截。

## 复跑方法

```bash
cd /opt/cloudpivot
bash docs/evidence/20261107-p005-expense-chain/01_setup.sh      # 部门 + 角色 + 3 账号（幂等）
bash docs/evidence/20261107-p005-expense-chain/02_type.sh       # 费用类型
bash docs/evidence/20261107-p005-expense-chain/03_report.sh     # 建单（脚本每次会新建一张单）
bash docs/evidence/20261107-p005-expense-chain/04_submit.sh     # 提交 + 待办核对
bash docs/evidence/20261107-p005-expense-chain/05_approve.sh    # 直属上级 → 财务经理
bash docs/evidence/20261107-p005-expense-chain/06_pay.sh        # 付款
bash docs/evidence/20261107-p005-expense-chain/07_budget.sh     # 编预算（BLOCK）
bash docs/evidence/20261107-p005-expense-chain/07b_budget_approve.sh
bash docs/evidence/20261107-p005-expense-chain/07c_budget_control.sh
bash docs/evidence/20261107-p005-expense-chain/08_verify.sh     # 全量核对 → 08_verify_output.txt
bash docs/evidence/20261107-p005-expense-chain/09_tiers.sh      # 金额三档
```

注意：
- 登录令牌写在**仓外** `/root/.p005-tokens/*.token`（`00_env.sh` 的 `tokf`），`raw/` 只放响应体 JSON 证据，令牌不进仓。
- `03_report.sh` 每次新建一张单（单号递增），`04/05/06` 读 `raw/report.id`；重跑前先清 `raw/` 或用新单。
- `07` 的预算槽有唯一约束（`uk_biz_budget_slot_active`），重跑要先把旧预算 `CLOSED` 或换科目。
- 预算审批的「部门主管」节点有两个 DEPT_LEADER 成员，脚本用 `demo_lead / Demo@123456`（P0-03 证据里记录的既有演练账号）签第二票；**未改任何既有账号口令**。

## 演练数据（保留，未清理）

本次写入 145 `cloudpivot_dev` 的数据：`sys_dept DRILL_EXP`、`sys_role DRILL_CLERK`、`sys_user lead_drill/fin_drill/emp_drill`、`biz_expense_type DRILL_TRAVEL`、7 张报销单（`EXP-20261107-000002..000008`，其中 4 张 APPROVED/PAID、1 张作废、1 张撤回、1 张被预算拦截）、3 张费用凭证 + 1 张预算凭证链、`biz_budget BDG-20261107-000001`（ACTIVE/BLOCK）、若干 `biz_budget_usage` / `biz_flow_instance` / `biz_flow_task`。

> `EXP-20261107-000001` 是开工时 `03_report.sh` 解析 `data.report.id` 写错留下的草稿单，已用 `DELETE /business/erp/expense/{id}` 软删（`deleted_at` 有值，故不在上表里）。

需要清理时按前缀删（先 usage/task/instance、再单据/凭证），**不要整表清空**（同库还有 P0-01…P0-04 与其他卡的演练数据）。
