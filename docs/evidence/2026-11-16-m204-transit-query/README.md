# M2-04 在途库存查询接口（卡 t_f1bce846）

> 2026-11-16 · 后端 · 分支 `main`
> 上游：M2-01（表 `V0420`）/ M2-02（发货过账写在途）/ M2-03（签收确认结转在途）
> 下游：M2-05（对账）、M2-06（在途单测）、M2-07（前端在途页）、M2-15（补货建议）

## 一、交付了什么

两个**只读**接口（同一权限码 `business:erp:inventory-transit:list`）：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/business/erp/inventory-transit/page` | 在途明细分页：`material_id` / `warehouse_id` / `customer_id` / `shipment_id` / `status` / `ship_date_from` / `ship_date_to` / 分页 |
| GET | `/api/business/erp/inventory-transit/summary` | 按 **物料 × 发货仓** 汇总未签收在途（含合计；行数上限 500，超了 `truncated=true`，合计不受截断影响） |

代码落点：

```
backend/app/src/business/erp/inventory_transit/
  mod.rs                   模块与容器说明
  dto.rs                   TransitFilter / TransitRes / TransitSummaryFilter / TransitSummaryRes + 纯逻辑校验
  model.rs                 取数层：明细分页（page_of）、汇总、主数据批量补齐（无写 SQL）
  handler.rs               接口层：两条 GET + 名字批量补齐编排 + 错误收口
  transit_query_tests.rs   真库用例（7 条）+ 纯逻辑用例，只在 cargo test 编译
backend/app/src/business/erp/shipment/shipment_transit.rs   + STATUSES / is_valid_status / status_label（状态域只此一份）
backend/app/src/business/erp/mod.rs                         注册 inventory_transit
backend/app/migrations/business/V0421__erp_inventory_transit_query.{up,down}.sql
scripts/verify/m204_transit_query_drill.sh                  真库演练（32 断言）
```

零写路径：本模块**一行写 SQL 都没有**。在途行的产生（`shipped_qty`）与结转（`received_qty`）仍然是
`shipment_transit::record_shipped` / `receive_open` 的独家地盘。

## 二、口径决定（逐条写理由）

| 决定 | 理由 |
|---|---|
| 权限码单开 `business:erp:inventory-transit:list`，不复用 `inventory:list` | 在途不是仓里的量（V0420 文件头），两本账授权面不同：能看「仓里有多少」≠ 要给「路上有多少」。与 V0143 给库存巡检单开 `inventory:audit` 同取向 |
| 码挂在 **menu_type=3（按钮）** 下（父页 = 「即时库存」） | `menu_type=2` 带 `path` 就是侧栏入口，而前端页是 M2-07；现在多一个 type=2 会立刻在侧栏出现点不通的菜单（命中 SPA fallback → 「响应体格式不符合」）。按钮在授权树里可见可勾，不进侧栏 |
| 授权**跟 `inventory:list` 的持有角色走**（`SELECT … FROM sys_role_menu` 现算） | 同一条仓库职责线，手抄角色名单会随矩阵漂移（V0258 的教训）。按**码**判重，重放零变化 |
| `status` 留空 = `IN_TRANSIT`，`CANCELLED` 只能显式点名 | M2-03 的 downstream_contract 原话「在途取数一律带 status <> 'CANCELLED'」。默认放开会把签完/作废的历史行翻出来，数字看着像在途 |
| 非法 `status` / 非法 UUID / 日期格式错 / 区间反了 → **400 + 66006** | 静默忽略条件是假数据（「按物料筛」被吞掉 = 返回全部物料）。与 `shipment_handler` 的筛错口径一致 |
| 客户经**发货单头**关联（`LEFT JOIN biz_shipment … AND s.deleted_at IS NULL`） | 在途行没有客户列。LEFT 而非 INNER：单头软删时在途行仍要可见（它是发货过账的派生账），只是 `ship_no` / `customer_id` 为 `null` |
| 三个数量**原样取库**（含生成列 `qty_in_transit`） | M2-01 把「在途 = 发货 − 签收」钉成 PG 生成列；在这里再算一遍就是第二条会漂的口径 |
| 明细最老在前（`ship_date ASC`），汇总按在途量倒序 | 在途页看的是**账龄**（哪批货挂最久）；汇总看的是**重点**（谁占的量最多） |
| 汇总行数上限 500 且多取一行判 `truncated` | 只 `LIMIT 500` 分不清「正好 500 组」与「被截断」，两者界面上长得一样 |
| 排序四列（`ship_date, shipment_id, material_id, warehouse_id`） | 唯一 → 分页稳定，同一行不会跨页重复 |

## 三、验证证据（`00_drill.out` 全文，exit=0）

`bash scripts/verify/m204_transit_query_drill.sh` → **PASS=32 / FAIL=0 / ALL PASS**

* **迁移本体**：`V0421` 应用成功；码只有 1 个承载节点、`menu_type=3`、`path` 为空、挂在「即时库存」下；
  持 `inventory:list` 的角色缺口 0；4 个非超管角色拿到码（N20 口径）。
* **幂等**：`up` 重放后节点数 / 授权行数都不变。
* **回滚**：`down` 后码与授权行清零、`down` 可重放、再 `up` 授权恢复且不重复。
* **真库用例**：`business::erp::inventory_transit` **19 条全绿**（真库 7 + 纯逻辑 12），SKIP=0；
  上游 `business::erp::shipment` **46 条全绿**（在途账写入口未被破坏），SKIP=0。
* **闸门（与「应用 V0421 之前」的基线逐条比对）**：
  * `perm_menu_gate` 迁移前**报出本卡的码**（证闸是活的）→ 应用后**转绿**；
  * `permission_audit_tests`（N14…N21）失败集与基线**完全一致**（0 新增红）；
  * `orphan_button_gate` 孤儿清单与基线**完全一致**（0 新增孤儿）；
  * `menu_invariants.sh` 8 条结构不变量全绿。
* 另外：`cargo fmt --check` 0 diff、`cargo clippy --lib -- -D warnings` 0 warning、
  `cargo clippy --all-targets -- -D warnings` 0 warning、`bash scripts/check.sh --l0` 全部通过。

## 四、诚实缺口（不是「已做」，是「没做」）

1. **HTTP 端到端未实跑**（99 无运行实例）。路由可用性的证据是编译期宏注解 +
   `permission_audit_tests::n16`（每条路由必须有 `#[nik::permission]`）全绿 + `perm_menu_gate` 转绿。
2. **专门的在途单测是 M2-06 的卡**；本卡的 19 条用例只覆盖「查询口径」，不覆盖 M2-01..M2-03 的写入不变量。
3. **前端页是 M2-07 的卡**：本卡零前端改动（`pnpm` 未跑，因为没有任何前端文件变更）。
   接页面时：路由 `meta.permission = 'business:erp:inventory-transit:list'`，页面码不需要新增菜单。
4. **基线红（与本卡无关，未修）**：
   * `permission_audit_tests::n14` 报 `business:erp:report:receivable-aging` 为死码
     （菜单行由 M1-23 的 `V0418` 种下，后端零引用 —— 属 AGENTS.md 约定 14 的「页面码后端零引用」，
     但 N14 还没有 ERP 侧的例外台账）；
   * `orphan_button_gate` 既有 4 对孤儿（`notice:mine` ×2、`receivable:aging`、`role-mutex:conflict`）。
   两者在**应用 V0421 之前就已经存在**，本卡的演练用「基线 vs 收尾」逐条 diff 证明没有新增。

## 五、下游契约

* **M2-05（对账）**：未签收在途一律用 `/summary` 的口径（`status='IN_TRANSIT'` + 同一句 `LEFT JOIN biz_shipment`）；
  恒等式 `sum(shipped_qty) = sum(received_qty) + sum(qty_in_transit)` 仍由生成列保证，别在应用层再算一遍。
* **M2-07（前端）**：列表用 `/page`（`page_size` 上限 200、默认 20、最老在前），顶部汇总卡用 `/summary`
  （`truncated=true` 时提示「已按在途量前 500 组展示，合计是全量」）。
* **M2-15（补货建议）**：按物料取在途用 `/summary?material_id=…`（`qty_in_transit` 直接可用），
  不要自己写 `SUM`；`shipment_transit::open_qty` 仍可用于「单仓单物料」的内核判据。
