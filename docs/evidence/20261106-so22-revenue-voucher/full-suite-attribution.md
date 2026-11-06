# 全量单测失败归因（SO-22）

## 结论

本卡（`voucher::create_for_sales_revenue` + 发票接线 + 应收冲销红冲）**没有引入任何新的单测失败**。
两轮全量的失败项逐条相同，且都是**本卡落地之前就存在**的：

| # | 用例 | 现象 | 归因 | 反证 |
|---|------|------|------|------|
| ① | `sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | 第 3 档末跳 `approve_task` 返回实例状态 `""`（期望 `APPROVED`） | **全量跑的顺序污染**（前面某模块留下的流程实例 / 待办状态），与本卡无关 | 在**同一份二进制 + 全新库** `cloudpivot_so22fresh2` 上**单跑该用例 → ok**（`1 passed`，见 §"单跑反证"）；只有跑完前面上千条用例后它才红 |
| ② | `stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | `so_approve` v1 `RowsEdited`：`stored_hash=da3c2eed…`、`recomputed_hash=97fe9377…` | **既有缺陷**（V0281 / SO-06 的 `so_approve` 节点行与快照 hash 漂移），在任何库上常红 | ① 单跑同样 FAILED（见下）；② hash 与 `docs/evidence/20261106-so08-shipment/full-suite-attribution.md` 记录的**逐字相同**（da3c2eed / 97fe9377）；③ 该文档已用「不含 V0283 的基线库」证明与本卡族的改动无因果关系 |

本卡**顺带修掉**了一条更早的红（不是本卡引入，但同在全量里红）：

| # | 用例 | 现象 | 处理 |
|---|------|------|------|
| ③ | `flow::linkage::tests::triggers_cover_state_machine_entities` | `RECEIPT`（SO-17）/ `INVOICE`（SO-20）两个状态机实体没登记在 `TRIGGER_EXEMPT` | 已在 `backend/src/business/flow/linkage/spec.rs` 补两条豁免（按 P1-FIX-5C 口径，附理由注释）。修后 `business::flow::linkage::tests` **53 passed / 0 failed**，全量第二轮不再出现该失败 |

## 两轮全量对照（同一二进制，`--test-threads=1`）

| # | 库 | 迁移 | 结果 | 失败项 |
|---|----|------|------|--------|
| round 1 | `cloudpivot_so22fresh`（空库，启动应用 225 个迁移） | 225（215 versioned + 10 seed，全部 `executed`） | **2846 passed / 3 failed**（1324.59s） | ① + ② + ③（`RECEIPT` 联动豁免） |
| round 2 | `cloudpivot_so22fresh2`（同上，空库重迁） | 225 | **2847 passed / 2 failed**（1471.95s） | 仅 ① + ②（既有的两条） |

⇒ 两轮之差只有 ③：本卡修掉它之后，除「既有两条」外全绿；本卡新增的 7 条用例两轮都 ok。

## 单跑反证

```
# 干净库（cloudpivot_so22fresh2，只跑这一条）
$ ERP_DB_URL=…cloudpivot_so22fresh2 cargo test --lib -- --test-threads=1 --exact \
    business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved
test …three_tiers_start_flow_and_approve_to_approved ... ok
test result: ok. 1 passed; 0 failed

# 同一条在「跑过上千条用例的库」上单跑 → 复现失败（证明是库/顺序状态，不是代码）
$ ERP_DB_URL=…cloudpivot_so22fresh（round 1 之后的残留库）…
test …three_tiers_start_flow_and_approve_to_approved ... FAILED  （left: "" right: "APPROVED"）

# ② p16：任何库都红，hash 与 SO-08 归因文档逐字相同
$ ERP_DB_URL=…cloudpivot_so22fresh cargo test --lib -- --test-threads=1 --exact \
    business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean
… Drift { flow_code: "so_approve", version_no: 1, kind: RowsEdited,
          stored_hash: "da3c2eed…", recomputed_hash: "97fe9377…" }   ← 与 SO-08 记录相同
```

## 复跑命令

```bash
su - postgres -c "dropdb --if-exists cloudpivot_so22fresh; createdb -O alun cloudpivot_so22fresh"
cd /opt/cloudpivot && DB=cloudpivot_so22fresh PORT=18125 \
  bash docs/evidence/20261106-so22-revenue-voucher/start-instance.sh   # 起一次把 225 个迁移应用完
# 停掉实例后：
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so22fresh' \
  cargo test --lib -- --test-threads=1
```

## 留给后续卡

- ② `so_approve` 的 `graph_hash` 重拍只能在**新迁移**里做（已应用迁移不可改），落点属 SO-06；`p16` 闸在修好之前会一直红，凡跑全量的卡都要按本文件的归因口径放行。
- ① 顺序污染建议由测试基建卡统一处理（例如每个 DB 用例模块在自己的事务里建夹具并在结束时回滚流程实例），否则每张卡都要重复一次归因。
