# 闸② 裁定证据 · business/task/task_model_flow.rs — biz_task / biz_flow_task

- **裁定**：exempt（函数级，先查了调用链——handler 已做单条可见性校验）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `task_model_flow.rs` `flow_ref_of_task()`：`FROM biz_task t JOIN biz_flow_instance i … LEFT JOIN biz_flow_task x … WHERE t.id = ?::uuid`（:122-127）

## 2. 调用链（查询函数 → 调用方）

- `handler::flow_of_task()`（handler.rs:4510）→ `model::flow_ref_of_task(task_id)`（handler.rs:4521）

## 3. 权限校验点（file:line）

**handler.rs:4515** `principal::resolve_for_claims(&claims)` → **handler.rs:4517** `model::find_detail(&me, task_id)`（内核 `task_model_query.rs:222` `scope::filter_for(principal, TASK_TARGET)` + 参与关系 `VISIBLE_OR_TAIL`）→ 看不见即 **handler.rs:4520** `.ok_or_else(not_found)?` 回 404。中间件另有 `business:task:read`（handler.rs:4507）。

## 4. 为什么不注入（注入了会怎样错）

查询本身是「按 task_id 单条取这张任务的流程块」（`WHERE t.id = ?::uuid`），不产生行集；可见性已由 handler 的单条校验（scope ∪ 参与关系）前置完成，函数内再注入一次 scope 会与那条「唯一实现」形成两个真相源，且会把「看得见任务、但流程实例不在本人范围内」的合法情况判成空流程块（功能性回归）。

## 5. 补充

卡面第 7 项要求的核实结论：**已做**，故裁定豁免并点名 handler.rs:4515/4517/4520。
