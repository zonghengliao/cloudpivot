# 闸② 裁定证据 · business/erp/stockin_bridge.rs — biz_flow_instance / biz_flow_task

- **裁定**：exempt（函数级，2 个函数各 1 处 SQL）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `stockin_bridge.rs` `reconcile_orphan_cancels()`：`FROM biz_flow_instance i LEFT JOIN biz_stock_in s …`（:265-280，`LIMIT 200`）
- `stockin_bridge.rs` `latest_completed_assignee()`：`SELECT assignee_id FROM biz_flow_task WHERE instance_id = ?::uuid … LIMIT 1`（:314-321）

## 2. 调用链（查询函数 → 调用方）

- ① `stockin_bridge.rs:330 register_builtin()` → `scheduler.register_cron(TASK_NAME, CRON="3-59/10 * * * *", …)` → `tokio::spawn(reconcile_orphan_cancels())`（后台，无操作人）
- ② `latest_completed_assignee()` ← `stockin_bridge.rs:162`（作废审批结论落库）/ `:467`（过账审批结论落库），均在审批引擎回调里执行

## 3. 权限校验点（file:line）

① 后台定时任务：无操作人，权限不适用；任务本身只做「实例 PENDING + 单据已删 / 已不在作废中」的全局一致性回收。② 审批回调：由引擎在审批结论落库时调用（`settle_guard::gate` 已先校验状态机边 `PENDING_CANCEL → CANCELLED|POSTED` / `PENDING_POST → POSTED|DRAFT`，见 :148 / :452），无界面操作人。

## 4. 为什么不注入（注入了会怎样错）

① 注入部门范围会让「别部门单据留下的孤儿实例」永远回收不掉（孤儿待办越积越多，审批中心里出现点不动的死链）。② 只取「最后一位审批人」这一个字段（LIMIT 1），用于写 `cancelled_by`/`posted_by` 与「复核人 ≠ 制单人」内控闸；`biz_flow_task` 无 dept 列，注入等于恒空 → 内控闸永远拿不到审批人，等于把内控关掉。

## 5. 补充

与 `business/flow/task_bridge.rs:579` 的同名函数、`payable_bridge.rs:435` / `voucher_bridge.rs:463` / `stocktake_bridge.rs:380` / `purchase_bridge.rs:403` / `contract_bridge.rs:454` 是同一形状的副本，各自在自己的文件内已过闸③ 口径。
