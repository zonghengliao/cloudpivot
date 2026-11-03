# 闸② 裁定证据 · business/task/task_model_hierarchy.rs — biz_task（10 处 SQL / 8 个函数）

- **裁定**：exempt × 8（逐条判：全部为写路径或按 id 单值内部读；唯一的「清单」是超管运维诊断，不是业务行集）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `:34` `ancestor_chain_sql()` — 递归 CTE 文本（供 validate_parent）
- `:66` `validate_parent()` — `SELECT id, deleted_at FROM biz_task WHERE id = ?`
- `:244` `scalar_status()` — `SELECT status FROM biz_task WHERE id = ?`
- `:419` `scan_parent_status_gaps()` — 父任务状态不一致巡检（`FROM biz_task p JOIN biz_task c …` + `HAVING`）
- `:489` `parent_status()` — `SELECT p.status FROM biz_task t JOIN biz_task p … WHERE t.id = ?`
- `:516/:518/:521` `cascade_cancel_sql()` — `WITH RECURSIVE … UPDATE … RETURNING`（写）
- `:601` `parent_of()` — `SELECT parent_id FROM biz_task WHERE id = ?`
- `:615` `scalar_progress()` — `SELECT progress FROM biz_task WHERE id = ?`

## 2. 调用链（查询函数 → 调用方）

- `validate_parent()` ← handler.rs:365（create）/ :639（update）/ :1628（subtask）
- `scalar_status()` / `parent_status()` / `parent_of()` / `scalar_progress()` ← `cascade_parent_status()` / `recalc_ancestors()` ← handler.rs:1342（change_status）/ :1558（change_progress）/ :1692（create_subtask）/ :1034、:1205（parent_status 守卫）
- `cascade_cancel_descendants()`（→ `cascade_cancel_sql()`）← handler.rs:1252（change_status 事务内）
- `scan_parent_status_gaps()` ← handler.rs:4703（repair_parent_status）

## 3. 权限校验点（file:line）

① 写路径：handler.rs:1005 / :1509 `model::find_detail(&mine, task_id)` 可见性（scope ∪ 参与关系）→ 角色/状态机判定 handler.rs:1047 `state::check_transition`；中间件 `business:task:status`（handler.rs:992）/ `business:task:progress`（:1498）。② create/update：中间件 `business:task:create`（handler.rs:311）/ `business:task:update`（:577）。③ 巡检：handler.rs:4686 中间件 `business:task:update` + **handler.rs:4693 `if !claims.is_super_admin()` → 403 + 63002**。

## 4. 为什么不注入（注入了会怎样错）

除巡检外全部是「按 id 取单值」的内部读或纯写路径（`UPDATE … RETURNING` 的返回是给 handler 写变更日志用的，不展示给用户）：注入 scope 会让级联 / 重算在跨部门父子链上断掉——子任务取消时父任务状态不跟着变、进度不汇总、`DONE` 之后父任务永远停在 `DOING`，属于数据一致性回归。巡检清单是**运维诊断**（父任务 id + 样本子任务），只有超管能读，而超管的 scope 恒为「全部」（`principal::resolve` 对超管返回 All），注入片段是 no-op；接口文档亦明确「跨全库批量修数，不按单个任务的数据范围做闸」。

## 5. 补充

卡面第 8 项要求「若某条产出用户可见行集（例如树/列表）→ 接 scope」：逐条核对后**本文件没有**业务树/列表查询（树与列表在 `task_model_query.rs`，早已 `scope::filter_for` / `paginate`），故 10 处全部豁免。
