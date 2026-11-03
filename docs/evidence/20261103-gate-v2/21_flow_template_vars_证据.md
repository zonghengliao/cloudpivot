# 闸② 裁定证据 · business/flow/template_vars.rs — biz_flow_instance_node

- **裁定**：exempt（函数级）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `template_vars.rs` `prev_approver()`：`SELECT u.real_name, u.username, u.id FROM biz_flow_instance_node n JOIN biz_flow_task t … JOIN sys_user u … WHERE n.instance_id = ?::uuid AND n.round = ? … LIMIT 1`（:226-243）

## 2. 调用链（查询函数 → 调用方）

- `prev_approver()` ← `template_vars.rs:166`（渲染流程变量 `prev_approver` / `prev_approver_id`）← 流程节点渲染链（引擎 / 预演，实例 id 由持有者确定）

## 3. 权限校验点（file:line）

实例 id + round 由调用方确定，`LIMIT 1` 取「本实例内上一审批人」一个字段；不产出行集，也没有「跨实例浏览」的可能。

## 4. 为什么不注入（注入了会怎样错）

注入范围会改变模板变量的语义：审批链上一位审批人可能不在当前操作人的数据范围内（跨部门审批是常态），过滤后变量变空 → 流程表达式 / 抄送 / 通知模板静默丢值。
