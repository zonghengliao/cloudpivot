# COST-B4-3 一键领料跳转 —— 证据

> 卡面：`ARCH-9-3 §8.1` B4-3 —— 成本对象 → 新建出库单预填（依赖 B1-11 / B1-10）
> 前置已交付：B1-10（`CreateStockOutReq.cost_object_id` + 借方 4001.01）、B1-12（出库单表单的成本对象选择器）

## 1. 卡面验收 → 落点 → 实测

| 卡面验收 | 代码落点 | 实测证据 |
|---|---|---|
| 成本对象详情页有「领料」按钮 | `cost-object/index.vue` 详情抽屉：`canIssueMaterial(detail)` 为真（`status='OPEN'`）时出按钮，`v-privilege="[PERM.issue]"`（= `business:erp:stock-out:create`） | `01_cost_object_issue_button.png`（真实例 + 真浏览器，OPEN 对象底部「领料」+ 说明）；`canIssueMaterial` 单测 4 条（OPEN/CLOSED/CANCELLED/null） |
| 跳到出库单新建页，预填成本对象 / 车间 / 物料范围 | 按钮 → `router.push('/business/erp/stock-out?cost_object_id=…')`；出库单页 `onMounted` 调 `GET /business/erp/stock-out/cost-object-issue-prefill`，`applyIssuePrefill()` 套表单 | `02_stock_out_prefilled.png`（横幅 + 出库类型=领用 + 仓库 WH02 + 事由 + 成本对象 CB-B43-000001 + 生产车间 总部 + 明细 原料B×5）；`prefill_history.json` / `prefill_nohistory.json` 是真实响应 |
| 保存后自动关联回成本对象 | 预填把 `cost_object_id` 写进表单，保存走 B1-10 的 `CreateStockOutReq.cost_object_id`（落库 `biz_stock_out.cost_object_id`） | 真浏览器点「保存草稿」→ 详情抽屉显示「成本对象 CB-B43-000001 / 过账借方 4001.01」（`03_saved_linked_to_cost_object.png`）；库内三张单 `cost_object_id` 均非空（见 §5 SQL 输出） |

## 2. 改了哪些文件

| 文件 | 改什么 |
|---|---|
| `backend/src/business/erp/stockout_dto.rs` | 新增 `IssuePrefillQuery`（必填 uuid，三态解析）+ `IssuePrefillRes` / `IssuePrefillMaterialRes` 出参 + `issue_prefill_reason()` 纯函数（**按字符**截 255，`VARCHAR(255)` 口径）+ 常量 `ISSUE_PREFILL_MATERIAL_LIMIT=50`；5 条纯逻辑单测 |
| `backend/src/business/erp/stockout_model.rs` | 新增 `issue_prefill_materials()`（`DISTINCT ON (material_id)` 取每个物料**最近一次**领用：只认 `ISSUE`、排除 `CANCELLED`、剔除软删物料）与 `last_issue_warehouse()`（上次领料仓库） |
| `backend/src/business/erp/stockout_handler.rs` | 新增 `cost_object_issue_prefill()`（查对象 → 复用 `ensure_cost_object_usable` 拦非 OPEN → 组装预填）+ HTTP 入口 `GET /business/erp/stock-out/cost-object-issue-prefill`（权限 `business:erp:stock-out:create`） |
| `backend/src/business/erp/stockout_cost_object_tests.rs` | 追加 4 条真库用例（无历史 / 有历史 / 非 OPEN 409+66057 / 作废单不进范围 + 缺参 400） |
| `frontend/src/api/business/erp/stock-out.ts` | `StockOutIssuePrefill` / `StockOutIssuePrefillMaterial` 类型 + `fetchStockOutIssuePrefill()` |
| `frontend/src/views/business/erp/stock-out/stock-out-logic.ts` | `applyIssuePrefill()`（纯函数）+ `IssuePrefillBanner` |
| `frontend/src/views/business/erp/stock-out/index.vue` | `?cost_object_id=` 入口（`applyRouteIssuePrefill`，先 `router.replace` 摘 query 防幽灵重开）+ 预填横幅 + 只读「生产车间」+ 预填物料补进下拉 + 建议仓库 |
| `frontend/src/views/business/erp/cost-object/cost-object-logic.ts` | `PERM.issue`（出库单建单权限）+ `canIssueMaterial()` |
| `frontend/src/views/business/erp/cost-object/index.vue` | 详情抽屉「领料」按钮 + `goIssue()`（关抽屉后跳转，带 `cost_object_id`） |
| `frontend/tests/unit/erp-stock-out.test.ts` / `erp-cost-object.test.ts` | 预填 4 条 + 按钮/权限 2 条（含接口路径与 query 摘除的源码级断言） |

## 3. 设计决策（写下来免得下游猜）

1. **「物料范围」= 该对象历史领用过的物料**（每物料取最近一次的数量），**不是 BOM**。
   设计 §8.3 R-12 明确一期不做 BOM 定额；凭空生成物料行等于替用户编数据。
   没有历史时返回空数组，前端给一行空明细并提示「这一批还没领过料，请手动选」。
2. **车间不落库**：`biz_stock_out`（V0203）单头只有仓库 / 日期 / 事由 / 状态，**没有车间列**。
   车间的唯一载体是成本对象（`workshop_dept_id`），所以预填把它做成
   ① 表单只读字段（可验收）② 写进出库事由「生产领料：CB-…（产品） 车间」（凭证摘要可见）。
   ⇒ **不做**给 `biz_stock_out` 加 `dept_id` 的迁移：成本归集按对象不按部门，
   加列会引入「单据部门与对象车间不一致该信谁」的第二个真相源。
3. **建议仓库 = 上次领料的仓库**，不是成本对象的 `target_warehouse_id`（那是**完工入库**仓）。
   拿成品仓预填领料，用户第一次保存就会吃「负库存 / 可用量 0」。
4. **权限用出库单的建单权限**（`business:erp:stock-out:create`）：按钮的最终动作是
   `POST /business/erp/stock-out`，用成本对象权限会让仓管看到按钮却 403。
5. **非 OPEN 对象当场 409**（`66057`，复用 B1-10 的码），不打开表单 ——
   否则用户白填一整张单再被拦。
6. **不预填库位账行**：历史桶可能已被并桶 / 改名，带过去就是 400；物料 + 数量够了。

## 4. 实测输出

| 文件 | 内容 |
|---|---|
| `04_pure_tests.out` | `cargo test --lib stockout_dto::tests` → **17 passed / 0 failed**（含新增 5 条） |
| `05_real_db_tests.out` | `ERP_DB_URL=…b43_cost_live cargo test --lib cost_object_tests -- --test-threads=1 --nocapture` → **12 passed / 0 failed**（B1-10 的 6 条回归 + B4-3 新增 4 条 + 下拉 2 条） |
| `06_frontend_gate.out` | `pnpm typecheck` exit 0；`pnpm test:unit` **1380 passed / 0 failed**（新增 27 条）；`pnpm build` exit 0 |
| `01~03_*.png` | 真浏览器（1440×900）三步：详情「领料」→ 预填表单 → 保存后详情挂上成本对象 |
| `prefill_nohistory.json` | 无历史：`materials: []`、`has_history:false`、`suggested_warehouse_id:null`、事由带对象号 + 车间 |
| `prefill_history.json` | 领过一次 5 个 M-W1A-002 之后：`materials=[{M-W1A-002, qty 5.0000, last_out_date 2026-11-11}]`、`suggested_warehouse_id=WH02` |
| `created.json` | `POST /api/business/erp/stock-out` 带 `cost_object_id` → 返回 `cost_object_no=CB-B43-000001`、`debit_account_code=4001.01` |
| `closed.json` / `missing.json` / `notfound.json` | 409+66057（已关批）/ 400+10001（缺参）/ 400+66002（不存在） |

关键几行（真实 HTTP，端口 18033，profile=test，库 `b43_cost_live` 空库首启自动迁移）：

```text
GET /api/business/erp/stock-out/cost-object-issue-prefill?cost_object_id=…a1
→ 200 {"out_type":"ISSUE","workshop_dept_name":"总部","reason":"生产领料：CB-B43-000001（成品E（种子）） 总部",
        "materials":[],"has_history":false}

POST /api/business/erp/stock-out（挂 cost_object_id）
→ 200 {"out_no":"OUT-20261111-000001","cost_object_no":"CB-B43-000001","debit_account_code":"4001.01"}

GET …prefill（同一对象，领过料之后）
→ 200 {"materials":[{"material_code":"M-W1A-002","qty":"5.0000","last_out_date":"2026-11-11"}],
        "suggested_warehouse_id":"…0002","suggested_warehouse_name":"分仓（种子）","has_history":true}

GET …prefill?cost_object_id=<CLOSED> → 409 {"code":66057,"msg":"成本对象 CB-B43-000002 当前是「已关批」…"}
GET …prefill（无参）               → 400 {"code":10001,"msg":"cost_object_id 必填…"}
GET …prefill?cost_object_id=<随机> → 400 {"code":66002,"msg":"…不存在或已删除"}
```

## 5. 库内交叉验证

```sql
SELECT out_no, out_type, status, cost_object_id, total_qty, reason FROM biz_stock_out ORDER BY created_at;
 out_no              | out_type | status |            cost_object_id            | total_qty | reason
---------------------+----------+--------+--------------------------------------+-----------+--------------------------------------
 OUT-20261111-000001 | ISSUE    | DRAFT  | b4c30000-0000-4000-8000-0000000000a1 |    5.0000 | 生产领料：CB-B43-000001（成品E（种子）） 总部
 OUT-20261111-000002 | ISSUE    | DRAFT  | b4c30000-0000-4000-8000-0000000000a1 |    5.0000 | 生产领料：CB-B43-000001（成品E（种子）） 总部
 OUT-20261111-000003 | ISSUE    | DRAFT  | b4c30000-0000-4000-8000-0000000000a1 |    5.0000 | 生产领料：CB-B43-000001（成品E（种子）） 总部
```

（000001 = curl 建单；000002 / 000003 = 真浏览器两次「领料 → 保存草稿」。三张都自动挂上了同一个成本对象。）

复跑：

```bash
# 后端
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<迁移到 V0381 的库>" \
  cargo test --lib cost_object_tests -- --test-threads=1 --nocapture
# 前端
cd frontend && pnpm typecheck && pnpm test:unit && pnpm build
```

## 6. 已知偏差 / 遗留

1. **无迁移**：预填不新增表 / 列（成本对象列 V0375 已有）⇒ 卡面「迁移幂等 + down.sql」N/A。
2. **物料范围上限 50**（`ISSUE_PREFILL_MATERIAL_LIMIT`）：出库明细上限 200 行，预填只给最常见的一批；
   超过 50 种物料的成本对象请手工加行。
3. **不预填库位账行 / 批号**：历史桶可能已变，带过去会 400。前端选完物料自动拉本仓可用库位。
4. **车间只进事由 + 只读展示**（见 §3 决策 2）：若要「按车间出成本报表」，应走成本对象的
   `workshop_dept_id`，不需要单据上再加一层车间。
5. **临时库**：`b43_cost_live`（空库首启自动迁移，本轮实测用）、`mig_replay_b43cost`（空库重放 301/301
   PASS，未首启）—— 交付后已 DROP；后端 18033 / vite 5199 已停。
