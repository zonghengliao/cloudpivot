# 闸② 裁定证据 · business/erp/reminder.rs — sys_user

- **裁定**：exempt（函数级）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `reminder.rs` `users_of_roles()`：`SELECT DISTINCT u.id FROM sys_user u JOIN sys_user_role … JOIN sys_role r … WHERE r.role_code IN (…)`（:513-533）

## 2. 调用链（查询函数 → 调用方）

- 调度器：`reminder.rs:795 register_builtin()` → `scheduler.register_cron(kind.task_name(), …)` → `tokio::spawn(run_kind(kind, Utc::now()))`（:799-803）
- 业务侧调用：`reminder_purchase.rs:106`、`reminder_payable.rs:91`、`reminder_contract.rs:116`、`reminder_stock.rs:128`

## 3. 权限校验点（file:line）

**后台任务无操作人**：调用链起点是 cron 调度，`tokio::spawn` 里没有 claims / principal，谈不上「操作人的数据范围」。收件人的授权口径是**角色定义**（`sys_role.role_code` 白名单 + 用户/角色 `status=1` + 未软删）。

## 4. 为什么不注入（注入了会怎样错）

越权风险方向相反：提醒要发给「该干活的那批人」。注入某个操作人的部门范围，会让同角色里其他部门的财务/采购/仓管**静默收不到提醒**（不报错、不告警），这比多收一条提醒危险得多。`select distinct u.id` 只取 id 集合用于 `NoticeTarget` 扇出与台账 `receiver_count`，不向任何界面展示用户行。

## 5. 补充

参照 `business/flow/timeout.rs` 的后台扫描豁免口径。
