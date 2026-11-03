# 闸② 裁定证据 · business/flow/task_node.rs — biz_task / sys_user

- **裁定**：exempt（函数级，2 个函数）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `task_node.rs` `find_existing()`：`SELECT id FROM biz_task WHERE flow_instance_id = ?::uuid AND flow_node_key = ? AND flow_round = ? … LIMIT 1`（:563-570）
- `task_node.rs` `owner_dept()`：`SELECT dept_id FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL`（:584-590）

## 2. 调用链（查询函数 → 调用方）

- `find_existing()` ← `task_node.rs:121`（开节点前判重）/ `:256`（返回已存在任务 id）
- `owner_dept()` ← `task_node.rs:213`（写 `biz_task.dept_id` 快照）
- 两者都在流程引擎「审批节点 → 生成任务」的事务里（形参 `ex: &Transaction`）

## 3. 权限校验点（file:line）

**后台/引擎路径无操作人**：任务由流程引擎在审批节点上生成，调用方是 `Transaction` 而非 handler，链路上没有 claims / principal。

## 4. 为什么不注入（注入了会怎样错）

`find_existing` 是幂等前置查询（同一实例 + 同一节点 + 同一轮只应有一行任务），返回 `Option<Uuid>` 单值，不产出行集；注入范围会让「本轮的重复任务」查不出来 → 幂等失效、重复建任务。`owner_dept` 按 user_id 取单列部门快照，用于看板按部门分组，注入范围会把负责人部门写成空 → 任务从部门看板消失。
