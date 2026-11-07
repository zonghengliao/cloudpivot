# FIX-2 证据 · 预算控制三连（DRAFT 不生效 / effective_from 无效 / usages 永远空 / budget_mode 记错）

卡片：`t_e2ee3d12`（FIX-2，P0-05 + P0-06 的 P2 级问题）
环境：**145 测试机**（`172.18.10.145`），隔离实例 `127.0.0.1:8039`，库 `cloudpivot_fix2g`（全新库，启动迁移 253 条 + 演示账号种子）。
**未碰 144 生产**；服役实例 8033 未重启（binary 另存为 `target/debug/cloudpivot-fix2`）。

## 一、验收结论（38 条断言全绿，PASS=38 FAIL=0）

| 卡面验收 | 实测结果 |
|---|---|
| ① 建 DRAFT 预算（BLOCK）→ 提交超支报销 → 收到 warning「预算未生效」+ 放行 | ✅ 提交 **200 SUBMITTED**；`budget_warnings[0] = {code: BUDGET_NOT_EFFECTIVE, status: DRAFT, status_label: 草稿}`；文案「科目 5602.02 的预算尚未生效（当前状态：草稿，预算单 BDG-20261107-000001，额度 10000.0000）：2026-11 的超支将不受该预算控制。请提交审批使其生效」；DB 单据 SUBMITTED、占用行已写 |
| ② ACTIVE 预算（`effective_from = 次月 1 日 = 2026-12-01`）→ 当月单据不受新额度约束 | ✅ 11 月报 2000 → **放行**，`over_amount = 2000.0000`（= 全额，说明 12 月额度没被算进 11 月；若被算进应为 0）；提示 `已生效（未到生效日）`。**反证**：同槽 12 月报 15000 → 额度生效 → **409 / code 66037**（超 5000），单据仍 DRAFT |
| ③ ACTIVE 预算（`effective_from = 2026-01-01` 已过）→ 正常占用和拦截 | ✅ 报 3000 → 200 SUBMITTED，占用 `OCCUPIED / over_budget=0`；报 20000 → **409 / code 66037**，零占用行（事务回滚），单据仍 DRAFT |
| ④ 预算详情 `usages[]` 能看到报销占用（含 source_type / source_id / 金额） | ✅ `usages[0] = {source_type: EXPENSE_REPORT, source_type_label: 费用报销单, source_id: <报销单 id>, source_no: EXP-20261107-000004, amount: 3000.0000, status: OCCUPIED}`，`year_used = 3000.0000` 与占用合计一致；DRAFT 槽（BDG-…000001）同样能看到自己那笔占用 |
| ⑤ 超支被 BLOCK → `biz_expense_report.budget_mode = 'BLOCK'`（不是 WARN） | ✅ 槽级 BLOCK + 额度内 → DB `budget_mode = BLOCK`（全局策略是 WARN，修复前这里会记 WARN）；同槽超支 → 409，不落「记错策略」的单。旁证：DRAFT / 未生效槽的单据记 **WARN**（那两张单**实际**由全局 WARN 管，口径自洽） |
| ⑥（附带）报表与控制同口径 | ✅ 执行报表未到生效日的行 `effective=false, effective_from=2026-12-01`；已生效行 `effective=true` |

原始输出：`raw/verify-145.log`（含逐条 ✅/❌）、`raw/db-assert.txt`（DB 侧三张表快照）、`raw/b3_detail.json`（预算详情）、`raw/b1_detail.json`、`raw/report.json`（执行报表）。

## 二、口径定稿（选项 A）

```
生效 ⟺ effective_from IS NULL OR effective_from <= 期间月末
```

* `budget_bridge::effective_from_of` **不改**（仍是审批通过次月 1 日）；
* `budget_guard::active_control_mode` / `slot_snapshot` / `budget_model::report` 三处同口径；
* 11 月单据不再受 12-01 才生效的额度约束，12 月才受（`raw/verify-145.log` 的「②反证」）。

## 三、修复清单（代码）

| 问题 | 修复 |
|---|---|
| ① DRAFT 不提示 | `budget_guard::pending_budgets` + `pending_warnings`（DRAFT / SUBMITTED / ACTIVE 未到生效日）；出口：报销详情 `budget_warnings[]`、采购详情 `order.budget_warnings[]`（只在详情回填，列表为空不序列化）；前端 `ExpenseDetailDrawer` / 采购详情抽屉各一条 `t-alert theme="warning"`。**控制口径不变**（仍然只认本期生效的 ACTIVE） |
| ② effective_from 无效 | 三个查询加 `(effective_from IS NULL OR effective_from <= ?::text::date)`，期间月末由 `budget_guard::period_end_of` 算（纯函数、单测覆盖平年/闰年/12 月进位）；报表行加 `effective_from` + `effective`，前端报表页标「未生效」 |
| ③ usages[] 永远空 | `budget_guard::usages_of_slot(dept, account, year)`；`UsageRow` 增加 `source_type` / `source_id`；`budget_dto::UsageRes::of(row)` 不再写死 `PURCHASE_ORDER`，新增 `source_type_label` / `source_id` 出参；预算详情表格加「来源类型」列 |
| ④ budget_mode 记全局 | `ControlMode::strictness()` + `ExpenseBudget::recorded_mode()`（取实际参与判定的最严槽级策略），`report_mark_submitted` 与 `form_data.budget_mode` 都改用它；`ReportRes` 新增 `budget_mode`（审计可见） |

## 四、复跑方式

```bash
# 1) 起隔离实例（145 本机；会建库 cloudpivot_fix2g / 端口 8039 / Redis DB 8）
bash scripts/fix2_setup_145.sh
# 2) 跑验收（38 条断言；任一 ❌ 即 exit 1）
bash scripts/fix2_verify_145.sh
```

> 脚本里两处**夹具**是刻意的，不是绕过验证：
> 1. `②③` 的 ACTIVE + `effective_from` 用 SQL 直接落库 —— 审批链本身已由 P0-06 演练证明（`docs/evidence/20261107-p006-budget-chain/`），本卡验的是**判定口径**；
> 2. `demo_sales` 的 expense 权限由脚本临时授予（`sys_role_menu`）—— V0258 角色矩阵没有「报销员」档，演示账号默认无 expense 权限。

## 五、幂等（迁移）

本卡**没有新增迁移**（纯代码口径变更），因此不存在迁移体变更风险；仍做了重启幂等验证：

```
重启前 schema_migrations = 253
重启后 schema_migrations = 253   （指纹校验通过，无新增、无回滚）
启动日志 ERROR = 0；health = 200
残留数据不变：biz_budget=3 / biz_budget_usage=3 / biz_expense_report=5
```

## 六、闸门

* `cargo test`（lib 3303 + 全部 tests 目标）：**EXIT 0**（详见卡内汇报）
* `cargo clippy --all-targets -- -D warnings`：本卡改动文件**零告警**；仓库基线仍有 1 条**既有**告警
  `sales_order_model.rs:699 update_order too_many_arguments (8/7)`（该文件本卡未改动，`git status` 干净，来自更早的 SO-40 提交）—— 已在卡内如实标注，不属本卡范围。
* 前端：`vue-tsc --noEmit` 对本卡改动文件零报错（剩余 3 条报错在 `price-inquiry`，属另一张卡未提交的在制品）；`vite build` **EXIT 0**。
