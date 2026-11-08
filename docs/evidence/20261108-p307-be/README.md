# P3-07 后端小补：`payments[].awaiting_executor`（t_6f2200e1）

日期：2026-11-08（CST）　执行人：backend-developer　上游：`t_41801d7f`（P3-07 前端配套）
改动文件：`backend/src/business/erp/payable_payment.rs`（+102 / −2）

## 1. 这张卡要解决什么

`erp.separation_of_duties.approve_execute` 打开后，付款审批通过**不记账**（`SettleOutcome::AwaitingExecutor`），
付款记录停在 `approval_status = PENDING`，必须由另一位有 `business:erp:payable:pay` 的人调
`POST /business/erp/payable/payment/{id}/execute` 才真正付出去。

前端（`t_41801d7f`）已经做好「执行付款」按钮，但判据是**兜底方案**：拿 `approval_instance_id` 再打一发
`GET /business/flow/instance/{id}` 读实例状态。而该接口只对 **发起人 / 审批人（含已办）/ 抄送人 /
`business:flow:admin`** 放行 —— **执行人按设计不是参与人**，拿 **403 + 65004**，按钮不出现，钱继续卡在「审批中」。

本卡让 `GET /api/business/erp/payable/{id}/payment-info` 的 `payments[]` **自己**下发这个事实，
执行人不需要任何审批实例的可见性。前端零改动（`payable-logic.ts::isAwaitingExecutor` 已是「字段优先」，
字段下发后连探针请求都不再发）。

## 2. 改了什么

### 2.1 出参加字段（`PaymentRes`）

```rust
/// 「审批通过但还没执行」（P3-07 `approve_execute` 开着才可能为 `true`）：
/// 付款仍 `PENDING` 且它挂的审批实例已是 `APPROVED`。
pub awaiting_executor: bool,
```

### 2.2 判据只有一份（纯函数 + 查库包装）

```rust
// 纯函数，单测直接打（不碰库、不看时间）
pub fn is_awaiting_executor(payment_status: &str, instance_status: Option<&str>) -> bool {
    payment_status == APPROVAL_PENDING
        && instance_status.is_some_and(|s| s.eq_ignore_ascii_case("APPROVED"))
}

// 查库版：非 PENDING 直接 false 不查库；有实例才查状态
pub async fn awaiting_executor_of(payment: &PaymentRow) -> Result<bool, ApiError>
```

`payments_res` 每行调 `awaiting_executor_of(row)` 填字段：
一张应付单同时最多一笔 `PENDING`（库侧 `uk_biz_payment_pending`），所以整个 `payments[]` **最多一次**额外查询，
其余付款行（`APPROVED` / `REJECTED` / `NONE`）零查询。

### 2.3 执行入口复用同一判据（不写两套）

`execute_approved_payment` 的判据 ③ 原本是内联的 `!instance.status.eq_ignore_ascii_case("APPROVED")`，
改成 `!is_awaiting_executor(&payment.approval_status, Some(&instance.status))` —— **同一份规则**，
且它手里已经有实例行，不必再走 `awaiting_executor_of` 白查一次库（函数注释里写明这个分工）。
错误文案、状态码、判据顺序**一行没动**（409 + 中文文案与 `t_41801d7f` 的契约单测一致）。

## 3. 门禁

| 项 | 结果 | 证据 |
|---|---|---|
| `cargo clippy --lib -- -D warnings` | **只剩既有基线** `business/erp/sales_order_model.rs:699 too_many_arguments`（本卡未碰该文件），本卡代码 0 新增告警 | `raw/clippy.txt` |
| `cargo test --lib business::erp::payable_payment::tests::` | **38 passed / 0 failed**（含本卡 4 条） | `raw/unit.txt` |
| 本卡新增单测 | **4/4 pass** | 同上 |
| `rustfmt +1.94.1 --edition 2024 --check`（单文件） | 无 diff | — |

新增 4 条单测（`p307_awaiting_executor_*`，跟既有纯函数用例同批）：

| 用例 | 输入 | 期望 |
|---|---|---|
| ① `..._true_when_instance_approved_and_payment_pending` | 实例 `APPROVED`（含小写 `approved`）+ 付款 `PENDING` | `true` |
| ② `..._false_when_instance_running` | 实例 `RUNNING`/`PENDING`/`REJECTED`/`CANCELLED`/`WITHDRAWN` + 付款 `PENDING` | `false` |
| ③ `..._false_when_payment_already_approved` | 付款 `APPROVED`/`REJECTED`/`CANCELLED`/`NONE`（开关关闭时审批通过即记账） | `false` |
| ④ `..._false_without_instance` | 无实例（直付 / 未配流程） | `false` |

## 4. 真机 HTTP（隔离实例，16/16 PASS）

环境：后端 `:18313`（本次改动现编，二进制 md5 `7f5674a9…`，cwd=`/opt/cloudpivot/backend`）
→ 空库 `cloudpivot_p307be`（273 迁移，`db_driver=postgres`）→ 账号与应付单见 `scripts/`。

| # | 断言 | 实测 |
|---|---|---|
| ①-0 | 发起付款 → `202` + `outcome=PENDING_APPROVAL` | PM-20261108-000001 / instance `01a1b9f2-…` |
| **④-1** | 实例仍 `RUNNING` → `awaiting_executor = false`（明确下发，不是缺席） | `false` / `PENDING` |
| ①-1 | 审批人待办里有这条付款审批任务 | task `01a1b9f2-4865-…` |
| ①-2 | 审批通过 → 实例 `APPROVED`，付款仍 `PENDING`（账没动） | `instance_status=APPROVED` |
| **①-3 ★** | `payment-info` 里那笔 `awaiting_executor = true` | `true` / `PENDING` |
| ①-4 | 同一行：未付仍 1200、paid=0 | `paid=0.0000 unpaid=1200.0000` |
| ⑥ | 每一笔付款都真的带这个字段（前端 contract） | keys 含 `awaiting_executor` |
| **②-1** | **执行人（非参与人）读 `GET /flow/instance/{id}` → 403 + 65004** | `403` / `65004`「你与这张审批单无关，不能查看」 |
| ②-2 | 同一时刻 `payment-info` 对他 200（后端字段绕过实例可见性） | `200` |
| ③-1 | 该人执行付款 → `200` + `data.message` | 「付款 PM-20261108-000001 已执行：应付回写 + 付款凭证已生成」 |
| **③-2** | 执行后再查：`awaiting_executor = false` + 付款 `APPROVED` | `false` / `APPROVED` |
| ③-3 | 账真动了：未付 1200 → 0，凭证已生成 | `paid=1200.0000` / `PZ-20261108-000001` |
| ⑤-1 | 关掉 `approve_execute` | `value=false` |
| ⑤-2 | 开关关闭：审批通过即记账，付款直接 `APPROVED`、paid=800 | `APPROVED` / `800.0000` |
| ⑤-3 | 已记账的付款 `awaiting_executor = false`（不会误显示按钮） | `false` |
| ⑤-4 | 开关复原为 `true` | `true` |

**②-1 + ②-2 是本卡的立论证据**：同一个令牌、同一时刻，实例详情 403 而付款记录里直接有答案 ——
这正是前端兜底方案必然漏按钮、而后端字段必然不漏的原因。

原始响应：`raw/01_pay.json` … `raw/08_info_switch_off.json`（`99_summary.json` 为汇总）。

## 5. 复现方式

```bash
# 1) 建空库（本机 su - postgres；需要 owner alun）
create database cloudpivot_p307be owner alun;

# 2) 起隔离后端（脚本会 cd 到 backend/，config/ 与 migrations/ 是相对 cwd 的）
bash scripts/start_be.sh          # :18313，密钥从环境变量取

# 3) 备环境（账号 / 开关 / 应付单），4) 跑验收
bash scripts/setup.sh
node scripts/verify.mjs
```

## 6. 边界与未做的事

- **未部署 145/144**：开关 `erp.separation_of_duties.approve_execute` 默认 `false`，本次只是接口多一个
  只读字段，前端零改动即可用；上线无需额外动作（与 `t_41801d7f` 同口径）。
- **不改前端**：`frontend/src/views/business/erp/payable/payable-logic.ts::isAwaitingExecutor` 已是
  「`awaiting_executor` 一旦下发就以它为准（`true`/`false` 都信）、并跳过实例探针请求」，本卡落地后
  该兜底分支自然不再触发。
- **多一条查询的代价**：每张应付单的 `payments[]` 最多一次 `biz_flow_instance` 主键查询（仅当存在
  `PENDING` 付款），无 `PENDING` 时零查询。
- **本卡没改行为**：`approve_execute` 的开关语义、执行入口的判据顺序 / 409 文案、审批回调路径一律未动。
