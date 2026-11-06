# 全量单测归因（同一二进制、串行 `--test-threads=1`、三个库对照）

## 结论

本卡（V0283 + `shipment_model`）**没有引入任何单测失败**。全量跑里的 2 条 FAILED 在本卡出现之前就存在：

| 用例 | 现象 | 归因 | 反证 |
|------|------|------|------|
| `stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | `so_approve` v1 `RowsEdited`（stored_hash ≠ recomputed_hash） | **既有缺陷**：V0281（SO-06）种 `so_approve` 后节点行与快照 hash 漂移 | 在**不含 V0283** 的库 `cloudpivot_so08_base`（max=282）上单跑同样 FAILED，漂移 hash 逐字相同 |
| `sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | 第 3 档末跳 `approve_task` 返回实例状态 `""` | **全量跑的顺序污染**（前面某个模块留下的流程实例/待办状态），与本卡无关 | ① 不含 V0283 的基线库 `cloudpivot_so08_base2` 跑**全量**时，同一行（第 863 条）同样 FAILED；② 两个库上**单跑该用例/该模块**都 ok（9 passed） |

## 三次全量跑对照

| # | 库 | 迁移 | 结果 |
|---|----|------|------|
| ① | `cloudpivot_so08` | max=283（含 V0283） | `FAILED. 2749 passed; 2 failed` = three_tiers + p16 |
| ② | `cloudpivot_so08_fresh` | max=283（含 V0283），`--skip p16_…` | `FAILED. 2749 passed; 1 failed; 1 filtered`（仅 three_tiers；filtered = 被 skip 的 p16） |
| ③ | `cloudpivot_so08_base2` | max=282（**不含** V0283） | `FAILED. 2744 passed; 7 failed` = three_tiers + p16 + **5 条 `shipment_model` 真库用例**（预期：该库没有 `biz_shipment`/`biz_shipment_item`，正好反证这 5 条用例真的在打表而不是空转） |

② 的失败与 ③ 的失败**逐条相同**（three_tiers + p16）⇒ 与本卡改动无因果关系；
把 V0283 加回去（②）后，5 条真库用例全部转绿、p16 之外没有新增失败。

## 复现命令

```bash
# 含 V0283 的干净库
ERP_DB=cloudpivot_so08_fresh RECREATE=1 PORT=18112 bash scripts/verify/erp_clean_db.sh
# 不含 V0283 的基线库：把 migrations 复制一份、摘掉 V0283 再建
REPO=/root/so08-base BIN=/opt/cloudpivot/backend/target/debug/cloudpivot \
  ERP_DB=cloudpivot_so08_base2 RECREATE=1 PORT=18113 bash scripts/verify/erp_clean_db.sh

ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<库> \
  cargo test --lib --features test-hooks -- --test-threads=1
```

## base（不含 V0283，max=282）单跑关键用例

```
$ … --exact business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean \
        business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved
test …three_tiers_start_flow_and_approve_to_approved ... ok
test …p16_graph_snapshot_selfcheck_is_clean ... FAILED
  … Drift { flow_code: "so_approve", version_no: 1, kind: RowsEdited,
           stored_hash: "da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2",
           recomputed_hash: "97fe937752c881c0c9b36f7aa8e2a410c2891a4327711fde96fcbf16a0bc6e80" }
```

## 含 V0283 的全新库（max=283）单跑

```
test …three_tiers_start_flow_and_approve_to_approved ... ok
test …over_ship_guard_blocks_more_than_remaining ... ok
test result: ok. 2 passed; 0 failed
```

## 遗留（不属本卡范围，建议另开卡）

`so_approve` 流程（V0281 / SO-06）的节点行与 `graph_hash` 快照对不上 ⇒
`p16_graph_snapshot_selfcheck_is_clean`（闸⑦「审批图冻结」）在任何库上常红。
修法只能新开迁移重算 hash / 重拍快照（已应用迁移不可改），落点属 SO-06 领地。
`three_tiers_start_flow_and_approve_to_approved` 的全量顺序污染同行报出，建议 SO-06/SO-09 一并查清
（它在单模块 / 单用例下是绿的，说明是**跨模块**残留：前面某模块留下了未清理的流程实例或待办）。
