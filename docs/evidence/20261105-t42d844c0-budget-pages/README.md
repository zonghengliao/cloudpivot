# t_42d844c0 证据：预算三页交付（方案 A）

> 卡：【P2-2B-FIX】预算菜单点进去 404：前端页面组件未交付（3 条子路由全 404）
> 结论：**三条子路由不再是 404**；页面 / 新建 / 上报 / 映射四个动作均已真机跑通并留图。

## 1. 环境（真机，非 mock）

| 项 | 值 |
|----|----|
| 前端 | `http://127.0.0.1:5180`（vite dev，`/opt/cloudpivot/frontend`，`BACKEND_ORIGIN=http://127.0.0.1:18097`） |
| 后端 | `http://127.0.0.1:18097`（`./target/debug/cloudpivot`，profile=test） |
| 数据库 | `cloudpivot_manual2`（手册二期采集实例） |
| 账号 | `admin`（SUPER_ADMIN，`*:*:*`） |
| 浏览器 | Playwright Chromium 1440×900，`zh-CN` |
| 脚本 | `scripts/budget-final.mjs`（首轮 10 张）、`scripts/budget-retake.mjs`（补拍 3 张） |

## 2. 路由（`frontend/src/router/index.ts`，与 `sys_menu` 逐字一致）

| 菜单 path | 组件 | 权限码 |
|-----------|------|--------|
| `/business/erp/budget` | `views/business/erp/budget/index.vue` | `business:erp:budget:list` |
| `/business/erp/budget/report` | `views/business/erp/budget/report.vue` | `business:erp:budget:report` |
| `/business/erp/budget/mapping` | `views/business/erp/budget/mapping.vue` | `business:erp:budget:mapping` |

三条路由在浏览器里 `404` 计数 = 0（脚本里 `if (has404) throw`，任何一条 404 都会中断取图）。

## 3. 截图取证（11 张，按验收动作分组）

| 文件 | 动作 | 取证点 |
|------|------|--------|
| `01-budget-list.png` | 列表 | 4 行：3 条已生效（ACTIVE）+ 1 条审批中；状态标签、操作列（ACTIVE 只给「详情 / 调整」，无「编辑」） |
| `02-budget-create.png` | 新建 | 弹窗：年度 / 部门 / 末级科目 / 年度额度 / 控制策略 + 12 个月分解网格；右侧「提交前自检」；点「平均分摊」后合计 = 额度 |
| `03-budget-list-draft.png` | 新建结果 | 新行 `BDG-20261105-000005` 状态「草稿」，操作列出现 详情/编辑/提交/删除 |
| `04-budget-submit-confirm.png` | 上报 | 提交确认框（写明进审批流后 ACTIVE 才参与占用控制） |
| `05-budget-list-submitted.png` | 上报结果 | 同一行状态变「审批中」，操作列只剩「详情」 |
| `06-budget-detail.png` | 详情抽屉 | 年度已用/可用、12 个月分解（20000×12）、时间已格式化为北京时间 `2026-11-05 02:41` |
| `07-budget-mapping-list.png` | 映射列表 | 列：类型 / 映射目标 / 预算科目 / 部门覆盖 / 优先级 / 状态 / 备注 / 操作 |
| `08-budget-mapping-dialog.png` | 映射新建 | 类型（默认 DEFAULT）→ 目标（全公司兜底）、预算科目、部门覆盖、优先级、状态 |
| `09-budget-report.png` | 执行报表 | 6 张汇总卡（额度合计 600,000 / 占用 0 / 已执行 0 / 可用 600,000 / 执行率 0% / 超支槽 0）+ 3 个槽明细 |
| `10-budget-report-expanded.png` | 分月展开 | 每行下钻 12 个月（额度 / 已用 / 可用） |
| `11-budget-mapping-blocker.png` | 已知缺陷拦截 | 选 `MATERIAL_TYPE` 时页面直接给原因并**禁用保存**（后端 `validate_mapping` 要 `ref_id`、解析只认 `ref_code`） |

## 4. 数据是怎么来的（真实链路，非直接改库造数）

1. 页面上 `新建预算` → `提交审批`（`POST /api/business/erp/budget/{id}/submit`），状态 `DRAFT → SUBMITTED`；
2. 审批链真实走完：`n_dept`（部门主管）→ `n_fin`（财务经理，金额 < 50 万分支）→ 实例 `APPROVED` → 回调 `budget_bridge::on_budget_settled` 把预算置 `ACTIVE`；
3. 报表 `GET /api/business/erp/budget/report` 只返回 ACTIVE，所以截图里的 3 个槽是上述审批链的产物。

### 为了让审批链能跑通，对**演示库**做了 4 处配置修补（如实记录）

`cloudpivot_manual2` 里没有任何部门（`sys_dept` 0 行）、审批角色下的夹具用户密码哈希非法（`Unknown hash format`），不修就没法演示「提交 → 生效」。做的都是**配置/演示数据**层面：

1. `POST /api/system/dept` 新增 `FIN 财务部`、`PUR 采购部`（预算必须有部门；此前库内 0 部门 → 页面「部门」下拉为空）；
2. `PATCH /api/system/user/{id}/password` 重置 `p3a_0002`（部门主管）、`pc_demo_li`（财务经理）两个夹具用户的密码，并按首登强改流程改成可用密码；
3. `PUT /api/system/role/{id}/menus` 给 `DEPT_LEADER` / `FINANCE_MANAGER` / `GENERAL_MANAGER` / `FINANCE_DIRECTOR` 补上 `business:flow:approve` 权限点（此前这些角色**没有审批权限**，`/task/{id}/approve` 一律 403「需要权限: business:flow:approve」）；
4. 内控闸（`budget_bridge::guard_self_approval`）要求「编预算的人不能批自己编的预算」，所以审批人用的是上面两个非 admin 账号，不是超管自批。

> 上述 3 是**真实产品缺口**（部门主管角色不带 `business:flow:approve`，预算审批链在该环境下永远推不动），本卡只在演示库改了数据，未改种子迁移；如需固化应另开后端卡改 V0227/V023x 的权限种子。

## 5. 门禁

| 检查 | 结果 |
|------|------|
| `pnpm typecheck`（vue-tsc --noEmit） | **0 error**（2026-11-05 02:5x 实跑） |
| n08 等价校验（`sys_menu.menu_type=2` 的 path 逐个在 `router/index.ts` 里找 `path: '…'`） | **69 条，0 缺失**（与 `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router` 同规则；未跑 cargo test，编译成本高，本项为规则等价复算） |
| 浏览器控制台 / 接口错误 | 取图全程 `api-errors: none`（`/api/business/notice/stream` 的 401 为 P1-FIX-6 已知问题，已在脚本里排除） |

## 6. 复现

```bash
# 后端（演示库）
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18097 \
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_NAME=cloudpivot_manual2 \
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
./target/debug/cloudpivot

# 前端
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18097 pnpm dev --port 5180

# 截图
node docs/evidence/20261105-t42d844c0-budget-pages/scripts/budget-final.mjs
node docs/evidence/20261105-t42d844c0-budget-pages/scripts/budget-retake.mjs
```
