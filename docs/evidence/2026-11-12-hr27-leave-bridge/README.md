# HR-27 验收证据 —— `leave_bridge`（请假审批回调桥）

- 卡：`t_4e909c79`（HR-27【HR】`leave_bridge`：审批回调 → 状态 + 实例终止 + 自审批守卫）
- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §4.3（四条硬口径）/ §8.1 HR-27
- 上游：HR-26（`V0393__hr_leave_flow` 种子）/ HR-25（`leave_handler` 6 接口）
- 范本：`erp/expense_bridge.rs`、`erp/requisition_bridge.rs`
- 演练库：**145（172.18.10.145）/ hr27_drill**，跑完已 DROP（99 上零建库）
- **零迁移**：本卡只加代码（流程种子属 HR-26），不改任何已应用迁移

## 一、交付验收（卡面 3 条）

| 验收项 | 落地证据 |
|---|---|
| **审批通过/驳回都正确回写** | `on_leave_settled`：`APPROVED → SUBMITTED→APPROVED`；`REJECTED/REJECTED_TERMINATED/WITHDRAWN/CANCELLED/TERMINATED → SUBMITTED→DRAFT`（打回，意见进 `biz_status_log`）。真库用例 `approve_writes_back_and_keeps_trace`（附录 ③）+ `reject_backs_to_draft_and_allows_resubmit`（附录 ③） |
| **自审批被拦** | `guard_self_approval`：① `instance.initiator_id == operator` ②请假单员工本人（`biz_leave_request.employee_id → biz_employee.user_id`）== operator，两条都 403 + `65019`。真库用例 `guard_blocks_self_approval`（两种情形 + 无关审批人放行 + 无关待办零影响） |
| **单测覆盖三条路径** | 单元 5 条：`verdict_maps_three_paths`（通过 / 驳回族 / **PENDING 中间态**）、`self_approval_blocks_initiator_and_employee`、`biz_type_and_flow_code_match_seed_and_handler`、`edges_match_registry_and_statuses_in_domain`、`status_constants_are_exact`。见 `cargo test --lib business::hr::leave_bridge`：**9 passed**（5 单元 + 4 真库） |

## 二、实现口径（每条都有理由，不是照抄 ERP 桥）

| 决策 | 为什么 |
|---|---|
| **白名单列终态，不写「非 APPROVED 就打回」** | `hr_leave` 是**两段式**（直属上级 → ≥3 天再加人事经理）。长假单在直属上级批完后，`approve_task` 回调传的是 `PENDING`；用「非 APPROVED 就打回」会把长假单在人事复核前直接打回草稿（FIN-3 真机踩过的同一个坑）。单测钉住 `PENDING → None` |
| **陈旧回调闸**（ERP 几个桥没有） | 撤回 → 回草稿 → 重提后，**旧实例**的结论可能迟到。只按「状态是 SUBMITTED 就推」会把新一轮审批中的单直接推成 APPROVED/DRAFT。`settle` 要求 `approval_instance_id == 这个实例`，否则 WARN 跳过；`approval_instance_id IS NULL` 且 SUBMITTED 时照常推进（老数据兜底，不留死单）。真库用例在 ②④ 两处反证 |
| **回调用「最后一票审批人」当 operator** | `biz_status_log.operator_id/name` 是审计锚点（AGENTS.md 约定 13）。读不到（免审直过 / 待办被清理）退回发起人快照，绝不写 NULL |
| **驳回不清 `approval_instance_id`** | 详情页的审批轨迹（`leave_detail.approval`）靠它渲染，驳回后用户要能看见「谁驳的、为什么」。重提不受影响：`uk_biz_flow_instance_biz_active` 只约束 `PENDING`，`submit_leave` 会把引用改写成新实例（真库用例 ② 断言重提成功且新实例 id 不同） |
| **实例终止放 `cancel_approval_of`，不放回调** | 回调发生在审批事务提交**之后**，那时实例已是终态，没有可终止的东西。真正要终止实例的是单据侧主动改状态（撤回/作废）→ 由 `leave_handler::cancel_leave` 在同一事务里调用（把 HR-25 的私有 `terminate_instance_of` 收进桥里，口径只有一处） |
| **不改考勤** | 设计 §4.3 第 1 行：考勤由导入/人工体现，daemon 只做校验。真库用例 ① 直接断言 `biz_attendance_record` 零行 |
| **失败口径** | 通过失败 `error!`（审批过了但单据没落 = 员工请假没落库，考勤口径错）；打回失败 `warn!`。两者都**不回滚审批单**、返回 `None` |

## 三、动到的文件

```
backend/src/business/hr/leave_bridge.rs            新 约 480 行（桥 + 5 条单测）
backend/src/business/hr/leave_bridge_db_tests.rs   新 约 700 行（4 条真库用例 + 夹具 + 流程夹具）
backend/src/business/hr/leave_handler.rs           改：删私有 terminate_instance_of，改调 leave_bridge::cancel_approval_of
backend/src/business/hr/mod.rs                     改：注册 pub mod leave_bridge + #[cfg(test)] mod leave_bridge_db_tests
backend/src/business/flow/handler.rs               改：4 处回调接线（approve/reject/withdraw/terminate）+ 1 处自审批闸
docs/evidence/2026-11-12-hr27-leave-bridge/        本目录（脚本 + 4 份实测输出）
```

接线位置（`flow::handler`，全部「事务提交之后 / 事务之前」按口径放）：

```text
approve_task      : leave_bridge::on_leave_settled(out.instance_id, &out.instance_status)
reject_task       : leave_bridge::on_leave_settled(out.instance_id, &out.instance_status)
withdraw_instance : leave_bridge::on_leave_settled(Some(instance_id), "WITHDRAWN")
terminate_instance: leave_bridge::on_leave_settled(Some(instance_id), "CANCELLED")
approve_task 之前 : leave_bridge::guard_self_approval(task_id, operator).await?
```

## 四、实测输出（真实运行，见同目录文件）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_seed_db.sh` | 145 建 `hr27_drill` + 应用全量迁移（313 条；`leave_flow_def=1`、`biz_leave_request` 存在） | 通过 |
| `02_static_checks.out` | `cargo fmt --check` / `cargo check --all-targets` / `cargo clippy --all-targets -- -D warnings` | exit 0 / 零警告 / 零警告（clippy 首跑 3m41s 通过，此处为缓存复跑） |
| `03_db_tests.out` | `cargo test --lib business::hr::leave_bridge_db_tests`（`HR_DB_URL` → 145/hr27_drill） | **4 passed / 0 failed** |
| `04_leave_tests.out` | `cargo test --lib business::hr::leave`（HR-25 的 54 + 本卡 5 单元 + 4 真库） | **63 passed / 0 failed**（重构无回归） |
| — | `cargo test --lib business::flow::` | **570 passed / 0 failed**（handler 接线未打断既有用例） |
| — | `bash scripts/check.sh --l0` | 全部通过（库存写入口零越界 / 迁移授权段无新增角色码） |
| — | `pnpm typecheck`（frontend） | 通过（本卡前端零改动，仅确认基线不红） |

演练库清理：`DROP DATABASE IF EXISTS hr27_drill`（见 `05_cleanup.out`）。

## 五、报出但不动的（纪律：不改别的模块）

1. **`flow::internal_control` 没有 HR 分支**：它按 `biz_type` 分流把「制单人」从审批人候选里摘掉（落 `SKIPPED`），目前只覆盖 ERP 十几种 `biz_type`。请假单没接进去意味着「员工本人如果恰好是这一签的审批人（部门负责人自己请假）」，待办会**派给他**，点同意时才被 `guard_self_approval` 403。体验比 ERP 差一步（多一次点击），但**不产生错误数据**。
   → 建议：后续卡在 `flow/internal_control.rs` 的 `of_biz` 里加一行 `biz_type == leave_bridge::BIZ_TYPE` + `leave_bridge::internal_control_creator(biz_id)`（复用 `biz_leave_request.employee_id → biz_employee.user_id`）。属 `business/flow` 模块，本卡未动。
2. **`flow::handler::TEMPLATE_CATEGORY_ORDER` 只有 4 类**（HR-26 已报）：`category='人力资源'` 的流程在模板库排末尾。属 `business/flow` 模块。
3. **`leave_form_data` 的条件字段镜像**（`duration_days_num`）是 HR-26 修的；本卡消费时确认引擎走的是数值镜像（`business::hr::leave` 63 条用例里含该边界）。
