# SO-07 销售订单前端页面 —— 实测证据（2026-11-06）

卡：`t_1ae4c684`（【SO-07】销售订单前端页面）· 承接 `t_860ea3d4`（SO-06 审批流，提交 `f0d75cf`）。

## 一、这次交付了什么

| 文件 | 内容 |
|------|------|
| `frontend/src/views/business/erp/sales-order/index.vue` | 页面：列表（单号 / 客户 / 订单日期 / 交期 / 价税合计 / 状态）+ 服务端分页 + 过滤、新建 / 编辑弹窗（客户选择器 + 明细行：物料 + 数量 + 单价 + 税率 → 实时算金额）、详情抽屉（含 **FlowProgress 审批进度**）、提交 / 撤回 / 作废 / 关闭按状态显隐 |
| `frontend/src/views/business/erp/sales-order/sales-order-query.ts` | 纯逻辑（可单测）：状态域 / 配色、按钮显隐（对齐 `LEGAL_EDGES`）、需要原因的边、查询参数（空串不发）、金额预览（`round(price×qty,4)` + `round(amount×rate/100,4)`）、表单校验、请求体构造 |
| `frontend/src/api/business/erp/index.ts`（追加「销售订单」段） | 6 个接口 + 逐字抄后端的类型（`SalesOrder` / `SalesOrderItem` / `CreateSalesOrderReq` / `UpdateSalesOrderReq` / `SalesOrderStatusReq`） |
| `frontend/src/router/index.ts` | 路由 `business/erp/sales-order` → `business/erp/sales-order/index.vue`（`meta.title = 销售订单`、`meta.permission = business:erp:sale-order:list`） |
| `frontend/tests/unit/erp-sales-order.test.ts` | 16 条单测（状态域 / 配色 / 按钮显隐 / 原因边 / 查询 / 金额 / 校验 / 请求体 / 回填） |
| `backend/src/business/erp/sales_order_handler.rs` | `SalesOrderRes` 新增 `approval_instance_id`（**只有详情填**），`detail_of` 补值 + 单测 `approval_instance_id_is_detail_only` —— 没有它前端画不出审批进度 |
| `backend/migrations/business/V0282__sales_order_menu_page.{up,down}.sql` | 把 V0265 故意留空的菜单 `path` / `component` / `is_visible` 补成终态（`/business/erp/sales-order`），title 收敛成「销售订单」（照 V0263 先例） |

## 二、两条硬口径（为什么这么写）

1. **审批结论不许人工写**：`APPROVED` / `DRAFT` 永远不发给 `PATCH /status`（后端 400）。
   撤回 = 撤审批实例（`POST /business/flow/instance/{id}/withdraw`），通过 / 驳回 = 审批中心。
2. **撤回要先拉详情**：列表行的 `approval_instance_id` 恒为 `null`（后端只在详情补），
   所以列表点「撤回」→ 先 `GET /sales-order/{id}` 拿实例 id → 再 withdraw。

## 三、实测环境

- 后端：`./target/debug/cloudpivot`，`NIK_SERVER_LISTEN=127.0.0.1:18109`，库 `cloudpivot_so07a`
  （= `cloudpivot_so06` 的克隆，启动时自动应用 V0282）。
- 前端：`pnpm dev --port 5199` + `BACKEND_ORIGIN=http://127.0.0.1:18109`（Vite 代理 `/api`）。
- 账号：`admin`（SUPER_ADMIN，短路权限）。

## 四、证据清单

| 文件 | 说明 |
|------|------|
| `curl-round-1.txt` / `curl-round-2.txt` | **连跑两轮**：真 token curl 17 条断言全绿（建单金额恒等 / 列表过滤 / 详情实例 id / 提交→SUBMITTED / 流程图节点 / 重复提交 409 / 人工 PATCH 审批结论 400 / 撤回→DRAFT / 审批中作废需原因 / 终态护栏） |
| `curl-e2e.sh` | 上面两轮的脚本（可复跑） |
| `unit-db.txt` | `cargo test --lib business::erp::sales_order_handler` → 9 passed（真库）；`system::menu::tests::n08` → 1 passed（菜单 path 能在前端路由里解析） |
| `menu-migration.txt` | `schema_migrations` 的 V0282 行（`kind=versioned` / `applied_via=executed`）+ `sys_menu` 页面节点与 3 个按钮终态 |
| `frontend-gates.txt` | `pnpm typecheck`（0 错）/ `pnpm test:unit`（876 passed，含本卡 16 条）/ `pnpm check:palettes`（330 条断言 0 失败）/ `pnpm build`（dist 341 个产物） |
| `ui-1-list.png` | 列表页：状态中文标签（草稿灰 / 已提交蓝 / 已审批蓝 / 已取消红）、按状态显隐的操作（草稿=编辑·提交·作废；已提交=撤回·作废；已审批=关闭·作废） |
| `ui-2-create-form.png` | 新建弹窗：客户选择器 + 明细行 + 实时金额（2×100 @13% → 200.00 / 26.00 / 价税合计 226.00） |
| `ui-3-detail-approval-progress.png` | 详情抽屉：**FlowProgress** 画出「开始（已通过）→ 销售主管审批（处理中）」，下方审批人明细带「自审」标记 |

## 五、UI 手工验证路径（在 ui-*.png 里可见）

1. 登录 → 侧边栏「销售订单」（V0282 的 `is_visible=TRUE` + 短 title）→ 列表 26 条；
2. 新建销售订单：选客户 `CUS-0001 北京云枢科技有限公司`、物料 `M-W1A-001 原料A`、数量 2、单价 100、税率 13
   → 表单实时显示 金额 200.00 / 税额 26.00 / 价税合计 226.00；
3. 创建 → 列表首行出现 `SO-20261106-000159`（草稿，操作 = 详情 / 编辑 / 提交 / 作废）；
4. 点「提交」→ 状态变「已提交」，操作变「详情 / 撤回 / 作废」，同时收到待办提醒（审批实例已发起）；
5. 点「详情」→ 抽屉里「审批进度」画出 开始（已通过）→ 销售主管审批（处理中，超级管理员待处理，标「自审」）。

## 六、已知边界

- 内控开关 `erp.separation_of_duties.approve` 在实测库被脚本改成 `false`（否则超管既是制单人又是唯一可审批人，提交会 422 —— SO-06 的结论）。
  **部署时请把用户绑 `SALES_SUPERVISOR` / `FINANCE_MANAGER` / `GENERAL_MANAGER`**，否则提交会被内控拦。
- `sys_menu` 里 `business:erp:sale-order:status` 按钮的 `title` 还写着「10 条合法边」，而 SO-06 之后边表是 11 条
  （`SUBMITTED→DRAFT`）。属 V0265 的文案陈旧，不影响功能，未在本卡改（要改得再来一条迁移）。
- 物料下拉一次拉 200 条启用物料、本地过滤（与其他单据页同口径）；超过 200 条请走物料页搜索。
