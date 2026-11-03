# 闸② 裁定证据 · business/task/task_model_schedule.rs — biz_task / biz_task_dependency（5 处）

- **裁定**：exempt × 5（输入是 task_id / 依赖边，事务内按 id 取数；调用方权限点已点名）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `:25` `find_schedule_row()` — `SELECT … FROM biz_task WHERE id = ?::uuid AND deleted_at IS NULL`
- `:49` `schedule_prerequisites()` — `FROM biz_task_dependency d JOIN biz_task p …`（FS 前置）
- `:69` `schedule_successors()` — `FROM biz_task_dependency d JOIN biz_task p …`（后继）
- `:89` `schedule_parent()` — `FROM biz_task p WHERE p.id = ?`
- `:110` `schedule_children()` — `FROM biz_task p WHERE p.parent_id = ?`

## 2. 调用链（查询函数 → 调用方）

- 唯一调用方 `business/task/schedule.rs` 的 `schedule()` 事务：`:545` find_schedule_row → `:566` prerequisites → `:567` successors → `:569` parent → `:572` children

## 3. 权限校验点（file:line）

handler.rs（`business/task/schedule.rs`）：中间件 `business:task:update`（:503 注解）；**`:517` `model::find_detail(&mine, task_id)`** 可见性（scope ∪ 参与关系，看不见 404）；**`:522` 写权限**「负责人 / 创建人 / 超管」否则 403。

## 4. 为什么不注入（注入了会怎样错）

五个查询都是「事务内按 task_id / parent_id 取排期快照与依赖边」，输出只喂给改期校验（前置/后继/父子区间），不返回给前端展示。注入 scope 会把「看不见的前置/后继」过滤掉 → 改期校验**静默漏判**（能拖出违反依赖的排期，且不报错），这比多看一眼数据危险得多。模块文档（schedule.rs:52-57）已把这条口径写死。
