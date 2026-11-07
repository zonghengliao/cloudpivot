# FIX-5 验收报告：409 报文策略标签 + 未编预算不再冒充超预算

- 卡号：`t_9a23375a`（FIX-5）
- 环境：**145 测试机**（绝未碰 144 生产）；隔离实例 `127.0.0.1:8046` + `cloudpivot_dev` 的**拷贝库**
- 代码：`b411b84`（运行时改动）＋ `16babf4`/`d538049`（验收脚本），145 已同步到 `d538049`
- 二进制：`/opt/cloudpivot/backend/target/debug/cloudpivot-fix5`（`cargo build` 于 145）
- 拷贝库：`cloudpivot_fix5_20261108001630`（`pg_dump -Fc cloudpivot_dev` → `pg_restore`，迁移前备份
  `/root/backup/cloudpivot_dev_FIX5_20261108-001630.dump`）
- 验收脚本：本目录 `00_env.sh` / `10_prepare_145.sh` / `11_start_instance.sh` / `12_restart_instance.sh` /
  `13_fresh_and_idempotency.sh` / `21_e2e.sh`；原始响应在 `raw/`

> **结论先行：30 条断言全绿（`raw/e2e.log` → `FAILS=0 RESULT=ALL PASS`）。**
> ① 超支被拦时，409 报文说「**槽级策略 BLOCK**」并逐槽给出已用/额度/超支，不再出现全局标签；
> ② 未编预算落 `over_budget=0 / missing_budget=1 / over_amount=null`（单据与占用行同口径）；
> ③ 列表接口能区分「超预算」（红）与「未编预算」（黄）；
> ④ 真超支照旧被拦（DRAFT、0 占用、0 实例）；⑤ P0-06 ⑤ 的超支拦截原样通过。

---

## 1. 卡面验收逐条对照

| 卡面验收 | 实测 | 证据 |
|---|---|---|
| ① 超支被 BLOCK → 409 报文说「槽级策略 BLOCK」（不是「标记并强制审批」） | ✅ 409 `code=66037`，报文含 `槽级策略 BLOCK（阻断提交）`、`科目「5602.02 管理费用-差旅费」2026年11月`、`月度已用 9500 / 月度预算 10000`、`本次 1000`、`超支 500`；不含 `标记并强制审批（WARN）`、不含 `当前策略是 BLOCK` | `raw/e2e.log` 场景②、`raw/r2_1000_submit.json` |
| ② 未编预算 → `over_budget=0，missing_budget=1，over_amount=null` | ✅ 单据 `0 1 NULL`；占用行 `1 行 / over_budget=0 / over_amount=NULL / OCCUPIED` | 场景④、`raw/r3_800_submit.json` |
| ③ 列表页能区分「超预算」和「未编预算」 | ✅ 列表接口该行 `over_budget=false / missing_budget=true / over_amount=null`；额度内行 `missing_budget=false`；前端新增「预算」列：红「超预算」/ 黄「未编预算」/ 灰 —（`budgetFlag` 单测 4 例） | 场景⑤、`raw/list_page.json`、`raw/unit-fin3.txt` |
| ④ 真超支仍被正确拦截（不能因为修文案把拦截改坏） | ✅ 被拦单据仍 `DRAFT`、`over_budget=0/missing_budget=0`、0 占用行、0 流程实例；槽已用仍 9500（事务整体回滚） | 场景③ |
| ⑤ P0-06 的超支 409 拦截回归 | ✅ 同一组数字（差旅费 BOOLK 10000 / 已用 9500 / 本次 1000）→ 409 `66037` | 场景② |
| 附加：BLOCK 下「未编预算」仍拦（强管控不放松） | ✅ 全局 BLOCK + 干净槽 5602.06 → 409，第一句是「涉及的预算科目未编制（当前策略 BLOCK，不予放行）」，逐槽说「未编制预算…全局策略 BLOCK」 | 场景⑥、`raw/r4_500_submit.json` |

### 409 报文 before / after（P0-06 ⑤ 同组数字）

```text
# before（P0-06 raw，全局 WARN + 槽级 BLOCK：标签自相矛盾）
报销单 EXP-20261107-000011 超出预算额度（1/1 个预算槽超额度，超支额最大 500.0000（策略 标记并强制审批））：
    当前策略是 BLOCK，提交被拒绝。请联系财务调整预算，或让管理员把控制策略改为 WARN（标记 + 强制审批）。

# after（本卡，槽级 BLOCK 逐槽说话）
报销单 EXP-20261108-000009 超出预算额度，提交被拒绝：
· 科目「5602.02 管理费用-差旅费」2026年11月：槽级策略 BLOCK（阻断提交），月度已用 9500 / 月度预算 10000，本次 1000，超支 500
请联系财务调整预算，或让管理员把该科目的控制策略改为「标记并强制审批」（WARN）。
```

「槽级策略」还是「全局策略」由 `ModeSource` 判定（槽级 `biz_budget.control_mode` 显式配了 = 槽级，
否则继承全局）—— 实测场景⑥ 的报文正确地说「全局策略 BLOCK」。

---

## 2. 改了什么

### 后端

| 文件 | 改动 | 行数 |
|---|---|---|
| `backend/src/business/erp/budget_guard.rs` | `evaluate` 收 `budget_id` 判 `missing_budget`；`over` 与 `missing_budget` 互斥；新增 `over_amount_opt()` / `ModeSource` / `ExpenseSlotCheck::{account_label,describe}` / `ExpenseBudget::{missing_budget,blocking_checks,block_reason,block_headline,over_amount_opt}` / `amount_text` / `period_label`；占用写入 `over_amount` 改 `NULLIF(?,'')::numeric`；模块文档 | +317/−40 |
| `backend/src/business/erp/expense_handler.rs` | 409 报文改按槽正文 + 第一句分岔；`form_data` 补 `missing_budget`；`report_mark_submitted` 传 `missing_budget` / `Option` 超支额 | +20/−9 |
| `backend/src/business/erp/expense_model.rs` | `REPORT_COLUMNS` 带 `missing_budget`；`ReportRow.missing_budget` / `over_amount: Option<Decimal>`；提交写入与「回草稿」清零 | +31/−11 |
| `backend/src/business/erp/expense_dto.rs` | `ReportRes.missing_budget`、`ReportRes/UsageRes.over_amount: Option<Decimal>` | +10/−4 |
| `backend/src/business/erp/budget_dto.rs` | `UsageRes.over_amount: Option<Decimal>` | +4/−4 |
| `backend/migrations/business/V0325__budget_missing_flag.{up,down}.sql` | `biz_expense_report.missing_budget`（0/1 + CHECK，幂等）；两张表 `over_amount` 放开 NOT NULL；自检 | 新增 90 / 24 |
| `backend/migrations/business/V0326__budget_over_amount_no_default.{up,down}.sql` | 去掉两张表 `over_amount` 的 `DEFAULT 0`（草稿不再落 `0.0000`，与「NULL = 没超支」一致） | 新增 47 / 12 |
| `backend/src/business/erp/budget_guard_tests.rs` | ⑥ 新增 6 条：报文按槽、槽级/全局标签、多槽排序、未编预算不算超支 + BLOCK 仍拦、额度内文案、金额/期间格式化；⑮ 重写（判据 = 有没有预算行）；⑤ 补 BLOCK 分支 | +240 |
| `backend/src/business/erp/expense_model_tests.rs` | `evaluate` 调用点补 `budget_id` | +1 |

**口径（三处同改，别只改一处）**

```text
missing_budget = (该槽本期没有生效的 ACTIVE 预算行 snapshot.budget_id.is_none())
over           = 有预算行 && (年缺口 > 0 || 月缺口 > 0)
blocks()       = mode==BLOCK && 金额>0 && (over || missing_budget)
over_amount    = over ? max(年缺口, 月缺口) : None   →  落库 NULL / JSON null
```

为什么 `missing_budget` 用「有没有预算行」而不是「额度是不是 0」：没编预算的槽**报过一次之后**
已用就 > 0，用额度推断会把第二张单又判成「超支」，列表页照样一片红（首轮自查发现的坑，二审修掉）。

### 前端

| 文件 | 改动 |
|---|---|
| `frontend/src/api/business/erp/expense.ts` | `ExpenseReport.missing_budget`；`over_amount: string \| null`（列表 + 占用） |
| `frontend/src/api/business/erp/budget.ts` | 占用行 `over_amount: string \| null` |
| `frontend/src/views/business/erp/expense/expense-logic.ts` | 新增 `budgetFlag()`（红「超预算」/ 黄「未编预算」/ null）；类型放宽 |
| `frontend/src/views/business/erp/expense/index.vue` | 列表新增「预算」列（`budgetCell` 兜底灰色 —） |
| `frontend/src/views/business/erp/expense/ExpenseDetailDrawer.vue` | 详情补「提交时未编制预算」提示（与红「超预算」分开）；`over_amount` 空值安全 |
| `frontend/src/views/business/erp/budget/budget-logic.ts` | `usageOverText` 空值安全 |
| `frontend/tests/unit/fin3-expense.test.ts` | `budgetFlag` 4 例（含「两者都为真时红标优先」） |

---

## 3. 测试与构建

| 项 | 结果 | 证据 |
|---|---|---|
| `cargo test`（全目标） | **23 目标 / 3556 passed / 0 failed**，`EXIT=0` | `raw/cargo-test-all.txt` |
| `cargo clippy -- -D warnings` | **基线红**：`sales_order_model::update_order` 8 参数（SO-42 币种卡引入，非本卡文件）；`-A clippy::too_many_arguments` 后本仓 0 warning | 见 §6 |
| `vue-tsc --noEmit`（全量） | 3 处报错**全部**在别人在途的 `price-inquiry`（未跟踪文件），本卡文件 0 报错 | `raw/typecheck-full.txt` |
| `vue-tsc --noEmit`（排除在途 P201 文件） | **退出码 0** | `raw/typecheck-excl-p201.txt` |
| `vite build` | **成功**（5374 modules；`expense-logic` chunk 里含「未编预算」字样，证明列文案进了产物） | `raw/vite-build.txt` |
| `tsx --test tests/unit/fin3-expense.test.ts` | 35/35 通过 | `raw/unit-fin3.txt` |
| `pnpm test:unit`（全量） | 1040/1042 —— 2 条**既有**失败（`erp-sales-order` 的 `currency_code`（f45e23e SO-42）、`flow-biz-link` 的 FlowBizType 15 vs 16（edcbba4 SO-14）），与本卡无关 | 同 §6 |

> 前端「预算」列**没有**在 145 上做浏览器 E2E：145 的 `frontend/dist` 是团队共用产物，
> 本卡不改它。验证方式是三件套 + 后端实测字段：`vue-tsc` 0 报错、`vite build` 成功且产物里
> 含「未编预算」文案、`budgetFlag` 单测 4 例、列表接口该行 `missing_budget=true / over_budget=false`。

## 4. 迁移：幂等 + 空库首启 + down

| 验证 | 结果 | 证据 |
|---|---|---|
| 存量库升级（拷贝库） | 首启「应用 1 个迁移」（V0325），换二进制后再启「应用 1 个迁移」（V0326） | `raw/e2e.log` 头部 / `12_restart_instance.sh` 日志 |
| **幂等**（同库再启一次） | 日志「**应用 0 个迁移**」，`schema_migrations` 的（条数/指纹/max 版本）逐字不变（260 条 / `max_version=91` 前后相同） | `raw/idempotency-fresh.log` ① |
| **空库首启** | 空 schema 一路跑到最新：**应用 257 个迁移**（含本卡新增的 V0325/V0326），迁移内自检全过；`missing_budget` 列、`ck_biz_exp_report_missing` 约束、两表 `over_amount` 可空且无默认值 | `raw/idempotency-fresh.log` ② |
| **down 语义 + 幂等** | 同一事务内把 V0326/V0325 的 down 各跑两遍：第二遍是 `UPDATE 0` + `NOTICE … does not exist, skipping`；随后 `ROLLBACK`，库回到应用态（`missing_budget=1 列 / over_amount nullable=YES`） | `raw/down_rollback.log` |
| 已应用迁移文件 | **未改**（V0319 的教训）：V0325 只追加，语义调整走 V0326 | `raw/schema_migrations_tail.txt`（两条 version 325 / 326，`applied_via=executed`） |

## 5. 与 P0-05 / P0-06 脚本的关系（诚实说明）

* 本目录**没有**直接重跑 `docs/evidence/20261107-p005-expense-chain` / `p006-budget-chain` 的脚本：它们的
  `api()` 里 `-H "Authorization: Bearer ***"` 在入库时被脱敏成字面量 `***`（所有请求必然 401），
  直接重跑拿不到真实结论。
* 因此把两条链**与 FIX-5 相关的部分**照原口径重写进 `21_e2e.sh`：
  - P0-05：费用类型 → 建报销单（含税 9500，税率 0）→ 提交 → 占用行（`OCCUPIED`、金额勾稽）；
  - P0-06 ⑤：差旅费槽 `BLOCK 10000`、已用 9500、本次 1000 → **409 `66037`**、单据 `DRAFT`、0 占用、0 实例。
  两处都用真实 token curl（`emp_drill`，P0-05 建的演练账号），原始响应在 `raw/`。
* 报销人用 `emp_drill` 而不是 admin：admin 提交会撞 FIX-1/ARCH-8 的「制单人不自审」内控（422 `65018`），
  那是另一张卡的口径，与本卡无关。

## 6. 本卡发现的既有缺陷（未在本卡修，已另行建卡）

1. **报销列表带任何筛选时，非超管用户 500**（`code=10004`）：
   `scope::paginate` 要求 `base_sql` 带 `{FILTER}` 标记，报销分页的 `base_sql` 没带 →
   `分页查询报销单: base_sql 带 4 个业务参数但没有 {FILTER} 标记`。8033 现役实例上同样可复现
   （`raw/list_page_emp_500.json`，`emp_drill` + `keyword/status` 均 500，不带筛选 200）。
   本卡 ⑤ 因此改用 admin 令牌验证列表字段（超管不做数据权限裁剪，同一 DTO 序列化路径）。
2. `cargo clippy -- -D warnings` 基线红：`backend/src/business/erp/sales_order_model.rs::update_order`
   8 个参数（SO-42 币种卡加的 `currency_code`），本卡未触碰该文件。
3. `pnpm test:unit` 2 条既有失败（见 §3 表）。

## 7. 上线注意

- **迁移顺序**：V0325（加列 + 放开 NOT NULL）→ V0326（去默认值），都必须先于新二进制对外服务（框架启动即迁移，正常发布流程天然满足）。
- **`over_amount` 语义变化**：不超支时从 `0` 变 `NULL`。任何**直接读这一列做算术的外部报表**
  要按 `coalesce(over_amount, 0)` 处理（仓内 SQL 已全部走 `Option` 反序列化；`biz_budget_usage` 同改）。
- **BLOCK 下未编预算仍拦**：与修复前行为一致（修复前是因为 `missing_budget` 同时置 `over`，
  现在是独立理由），所以**不会**有客户在 BLOCK 下突然被放行。
- **WARN（出厂默认）下未编预算**：照旧提交，只是标记从「超预算 + 全额超支」变成「未编预算」。
- 前端「预算」列依赖 `missing_budget` 字段；老前端 + 新后端不会报错（多一个字段），
  新前端 + 老后端则这一列恒为 `—`（不会误标）。
