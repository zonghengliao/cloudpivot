# LNK-07 合同（CONTRACT）接入联动引擎 — 物证

> 卡片：`t_2df429aa`【LNK-07】接线：合同（CONTRACT）
> 目标：把合同的**状态推进**接进 `linkage::engine`，并且**审批通过只触发一次**
> （flow 回调 / `PATCH /status` 两条路不许重复触发），同时排除 `GENERATE_VOUCHER`（合同不产生凭证）。

## 1. 接了什么（代码事实）

| 触发点 | 调用点 | 目标态 |
|---|---|---|
| 提交审批 | `backend/src/business/erp/contract_handler.rs::contract_change_status`（目标态 `PENDING_APPROVAL` 分支，`tx.commit()` 之前） | `DRAFT → PENDING_APPROVAL` |
| 审批通过 / 驳回 | `backend/src/business/erp/contract_bridge.rs::settle`（状态机过闸 + 状态 CAS 成功之后、`tx.commit()` 之前） | `PENDING_APPROVAL → SIGNED` / `→ DRAFT` |
| 生效 / 完成 / 终止 / 撤回 | `contract_handler.rs::contract_change_status` 通用分支（状态机过闸 + CAS 成功之后） | `SIGNED → ACTIVE` / `ACTIVE → COMPLETED` / `SIGNED\|ACTIVE → TERMINATED` / `PENDING_APPROVAL → DRAFT` |

- 唯一 hook：`backend/src/business/erp/contract_linkage.rs`
  - 事务内 `linkage::engine::run_excluding(ex, &ctx, spec::CONTRACT_EXCLUDED_ACTIONS)`
  - commit 后 `linkage::engine::run_deferred`（通知 / 建任务这类 `AfterCommit` 动作）
- 声明：`backend/src/business/flow/linkage/spec.rs` 的 `CONTRACT` 条目 `wired = true`
  + `wiring`（实际调用点）+ `proofs`（文件 + 必须出现的调用片段），
  `tests_wiring::contract_wires_generic_engine_and_excludes_voucher` 双向扫描钉住。
- 排除：`spec::CONTRACT_EXCLUDED_ACTIONS = [GENERATE_VOUCHER]`（运行时）；
  另有 `spec::CONTRACT_FORBIDDEN_ACTIONS`（配置期 400，`model::upsert_rule` +
  `admin::check_draft` / `rule_problems` 读同一判据）—— 合同上跑 `GENERATE_VOUCHER`
  永远只会被 `Skipped`，不允许存一条「看起来生效、账上没动」的规则。

## 2. 防重复触发（卡面的重点）

审批通过确实有两条路，但**两条路只写同一列** `biz_contract.status` 且都带 CAS
（`WHERE status = <旧状态>`）：

| 后到的那条路 | 被谁挡掉 | 结果 |
|---|---|---|
| `PATCH /status` 直改 `SIGNED`（合同已是 `SIGNED`） | 状态机边判定（`SIGNED → SIGNED` 不是合法边） | 409 / `66015`，**进不到联动调用点** |
| flow 回调重放（同一实例再 `APPROVED`） | `flow::settle_guard::gate` → `SettleCheck::AlreadySettled` | `settle()` 直接 `Ok(None)`，**进不到联动调用点** |

即「状态列 CAS 成功」== 「联动恰好跑一次」。通知另有幂等键
`LNK:CONTRACT:<状态>:<合同 id>`（`uk_sys_notice_idem`）兜底。

## 3. 验证（全部真跑，非推断）

### 3.1 单元 + 真库用例

```bash
cd backend
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_so44" \
  cargo test --lib business::erp::contract_linkage -- --test-threads=1
```
→ **8 passed / 0 failed**（2 条纯逻辑 + 6 条真库）：

| 用例 | 断言 |
|---|---|
| `lnk07_submit_triggers_pending_approval_notice` | 提交审批触发 `PENDING_APPROVAL` 通知 |
| `lnk07_approval_triggers_once_and_repeat_is_suppressed` | 审批通过推进 `SIGNED` + 通知 1 条；**再回调一次 → `None`、通知仍 1 条**；换实例号重放同样不触发 |
| `lnk07_rejected_approval_reverts_and_triggers` | 驳回 → `DRAFT` + 通知 1 条 |
| `lnk07_excluded_generate_voucher_never_runs` | 引擎级对照（`run` 有该规则 `SKIPPED` / `run_excluding` 整条不跑）+ 端到端：合同名下 **0 张凭证**、同状态 `SEND_NOTICE` 正常 |
| `lnk07_manual_transitions_trigger_once` | `ACTIVE` / `TERMINATED` 各 1 条；重复生效被 409 挡住、通知不增 |
| `lnk07_generate_voucher_rule_is_rejected_on_save` | 合同上配 `GENERATE_VOUCHER` → 400 且不落库；`SEND_NOTICE` 无需 `allow_unwired` 即可保存 |

全库回归：`cargo test --lib` → **3199 passed / 0 failed**（基线 3190 → +9）。
`cargo clippy --lib --tests`：本卡 0 警告（余 2 条为他人既有文件 `sales_order_model` / `fx_revaluation`）。

### 3.2 变异红证据（证明用例不是虚绿）

| 变异 | 期望 | 实测 |
|---|---|---|
| `contract_bridge::settle` 调 hook 时把 `to_status` 换成 `PENDING_APPROVAL` | 审批通过不再触发 `SIGNED` 规则 | 3 条真库用例立刻红（`SIGNED` 通知 0 条），还原后全绿 |
| 把 hook 的排除清单改成 `&[]` | `tests_wiring` 静态守卫红 | `contract_wires_generic_engine_and_excludes_voucher` 红（「必须真的把 CONTRACT_EXCLUDED_ACTIONS 传下去」），还原后绿 |

### 3.3 真机 HTTP（`curl_evidence.txt`，生产同款链路）

`bash scripts/verify/lnk07_start.sh`（隔离实例 :18054 + `cloudpivot_so44`）→
`python3 scripts/verify/lnk07_curl_evidence.py` → **ALL PASS**：

1. `GET /api/business/flow/linkage?trigger_entity=CONTRACT` → `wired = true`（配置页不再骗人）
2. `PUT` 配 `CONTRACT/SIGNED` + `GENERATE_VOUCHER` → **HTTP 400** `10001`，文案「合同不产生任何凭证…」，库里 0 行
3. `PUT` 三条 `SEND_NOTICE`（`PENDING_APPROVAL` / `SIGNED` / `ACTIVE`）→ 均保存成功（无需 `allow_unwired`）
4. 建合同（制单人 `so44_sales_a`）→ `DRAFT`
5. 提交审批 → `PENDING_APPROVAL`，通知 **1** 条
6. `admin` 审批通过（真 `contract_approve` 流程）→ 合同 `SIGNED`、通知 **1** 条、**合同名下凭证 0 张**
7. 重复 `approve` 同一待办 → 200 且 `idempotent = true`（引擎幂等重放），合同仍 `SIGNED`、通知仍 **1** 条
8. 生效 → `ACTIVE`、通知 **1** 条；重复生效 → 409 `66015`，通知仍 **1** 条

清场：合同 / 规则 / 通知 / 审批实例全部删除；借用的 `PURCHASE_MANAGER` 成员原样挂回、
`admin` 的临时绑定与 `so44_sales_a` 的临时 `SUPER_ADMIN` 均已删除（复核 SQL：0 残留）。

## 4. 不在本卡范围（刻意不做）

- 不改表 / 不加迁移。
- 不接 `FLOW_INSTANCE`（审批实例自身的状态触发点）：它和单据侧触发点的先后顺序要单独设计
  （见 `docs/architecture/P1-FIX-5C-联动触发点接线清单.md` §3.3-D），本卡只保证**合同侧**不重复触发。
- 合同没有「负责人」字段，通知默认收件人取制单人（`create_by`），历史行没有制单人时退操作人。
