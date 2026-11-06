# 全量单测归因（本卡提交前后，同一台机、同一二进制、串行 `--test-threads=1`）

## 结论

本卡（V0283 + shipment_model）**没有引入任何单测失败**。全量跑出的 2 条 FAILED 与本卡无关：

| 用例 | 现象 | 归因 | 证据 |
|------|------|------|------|
| `stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | `so_approve` v1 `RowsEdited`（stored_hash≠recomputed_hash） | **既有缺陷**：V0281（SO-06）种流程后节点行与快照 hash 漂移 | 在**不含 V0283** 的基线库 `cloudpivot_so08_base`（max=282）上同样 FAILED，漂移字段逐字相同 → 见 §baseline |
| `sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | 第 3 档末跳 `approve_task` 返回实例状态 `""` | **同库连跑残留**（该库已被前一轮全量跑污染），不是迁移缺陷 | 在同一「含 V0283」的**全新库** `cloudpivot_so08_fresh` 上单跑 **ok** |

## 复现命令

```bash
# ① 含 V0283 的全新干净库（283|211）
ERP_DB=cloudpivot_so08_fresh RECREATE=1 PORT=18112 bash scripts/verify/erp_clean_db.sh
# ② 不含 V0283 的基线库（282|210）：把 migrations 复制一份、摘掉 V0283 再建
REPO=/root/so08-base BIN=/opt/cloudpivot/backend/target/debug/cloudpivot \
  ERP_DB=cloudpivot_so08_base RECREATE=1 PORT=18111 bash scripts/verify/erp_clean_db.sh
```

## baseline（不含 V0283，max=282）

```
$ ERP_DB_URL=…/cloudpivot_so08_base cargo test --lib --features test-hooks -- \
    --test-threads=1 --exact \
    business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean \
    business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved
test …three_tiers_start_flow_and_approve_to_approved ... ok
test …p16_graph_snapshot_selfcheck_is_clean ... FAILED
  … Drift { flow_code: "so_approve", version_no: 1, kind: RowsEdited,
           stored_hash: "da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2",
           recomputed_hash: "97fe937752c881c0c9b36f7aa8e2a410c2891a4327711fde96fcbf16a0bc6e80" }
test result: FAILED. 1 passed; 1 failed
```

## 含 V0283 的全新库（max=283）

```
$ ERP_DB_URL=…/cloudpivot_so08_fresh cargo test --lib --features test-hooks -- \
    --test-threads=1 --exact \
    business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved \
    business::erp::shipment_model::tests::over_ship_guard_blocks_more_than_remaining
test …three_tiers_start_flow_and_approve_to_approved ... ok
test …over_ship_guard_blocks_more_than_remaining ... ok
test result: ok. 2 passed; 0 failed

$ ERP_DB_URL=…/cloudpivot_so08_fresh cargo test --lib --features test-hooks -- \
    --test-threads=1 --exact business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean
test …p16_graph_snapshot_selfcheck_is_clean ... FAILED      # 与 baseline 同一漂移
```

## 遗留（不属本卡范围，需另开卡）

`so_approve` 流程（V0281，SO-06）的节点行与 `graph_hash` 快照对不上 ——
`p16_graph_snapshot_selfcheck_is_clean` 在**任何**库上都红，闸⑦「审批图冻结」处于常红状态。
修法只能是新开迁移重算 `graph_hash` / 重拍快照（已应用迁移不可改），属 SO-06 领地。
