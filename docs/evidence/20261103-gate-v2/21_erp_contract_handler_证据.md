# 闸② 裁定证据 · business/erp/contract_handler.rs — biz_flow_instance

- **裁定**：exempt（函数级）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `contract_handler.rs` `fill_approval_no()`：`SELECT id, instance_no FROM biz_flow_instance WHERE id = ANY(?::text::uuid[])`（:232-248）

## 2. 调用链（查询函数 → 调用方）

- `contract_page()`（:285）→ `fill_approval_no()`（:240）；`detail_of()`（:216）→ `fill_approval_no()`

## 3. 权限校验点（file:line）

① 行集权限：`contract_model::contract_page()` 的 SQL 由 `scope::paginate_unscoped` 统一接管（contract_model.rs:496；`CONTRACT_TARGET` 在 :253 声明为 unscoped —— 合同表无归属列，口径见该模块头）。② 中间件：`erp:contract:list`（contract_handler.rs:283 注解）。

## 4. 为什么不注入（注入了会怎样错）

本函数只做**展示字段补全**：输入是上一步已经过 scope 授权的行集合，按这批行的 `approval_instance_id` 数组回查单号，输出写回同一批行（`for row in rows.iter_mut()`）。它不产生行集、也不决定谁能看到哪一行——注入 scope 反而会把「用户有权看的合同」的审批单号抹成空（`biz_flow_instance` 无 dept 列，注入等于 `deny_all`），属于功能回归而不是收紧权限。

## 5. 补充

批量一次查（`= ANY(...)`）而不是逐行查，是列表 20 行避免 20 次往返的既有实现，未改。
