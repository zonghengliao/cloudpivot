# 闸② 裁定证据 · business/task/task_model_write.rs — biz_task / biz_task_dependency / biz_task_member（3 处）

- **裁定**：exempt × 3（取号必须跨全表；两个删除按 id 的写路径）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `:102` `next_task_no()` — `SELECT COALESCE(max(substring(task_no from 12)::bigint),0)+1 FROM biz_task WHERE task_no LIKE ?`
- `:265` `purge_dependencies()` — `DELETE FROM biz_task_dependency WHERE task_id = ? OR depends_on_id = ?`
- `:520` `remove_member()` — `DELETE FROM biz_task_member WHERE task_id = ? AND user_id = ?`

## 2. 调用链（查询函数 → 调用方）

- `next_task_no()` ← `create_task()`（task_model_write.rs:41）← handler.rs:314 create（中间件 `business:task:create`，handler.rs:311）/ `flow/task_node.rs:221`（流程引擎事务，无操作人）
- `purge_dependencies()` ← `soft_delete()`（task_model_write.rs:246）← handler.rs:716 delete（`:735 require_task` 取行 + `:737` 负责人/创建人/超管 403；中间件 `business:task:delete` handler.rs:713）
- `remove_member()` ← handler.rs:4394（`:4375 require_task` 取当前行 + `:4382 may_manage_members(role, has_permission(PERM_MEMBER))`；中间件 `business:task:read` handler.rs:4365）

## 3. 权限校验点（file:line）

见上：create / delete / remove_member 各有中间件闸 + handler 内行级判定；`next_task_no` 的流程引擎调用点无操作人（引擎内部事务）。

## 4. 为什么不注入（注入了会怎样错）

① 取号必须**全表** `MAX+1`：表上有唯一索引 `uk_biz_task_task_no`，注入部门范围会让不同部门在同一月份算出同一个号 → 撞唯一索引 / 串号，这是取号语义本身的硬要求（`pg_advisory_xact_lock` 也只是按月份串行化，不按部门）。② 两个 DELETE 是「按 (id) / (task_id,user_id) 的写路径」，只返回受影响行数，不产出行集；注入 scope 会让「删任务时清依赖」「移除协作人」在跨部门关系上漏删，留下孤儿依赖行。
