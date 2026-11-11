# COST-B3-3 提交 / 撤回 / 驳回接线 · 证据

卡：`t_4bc66290`（批次 B3，ARCH-9 §4.2 / §5.2 #28 · 拆卡清单 B3-3）
日期：2026-11-11
真库：`costb31_drill`（B3-1 保留的演练库，301 条迁移已应用，含 `V0381` 结算审批流种子）

## 一、卡面验收 → 实证

| 卡面验收 | 实现 | 证据 |
|---|---|---|
| **提交 → 引擎启动实例** | `POST /business/erp/cost-period/{id}/submit` → 同一事务：状态机 `DRAFT→SUBMITTED` + CAS 挂 `approval_instance_id` + `engine::start_in_tx`（引擎业务拒绝 ⇒ 整笔回滚，不留「SUBMITTED 无待办」死单） | 真库用例 ① `submit_starts_instance_and_freezes_batch`（02_real_db_tests.out）：实例 `PENDING` / `biz_type=COST_SETTLE` / `biz_id=批次 id` / 待办落财务经理 / 制单人被内控排除 |
| **驳回 → 状态回 `DRAFT` 且清空审批痕迹** | `flow::handler::{approve,reject}_task` 提交后回调 `cost_period_bridge::on_cost_settle_decided` → `SUBMITTED→DRAFT` + `approval_instance_id = NULL` + 意见追加 `remark` | 真库用例 ② `reject_returns_batch_to_draft_and_clears_traces`：`approval_instance_id` 变 `NULL`、`remark` 含「审批驳回：分摊基数…」且原备注保留、状态日志 `SUBMITTED→DRAFT`、**回草稿后能再提交** |
| **撤回 → 实例终止** | `POST …/{id}/withdraw` → 同一事务：`engine::withdraw`（实例 `WITHDRAWN`）+ 桥 `apply_decided_in_tx`（批次回 `DRAFT`、清痕迹）；审批中心撤回同样落这里 | 真库用例 ③ `withdraw_terminates_instance_and_returns_draft`：实例 `WITHDRAWN`、批次 `DRAFT`、`approval_instance_id=None`、**零残留 PENDING 待办**、`remark` 含「审批撤回」 |
| **单测：三条路径各一例** | ① 提交 ② 驳回 ③ 撤回 + ④ 两级审批通过 + ⑤ 状态护栏 + ⑥ 回调失败自愈 | `cargo test --lib cost_period_approval`：**6 passed / 0 failed** |

## 二、改了什么

| 文件 | 说明 |
|---|---|
| `backend/src/business/erp/cost_period_bridge.rs`（新） | `COST_SETTLE` 业务桥：`published_cost_settle_flow()`（取 `cost_settle_approve` 发布版）/ `verdict_of()`（白名单：APPROVED→通过，REJECTED/WITHDRAWN/CANCELLED/REJECTED_TERMINATED→打回，`PENDING` 等中间态一律不动）/ `on_cost_settle_decided()`（回调入口）/ `apply_decided_in_tx()`（**调用方事务内**回写，撤回端点用）/ `guard_self_approval()`（制单人不能批自己的结算，403+65019）/ `internal_control_creator()`（生成待办前排除制单人） |
| `backend/src/business/erp/cost_period_model.rs` | +3 个 CAS 写入口：`mark_submitted`（`WHERE status='DRAFT'` + 挂实例）、`mark_approved`（保留实例 id 作审计）、`mark_back_to_draft`（`approval_instance_id=NULL` + `concat_ws` 追加 `remark`，不覆盖用户备注） |
| `backend/src/business/erp/cost_period_handler.rs` | +`submit_period()` / `withdraw_period()` 与两个 HTTP 入口（权限码都是 `business:erp:cost-period:submit`，不新增码）+ 路由注解成对自检单测 |
| `backend/src/business/flow/handler.rs` | 回调注册 4 处（同意 / 驳回 / 撤回 / 行政终止）+ 自审闸 1 处 |
| `backend/src/business/flow/internal_control.rs` | `COST_SETTLE` 分支：生成待办前排除制单人（否则财务经理本人制单 = 看得见点不动 = 死单） |
| `backend/src/business/erp/mod.rs` | 注册 `cost_period_bridge` + `cost_period_approval_db_tests` |
| `backend/src/business/erp/cost_period_approval_db_tests.rs`（新） | 6 条真库用例（`ERP_DB_URL` 开关，不设则 SKIP） |

无迁移（只读 `V0373` 表 + `V0381` 流程种子；`CHECK ck_biz_cost_period_appr` 已存在）。

## 三、口径（为什么这么做）

1. **提交与起流程同事务**（`engine::start_in_tx`，不是 `engine::start`）：引擎业务拒绝（无可用审批人 / 自审被禁）先 commit 再判，会留下「批次 SUBMITTED、零待办」——批不了、撤不回、重提不了，只能改库（报销单 `t_0127714a` 的同款事故，AGENTS.md 约定 11）。
2. **撤回与回写同事务**：`withdraw_period` 一个事务里跑 `engine::withdraw` + `apply_decided_in_tx`，杜绝「实例 WITHDRAWN、批次仍 SUBMITTED」。
3. **审批中心的驳回/撤回只能事后回调**（审批事务先提交），回调失败时批次会停在 `SUBMITTED`。为此 `withdraw_period` 增加**自愈分支**：实例已不在 `PENDING` 时按实例**真实结论**回写批次（APPROVED→APPROVED，其余→DRAFT），用户点一次「撤回」即可恢复。用例 ⑥ 实证（把「驳回后不跑回调」的死单现场造出来，再点撤回 → DRAFT）。
4. **驳回清空 `approval_instance_id`**（卡面要求）：`DRAFT` 允许 `NULL`（`ck_biz_cost_period_appr` 只约束 SUBMITTED 起），审批痕迹留在审批中心（实例/待办一条不删）。与 V0332 采购退货、合同 R-P1-1 同款。注意这与 B3-2 交接里「不清 approval_instance_id」的建议不同 —— 以卡面验收为准，理由写在 `cost_period_bridge.rs` 文件头。
5. **中间态一律不改单据**：`verdict_of` 是白名单。两级流程批完第一站时实例仍是 `PENDING`，若按「非 APPROVED 就打回」会把手上的在途批次打回草稿（FIN-3 真机踩过）。
6. **没有已发布流程 = 默认拒绝**（409 + `66033`），不静默直过（`approval_gate` 口径）。

## 四、实测输出

| 文件 | 内容 |
|---|---|
| `01_unit_tests.out` | 纯单测：桥的 6 条（常量对齐 V0381 / verdict 白名单 / 接线自检）+ handler 的路由注解成对自检 |
| `02_real_db_tests.out` | 真库 6/6（`ERP_DB_URL=…/costb31_drill`，`--test-threads=1`） |
| `03_gates.out` | `rustfmt --check`（0 差异）+ `cargo test --lib` 全仓 4673 passed / 0 failed + `cargo clippy --all-targets -- -D warnings` |
| `04_residue.out` | 演练库残留：批次 / 实例 / 待办 / 状态日志 / 夹具账号 全部 0 |

## 五、已知偏差 / 未做

1. **多了 `POST /{id}/withdraw` 一个入口**（设计 §5.2 无）：设计里「撤回」只在审批中心；卡面把撤回列为验收项且批次列表需要入口，故补一个，权限码复用 `cost-period:submit`（不新增码）。
2. **`withdraw_period` 的自愈分支**（见上 §三.3）：设计没写，属约定 11 的补偿要求。
3. **未做前端**：B3-11 是「前端成本核算页（5 步向导 + 审批进度 + 红冲）」，提交/撤回按钮随那张卡。本卡只交付后端接口与权限码。
4. **未做 HTTP 冒烟**：卡的验收是三条路径的真库用例；HTTP 层已由 `submit_and_withdraw_routes_are_annotated` 单测钉住「路由 + 权限码成对」。
5. **结算（`APPROVED → SETTLED`）不在本卡**：B3-4。回调只把批次推到 `APPROVED`（数据冻结）。
