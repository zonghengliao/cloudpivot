# M2-12 库龄：前端页面（卡 t_5e045c0f）· 证据

产物：`/business/erp/inventory-aging` 前端页（三个 Tab：**库龄分析** / **呆滞清单** / **批次层明细**），
消费 M2-08 / M2-09 / M2-10 已落地的三个只读接口，背后是同一个库函数
`biz_inventory_aging_layers`（V0424）。本卡**零后端业务代码改动**，后端只加了一条迁移
（页面显形码）与一行例外台账登记。

## 改动清单

| 文件 | 说明 |
|------|------|
| `frontend/src/api/business/erp/inventory-aging.ts` | 三个只读接口的 TS 契约（类型 + fetcher），字段逐字抄 Rust DTO |
| `frontend/src/views/business/erp/inventory-aging/aging-logic.ts` | 纯逻辑：拼参 / 库龄区间校验 / 数量比例格式化 / 口径文案 / 行键 / 钻取 |
| `frontend/src/views/business/erp/inventory-aging/index.vue` | 页面（三个 Tab；按取数码决定渲染哪几个 Tab） |
| `frontend/src/router/index.ts` | 注册路由 `business/erp/inventory-aging`，`meta.permission = …:inventory-aging:page` |
| `frontend/tests/unit/m212-inventory-aging-page.test.ts` | 35 条页面契约单测（纯逻辑 22 + 源码契约 13） |
| `backend/app/migrations/business/V0427__erp_inventory_aging_page.{up,down}.sql` | 页面显形码 + 菜单页面行 + 授权 + 祖先闭包 + 自检 |
| `backend/app/src/system/menu/permission_audit_tests.rs` | `ROUTE_LEVEL_ONLY` 登记 `…:inventory-aging:page`（N18 持续校验） |
| `docs/dead-permissions-20261101.md` | §10 补记（四个码的分工） |

## 门禁（全部真跑，输出见同目录 `.out`）

| 门禁 | 命令 | 结果 | 文件 |
|------|------|------|------|
| 页面单测 | `npx tsx --test tests/unit/m212-inventory-aging-page.test.ts` | **35 passed / 0 failed** | `01_unit_tests.out` |
| 前端类型 | `pnpm typecheck` + `pnpm typecheck:unit` | 两者 rc=0 | `02_typecheck.out` |
| 前端构建 | `pnpm build`（含 typecheck） | **EXIT=0**；产物含页面 chunk（`aging-report-table` / 三个接口路径都在 bundle 里） | `03_build.out` |
| 迁移 | 对 145 的 drill 库跑 V0427：首跑 / 重放（幂等）/ down / 再 up | 四条路径全绿，自检块 `NOTICE: V0427 自检通过：页面授权 5/15 角色（含超管）` | `04_migration.out` |
| 菜单结构 | `bash scripts/verify/menu_invariants.sh cp_m212_aging_page` | **8 条断言全绿**（含 ⑤ 总数 520 / ⑥ title ≤12 / ⑦ 同父 sort 唯一 / ⑧ icon 存在） | `05_menu_invariants.out` |
| 后端 | `rustfmt --check` + `cargo clippy --lib -- -D warnings` + `cargo test --lib system::menu` | fmt rc=0；clippy 0 warning（EXIT=0）；menu 用例 **22 passed / 0 failed**（含 N14/N18/N16/n08/n09/n10） | `06_backend_gates.out` |
| 契约对拍 | 见下「契约怎么验的」 | **11 个 struct 字段全等（ALL MATCH）** | `07_contract_diff.out` |

drill 库：`cp_m212_aging_page` @ `172.18.10.145:5432`（`cloudpivot_test` 克隆后手工补齐
V0405~V0426 —— 模板库只到 V0404，否则 V0424~V0426 的按钮行不存在，V0427 的前置护栏会先炸）。

## 契约怎么验的（没有起后端做 HTTP 实测，所以补了字段对拍）

本机（99）没有可达的 8023 实例（145/144 都拒绝连接），因此**没有**做「登录 → 调接口 → 看 JSON」
这一步。替代验证是把前端 TS interface 的字段集合与 Rust DTO 的 `pub <field>:` 逐个对拍：

```
OK   AgingLayerRes               -> AgingLayerRow          rust=23 ts=23
OK   AgingFilter                 -> AgingLayerQuery        rust= 9 ts= 9
OK   AgingBucketQtyRes           -> AgingBucketQty         rust= 5 ts= 5
OK   MaterialWarehouseAgingRes   -> InventoryAgingRow      rust=15 ts=15
OK   InventoryAgingReportRes     -> InventoryAgingReport   rust=16 ts=16
OK   InventoryAgingReportQuery   -> InventoryAgingReportQuery rust= 3 ts= 3
OK   SlowMovingRowRes            -> SlowMovingRow          rust=17 ts=17
OK   SlowMovingSummaryRes        -> SlowMovingSummary      rust= 9 ts= 9
OK   SlowMovingRes               -> SlowMovingReport       rust=13 ts=13
OK   SlowMovingQuery             -> SlowMovingQuery        rust= 4 ts= 4
OK   AgingBucketDef              -> AgingBucketDef         rust= 4 ts= 4
ALL MATCH
```

脚本：`/root/.hermes/profiles/backend-developer/cache/scratch/m212_contract.py`（临时脚本，未入库）。
三个接口本身的语义与真库行为由上游卡的真库用例覆盖（M2-08 `aging_db_tests`、M2-09
`report_db_tests`、M2-10 `slow_moving_db_tests` / `default_threshold_matches_report_over_90`）。

## 页面口径（写死在页面里，别再各解释一遍）

- **三个 Tab 各判各的码**：报表 `…:inventory-aging:report` / 呆滞 `…:slow-moving` / 批次层 `…:list`。
  页面显形码 `…:page` 只判侧栏 + 路由。缺某个码的角色只少那个 Tab（不是整页 403）。
- **基准日留空 = 后端取今天（CST）**，前端不自己算今天；页头显示后端回显的 `as_of` 与
  `as_of_source`（`QUERY`=手填 / `TODAY`=今天）。
- **合计只认后端**：报表 `summary`、呆滞 `summary` 都是**截断前**全量；页面零 `reduce`
  （单测钉住：`withoutComments(PAGE).includes('.reduce(') === false`）。
- **档位列由后端 `report.buckets` 驱动**（`v-for` + 按 `key` 取格），加档位前端自动跟上。
- **批次层 `include_consumed` 永远显式发** `'true'/'false'`：开关拨了必须生效，不靠后端缺省。
- **库龄区间写反**在 `queryBuilder` 里直接拦住（不发请求），并给出点名两个值的提示。
- **无导出按钮**：后端没有导出接口，画一个点了 404 的按钮比没有更糟。

## 已知形态（不是缺陷，别顺手「修」）

1. **三个取数按钮仍挂在「即时库存」页下**（V0424~V0426 的既有形态，上游验证据本按父节点断言过）。
   授权树上它们与「库龄分析」页面行并列略乱 —— 与 V0423（在途库存页）同款记账，等 ERP 模块统一
   解耦（PERM-01 §6）时一并收。
2. **菜单总数 = 520，正好压在上限**（`menu_invariants` ⑤ 的区间是 400–520）。本卡加 1 条后到 520；
   下一张加菜单的卡会把它顶红，届时按脚本注释里的口径先确认真实增长再同步改阈值
   （`MENU_TOTAL_MAX`）与 `scripts/verify/README.md`。本卡不动阈值（不是本卡范围）。
3. **前置数据依赖**：三维库龄层由 `biz_inventory_txn` 的 IN 流水推导 —— 空库上三个接口都给 0 行，
   页面显示的是「没有结存 / 没有呆滞」的空态文案（不是报错）。

## 复验

```bash
# 前端（99，工作仓）
cd frontend && npx tsx --test tests/unit/m212-inventory-aging-page.test.ts && pnpm typecheck && pnpm build

# 迁移 + 菜单不变量（145 的 drill 库；库名必须含 drill/e2e/test）
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 PGHOST=172.18.10.145 PGUSER=cloudpivot
createdb -T cloudpivot_test cp_m212_recheck
psql -d cp_m212_recheck -v ON_ERROR_STOP=1 -f backend/app/migrations/business/V0427__erp_inventory_aging_page.up.sql
bash scripts/verify/menu_invariants.sh cp_m212_recheck
dropdb cp_m212_recheck

# 后端
cd backend/app && cargo clippy --lib -- -D warnings
SYS_DB_URL='postgres://cloudpivot:…@172.18.10.145:5432/cp_m212_recheck' \
  cargo test --lib system::menu -- --test-threads=1
```
