# P1-FIX-4 状态机配置化断链 —— 结案证据（t_54ee878d）

日期：2026-11-05 · 执行：backend-developer · 结论：**卡面证据解读有误（0 行是设计行为），但排查中确有一条真实断链，已修通并真机端到端验证**

---

## 0. 一句话结论

| # | 结论 | 依据 |
|---|------|------|
| A | `biz_flow_state_edge` 0 行 **不是**「改配置审批与落边断链」：生产那 4 张 `FLOW_EDGE_CFG` 单是 **WITHDRAWN×3 + REJECTED×1**，按设计驳回/撤回**一分不动配置** | `prod_flow_edge_cfg_probe.txt`（生产 144 实测） |
| B | **真实断链存在**，在旧批量审批端点 `POST /business/flow/task/batch`：它自己写了一套循环，只 `drain` 引擎副作用就返回，**不跑任何业务回调、不跑内控闸** → 用它批完一张配置审批单，审批单 `APPROVED` 而 `biz_flow_state_edge` 仍是 **0 行** | `e2e_before_fix.txt`（修复前二进制真机复现：`FAIL [batch-old] 审批通过后 biz_flow_state_edge 有 1 行 [count=0]`） |
| C | 已修：批量层改成**逐条委托** `flow::batch::batch_approve`（即 `approve_task` / `reject_task` 本体），批量与单条不可能再分叉 | `code_before_batch_task.txt` / `code_after_batch_task.txt` |
| D | 修复后真机端到端 **PASS=43 FAIL=0**，连跑两轮；`biz_flow_state_edge` 有 3 行真实配置，每行都能追到那张 APPROVED 审批单 | `e2e_after_run1.txt` / `e2e_after_run2.txt` / `state_edge_rows_after.txt` |

---

## 1. 断链根因（贴代码 + 为什么没执行）

### 1.1 先纠正：生产 0 行是**设计行为**，不是断链

生产 144 实测（`prod_flow_edge_cfg_probe.txt`，`prod_probe.sql`）：

```
== 1) FLOW_EDGE_CFG 审批单（全部）
 FLW-20261103-E0719F70 | WITHDRAWN | STOCK_OUT DRAFT>POSTED | 2026-11-03
 FLW-20261103-32DBADFD | WITHDRAWN | STOCK_OUT DRAFT>POSTED | 2026-11-03
 FLW-20261103-4C65B63B | WITHDRAWN | STOCK_OUT DRAFT>POSTED | 2026-11-03
 FLW-20261103-B8005F58 | REJECTED  | STOCK_IN DRAFT>POSTED  | 2026-11-03

== 2) FLOW_EDGE_CFG 审计行（from_status -> to_status 计数）
 REJECTED | REJECTED | 4        ← 全是「审批未通过：配置未变更」，APPLIED 0 行

== 3) biz_flow_state_edge 行数（存活 / 全部）
 alive | all_rows
     0 |        0
```

四条单**没有一条 APPROVED**。代码侧的判定就写在这里：

- `backend/src/business/flow/edge_admin.rs:969` `change_verdict()`：只有 `APPROVED → Apply`，
  `REJECTED / WITHDRAWN / CANCELLED → Drop`；
- `edge_admin.rs:1042` `if verdict == ChangeVerdict::Drop { … note_approval("REJECTED") … return Ok(None) }`
  —— **直接 return，不碰配置表**；
- 只有 `edge_admin.rs:1092` 的 `state_machine::upsert_edge_config(db(), &claims, patch)` 才会写 `biz_flow_state_edge`。

迁移头注释也写死了这条契约：`V0204__flow_state_edge.up.sql:18-24`（第 2 层只能加严、读失败退化）、
`V0206__flow_edge_config_admin.up.sql:32-39`（审批单 `form_data` 是待变更内容，通过时才写）。

⇒ 「4 张单 + 0 行」= 3 次撤回 + 1 次驳回的正常结果。**要判断断链，必须看单子的 status，不能只看行数。**

### 1.2 真实断链：旧批量端点绕过全部业务回调

`POST /business/flow/task/batch`（`handler.rs::batch_task`，HEAD 版本 `handler.rs:2011-2124`，
原文见 `code_before_batch_task.txt`）:

```rust
// 逐条独立事务：一条 409 不影响另外 49 条
let result = db().transaction(|tx| async move {
    if approve { engine::approve(&tx, task_id, operator, comment.as_deref()).await }
    else { engine::reject(&tx, task_id, operator, …, return_to).await }
}).await;
match result {
    Ok(mut out) => {
        let rejected = drain(&mut out).await;   // ← 只发通知/草稿，然后就结束了
        match rejected { None => { succeeded += 1; … } … }
    }
    …
}
```

对比单条 `approve_task`（`handler.rs:1742-1816`）在**事务提交之后**还要做两件事：

1. **9 道内控闸**（`guard_self_approval`，`handler.rs:1745/1747/1748/1751/1753/1755/1757/1759/1761`）：
   付款 / 采购 / 收货 / 凭证红冲 / 合同 / 盘点 / 预算 / 资产 / 报销 —— 制单人 ≠ 审批人；
2. **14 个业务回调**（`handler.rs:1772-1816`）：任务、调拨、收货作废、收货过账、采购、预算、资产、
   报销、合同、盘点、应付付款、应付作废、凭证红冲、**状态机配置**（`edge_admin::on_settled`）。

旧批量端点**一个都没有**。

#### 为什么「没执行」——真机复现（修复前二进制 `/root/p1fix4-before-bin`）

`e2e_before_fix.txt`（同一套断言，只跑旧批量端点那一段）：

```
PASS  [batch-old] 审批通过 HTTP 200            [http=200 ok]
PASS  [batch-old] 审批单落终态 APPROVED
FAIL  [batch-old] 审批通过后 biz_flow_state_edge 有 1 行（配置落库）   [count=0]
FAIL  [batch-old] 生效：effective_edges 里这条边消失
FAIL  [batch-old] 管理面 configs 里能看到 enabled=false 的配置行
===== 汇总：PASS=38 FAIL=3 =====
```

同一轮里 `single`（`/task/{id}/approve`）与 `batch-new`（`/task/batch-approve`，D7 的实现，
逐条委托 `approve_task`）都是 13/13 全绿 —— 三个通道唯一的差别就是这一层有没有委托。

**影响面不止状态机**：同一条路径上，批量同意一张付款单 → 审批单 APPROVED 但钱没付、凭证没生成；
批量同意收货过账 → 库存/应付不动；并且**批量审批可以绕过 9 道自审自批内控闸**（越权）。

---

## 2. 修复

**文件**：`backend/src/business/flow/handler.rs` → `batch_task`（`handler.rs:2028`）

做法：把「第二套实现」删掉，改成**逐条委托** `flow::batch::batch_approve`（`handler.rs:2084-2104`）：

```rust
// ② 真正的批量：逐条委托单条 handler（内控闸 + 事务 + 全部业务回调都走同一条路）
let outcome = crate::business::flow::batch::batch_approve(
    &claims,
    &BatchApproveReq { ids: valid.clone(), decision: Some(…), comment: comment.clone(),
                        return_to: req.return_to.clone() },
).await?;
```

老契约**一条不改**（向后兼容）：

| 契约 | 保持方式 |
|------|----------|
| 响应 `{total, succeeded, failed, items[{task_id, ok, code, message}]}` | `BatchResult` → `dto::BatchRes` 映射，**按入参顺序**回填 |
| ≤ 50 条 / 空列表 / 动作白名单 `APPROVE|REJECT` → 400 + 65005 | 保留原校验（`handler.rs:2032-2051`） |
| 非法 UUID **只让那一条失败**（不整请求 400） | 先按主键形态分流（`handler.rs:2068-2081`），合法的一批交给 `batch_approve` |
| 驳回并终止 / 退任意节点需 `business:flow:admin`（403 + 65004） | 保留原校验 |
| 失败码 / 文案与单条一致 | 直接取单条接口的 `ApiError`（`BatchFailItem.error`），单测 ⑯ 钉死 |

文档同步：`handler.rs:1110-1111` 路由表标注「兼容层 / 唯一实现」，
`docs/批次D-审批流-使用手册.md:299-305, 473-474` 增加 `batch-approve` 推荐端点。

---

## 3. 端到端（真机，全部走 HTTP）

**环境**：干净迁移库 `cloudpivot_p1fix4`（`schema_migrations` max=**V0251**、**193** 条、超管 1 人，
见 `cleandb_fingerprint.txt`）+ 隔离实例 `127.0.0.1:18119`（`p1fix4_instance.sh`）。
脚本 `p1fix4_state_edge_e2e.py`，三段用三条不同的可选边，互不干扰：

| 段 | 边 | 审批通道 |
|----|----|----------|
| single | `STOCK_OUT DRAFT→POSTED` | `POST /business/flow/task/{id}/approve` |
| batch-new | `STOCK_IN DRAFT→POSTED` | `POST /business/flow/task/batch-approve` |
| batch-old | `PURCHASE_ORDER SUBMITTED→CLOSED` | `POST /business/flow/task/batch`（本卡修的） |

每段 13 条断言：基线边可用 / 0 行 → 提交配置「送审不直接生效」 → 审批单 PENDING 且配置仍 0 行 →
待办生成（FALLBACK_ADMIN → 超管）→ 审批通过 200 → 实例 APPROVED → **配置 1 行 + enabled=false** →
审计有 APPLIED 行 → 有效边里消失 → 管理面能看到配置行。

**真单据证明「边真的生效」**（`e2e_after_run1.txt` 第 2 段，STOCK_OUT DRAFT→POSTED 被禁用后）：

```
PASS  非法转换 DRAFT→POSTED 被拒  [http=409
      msg=出库单 OUT-20261105-000001 的流转「DRAFT → POSTED」已被状态机配置禁用（biz_flow_state_edge）：
          这条边在代码里合法，但管理员把它…]
PASS  合法转换 DRAFT→PENDING_POST 通过（只禁了那一条边）  [http=200 ok]
```

**两轮汇总**：

| 轮次 | 结果 | 文件 |
|------|------|------|
| 修复前 | PASS=38 **FAIL=3**（batch-old 全红） | `e2e_before_fix.txt` |
| 修复后 第 1 轮 | **PASS=43 FAIL=0** | `e2e_after_run1.txt` |
| 修复后 第 2 轮（先清夹具再跑） | **PASS=43 FAIL=0** | `e2e_after_run2.txt` |

**`biz_flow_state_edge` 实际数据**（`state_edge_rows_after.txt`，两轮跑完后的库）：

```
     entity     | from_status | to_status | enabled | requires_reason
 PURCHASE_ORDER | SUBMITTED   | CLOSED    | f       | f
 STOCK_IN       | DRAFT       | POSTED    | f       | f
 STOCK_OUT      | DRAFT       | POSTED    | f       | f

来源审批单（biz_status_log APPLIED）：
 STOCK_OUT DRAFT>POSTED          | 审批 FLW-20261105-0FDBA7D1 通过，配置已生效（…禁用）
 STOCK_IN DRAFT>POSTED           | 审批 FLW-20261105-11DB0596 通过，配置已生效（…禁用）
 PURCHASE_ORDER SUBMITTED>CLOSED | 审批 FLW-20261105-635781A2 通过，配置已生效（…禁用）
三张审批单 status 均 = APPROVED
```

---

## 4. 单测（新增 10 条，全部真库）

`backend/src/business/flow/edge_admin_tests.rs`（`DB_LOCK` 串行 + `purge()` 夹具，`ERP_DB_URL` 开关）：

| # | 用例 | 钉住什么 |
|---|------|----------|
| ⑫ | `batch_task_writes_edge_after_approval` | **核心回归**：批量同意 → 配置 1 行 + 边从有效边消失（修复前必红） |
| ⑬ | `batch_task_reject_keeps_edge_config_untouched` | 批量驳回（`NONE`）→ 实例 REJECTED、配置 0 行、审计留痕 |
| ⑭ | `batch_task_invalid_uuid_only_fails_that_item` | 非法 UUID / 空串只让那一条失败，合法那条照常落边，结果按入参顺序 |
| ⑮ | `batch_task_repeated_approval_keeps_single_row` | 重复批同一待办 → 引擎幂等，配置仍是 1 行 |
| ⑯ | `batch_task_error_code_matches_single_approve` | 同一个不存在的待办：批量与单条**同码同文案**（薄壳证明） |
| ⑰ | `batch_task_reject_requires_five_char_comment` | 驳回意见 < 5 字 → 400 + 65005，配置不动、实例仍 PENDING |
| ⑱ | `batch_task_requires_approve_permission` | 缺 `business:flow:approve` → 403（不是「默默 0 条」） |
| ⑲ | `batch_task_rejects_oversized_batch_and_illegal_action` | 51 条 / 动作 `DELETE` → 400 + 65005 |
| ⑳ | `batch_task_empty_ids_is_rejected` | 空 `task_ids` → 400 + 65005 |

结果（`unit_round1.txt` / `unit_round2.txt`）：

```
business::flow::edge_admin  → 23 passed; 0 failed   （13 条既有 + 10 条新增）× 两轮
business::flow::batch       →  2 passed; 0 failed
```

`cargo clippy --lib --tests -- -D warnings` EXIT 0；`rustfmt --check` 两个改动文件无 diff。
编译与测试在**独立 worktree**（`/root/p1fix4-wt`，detached `0394c6f`）+ 独立 target
（`/root/p1fix4-target`）完成 —— 主工作区此时有另一张卡的 `flow_model_*` 拆分 WIP，尚不可编译。

---

## 5. 验收项逐条对照

| 卡面验收项 | 结果 |
|------------|------|
| 断链根因定位（贴代码 + 说明为什么没执行） | ✅ §1.1（纠正错误结论）+ §1.2（真实断链，代码 before/after + 真机复现） |
| 修通，端到端走通 | ✅ §2 委托式修复；§3 三段通道 43/43 × 两轮 |
| `biz_flow_state_edge` 有实际数据（贴查询） | ✅ §3 三行 + `state_edge_rows_after.txt` |
| 配置的边真的生效（非法转换被拒，合法转换通过） | ✅ 409「已被状态机配置禁用」/ 200 PENDING_POST |
| 单测 ≥ 8 条 | ✅ 10 条（真库），23 passed × 两轮 |
| git commit | ✅（见卡内汇报的 commit 号） |

## 6. 偏差 / 遗留（如实登记）

1. **未部署生产 144**：卡面只要求端到端；上线走 `scripts/deploy/deploy.sh`（另开或按发布流程执行）。
2. **端到端只覆盖 `FLOW_EDGE_CFG`**：其余 13 个回调（付款/收货/采购…）这次是**结构性复用** `approve_task`
   获得修复，没有逐个做真机 e2e（那是另一批卡的量级）。单测 ⑯ 证明「批量层只是薄壳」，风险已大幅收敛。
3. **`withdraw_instance` 没调 `task_bridge`** —— 核查后**不是缺陷**：`task_bridge::settle_target("WITHDRAWN")`
   就是 `None`（`task_bridge.rs:96` 的映射表），撤回本来就不推进任务。
4. **前端 `flow-api.ts::batchFlowTasks`（走旧端点）已无人调用**，前端改用 `api/business/batch.ts` 的
   `batch-approve`。建议后续删掉这段死代码（另开卡，避免本卡动前端）。
5. 隔离库 `cloudpivot_p1fix4`、worktree `/root/p1fix4-wt`、独立 target `/root/p1fix4-target` 已留，
   供后续卡复用（target 约 10G+，如需回收请显式删除）。
