# LNK-04 盘点单（STOCK_TAKE）接入联动引擎 —— 取证包

> 卡面：把 `STOCK_TAKE` 接进联动引擎（照抄 `STOCK_IN` 的模式），并排除会造成重复落账的动作。
> 结论：**已真正接线**（过账 + 作废两条状态变更路），`spec::TRIGGER_SPECS` 已接线 6/15 → **7/15**。

## 1. 接线位置（谁在调引擎）

| 触发点 | 调用点 | 事务内动作 | 提交后动作 |
|--------|--------|-----------|-----------|
| `STOCK_TAKE/POSTED` | `erp/stocktake_handler.rs::post_tx` 第 ⑥ 步（过账：三本账 + 差异凭证之后） | `stocktake_linkage::run_in_tx` → `linkage::engine::run_excluding(spec::STOCKTAKE_EXCLUDED_ACTIONS)` | `post_stock_take` commit 后 `run_deferred_after_commit` → `linkage::engine::run_deferred` |
| `STOCK_TAKE/CANCELLED` | `erp/stocktake_handler.rs::cancel_tx` 第 ③ 步（作废：状态 CAS 之后，覆盖 DRAFT / PENDING_POST / POSTED 三条来源态） | 同上 | `cancel_stock_take` commit 后 `run_deferred_after_commit` |

- 唯一 hook：`erp/stocktake_linkage.rs`（上下文 / 文案 / 幂等键 / 排除清单只在这里写一份）；
  两个入口共用同一个 `run_in_tx`，两处各写一份迟早漂移。
- 幂等键：`LNK:STOCK_TAKE:<状态>:<盘点单 id>`；重复过账被状态闸 409 挡住，不会第二次进 `post_tx`。

## 2. 排除清单（为什么必须排除）

`spec::STOCKTAKE_EXCLUDED_ACTIONS = [WRITE_INVENTORY, WRITE_INVENTORY_TXN, GENERATE_VOUCHER]`

| 动作码 | 盘点侧的专用落账点 | 通用引擎再跑一次的后果 |
|--------|--------------------|------------------------|
| `WRITE_INVENTORY` | `post_tx` ③ `apply_gain` / `apply_loss`（成本账） | 库存被加/减两次，账面数量 ≠ 实盘数 |
| `WRITE_INVENTORY_TXN` | 同上（`biz_inventory_txn` 流水） | 一张单四条进销存流水 |
| `GENERATE_VOUCHER` | `post_tx` ⑤ `create_voucher`（借 1405 / 贷 1901 成对） | 一张盘点单两张差异凭证，总账翻倍 |

`SEND_NOTICE` / `CREATE_TASK` **不在**排除清单里：通知与待办就靠通用引擎的 `AfterCommit` 动作。

## 3. 代码改动

| 文件 | 改动 |
|------|------|
| `backend/src/business/erp/stocktake_linkage.rs` | **新增**（thin hook：`StockTakeStatusChange` + `run_in_tx` + `run_deferred_after_commit`） |
| `backend/src/business/erp/stocktake_handler.rs` | `post_tx` / `cancel_tx` 返回 `StockTakeStatusChange`；`post_tx` ⑥ / `cancel_tx` ③ 调 hook；`post_stock_take` / `cancel_stock_take` commit 后 flush |
| `backend/src/business/erp/stocktake_tests.rs` | 5 条真库用例（过账触发 / 已过账作废触发 / 草稿作废触发 / 重复过账不重复触发 / 排除动作不重复落账）；`purge_fixtures` 补联动夹具清理 |
| `backend/src/business/flow/linkage/spec.rs` | `STOCKTAKE_EXCLUDED_ACTIONS`；`STOCK_TAKE` wired=true + wiring + 2 条 proofs；表头 7/15 |
| `backend/src/business/flow/linkage/tests_wiring.rs` | 已接线清单 + 调用点 7→8 文件 + 2 条守卫（接线细节 / 排除清单语义） |
| `backend/src/business/flow/linkage/admin.rs` | 配置页字典的已接线清单同步 |
| `scripts/verify/lnk04_start.sh` / `lnk04_curl_evidence.py` / `lnk04_cleanup.sh` | 真机取证入口 / 取证脚本 / 夹具硬清 |

## 4. 验证结果（真实执行）

### 4.1 单测

```text
cargo test --lib                                        → 3175 passed / 0 failed（基线 3168，本卡 +7）
cargo test --lib business::flow::linkage                → 73 passed / 0 failed（基线 71，+2 守卫）
ERP_DB_URL=…cloudpivot_lnk03 cargo test --lib business::erp::stocktake -- --test-threads=1
                                                        → 24 passed / 0 failed（含本卡 5 条真库用例）
rustfmt +1.94.1 --edition 2024 --check（本卡 6 文件）    → 无 diff
cargo clippy --lib --tests（本卡文件）                   → 0 警告（余 2 条他人文件既有）
```

**红证据（证明用例有牙）**：把 `post_tx` 第 ⑥ 步的状态参数从 `STATUS_POSTED` 改成 `"DRAFT"`
（模拟「接线了但触发点写错」）→ `lnk04_post_triggers_linkage_notice` 立刻红：
`left: 0, right: 1`（通知一条都没发出来）。改回即绿。

### 4.2 真机（生产同款 HTTP）

```text
bash scripts/verify/lnk04_start.sh            # 隔离实例 127.0.0.1:18051，DB=cloudpivot_so44
python3 scripts/verify/lnk04_curl_evidence.py # 结果 ALL PASS，完整输出见 curl_evidence.txt
```

| 步骤 | 观测 |
|------|------|
| 配置页 `GET /api/business/flow/linkage?trigger_entity=STOCK_TAKE` | `wired: true`，wiring 指向 `post_tx ⑥ / cancel_tx ③` |
| `PUT /api/business/flow/linkage` 两条 `SEND_NOTICE`（POSTED / CANCELLED） | `problems: []`（未接线触发点会被 400，这里过） |
| 建单 + 填实盘 92（账面 100，盘亏 8）+ 提交（显式跳过审批）+ 过账 | `POSTED`，凭证 `PZ-20261107-000538`，盘亏 8 / 金额 64 |
| 过账后库内 | 成本账 `92.0000 / 736.0000`、库位账 `92.0000`、盘点流水净额 `-8.0000`、差异凭证 **1** 张、联动通知「盘点单已过账」 |
| 已过账作废 | `CANCELLED`，红字凭证 `PZR-20261107-000105` |
| 作废后库内 | 成本账 `100.0000 / 800.0000`、库位账 `100.0000`、流水净额 `0.0000`、红字凭证 1 张、联动通知「盘点单已作废」 |

**三账一致**：盘点后库存 = 实盘（92），作废后回到账面（100）；每张盘点单差异凭证恰好一张
（排除清单生效，联动没有再造一张）。取证夹具（仓 / 物料 / 库存 / 盘点单 / 凭证 / 流水 / 规则 / 通知）
全部自建自清；`stock_take_post` 流程取证期间临时置 DRAFT、结束恢复 PUBLISHED；实例已停、18051 已释放。

复跑前如遇规则唯一键冲突：`bash scripts/verify/lnk04_cleanup.sh`（配置页 DELETE 是软删，
`uk_flow_linkage_rule` 不含 `WHERE deleted_at IS NULL`，软删规则仍占坑位）。
