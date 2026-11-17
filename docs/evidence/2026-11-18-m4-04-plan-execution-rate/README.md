# M4-04 采购计划执行率跟踪（已下单 / 计划量）—— 证据包

- 卡：`t_901d117b`（M4-04【M4·采购】采购计划：执行率跟踪）
- 上游：M4-01（表结构 V0446）/ M4-02（CRUD + 审批 V0447）/ M4-03（计划 → 申请 V0448）
- 下游：M4-06 前端页面（本卡**零前端**）

## 一句话

**执行率 = 已下单 / 计划量**，两个只读接口把它算出来：`GET /api/business/erp/purchase-plan/execution/page`（跨计划总览）
与 `GET /api/business/erp/purchase-plan/{id}/execution`（单计划：单头汇总 + 逐行 + 逐行来源单据）。
数字**不落列、不靠回写**，由 V0449 的两个视图从单据链现算：

```text
计划明细 ──(plan_item_id, V0448)──▶ 申请明细 ──(申请单头 po_id)──▶ 采购订单
```

## 三个口径（每个都有纯函数 + 单测 + 真库用例钉住）

| 量 | 定义 | 为什么这么定 |
|:--|:--|:--|
| 已申请量 `req_qty` | Σ 申请明细 qty（`plan_item_id` 指回本行、申请单未软删） | DRAFT/SUBMITTED/APPROVED/CONVERTED **都算**：量已从计划拨走，只算审批过的会让计划显示「还有余量」→ 用户再转一次 → 同一段量转两遍 |
| 已下单量 `ordered_qty` | Σ 申请明细 qty，且申请单的 `po_id` 指向的采购单**存活且未作废** | 草稿采购单**算**（单已生成、量已拨走，不算会让执行率回落 0 引发重复下单）；作废 / 软删**不算**（链已断） |
| 执行率 `exec_rate` | `ordered_qty / planned_qty`（6 位小数）+ `exec_rate_pct`（0–100，2 位） | 分母恒 > 0（`ck_biz_ppi_qty` 拒 0），无明细时给 `null` 而不是 0（0 会被读成「一行都没下」） |

行状态 `exec_state`：`NOT_STARTED`（已下单 0）/ `PARTIAL`（0 < 已下单 < 计划）/ `DONE`（恰好等于）/ `OVER`（超过计划，执行率 > 100%）。
**剩余** `remaining_qty = planned − ordered`（**可为负**，V0446 头注释 4 刻意不拦超计划，这里就是它露头的地方）；
**还能再转申请** `convertible_qty = planned − booked`（两个口径不混：一个是「还没下单」，一个是「还能再转」）。

## 为什么不用申请明细的 `po_item_id` 反查采购明细（一条实测过的坑）

采购订单改单是**明细致删旧插新**（`purchase_handler` 的 `PUT` 既有口径）。改一次量，
`biz_purchase_requisition_item.po_item_id` 就指向一行已经不存在的采购明细 —— 用它算执行率会**静默打成 0**。
申请单头的 `po_id` 指向采购**单头**，改单不动单头 id，所以口径取「这张申请单已被并进哪张采购单」。

粒度差异（合并采购）：多张申请单的同物料会并进采购单**同一行**，所以「已下单量」按**申请明细的 qty** 计，
不按采购明细行的合并量计 —— 否则同一批量会被每张申请单各记一遍（用例 `merged_po_is_not_double_counted` 钉住：合计 14 而不是 28）。

## 交付物

| 文件 | 说明 |
|:--|:--|
| `backend/app/migrations/business/V0449__purchase_plan_execution.up.sql` | 两个视图（明细级 / 单头级）+ 1 行菜单 + 2 行角色授权 + 探针验算自检（含作废 / 软删两种「链断」边界） |
| `backend/app/migrations/business/V0449__purchase_plan_execution.down.sql` | 先授权 → 菜单 → 视图；**不删业务单**（视图只是读结构） |
| `backend/app/src/business/erp/purchase/purchase_plan_execution_dto.rs` | 契约 + 纯函数（率 / 状态 / 剩余 / 可转 / 来源判定 / 汇总）+ 11 条单测 |
| `backend/app/src/business/erp/purchase/purchase_plan_execution_model.rs` | 只读数据层：总览分页（`scope::paginate_unscoped`）、单头、逐行、下钻来源 + 3 条列清单单测 |
| `backend/app/src/business/erp/purchase/purchase_plan_execution_handler.rs` | 两个路由 + 组装 + 5 条单测（含“非法 uuid → 400”“超计划标红”） |
| `backend/app/src/business/erp/purchase/purchase_plan_execution_db_tests.rs` | 10 条真库用例（真审批、真转单、真作废） |
| `scripts/verify/m404_plan_execution_drill.sh` | 空库首启 → 真 curl 全链 → 迁移幂等 / down / up → 死码闸 → 菜单不变量（**97 断言全绿**） |

## 验收（全部为真跑输出，见同目录 `.out`）

| 项 | 结果 | 证据 |
|:--|:--|:--|
| 空库首启（381 个迁移，含 V0449 自检） | `/health` 200 | `06_drill_full.out` §1 |
| 演练脚本 | **PASS=97 FAIL=0** | `06_drill_full.out` |
| 单卡单测 + 真库用例 | **29 passed / 0 failed**（19 纯逻辑 + 10 真库） | `01_unit_and_db_tests.out` |
| 采购域全量 | **227 / 227** | `02_purchase_module.out` |
| 权限码审计（N14/N18 等） | **8 / 8** | `03_permission_audit.out` |
| 菜单不变量 | **8 条全绿** | `04_menu_invariants.out` |
| 死权限码闸 `perm_menu_gate` | `missing_in_menu = 0` | `05_perm_menu_gate.out` |
| `cargo fmt --check` / `cargo clippy --lib -D warnings` | rc=0 / 0 告警 | `07_fmt_clippy.out` |

真库用例（`ERP_DB_URL` 指向 145 的 `cp_m404_drill`，`--test-threads=1`）：

| 用例 | 钉住的口径 |
|:--|:--|
| `fresh_plan_has_zero_execution` | 新计划率 0、两行未开工、剩余 = 计划、来源为空 |
| `requisition_without_po_counts_as_requested_only` | 转了申请没下单：已申请 = 计划、已下单 0、`booked == req`（回写列不分叉）、原因「未转采购单」 |
| `converted_po_counts_as_ordered_and_rate_is_full` | 转采购单后：已下单 = 计划、率 1.0、DONE、草稿采购单也算、来源带单号 |
| `cancelled_po_drops_out_of_ordered` | 采购单作废：已下单回落 0、**已申请不变**、原因「采购单已作废」 |
| `partial_conversion_keeps_remaining_and_convertible` | 分批 4/10：已申请 4、剩余 10、可转 6（两个口径不混） |
| `merged_po_is_not_double_counted` | 合并转单：两行各算自己那段，合计 = 采购单量 14（不是 28） |
| `over_plan_reports_over_state` | 超计划：率 1.5、OVER、剩余 −5、`min_rate=100` 能筛出 |
| `head_view_matches_item_summary` | 单头视图与 `summary_of(明细)` 逐项相等 |
| `overview_filters_and_hides_soft_deleted` | 总览筛选 + 软删计划不进总览、详情 404 |
| `zz_teardown_purges_fixtures` | `M404-` 夹具 0 残留 |

## 决定（写进代码注释，不重复设计）

1. **只读、不落列**：执行率有两份真相（单据链 vs 回写列）就会分叉；视图现算永远自愈。回写列 `executed_qty` 以 `booked_qty` 并排带出供对账（不等即分叉）。
2. **草稿采购单算已下单**，作废 / 软删不算。草稿不算会让「刚转出来的单」把执行率打回 0，用户会重复下单。
3. **整单状态按汇总算**（`Σordered / Σplanned`），不是「明细行状态的多数」：9 行下满 + 1 行未开工 = 90% 部分下单。
4. **超计划不截断**：剩余保留负数、状态给 `OVER`、`min_rate=100` 可筛 —— V0446 刻意在库层留的口子，在这里显形而不是在插行时报 500。
5. **一个权限码**（`business:erp:purchase-plan:execution`，V0449 菜单行 `…004478`），两个只读接口共用；授采购经理（编制口径）+ 财务经理（≥50 万复核口径）；部门主管 / 普通员工不给（需求表达走采购申请）。
6. **零新增错误码**：参数错误复用 400 / `PARAM_INVALID`，不存在复用 404 / `RESOURCE_NOT_FOUND`（不为只读报表再开号段）。
7. **零前端**：页面与按钮属 M4-06；本卡只落后端两个接口 + 菜单权限承载行。

## 交接给 M4-06（前端）

- 总览分页：`GET /api/business/erp/purchase-plan/execution/page?page&page_size&keyword&status&plan_type&plan_year&plan_month&dept_id&min_rate`
  - 返回行里 `exec_rate_pct`（0–100，2 位）直接做进度条宽度；`over_lines > 0` 或 `exec_state === 'OVER'` 标红；`remaining_qty` 可为负。
- 详情：`GET /api/business/erp/purchase-plan/{id}/execution` → `{ plan, items[] }`，每行 `sources[]` 是下钻（`counted` + `reason` 说明这批量算不算已下单）。
- 两个接口都要 `business:erp:purchase-plan:execution`（不是页面码 `…:page`）。
- 空计划（`line_count = 0`）详情是 **404**（没有执行率可言），前端按「空态 + 提示先编明细」处理。
