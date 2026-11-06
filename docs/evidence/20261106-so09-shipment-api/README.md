# SO-09 · 发货单后端 API（t_b115c1a4）实测证据

> 卡面：实现发货单 CRUD + 状态；从销售订单带出明细（只带未发完的行）；过账前校验库存充足；带测试。
> 验收：`cargo test` 全绿 + 真实 token curl 实测建单 / 过账。

## 1. 交付了什么

### 1.1 接口（5 个，前缀 `/api` 由 `[router].prefix` 注入）

| 方法 | 路径 | 权限码 | 说明 |
|------|------|--------|------|
| POST | `/business/erp/shipment` | `business:erp:shipment:create` | 建单（`DRAFT` + 取号 `SH-yyyyMMdd-000001`）；`items` **留空 = 从订单带出全部未发完的行** |
| GET | `/business/erp/shipment/{id}` | `business:erp:shipment:list` | 详情（含明细 + 订单号 / 客户 / 仓库） |
| GET | `/business/erp/shipment/page` | `business:erp:shipment:list` | 分页（`ship_no` 模糊 / `status` 精确 / `so_id` / `customer_id`） |
| GET | `/business/erp/shipment/shippable/{so_id}` | `business:erp:shipment:list` | 「从订单带出」对话框的读数：**只返回未发量 > 0 的行** |
| PATCH | `/business/erp/shipment/{id}/status` | `business:erp:shipment:status` | 状态流转（只走 4 条合法边） |

> 卡面列了 4 个接口；第 5 个 `shippable/{so_id}` 是**只读**的取数口（SO-11 的前端对话框必须有它，
> 否则前端只能自己算未发量 = 又一套口径）。它复用 `shipment_model::shippable_items`，
> 与建单的带出、超发守卫是**同一个公式**。

### 1.2 状态机（登记进 `state_machine` 注册表，不是本地私表）

```text
DRAFT ──提交──▶ PENDING_POST ──过账──▶ POSTED
  │                 │
  └──草稿作废───────┴──待过账作废（要 reason）──▶ CANCELLED
```

* 4 条边登记在 `state_machine::SHIPMENT_EDGES`，`SHIPMENT_BASE`（全 mandatory）、
  `SHIPMENT_REASON_EDGES`（只 `PENDING_POST → CANCELLED` 要原因）、`SHIPMENT_LOCKED`（`POSTED`/`CANCELLED`）；
* **故意没有** `POSTED → CANCELLED`（已过账作废要红冲 + 回滚三本账，是 SO-10 的卡，不能靠一条状态边假装账回来了）
  与 `PENDING_POST → DRAFT`（发货单没有审批，改就作废重建）；
* `DocKind::Shipment`（`SHIPMENT` / 发货单 / `biz_shipment` / REST 段 `shipment`）同步进前端
  `doc-kinds.ts` 与巡检登记表 `docs/architecture/snapshot-drift/10_checks.sql`（`STATUS_ENTITY`）。

### 1.3 三道闸（各自独立，缺一个就是线上事故）

| 闸 | 位置 | 挡什么 | 实测 |
|----|------|--------|------|
| 订单状态闸 | `create_shipment` / 提交 | 给 `DRAFT`/`SUBMITTED` 订单发货；给 `OUTBOUND`/`CLOSED`/`CANCELLED` 订单发货 | ④ 409 |
| 超发闸 | `shipment_model::ensure_qty_within_remaining`（SO-08 交付） | 累计发货 > 订单行数量（含**其他未过账单的占用**） | ⑧ 400 |
| 库存闸 | `transition_to_posted` → `stockout_model::fifo_plan` | 过账时仓库不够量 | ⑪ 409 / ⑬ 200 |

库存闸**复用销售出库过账的同一个函数**（`fifo_plan` 读库位账 `biz_inventory_location`），
所以「这里说够、过账时说不够」不会出现两套口径。

### 1.4 与 SO-10 的边界（写在代码注释里，免得下一卡猜）

本卡的「过账」= **状态到 `POSTED` + 过账前库存充足校验**。
**库存三本账扣减 / 生成销售出库单 / 回写 `delivered_qty` / 订单转 `PARTIAL_OUT|OUTBOUND` 是 SO-10 的事**
（`inventory_writer` 是唯一写入口）—— 本卡 `shipment_model` / `shipment_handler` 里**一行库存写 SQL 都没有**。
插入点已在 `change_shipment_status` 标注：`state_machine::transition` 之后、`tx.commit()` 之前。

> ⚠ 已知的**单卡窗口缺口**（SO-10 落地即闭合）：POSTED 之后 `delivered_qty` 尚未累加，
> 所以同一订单行的未发量在 SO-10 之前仍按「全部未发」计算（e2e ⑮ 里第二/三张单还能各发 1 件就是这条）。

### 1.5 复用了什么 / 没复用什么（卡面「必须遵守 #1」逐条回答）

| 机制 | 本卡怎么用 | 为什么 |
|------|-----------|--------|
| `biz_voucher`（凭证） | **不用** | 发货单不是记账触发点：销售成本结转凭证（借 6401 / 贷 1405）与应收 `biz_receivable` 是**出库过账**的产物（SO-10 / 应收卡）。这里造凭证 = 一张单两个记账点 |
| `inventory_writer.rs` | **不写**，只读复用 `stockout_model::fifo_plan` 判够不够量 | 三本账的唯一写入口；扣减是 SO-10，本卡一行库存写 SQL 都没有 |
| 流程引擎 | **不接** | ARCH-7 §4.2 的发货单状态机**没有审批态**（4 值里没有 `PENDING_APPROVAL`）：发货是执行动作，已由订单审批把关。硬加审批只会多一道没人审的待办 |
| `LEGAL_EDGES` 状态机 | **登记** `SHIPMENT_EDGES`(4) + BASE + REASON + LOCKED，流转走 `state_machine::transition`（边表核对 + reason + 写 `biz_status_log`） | 状态机唯一真相在注册表；调用点不许私自放宽（`check_registration` 会 500） |
| `DocKind` 词表 | **登记** `DocKind::Shipment`，同步前端 `doc-kinds.ts` 与巡检 `10_checks.sql` | 三处逐字比对是单测硬门禁（`frontend_ts_mirror_matches_rust` / `doc_kind_registries_match_doc_kind_table`） |

## 2. 改了哪些文件 / 函数

| 文件 | 类型 | 内容 |
|------|------|------|
| `backend/src/business/erp/shipment_handler.rs` | **新增** | DTO（`CreateShipmentReq` / `ShipmentItemReq` / `ShipmentStatusReq` / `ShipmentFilter` / `ShipmentRes` / `ShipmentItemRes` / `ShippableItemRes`）、纯校验（`validate_target` / `validate_qty` / `validate_serials` / `ensure_unique_so_items` / `today_cst`）、业务（`create_shipment` / `resolve_items` / `detail_of` / `get_shipment` / `page_shipment` / `shippable_of` / `change_shipment_status` / `transition_to_posted`）、5 个 HTTP 入口、`SHIPMENT_PERMISSIONS`、7 条纯逻辑单测 |
| `backend/src/business/erp/shipment_model.rs` | 追加 | `SHIP_COLUMNS` / `ITEM_SELECT` / `LEGAL_EDGES` / `can_transition` / `allowed_targets` / `SHIP_TARGET` / `SHIP_ORDER` / `SHIPPABLE_SO_STATUSES`；行结构 `ShipmentRow`（`is_locked`）/ `ShipmentItemRow` / `NewShipmentItem` / `NewShipment` / `StockShortfall`；`total_qty_of` / `split_serials`；`find_shipment` / `items_of` / `shipment_page` / `so_item_brief` / `so_status_of` / `so_no_of` / `so_item_belongs` / `warehouse_label` / `warehouse_labels` / `stock_shortfalls` / `create_shipment`（`insert_items`）/ `apply_status`；3 条真库单测 |
| `backend/src/business/shared/doc_kind.rs` | 改 | `DocKind::Shipment`（ALL 19→20 / `as_str` / `label` / `table` / `is_document` / `route`）+ `DOC_KIND_VALUES` 20 |
| `backend/src/business/shared/state_machine.rs` | 改 | `ENTITY_SHIPMENT`、`SHIPMENT_EDGES`(4)、`SHIPMENT_BASE`(4)、`SHIPMENT_REASON_EDGES`(1)、`SHIPMENT_LOCKED`(2)、`Entity::Shipment`（ALL 15→16 + 5 处 match），并把 `contract_registered_in_all_seven_places` 的注册表计数断言 15→16 |
| `backend/src/business/erp/mod.rs` | 改 | `pub mod shipment_handler;` |
| `frontend/src/api/business/erp/doc-kinds.ts` | 改 | `SHIPMENT` 进 `DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES`（与 Rust 逐字一致，后端单测会比对） |
| `backend/src/business/flow/linkage/spec.rs` | 改 | `TRIGGER_EXEMPT` 增加 `ENTITY_SHIPMENT`（+ 理由）：注册状态机实体的连带要求 —— 每个实体必须是联动触发点或显式豁免；发货单本卡不调联动引擎，SO-10 接上后再移到 `TRIGGER_SPECS`（与 SO-05 处理 `SALE_ORDER` 同口径） |
| `docs/architecture/snapshot-drift/10_checks.sql` | 改 | `STATUS_ENTITY / SHIPMENT / biz_shipment` 登记行（巡检 DI 口径） |
| `backend/migrations/business/V0284__sales_shipment_permissions.{up,down}.sql` | **新增** | 3 个权限码 + 页面节点（`…2841`，`path` 留 NULL 等 SO-11）+ 2 个按钮（`…2842/2843`）；末尾 DO 自检；down 带断言 |

迁移号：**V0284**（V0283 是 SO-08 的发货单表；`schema_migrations.version=284 / kind=versioned / applied_via=executed`）。

## 3. 门禁结果

| 门禁 | 结果 | 证据 |
|------|------|------|
| `cargo test --lib`（发货单模块，真库，**两轮**） | 两轮各 **21 passed / 0 failed** | `unit-round-1.txt` / `unit-round-2.txt` |
| `cargo test --lib`（shared：doc_kind + state_machine，真库） | **134 passed / 0 failed** | `unit-shared-round-1.txt` |
| `cargo test --lib`（全量，干净库 `cloudpivot_so09b`，`--test-threads=1`，**两轮**） | 首轮 **2758/3**（含本卡引入 1 条）→ 修复后**二轮 2759 passed / 2 failed**（1392s），剩余 2 条为既有缺陷（SO-06 线：`so_approve` graph_hash 漂移、审批夹具顺序污染） | `full-suite.txt` + `full-suite-round-2.txt` + `full-suite-attribution.md` + `after-fix-subset.txt` |
| `cargo fmt`（rustfmt +1.94.1 / edition 2024，改动文件全部 --check） | **rc=0** | `fmt.txt` |
| `cargo clippy --all-targets -D warnings` | **0 warning** | `clippy.txt` |
| 真机 curl（真实 token，**两轮**） | 两轮各 **25 条断言全通过** | `curl-round-1.txt` / `curl-round-2.txt` / `curl-e2e.sh` |
| 迁移应用 + 闸⑥ + 回滚演练 | APPLY_OK `284/versioned/executed`；`perm_menu_gate` **PASS**；down→0 行→up→3 行 | `migration-apply.txt` / `rollback-drill.txt` |
| 前端 `pnpm typecheck` | **rc=0（0 个类型错误）** | `frontend-typecheck.txt` |
| 前端 `pnpm build` | **rc=0，✓ built in 39.78s** | `frontend-build.txt` |

### curl 实测覆盖（e2e 脚本 ①–⑯）

登录（含首登改密）→ 夹具订单（2 行 10/5）→ 订单置 APPROVED（夹具）→
**草稿订单建单 409** → `shippable` 带出 2 行 → **建单（items 留空自动带出，total_qty=15，SH-号）** →
详情 / 分页 / 域外状态 400 → **超发 400 / 重复行 400 / qty=0 400 / 行不存在 400 / 仓库不存在 400 / UUID 非法 400** →
跳步迁移 409 / 域外目标 400 → 提交待过账 200 → **库存 0 过账 409** → 补库位账 →
**过账 200（POSTED）** → **终态再流转 409** → 草稿作废 200 / 待过账作废缺 reason 400 / 带 reason 200 →
库层核对（8 条 `biz_status_log`、3 张单状态、`stock_out_id` 仍为 NULL = SO-10 待回填）。

## 4. 跑法（复现）

```bash
# 隔离实例（库 cloudpivot_so09 / 端口 18099）
bash ~/.hermes/profiles/backend-developer/cache/scratch/so09_instance.sh start

# 真机 curl（两轮都跑同一个脚本，脚本自带幂等的库存夹具清理）
bash docs/evidence/20261106-so09-shipment-api/curl-e2e.sh | tee curl-round-1.txt
bash docs/evidence/20261106-so09-shipment-api/curl-e2e.sh | tee curl-round-2.txt

# 真库单测
cd backend && ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so09' \
  cargo test --lib --features test-hooks -- business::erp::shipment --test-threads=1

# 干净库全量
ERP_DB=cloudpivot_so09b PORT=18097 RECREATE=1 bash scripts/verify/erp_clean_db.sh
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so09b' \
  cargo test --lib --features test-hooks -- --test-threads=1
```

## 5. 交接给 SO-10 / SO-11

* **SO-10（过账落库存）**：在 `shipment_handler::change_shipment_status` 的
  `state_machine::transition` 之后、`tx.commit()` 之前插入 —— 那时单据状态 / 明细 / 仓库都已锁好；
  库存写入必须走 `inventory_writer`（禁直写三本账）；同时回填 `biz_shipment.stock_out_id`、
  `biz_sales_order_item.delivered_qty += qty`、订单 `PARTIAL_OUT|OUTBOUND`。
  过账前置的库存校验已经在这里做了（`transition_to_posted`），SO-10 的扣减失败仍须整单回滚。
* **SO-11（前端页）**：页面节点 `20000000-…2841` 的 `path/component/is_visible` 由前端卡一条 UPDATE 补齐
  （`/business/erp/shipment` + `business/erp/shipment/index`），否则 `system::menu::tests::n08` 会红。
  列表读数用 `GET /page`；「从订单带出」用 `GET /shippable/{so_id}`（`remaining` 即默认发货量）。
