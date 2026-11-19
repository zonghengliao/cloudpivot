# M5-30【M5·成本】成本还原：前端页（还原明细）

> 卡号：`t_2ce07449` · 交付时间：2026-11-20 · 执行位置：99（开发机），PG 走 `172.18.10.145:5432`

## 1. 这一卡交付了什么

一个页面 **`/business/erp/cost-restore`（菜单「成本还原」，挂在「成本核算」目录 sort 78）**，两个页签：

```
┌ 页签 ① 还原明细   = 逐成本对象（一行一批）：还原前料工费 → 逐层卷积 → 还原后料工费
│                    展开行 = 这一批是什么 + 三要素对照（前/后/差额）+ 逐层台账
│                            + 半成品还原明细 + 这一行怎么修
└ 页签 ② 还原后报表 = 按车间（缺省）/ 产品 / 对象 / 整表 汇总的料工费占比与结构变化（pp）
```

| 层 | 文件 | 说明 |
|---|---|---|
| 契约 | `frontend/src/api/business/erp/cost-restore-kinds.ts` | 字段逐字抄 M5-27 / M5-28 后端 DTO；词表（三档状态 / 四档维度 / 容差） |
| 请求 | `frontend/src/api/business/erp/cost-restore.ts` | `fetchRestoreRollup` / `fetchRestoreReport`（路径写全 `/api`，AGENTS 约定 9） |
| 纯逻辑 | `frontend/src/views/business/erp/cost-restore/cost-restore-logic.ts` | 入参拼装 / 格式化 / 判色 / 卡片 / 展开行 / 话术 —— **不重算任何业务公式** |
| 页面 | `frontend/src/views/business/erp/cost-restore/index.vue` | 两个页签 + ListPage 复用 + 展开行三段表 |
| 路由 | `frontend/src/router/index.ts` | `business/erp/cost-restore` → `BusinessErpCostRestore`（meta.title 成本还原） |
| 菜单 | `backend/app/migrations/business/V0472__cost_restore_menu.{up,down}.sql` | 1 条页面行（复用 `business:erp:cost-object:list`，零新增码）+ 派生授权 + 7 条自检 |
| 用例 | `frontend/tests/unit/erp-cost-restore.test.ts` | 35 条纯逻辑用例 |

**零**：零后端 Rust 代码改动、零新权限码、零按钮行、零新接口、零写动作。

## 2. 为什么两个页签、为什么不新建权限码

* **两页签**：M5-26/27/28 是「结构 → 还原 → 报表」三层，本卡是这三层**唯一的前端落点**
  （M5-30 是成本还原的最后一张卡）。明细页签消费 M5-27（`cost-restore-rollup`），
  报表页签消费 M5-28（`cost-restore-report`）；M5-26 的逐层结构树已经被 M5-27 卷积成
  `flows[]` / `semis[]`，再单独画一棵树是同一份数据的第三种视图，属夹带，本卡不做。
* **复用 `cost-object:list`**：两条接口的 `#[nik::permission]` 闸都是它（后端单测钉住）。
  ERP 全域尚未做 PERM-01「页面码 / 接口码解耦」（只在 HR 试点），新开 `cost-restore:*`
  会立刻变成没有接口消费的死码，`permission_audit_tests` 的 N14/N18 判红 —— 与 V0464/V0466/V0471 同一条理由链。
* **零写动作**：还原是只读计算，页面没有保存按钮，因此不需要写码，也不需要「提交后失败补偿」的面（AGENTS 约定 11）。

## 3. 前端不重算（本卡最重要的口径）

| 数 | 唯一实现 | 页面怎么用 |
|---|---|---|
| 逐层卷积（半成品 → 料工费） | `cost_restore_rollup_dto::restore_of`（M5-27，43 条单测） | 直接显示 `after` / `delta` / `flows[]` / `semis[]` |
| 分组汇总 + 料工费占比 | `cost_restore_report_dto::aggregate` / `group_view`（M5-28，24 条单测） | 直接显示 `restored.*_share` / `shift.*_pp` / `share_sum` |
| 合计 | 后端 `totals`（整表，不受分页影响） | 汇总卡片逐字取，**不拿当前页累加** |

前端只做四件事：格式化（千分位 / 百分比 / 带符号 pp）、判色（状态 / `share_sum` / `check`）、
入参拼装（期间必填、层数夹 1–12）、话术（没有批次 / 不彻底 / 空态 / 口径去 markdown）。

## 4. 三个「按上游口径照做」的坑（本卡逐条落进代码与用例）

1. **`share_sum` 可能是 `100.01`** —— 四项各自由 4 位金额四舍五入到 2 位，容差 ±0.02
   （`SHARE_SUM_TOLERANCE`），**不硬判 100**。用例钉住 `100.01 → ok`、`100.03 → off`，
   报表页 `100.01%` 的行显示为正常（见截图 `ui-02-report.png` 总装车间行）。
2. **逐层台账各层不能相加** —— 第 2 层起是上一层「材料部分」往下拨的钱（`carried_down`）。
   页面按层展示、列头与说明都写死这句话；每层再做一次守恒自检
   （`imputed − (外购+工+费+下拨+不彻底) = 0`，**4 位精度**，`0.0001` 也算不平）。
3. **`state = partial` 是正常口径，不是 bug** —— 缺结构 / 已红冲 / 结构闭环 / 层数上限都会让金额停在
   半成品形态。页面给红 tag + 展开行逐条 `issues`（事实 + 怎么修）+ 「调大层数」提示。

## 5. 验收实测（全部可复核）

| 项 | 命令 | 结果 |
|---|---|---|
| 前端类型（页面 + 用例） | `pnpm typecheck` / `pnpm typecheck:unit` | rc=0 |
| 前端构建 | `pnpm build` | rc=0（含 typecheck） |
| 纯逻辑用例 | `npx tsx --test tests/unit/erp-cost-restore.test.ts` | **35 passed / 0 failed**（`unit-tests.log`） |
| 迁移（克隆库，四次操作） | 应用 → 重放 → 回滚 → 再应用 | 首次 4 组 INSERT；重放 **INSERT 0 0**（幂等）；回滚删 3+1+3；再应用复原（`migration-roundtrip.log`） |
| 菜单结构不变量（克隆库） | `bash scripts/verify/menu_invariants.sh cp_m530_menu` | **8 条全绿**（`migration-roundtrip.log` ⑤） |
| 空库首启（cp_m530_head，无模板） | 空库直接跑二进制 | **应用 401 个迁移 / 203 表 / `/health` 200**（`fresh-db-stats.log`） |
| 菜单结构不变量（空库首启后） | `bash scripts/verify/menu_invariants.sh cp_m530_head` | **8 条全绿**，菜单总数 573 / 成本还原 sort 78（`menu-invariants-fresh-db.log`） |
| 菜单↔路由 / 图标回归 | `ERP_DB_URL=…/cp_m530_head cargo test --lib system::menu` | **22 passed / 0 failed**（含 n08 路由可解析、n09 图标真实存在） |
| 权限审计 | `cargo test --lib permission_audit_tests` | 通过（零新增码，N14/N18 不动） |

### 5.1 UI 冒烟（真实浏览器，dev server + 本地实例）

* 环境：`NIK_PROFILE=test` + `NIK_DATABASE_*`（145 的 `cp_m530_head`）+ `NIK_SERVER_LISTEN=127.0.0.1:18036`
  + 三把密钥 + `NIK_ADMIN_PASSWORD`（一次性本地初始口令），`vite --port 5199` 反向代理到该实例；
* 步骤：admin 登录（首登强制改密）→ 打开 `/business/erp/cost-restore` → 两个页签 + 展开行；
* 结果：**页面挂载无运行时异常**（`window.__errs` 空）、真实接口返回 200（空库 = 空态）、
  两个页签在有数据时渲染完整（截图）；
* 数据来源说明：空库首启的实例里没有成本对象，`ui-01/ui-02` 两张截图是
  **在浏览器里把 `cost-restore-rollup` / `cost-restore-report` 两个 XHR 用固定夹具改写**后拍的
  （夹具脚本见本目录 `ui-stub-fixture.js`，**不进仓库代码、不影响构建产物**），
  目的是验证「有数据时」的渲染路径；真实接口同口径的空态另行验证（返回 200 + `totals.row_count = 0`）。

### 5.2 库与环境

* 库：`cp_m530_head`（**空库首启**，不用漂移的 `cloudpivot_test` 模板库）、`cp_m530_menu`（克隆自模板库，验迁移幂等/回滚）；
* 验证完两个库都已 `dropdb`，临时实例与 dev server 已停（18036 / 5199 无监听、无 cloudpivot 进程残留）；
* 未新建任何 config profile 文件（全程 `NIK_*` 环境变量）。

## 6. 与上游卡的口径对拍

* M5-27 的守恒式 `after.total + unresolved = before.total` → 页面「守恒自检」卡与每行 `check`；
* M5-28 的 `share_sum` 舍入差如实暴露 → 报表页「占比合计（自检）」卡 + 行内 tooltip，容差 0.02；
* M5-29 的四层链口径（截断只挡材料、已拆出的工费不动）→ 明细页「层数」列 + `depthHint` 话术；
* M5-29 提醒的「结构表 semi 而还原表 partial 是正常口径差」→ 本页只消费还原表/报表，
  不对两个接口做交叉判红，`PARTIAL_HINT` 里写明原因清单。
