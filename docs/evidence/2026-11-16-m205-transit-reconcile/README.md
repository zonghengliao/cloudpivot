# M2-05 在途库存：对账（在途 + 已出 = 发货总量）· 证据

- 卡：`t_b545b37a`（M2-05【M2·仓库】在途库存：对账）
- 上游：`t_f1bce846`（M2-04 查询）/ `t_5f324a4c`（M2-03 签收）/ `t_6c090f97`（M2-02 过账）/
  `t_7ba5b0e2`（M2-01 表 `biz_inventory_in_transit`）
- 交付形态：**只读对账接口** + 迁移 V0422（权限码）+ 真库用例 + 口径文档 + 演练脚本
- 结论：演练 `PASS=35 FAIL=0 / ALL PASS`（`00_drill.out`）

## 一、交付物

| # | 文件 | 内容 |
|---|---|---|
| 1 | `backend/app/src/business/erp/inventory_transit/reconcile.rs` | 对账取数（一条 `WITH … FULL OUTER JOIN`）+ 六个差异码判定 + 截断/汇总口径；**零写 SQL** |
| 2 | `backend/app/src/business/erp/inventory_transit/dto.rs` | `TransitReconcileFilter` / `TransitReconcileRow` / `TransitReconcileTotals` / `TransitReconcileRes` |
| 3 | `backend/app/src/business/erp/inventory_transit/handler.rs` | `GET /business/erp/inventory-transit/reconcile`（权限 `business:erp:inventory-transit:audit`）+ 名字批量补齐（2 条 SQL，无 N+1） |
| 4 | `backend/app/src/business/erp/inventory_transit/transit_reconcile_tests.rs` | 真库用例 10 条 |
| 5 | `backend/app/migrations/business/V0422__erp_inventory_transit_reconcile.{up,down}.sql` | 只种 1 个权限码（按钮挂「即时库存」页下、path NULL、跟随在途读码授权 + 超管），幂等 + 回滚 |
| 6 | `scripts/verify/m205_transit_reconcile_drill.sh` | 演练：克隆库 + 迁移幂等/回滚 + 真库用例 + 三道权限/菜单闸与基线 diff |
| 7 | `docs/ERP-库存对账口径.md` §8 | 对账式 / 六个差异码 / 三个范围边界 / 手工核对 SQL（唯一口径落文档） |

零前端改动（前端在途页是 M2-07），零表结构改动（表在 V0420）。

## 二、对账式（唯一口径）

```text
发货总量 item_qty       = Σ biz_shipment_item.qty                    （明细是 SSOT）
已出     received_qty   = Σ biz_inventory_in_transit.received_qty    （status <> 'CANCELLED'）
在途     qty_in_transit = Σ biz_inventory_in_transit.qty_in_transit  （status <> 'CANCELLED'）

行恒等（V0420 生成列）：shipped_qty = received_qty + qty_in_transit
对账式（本卡）        ：item_qty    = received_qty + qty_in_transit
```

粒度 `(发货单 × 物料)`；明细侧恒带 `s.status = 'POSTED'`（草稿/待过账单本来就没有在途行）；
两侧 `FULL OUTER JOIN`（`LEFT JOIN` 会漏掉「有在途没明细」那一侧）。

## 三、六个差异码（四个方向都查）

| issue | 含义 | 硬问题 |
|---|---|:--:|
| `QTY_MISMATCH` | 两侧都有行但发货量不等 | ✅ |
| `MISSING_TRANSIT` | 有明细、无在途行 | ✅ |
| `ORPHAN_TRANSIT` | 有在途行、明细里没这个物料 | ✅ |
| `IDENTITY_BROKEN` | 行级 `shipped ≠ received + in_transit`（绕过生成列） | ✅ |
| `MULTI_WAREHOUSE` | 同单同物料多仓在途（同一批货记两份） | ✅ |
| `CANCELLED_ONLY` | 整批作废（只有在途作废行） | ❌（标注，不算不平） |

`balanced = 零硬问题 && 对账差额 = 0 && 未截断`。

## 四、怎么验的（全部真跑，输出在 `00_drill.out`）

| 步骤 | 内容 | 结果 |
|---|---|---|
| 迁移 | 克隆 `cloudpivot_test` → 补 V0405…V0421 → 应用 V0422 | PASS |
| 码形状 | 1 个承载节点 / `menu_type=3` / `path IS NULL` / 挂在「即时库存」页 / `sort_order=30` | 6 条 PASS |
| 授权 | 持 `inventory-transit:list` 的角色全部拿到对账码（缺口 0）；非超管 4 个角色持有 | 2 条 PASS |
| 幂等/回滚 | up 重放（节点数、授权行不变）→ down → down 再跑 → up（授权恢复不重复，且不带走 V0421 的读码） | 8 条 PASS |
| 真库用例 | `business::erp::inventory_transit` **41/41**（真库 18：对账 10 + 查询 8；纯逻辑 23），SKIP=0 | PASS |
| 上游回归 | `business::erp::shipment` **46/46**（在途写入口未被破坏），SKIP=0 | PASS |
| 三道闸 | perm_menu_gate 迁移前报出本卡码 → 应用后转绿；permission_audit 失败集与基线**完全一致**（0 新增红，既有 1 条是 M1-23 的 `receivable-aging`）；孤儿清单与基线**完全一致**（0 新增孤儿，既有 4 对属别的卡）；menu_invariants 8 条全绿 | PASS |

### 对账真库用例 10 条（`transit_reconcile_tests`）

| 用例 | 钉住什么 |
|---|---|
| `three_stages_are_all_balanced` | 未签收 / 部分签收 / 全签收三形态都恒等；合计 300 = 已出 140 + 在途 160 |
| `quantity_mismatch_reports_diff` | 明细 100 / 在途账 90 → `QTY_MISMATCH` + 差额 10 逐分给出 |
| `missing_transit_is_reported` | 有明细无在途行 → `MISSING_TRANSIT`；发货量进 `untracked_item_qty`，不进对账式 |
| `orphan_transit_is_reported` | 在途行多出物料 → `ORPHAN_TRANSIT`（差额为负） |
| `cancelled_only_is_flagged_not_mismatched` | 整批作废 → 标注、不计不平、不进对账式 |
| `multi_warehouse_rows_are_flagged` | 同单同物料两仓在途 → `MULTI_WAREHOUSE` |
| `draft_shipments_are_out_of_scope` | `DRAFT` / `PENDING_POST` 不参与对账（防满屏假红） |
| `filters_narrow_the_scope` | 物料 / 日期区间（闭边界）各自收窄 |
| `include_matched_controls_row_detail` | 缺省只给差异行；`include_matched=1` 连对平的组一起给 |
| `identity_never_breaks_on_generated_column` | 全表 `shipped = received + in_transit` 0 行破例；`scan` 与库一致 |

纯逻辑 10 条（`reconcile::tests`）：恒等式、四类硬问题各自命中、多仓、作废、汇总口径、
明细侧恒 POSTED、两侧筛选对称（占位符=参数）、截断判据、issue 文案覆盖。
handler 新增 2 条纯逻辑：`include_matched` 严格解析（`1/yes/on` 认、拼错 400）、对账筛错 400+66006。

## 五、诚实说明（不藏）

1. **HTTP 端到端未实跑**：99 上无运行实例。路由证据 = 编译期 `#[nik::permission]` 注解 +
   `n16`（每条路由都有权限闸）全绿 + `perm_menu_gate` 转绿；接口层编排（`reconcile_of`）由
   10 条真库用例直接调用覆盖（含出参字段、名字补齐、include_matched、筛错 400）。
2. **`MISSING_TRANSIT` 会报历史单**：M2-02 之前的发货过账 = 立即出库，没有在途账这一层；
   这类单据本该被看见（不是 bug，也不该「补一行假在途账」）。用法是按 `ship_date_from/to`
   收到在途账上线之后的区间。已写进口径文档 §8.3 与 DTO 注释。
3. **一次最多比对 2000 组**（`RECONCILE_SCAN_LIMIT`），超限 `truncated=true` 且 `balanced=false`；
   不像 `/page` 那样翻页 —— 对账是「全量结论」类接口，翻页会把合计变成半截账。
4. **既有基线红（别人的卡，未修）**：`permission_audit_tests::n14` 报
   `business:erp:report:receivable-aging` 死码（M1-23 的 V0418 页面行 + 后端零引用）、
   `orphan_button_gate` 既有 4 对孤儿。本卡用「迁移前基线 vs 收尾」逐条 diff 证明 0 新增红/孤儿；
   死码已在 `t_fa41e0e1` 另开卡。
5. **前端零改动**：未跑 `pnpm typecheck/build`（无前端文件变更）；前端在途页 = M2-07。

## 六、复跑

```bash
# 99 上跑（连 145 的 PG，脚本自建自删演练库）
bash scripts/verify/m205_transit_reconcile_drill.sh

# 只跑本卡真库用例（自己建库）
HOST=172.18.10.145
PGPASSWORD=*** createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m205_<用途>
ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_m205_<用途>" \
  cargo test --lib business::erp::inventory_transit -- --test-threads=1
PGPASSWORD=*** dropdb -h $HOST -U cloudpivot cp_m205_<用途>
```
