# 种子审批流「空转」：admin 提交采购单无人复核就到 APPROVED（t_72e95761）

- 日期：2026-11-03
- 卡：`t_72e95761`（ERP-P1，来源：ARCH-3 审计的实施连带发现，**不是** DC-01 的 6 处之一）
- 基线：`e24ac8b`（本卡开工时的 `origin/main`）
- 环境：本机（172.18.10.99）隔离实例 + 本机 PG 上的**干净迁移库**（8 条流程全部 `PUBLISHED`）

---

## 0. 一句话结论

**不是**「`self_approval=ALLOW` + 发起人 = 审批人 ⇒ 自动通过」，而是：

> 种子 `po_approve` 的 `n_dept` 是 `empty_assignee=FALLBACK_ADMIN`。只有一个超管的库里，
> 兜底出来的**正是发起人本人**（`assignee::resolve` 的兜底分支**不再过自审策略**）。
> 引擎随后按内控把他剔出待办（落 `SKIPPED` 留痕），节点变成 **0 个可用审批人**，
> 于是走进「空审批人 → 节点 SKIPPED + 自动通过」那条分支（`engine.rs:2659`），
> 实例直接 `APPROVED`，业务回调把采购单改成 `APPROVED`。

即卡面列的**第三种形态**：「节点被 SKIP」。任务行存在，但一出生就是 `SKIPPED`。

修法：**「内控把审批人排除到 0 人」不再等于「没人可派」**。引擎在这种情况下 `422 + 65018`
整单发起失败（报文点名节点 + 每个人为什么被排除 + 两条出路），不再静默通过。
「真的没有候选」（角色 0 人 + `empty_assignee=SKIP`）保持原有自动通过语义不变。

---

## 1. 复现（修复前，二进制 = 基线 `e24ac8b`）

干净库 `cp_po_diag1`（迁移到 V0203，8 条流程全 `PUBLISHED`，`sys_user` 只有 bootstrap 的 `admin`）：

```
POST  /api/business/erp/purchase-order              → 200，status = DRAFT
PATCH /api/business/erp/purchase-order/{id}/status  {"status":"SUBMITTED"}
      → 200（不是 409/422），body 里 status = APPROVED
```

库内实读（`PO-20261103-000002`，含税 8000）：

```
 biz_purchase_order: order_no=PO-20261103-000002  status=APPROVED
                     approval_instance_id=01a19d28-e01f-7506-bba7-9269420fe875

 biz_flow_instance:  FLW-20261102-9C81FE62  status=APPROVED

 biz_flow_instance_node:
   s      APPROVED  approver_count=0
   c_low  APPROVED  approver_count=0
   n_dept SKIPPED   approver_count=0     ← 部门主管审批节点，一个人都没到
   e      APPROVED  approver_count=0

 biz_flow_task（全实例只有这 1 行）:
   node_key=n_dept  assignee=超级管理员(admin)  assignee_source=ADMIN
   status=SKIPPED   is_self_approval=t
   comment=内控：单据制单人不可审批自己制单的单据（职责分离），已跳过并留痕
```

实例日志（同一次请求，逐行原样）：

```
INFO  审批人解析完成（冻结成行） node_key=n_dept round=1 approvers=1 skipped=0 auto_pass=false
      trace=["ROLE 展开 0 行（角色 1 个）", "候选 0 人（去重前）",
             "去重 + 策略后：待办 0 行 / SKIPPED 留痕 0 行 / 自动通过=false",
             "FALLBACK_ADMIN 命中 1 个超管"]
WARN  内控排除了本节点的全部审批人，节点将自动通过 —— 审批人配置有问题：
      该节点的审批岗位不能由制单人 / 发起人担任   excluded=1 kept=0
DEBUG 节点自动跳过：部门主管审批（通知报文：1 人因内控被排除 …）
DEBUG 审批已通过：PO-20261103-000002 采购订单审批（报文：其中 1 个节点因无有效审批人自动通过）
```

对照库内种子数据（`V0182`）：

```
n_dept     ROLE=DEPT_LEADER  empty_assignee=FALLBACK_ADMIN  self_approval=ALLOW
DEPT_LEADER 角色成员数 = 0    SUPER_ADMIN 成员数 = 1（admin）
```

---

## 2. 诊断：卡面两个可疑根因的核实结果

### 2.1 「`self_approval=ALLOW` + assignee==initiator ⇒ 自动通过」——**不成立**

`assignee::finalize` 对自审的 `ALLOW` 只是**保留候选并打自审标**（`assignee.rs:362-369`），
引擎没有任何「ALLOW + 自己 ⇒ 直接 settle」的分支；实测 trace 里同一节点
`auto_pass=false`、`approvers=1`（就是 admin 本人）。所以 ALLOW 只是让这个人**成为候选**，
不是「自动批」。

### 2.2 「`FALLBACK_ADMIN` 解析出唯一审批人 = 发起人时直接 settle」——**机制对了一半**

- 对的一半：`assignee::resolve` 的兜底分支明确写着
  「**回退给平台超管（这里只查一次，不再走策略 —— 超管兜底不该再被过滤）**」
  （`assignee.rs:666-681`），所以兜底出来的 admin 会**以发起人身份**成为候选，`self_approval` 形同虚设。
- 不对的一半：真正让单子 settle 的**不是** resolve，而是引擎在落待办前做的内控剔除：

```
engine.rs:2542  let control = internal_control::of(&self.instance).await?;
engine.rs:2543  let (approvers, control_excluded) =
                    internal_control::partition(finalized.approvers, &control);
                ⇒ admin 既是 create_by 又是 initiator → 命中「制单人不可自审」→ SKIPPED 留痕
engine.rs:2606  approvers 空了 ⇒ 旧代码落到「空审批人」分支
engine.rs:2659  end_node_row(SKIPPED) + sync_auto_passed  ⇒ 节点自动通过 ⇒ 实例 APPROVED
```

**结论：根因是「空审批人自动通过」这条兜底分支把「审批岗位配错」当成了「没人需要批」。**
`self_approval` / `FALLBACK_ADMIN` 只是把人送到了这个分支门口。

---

## 3. 修复（按卡面决策：422，让发起人去配审批人）

### 3.1 引擎（`backend/src/business/flow/engine.rs`，审批 / 会签节点）

在**落 SKIPPED 留痕之前**加 fail-closed：

```rust
// 内控把审批人排除到 0 人 ≠ 没人可派
if approvers.is_empty() {
    return Ok(Step::Fail(ApiError::new(
        422,
        error_code::FLOW_NO_ASSIGNEE,      // 65018
        format!("节点「{}」的审批人全部是制单人 / 发起人本人，内控（职责分离）不允许他们审批自己的单据，\
                 本节点没有一个可用审批人：{who}。请先把该节点的审批岗位（角色成员 / 指定审批人）\
                 指派给非制单人、非发起人；或改由他人提交本单。", spec.name),
    )));
}
```

- 业务入口（采购提交 / 收货过账 / 作废 / 付款 / 凭证红冲）本来就把引擎的 `out.rejected`
  转成 `Err` 并**回滚事务**，所以失败后**库内不留实例 / 节点 / 待办**（不是「半截审批单」）。
- 下面那条「空审批人 → SKIPPED 自动通过」现在**只有** `control_excluded` 为空时才会走到
  （角色 0 人 + `empty_assignee=SKIP`，客户显式配置的免审），并加了 `debug_assert!` 钉住这个前提。
- 采购提交入口的 `rejected` 文案拼接改成「只给 `FLOW_SELF_APPROVAL_FORBIDDEN` 补一句通用提示」，
  避免在已经写明出路的 422 报文后面再叠一句泛泛的「请由他人提交」。

### 3.2 文档

`internal_control.rs` 的模块头「⚠️ 配置红线」从「全部被排除 → 自动通过」改成
「全部被排除 → 发起即失败（422）」，并写清与「真的没有候选」的语义差别。

### 3.3 单测

| 用例 | 位置 | 断言 |
|---|---|---|
| `countersign_all_excluded_fails_closed`（原 `countersign_all_excluded_auto_passes`，**语义反过来了**） | `flow/internal_control.rs` | 发起返回 `422 + 65018`；报文含「内控」「全部是制单人」「节点名」「指派给非制单人」；**实例 / 待办 0 行**（回滚） |
| `t72957_admin_submit_8000_waits_for_a_real_approver`（新增） | `erp/purchase_handler.rs` | 以 admin（真 bootstrap 超管）造单 + 提交 8000 元 → 单据 `SUBMITTED`、`approved_at` 为空、实例 `PENDING`、**恰好 1 条 PENDING 待办**且落在部门主管头上、admin 自己**没有任何待办**；部门主管批完才 `APPROVED` |
| 既有 4 条 pflow3a（`pflow3a_po_8000_goes_to_dept_leader` 等） | 同上 | 逐条仍绿（角色有人时行为不变） |

### 3.4 回归脚本 `scripts/verify/d4_po_to_stockin.sh`

去掉 DC-01 加的「临时下线 `po_approve`/`stock_in_post` + `skip_approval`」整段，改成
**两个流程全程在线、不带任何跳过**：

- 新增夹具账号 `d4.approver`（口令直接抄 admin 改密后的哈希；绑 `DEPT_LEADER` +
  `WAREHOUSE_MANAGER` + `SUPER_ADMIN`（为了拿 `business:flow:approve`））；
- 断言「两个流程都 `PUBLISHED`（未临时下线）」；
- 采购单提交后：`SUBMITTED` + **1 条 `PENDING` 待办** + **0 条 `SKIPPED`**；
- 收货过账：提交 → `PENDING_POST` → 仓管主管真点通过 → `POSTED`（第二张 700 元 < 1000，
  种子里就是免审直过，单独给一条说明性断言）。

---

## 4. 验收结果（全部为本机实跑）

### 4.1 修复后同一台隔离实例（干净库，只有 admin 一个超管）

```
PATCH /purchase-order/{id}/status {"status":"SUBMITTED"}
→ 422
{"code":65018,"msg":"节点「部门主管审批」的审批人全部是制单人 / 发起人本人，内控（职责分离）不允许
 他们审批自己的单据，本节点没有一个可用审批人：超级管理员（内控：单据制单人不可审批自己制单的单据
 （职责分离），已跳过并留痕）。请先把该节点的审批岗位（角色成员 / 指定审批人）指派给非制单人、
 非发起人；或改由他人提交本单。"}

PO-20261103-000003  status=DRAFT   approval_instance_id=NULL   ← 单据没被推进
biz_flow_instance / biz_flow_task 的新增行数 = 0                ← 事务回滚，没留半截审批单
```

### 4.2 同库配上部门主管后（同一张草稿重新提交）

```
提交：200  status=SUBMITTED
库内：FLW-20261102-DA830EF4  PENDING
      n_dept  ACTIVE  approver_count=1
      biz_flow_task: node_key=n_dept  username=diag.leader  status=PENDING  is_self_approval=f
```

即卡面要求的「**admin 提交 8000 元采购单 → 停在 SUBMITTED + 有 1 条待办**」。
（没有配审批人时不是「静默通过」，而是 422 告诉发起人怎么配 —— 这两种形态都已实测。）

### 4.3 `d4_po_to_stockin.sh`（流程在线、无跳过）

```
==> 汇总：PASS=55 FAIL=0
[PASS] 两个审批流程都在线（PUBLISHED，未临时下线）（2）
[PASS] 提交后停在 SUBMITTED（没人批不许 APPROVED）（SUBMITTED）
[PASS] 提交后确实有 1 条待办在等人批（1）
[PASS] 没有人被内控跳过（审批人不是制单人）（0）
[PASS] 提交后停在 PENDING_POST（等仓管主管复核）（PENDING_POST）
[PASS] 仓管主管审批通过（200）
[PASS] 单据变 POSTED（POSTED）
```

比卡面预期的 49 多 6 条，是本次**新增**的审批段断言（原脚本在 DC-01 时靠「下线流程 + skip」跳过审批段）。
**0 FAIL**，且 `po_approve` 全程 `PUBLISHED`、请求里没有任何 `skip_approval`。

### 4.4 全量单测（真库 `ERP_DB_URL`，干净库 `cloudpivot_test_t72957b`，迁移到 V0203）

```
cargo test --lib -- --test-threads=1
test result: ok. 2003 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 737.67s
```

（DC-13 基线 2002 → 本卡净增 1：新增 1 条 + 1 条旧用例改名改语义。）

### 4.5 检查链

```
cargo fmt -p cloudpivot -- --check   → exit 0（0 diff）
cargo clippy --lib -- -D warnings    → exit 0（0 warning）
cargo check --lib --tests            → exit 0
```

---

## 5. 影响面与上线注意

| 场景 | 变化 |
|---|---|
| 审批节点解析出的候选人**全部**被内控（制单人 / 发起人）排除 | **改**：`422 + 65018` 发起失败（原来是节点 SKIPPED、实例 APPROVED） |
| 角色 0 人 + `empty_assignee=SKIP`（客户显式免审） | **不变**：仍然节点 SKIPPED + 自动通过 |
| 候选里只有部分人被排除（会签 / 多审批人） | **不变**：被排除的人落 `SKIPPED` 留痕，其余人照常 `PENDING` |
| `self_approval=FORBID` / 审批入口 403（`guard_self_approval`） | **不变**（转办 / 加签 / 老数据仍被拦） |
| 不受内控约束的 `biz_type`（任务验收 / 调拨…） | **不变**（`governed=false`，一个字节没动） |

**上线要看的配置**（生产库只读排查建议）：凡是节点 `empty_assignee=FALLBACK_ADMIN` 且审批岗位
实际只有「制单人 / 发起人」那几个人能当的流程，上线后**提交会 422**（这正是内控要的），
运维要先在「流程定义」里把对应角色（部门主管 / 采购经理 / 仓管主管…）绑上非制单人。
生产现有 4 个超管，常规单据不会命中；命中的是「只有一个人能操作」的部署形态。

## 6. 边界（刻意没做的事）

- **没有**改动种子流程的 `self_approval` / `empty_assignee`（模板 ≠ 写死；改的是引擎语义）。
- **没有**新增错误码：语义就是 65018「本节点没有一个可用审批人」，报文负责说清是哪一种。
- **没有**在前端做额外提示（422 走现有错误管道，报文自带可执行指引）。
- 调拨（`STOCK_TRANSFER`）不在内控 `governed` 名单里，本卡未改其行为（自检端点仍覆盖它）。
