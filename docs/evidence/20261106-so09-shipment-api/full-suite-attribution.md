# 全量 `cargo test --lib` 归因（SO-09）

## 跑法与口径

```bash
ERP_DB=cloudpivot_so09b PORT=18097 RECREATE=1 bash scripts/verify/erp_clean_db.sh   # 干净库（一卡一库）
cd backend && ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so09b' \
  cargo test --lib --features test-hooks -- --test-threads=1
```

原始输出：`full-suite.txt`（`test result: FAILED. 2758 passed; 3 failed; … finished in 1342.02s`）。

## 3 条失败逐条归因

| # | 测试 | 归属 | 依据 |
|---|------|------|------|
| 1 | `business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | **既有缺陷（非本卡）** | 失败文案点名 `flow_code: "so_approve"`、`version_id: …0d0282`、`kind: RowsEdited` —— V0281（SO-06 销售订单审批流）的**流程节点行与其 `graph_hash` 快照漂移**。SO-08 交接里已登记为「在任何库上常红，需新开迁移重算 hash」；本卡没碰 V0281、也没新增任何流程 |
| 2 | `business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | **既有缺陷（非本卡）** | 断言 `left: "" / right: "APPROVED"`（审批通过后订单状态没被回调写成 APPROVED）。SO-08 交接：**全量跑顺序下失败、单模块 9 passed**，疑前序模块残留流程实例 / 待办；本卡没动审批桥、没动流程引擎 |
| 3 | `business::flow::linkage::tests::triggers_cover_state_machine_entities` | **本卡引入，已修复** | `状态机实体 SHIPMENT 既不在联动触发点白名单里、也没登记在 TRIGGER_EXEMPT` —— 注册 `Entity::Shipment` 的必然连带（每个状态机实体都必须是联动触发点或显式豁免）。修复：`flow/linkage/spec.rs::TRIGGER_EXEMPT` 增加 `ENTITY_SHIPMENT` 并写明理由（发货单本卡不调联动引擎，SO-10 过账卡接上后再移到 `TRIGGER_SPECS`），与 SO-05 给 `SALE_ORDER` 的处理完全同口径 |

## 修复后的复跑（`after-fix-subset.txt`）

```bash
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so09' \
  cargo test --lib --features test-hooks -- \
  business::shared business::erp::shipment business::erp::sales_order business::flow::linkage system::menu \
  --test-threads=1
# => test result: ok. 240 passed; 0 failed (33.46s)
```

覆盖：`DocKind`/`Entity` 注册表与前端镜像（134）、发货单模块 21（**第二轮真库**，见 `unit-round-2.txt`）、
销售订单、联动触发点漂移守卫（65，含 `wired_triggers_match_call_sites`）、菜单路由闸 `system::menu`。

> 说明：**第 1、2 条是既有缺陷，本卡不修**（修它们要新开迁移重算 V0281 的 graph_hash / 清审批夹具残留，
> 属于 SO-06 线的独立缺陷卡）。因此「全量 0 失败」在当前 main 上不可达 —— 本卡的口径是：
> **本卡引入的失败已归零**（第 3 条修复并复跑 240 绿），另 2 条与 SO-08 交接记录的失败**逐条同因同点**。

## 修复后的第二轮全量（`full-suite-round-2.txt`）

同一个干净库 `cloudpivot_so09b`、同一命令跑第二轮：

```
test result: FAILED. 2759 passed; 2 failed; … finished in 1392.53s
failures:
    business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved
    business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean
```

对比首轮（2758/3）：`flow::linkage::tests::triggers_cover_state_machine_entities` **已绿**，
`passed` +1、`failed` −1 —— 本卡引入的失败**归零**，剩下两条就是上表 #1 / #2 的既有缺陷。
（两条既有缺陷的修复建议：新开迁移重算 V0281 `so_approve` 的 graph_hash；审批夹具加清场。属 SO-06 线缺陷卡，不在本卡范围。）
