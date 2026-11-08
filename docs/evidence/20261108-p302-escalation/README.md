# P3-02 · 审批降级 ②：SKIP 后向上找上级角色（ARCH-8 场景 B）

> 卡面：`t_ea6b0be1`｜上游：`docs/architecture/P3-01-工作流引擎差距分析与拆卡清单.md` §3.2 / ARCH-8 §1、§7.3
> 迁移：**V0333**（`business/V0333__role_escalation_chain`）｜错误码：**65040 / 65041**
> 日期：2026-11-08｜执行机：`ubuntu-svr`（99），隔离实例 + 独立库，二进制 `/root/p302-bin/cloudpivot`

## 一句话结论

`assignee::finalize` 的自审 `SKIP` 把审批人剔光后，引擎不再直接 `422 / 65018` 卡死，而是
**沿 `sys_role.escalation_role_id` 逐级上溯**找第一个有可用成员的角色（每级重跑停用 / ROBOT / 自审过滤），
找到就把待办派给他们并在 `biz_flow_task` 留痕 `escalated_from_role` + `escalate_reason`；
走到顶仍无人时保持 `422 / 65018`，**但报文与结构化诊断里写明「升级链已走完」和每一级的角色名与人数**。

真 HTTP 实测：**57 项断言全绿**（`raw/20_e2e_scenarios.txt`），迁移升级/回滚 **17 项全绿**（`raw/10_upgrade.txt`）。

## 一、改了什么（按卡面「必须实现」1–5）

| # | 卡面要求 | 落地 | 证据 |
|---|---|---|---|
| 1 | 迁移 V0333：`sys_role.escalation_role_id` + `biz_flow_task.escalated_from_role/escalate_reason` + 预置链 + down | `backend/migrations/business/V0333__role_escalation_chain.{up,down}.sql`；`CHECK (escalation_role_id IS NULL OR escalation_role_id <> id)` + 部分索引；预置 12 条 `role_code → 候选上级码` 规则，按 `unnest … WITH ORDINALITY` 取第一个**存在**的上级（客户库里没有「销售总监」时自动落到总经理），源/目标任一不存在就跳过 | `raw/10_upgrade.txt`：V0332 库上「应用 1 个迁移」、重启「应用 0 个迁移」、down 四列/约束/索引全消失、再 up 预置链条数不变（模板库上 7 个角色被配上上级） |
| 2 | `assignee.rs` 第 ⑥ 步升级（两种触发、8 级上限 + visited 防环、每级重过滤、留痕两列、到顶 422 带链路、65040/65041） | `assignee::escalate()` + `Finalized.escalated_from_role / escalate_reason / escalation`；`resolve()` 改成 **过滤 → 升级链 → 空审批人策略** 三步 | `raw/20_e2e_scenarios.txt` 场景 B / 二级上溯 / 到顶无人 / 环 / 超深链 |
| 3 | 清理 `engine.rs` 死代码（决策 D6） | 删掉 2706 无条件 422 之后**永不可达**的「`empty_assignee=SKIP` 自动通过」整段，留注释说明「为什么删除」+ `debug_assert!(!approvers.is_empty())` 不变式自检 | `backend/src/business/flow/engine.rs` 注释块（★ P3-02 / D6） |
| 4 | 前端：时间线提示 + `sys_role` 管理页「上级角色」下拉（保存校验不能指向自己） | `FlowProgress.vue` / `instance/detail.vue` 显示「⚠ 因自审跳过，已升级至「X 角色」审批（原角色 SALES_CLERK）」；`role/index.vue` 新增下拉（排除自己）+ 提交前兜底校验；`role-api.ts` / `flow-api.ts` / `flow-progress-tasks.ts` 补字段与纯函数 | `raw/30_checks.txt`（typecheck exit 0 + 新增 3 条前端单测）；纯函数 `escalationHint` / `escalateReasonLabel` |
| 5 | 单测 ①–④ | `assignee.rs::escalation_tests`（6 条纯逻辑：命中 2 人 / 本级 0 人继续上溯 / 成环 / 自引用 / 到顶链路文案 / 列宽截断）+ `assignee.rs::escalation_db_tests`（3 条**真库**：跳过空级命中 2 人、Exhausted、环 65040；`ERP_DB_URL` 开关，脚本 30 里带真库跑过一次证明不是假绿） + 真 HTTP 里覆盖 ②③④ 的真库版 | `raw/30_checks.txt`、`raw/20_e2e_scenarios.txt` |

## 二、验收对照（卡面 checklist）

| 卡面条目 | 结论 | 证据位置 |
|---|---|---|
| 迁移在 V0331/V0332 之上应用成功（1 个）、重启 0 个、down 可回滚 | ✅ | `raw/10_upgrade.txt` |
| 真 HTTP 场景 B：角色只 1 人且是制单人 → 提交 → 升级到上级角色 → 上级成员收到待办并审批 → 单据生效；DB 断言两列有值、原 SKIPPED 行仍在 | ✅ | `raw/20_e2e_scenarios.txt`「场景 B」段（待办 2 行、escalated_from_role=P302_MAKER、escalate_reason=SKIP_SELF_APPROVAL、SKIPPED 1 行、审批后实例 APPROVED、时间线 API 三行含 `escalated_to_role`） |
| 场景 A（角色 3 人、制单人 1 人 → 其余 2 人审批）不退化 | ✅ | 同文件「场景 A 回归」（2 行待办、无人被升级、制单人 SKIPPED 1 行） |
| 场景 F（FORBID → 65019）不退化 | ✅ | 同文件「场景 F 回归」（422 / 65019、零残留实例） |
| `cargo fmt` + `cargo clippy --lib -D warnings`（只允许既有基线告警）+ 新增单测全绿 | ✅ | `raw/30_checks.txt` |
| `pnpm typecheck` exit 0 | ✅ | `raw/30_checks.txt`、`raw/30_typecheck.log` |
| 证据文档（README + 断言脚本 + 输出） | ✅ | 本文件 + `scripts/` + `raw/` |
| 提交只按路径 `git add` | ✅ | 见 §五 提交清单 |

## 三、设计要点（给后来人）

### 3.1 触发条件（只有两种，别的来源不升级）

```
approvers.is_empty() 且
  ① skipped 非空            → ESCALATE_REASON_SKIP  = SKIP_SELF_APPROVAL（自审剔光，场景 B 主形态）
  ② candidates 为空且有 ROLE 来源 → ESCALATE_REASON_NO_MEMBER = NO_MEMBER（角色 0 成员）
```

`USER` / `DEPT_LEADER` / `INITIATOR_SUPERIOR` / `FORM_VAR` 解析出 0 人**不走升级链**：
链是「角色→角色」，这些来源没有上级角色概念，仍按 `empty_assignee` 处置（行为与改动前一致）。

### 3.2 为什么在 `assignee.rs` 而不是引擎

卡面 D 决策：「找人逻辑只能改 `assignee.rs`（唯一出口）；handler/引擎只消费 `Finalized`」。
因此升级链的**全部查询与判定**都在 `assignee::escalate()`，引擎只读
`Finalized.{approvers, escalated_from_role, escalate_reason, escalation}` 决定落什么行、报什么错。

### 3.3 每级都要重过滤

升级不是「换个角色就完事」：上级角色里也可能只有发起人一个人（很多小公司总经理就是老板）。
`decide_level()` 对每一级跑同一套 `filter_candidates`（停用 / ROBOT / 自审），并且**固定按 SKIP 处理自审** ——
`FORBID` 在过滤阶段就已经整单失败，走不到升级链。

### 3.4 配错要吵（65040 / 65041）

| 情形 | 结果 | 为什么不能静默跳过 |
|---|---|---|
| `escalation_role_id` 指向自己 | API 层 `400 + ROLE_INVALID(30013)`；DB 层 `ck_sys_role_escalation_self` | 自引用等于「升级到自己」，永远找不到人 |
| 链成环（A→B→A） | 写入口 `400 ROLE_INVALID`（沿链上溯 32 跳）；运行期 `422 + 65040` | 静默跳过会让「以为有上级兜底」变成事故 |
| 链上角色已软删 | 运行期 `422 + 65040` | 同上 |
| 链超过 8 级 | `422 + 65041` | 正常组织不会这么深，多半是配反了 |

### 3.5 到顶仍无人：本卡**不兜底**，但要说清楚

按卡面：本卡只做第 ② 层，第 ③ 层兜底人交给 P3-03。所以到顶仍无人时：

* 仍 `422 / 65018`（保持现状）；
* 报文追加「**升级链已走完**：A（原角色，角色无成员） → B（0/0 人） → C（0/1 人）」；
* 结构化诊断 `data.escalation_chain[]` 带每一级的 `role_code / role_name / member_count / usable_count`，
  `data.escalation_reason`、`data.escalated_from_role` 也在（前端 `flow-assignee-guard.ts` 契约向后兼容）。

### 3.6 留痕列只写在「升级派下来的」待办上

`escalated_from_role` / `escalate_reason` **只写 PENDING 行**；`SKIPPED` 留痕行（自审被剔的人）
这两列恒为 NULL —— 他们是「被剔掉的」，不是「升级派来的」。时间线上两者并列出现：
`制单人 SKIPPED`（escalate=NULL）+ `主管甲/乙 PENDING→APPROVED/CANCELLED`（escalate=SKIP_SELF_APPROVAL）。

### 3.7 目标角色名不落库

前端要显示「已升级至「X 角色」审批」，但业务表只留**原角色编码**（卡面只要两列）。
目标角色名由 `TASK_COLUMNS` 里一条相关子查询算出（`sys_role.role_name where id = assignee_source_ref
and escalate_reason is not null`）→ `TaskRes.escalated_to_role`：角色改名后时间线跟着变，
留痕要的是「派给了谁」，不是「当时那个角色叫什么」。

## 四、怎么写复现（脚本）

```bash
cd /opt/cloudpivot/docs/evidence/20261108-p302-escalation
BIN=/root/p302-bin/cloudpivot bash scripts/10_upgrade_migration.sh   # 迁移升级/重启/down/up
BIN=/root/p302-bin/cloudpivot bash scripts/20_e2e_scenarios.sh       # 真 HTTP 场景 B / A / F / 链路
bash scripts/30_checks.sh                                           # fmt / clippy / 单测 / 前端
```

* 端口：18202（迁移）、18203（e2e）；库：`cloudpivot_p302_upgrade`（**克隆** `cloudpivot_prt03`，它停在 V0332）、
  `cloudpivot_p302_e2e`（空库，一次装起全部 266 个迁移）。
* 环境变量从既有隔离实例的 `/proc/<pid>/environ` 抄（JWT / 主密钥 / admin 口令），**仓库里不落任何口令**；
  实例一律 `setsid` 起、按**监听端口反查 pid** 停（`setsid` 会 fork，`$!` 不是真 pid —— 这是本轮踩到的坑）。
* 夹具全部用 API 建（角色 → 授权 → 用户 → 流程 create/saveGraph/publish），
  只有「按 auth_code 直接授 `sys_role_menu`」是 SQL（等价于权限页勾选，注释里写明）。

## 五、提交清单（只按路径 add）

```
backend/migrations/business/V0333__role_escalation_chain.up.sql
backend/migrations/business/V0333__role_escalation_chain.down.sql
backend/src/shared/error_code.rs
backend/src/business/flow/assignee.rs
backend/src/business/flow/engine.rs
backend/src/business/flow/no_assignee.rs
backend/src/business/flow/dto.rs
backend/src/business/flow/handler.rs
backend/src/business/flow/flow_model_base.rs
backend/src/business/flow/flow_model_instance.rs
backend/src/business/flow/settle_guard.rs
backend/src/system/role/dto.rs
backend/src/system/role/handler.rs
backend/src/system/role/model.rs
backend/src/system/role/tests.rs
frontend/src/api/business/flow-api.ts
frontend/src/api/system/role-api.ts
frontend/src/views/business/flow/components/FlowProgress.vue
frontend/src/views/business/flow/components/flow-progress-tasks.ts
frontend/src/views/business/erp/shared/approval-progress.ts
frontend/src/views/business/flow/instance/detail.vue
frontend/src/views/system/role/index.vue
frontend/tests/unit/purchase-approval-progress.test.ts
docs/evidence/20261108-p302-escalation/**
```

## 六、已知既有红（与本卡零交集，均已证明文件未被本卡触碰）

| 位置 | 现象 | 归属 |
|---|---|---|
| `cargo test --lib` → 3 条 `business::flow::linkage::*` | `wired_triggers_match_call_sites` 期望 16 个调用点文件、实际 17（多出 `erp/purchase_return_handler.rs`） | P2-03（commit `9cf6cc1`）新增了调用点，`tests_wiring.rs` 的期望集合没同步 |
| `pnpm test:unit` → `create payload keeps dates and folds empty remark to null` | 多出 `currency_code: null` | P2-02/P2-03 的销售订单字段变更 |
| `pnpm test:unit` → `FlowBizType 15 个` | 实际 16（多了 `RECEIPT_REFUND`） | P2-03 的退货业务类型 |
| `cargo clippy -- -D warnings` | `erp/sales_order_model.rs::update_order` 8 参数（阈值 7） | 既有基线（该文件 `git status` 干净，工具链 1.94 的 clippy 默认更严）；本卡用 `-A clippy::too_many_arguments` 证明零新增告警 |
