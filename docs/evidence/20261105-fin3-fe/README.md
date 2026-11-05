# FIN-3-FE 费用报销三页 — 真机验收实录（2026-11-05）

对应卡片：`t_32412c8f`【FIN-3-FE】费用报销三个页面；后端卡片 `t_0499dd6b`（FIN-3，commit 14d87d8）；
设计依据：`docs/architecture/ARCH-6-财务扩展设计.md` §4.10。

## 一、交付物

| 文件 | 作用 |
|---|---|
| `frontend/src/api/business/erp/expense.ts` | 15 个接口 + DTO 类型（字段逐字抄 `expense_dto.rs`，路径全写 `/api`） |
| `frontend/src/api/business/erp/expense-kinds.ts` | 状态 / 付款状态 / 发票类型 / 付款方式词表（纯常量，node 单测可直接引用） |
| `frontend/src/views/business/erp/expense/expense-logic.ts` | 纯逻辑：价税拆分、合计、动作可用性、表单校验、payload、预算占用行 |
| `frontend/src/views/business/erp/expense/ExpenseDetailDrawer.vue` | 详情抽屉（明细 / 预算占用 / 付款记录 / 审批轨迹 / 凭证号 / 附件） |
| `frontend/src/views/business/erp/expense/index.vue` | 我的报销（默认 `reporter_id` = 当前用户） |
| `frontend/src/views/business/erp/expense/pay/index.vue` | 待付款（付款登记） |
| `frontend/src/views/business/erp/expense-type/index.vue` | 费用类型 CRUD + 启停 |
| `frontend/src/router/index.ts` | 手工注册 3 条路由（与 V0242 菜单 component 逐字一致） |
| `frontend/tests/unit/fin3-expense.test.ts` | 34 条纯逻辑 + 契约用例 |

## 二、隔离实例与库（本次验收用）

```bash
# 库：cloudpivot_fin3fe（TEMPLATE cloudpivot_fin2asset，V0238）
# 实例：./target/debug/cloudpivot，端口 18027，启动时由 refinery 真跑 V0239–V0244
# 前端：vite dev :5190，BACKEND_ORIGIN=http://127.0.0.1:18027
python3 scripts/verify/fin3_e2e.py http://127.0.0.1:18027   # → 67/67 通过
```

`sys_menu` 落库 4 行（目录 + 我的报销 + 待付款 + 费用类型），`biz_expense_type` 8 条种子，
`pnpm dev` 侧边栏「财务管理 → 我的报销 / 待付款 / 费用类型」三处均可点开（截图 01/04/06）。

## 三、逐条验收（全部真机操作 + 接口/DB 复核）

| 验收项 | 结果 | 证据 |
|---|---|---|
| 三页都能打开（不 404） | ✅ | 截图 01 / 04 / 06；router 三条路由 + 迁移 component 逐字一致（单测钉住） |
| 列表 | ✅ | 5 张单分页展示，状态/付款状态中文，`共 5 张` 与接口 total 一致 |
| 新建 + 保存并提交 | ✅ | UI 建 `EXP-20261105-000005`（2 行 4,240.00）→ SUBMITTED；`EXP-...000007`（226.00）→ 审批中 |
| 编辑草稿 | ✅ | 000005 撤回后改事由 → 列表即时变更 |
| 提交 | ✅ | 000007 DRAFT → SUBMITTED → 审批中 |
| 撤回 | ✅ | 000005 SUBMITTED → DRAFT（草稿） |
| 作废（原因 ≥10 字） | ✅ | 5 字被前端拦（`作废原因至少 10 个字（当前 2 个字）`）；36 字成功 → 已作废，toast「已作废（入账的会红冲，预算占用已释放）」 |
| 删除草稿 | ✅ | 000005 删除后接口列表不再返回 |
| 付款登记 | ✅ | 000003 付 300（金额预填未付）→ 待付款列表 2 张 → 1 张；接口 `PAID / paid_amount=300` |
| 明细行内编辑 | ✅ | 增删行 + 含税 3180/1060 → 不含税 3,000.00 / 1,060.00，税额 180.00 / 0.00，合计 4,060 / 180 / 4,240（截图 02） |
| 招待费税额置灰 0 + 提示 | ✅ | 选「业务招待费（MEAL · 不可抵扣）」→ 税率下拉禁用、税额框 disabled 显示 `0.00`、提示「招待费进项税不得抵扣」（截图 02） |
| 详情：审批轨迹 | ✅ | `FlowProgress` 5 节点 + 审批人/时间/意见，含「自审已跳过（职责分离）」留痕（截图 03） |
| 详情：计提凭证号 | ✅ | `PZ-20261105-000031`（000004，APPROVED） |
| 详情：付款记录 | ✅ | 000001 两条付款（3,000 + 3,360）带凭证号；000004 显示「暂无付款记录」 |
| 详情：预算占用 | ✅ | 部门 / 科目 / 占用金额 / 占用月份 / 状态（占用中·已消耗·已释放）/ 超预算（截图 03） |
| 默认只查自己 | ✅ | 列表默认带 `reporter_id=<当前用户 id>`，开关可切「全部」；单测钉住源码契约 |
| `pnpm typecheck` | ✅ | 0 错 |
| `pnpm test:unit` | ✅ | 807 passed（773 存量 + 34 新增） |
| `pnpm build` | ✅ | 成功（见提交说明） |

## 四、过程中发现并修掉的前端问题

1. **`pay/index.vue` 抽屉 import 路径错**（`./ExpenseDetailDrawer.vue` → `../`）：`vue-tsc` 不报（`.vue` shim 是 any），
   但 vite dev/build 直接 500 / build 失败。真机点开「待付款」才发现 —— 说明**只有 build + 真机才能兜住这类错**。
2. **预算占用状态漏了 `CONSUMED`**：后端入账后 usage 从 OCCUPIED → CONSUMED，页面原样吐出英文枚举；已补「已消耗」并加单测。
3. **列表列宽超宽**：1280 视口下固定「操作」列压住「状态」列；三页列宽全部收到 ≤942px。
4. **费用类型弹窗一打开就满屏红字**：校验改为「点过保存才标红」，未输入时只给中性提示。

## 五、遗留：后端缺陷（不在本卡范围，已另开卡）

**撤回后重新提交 → HTTP 500**，日志 `写预算占用: query failed`。

- 复现：`EXP-20261105-000005` 提交成功 → 撤回（usage 置 RELEASED）→ 再次提交 → 500，单子停在 DRAFT。
- 根因：`biz_budget_usage` 的唯一索引把**已释放**的行也算进唯一性：
  `uk_biz_budget_usage_source ON (source_type, source_id, dept_id, account_id, fiscal_year) WHERE deleted_at IS NULL`，
  而重新提交是「再插一条 OCCUPIED」，与历史 RELEASED 行撞唯一键。
- 证据：`select status, count(*) from biz_budget_usage where source_no='EXP-20261105-000005'` → 2 行均 RELEASED。
- 前端行为正确（提交/撤回都按状态机走），这一层需要后端在重新提交时复用/重置原 usage 行（或把 RELEASED 排除出唯一键）。

## 六、截图

| 文件 | 内容 |
|---|---|
| `01-expense-list.png` | 我的报销列表（默认只看自己，5 张单） |
| `02-expense-form.png` | 新建报销单：明细行内编辑 + 招待费税额置灰 0 + 提示 |
| `03-expense-detail.png` | 详情抽屉：预算占用 / 付款记录 / 审批轨迹 / 计提凭证号 |
| `04-pay-list.png` | 待付款列表（已审批未付清） |
| `05-pay-dialog.png` | 付款登记弹窗（金额预填未付） |
| `06-type-list.png` | 费用类型列表（8 条种子） |
| `07-type-dialog.png` | 新建费用类型弹窗（末级科目下拉） |

截图由 `playwright` 脚本在 :5190 真机页面抓取（登录走接口 token 注入 localStorage，不碰密码框）。
