# P0-FIX 采购审批死锁 —— 交付与验证证据

> 卡面：t_5a2a8ba5【P0-FIX】采购审批死锁（内控排除 + 会签 + 角色配置三重修复，10 单测）
> 日期：2026-11-02　执行：backend-developer
> 提交：`4d4efdf` 功能 → `533b202` 脚本 → `3f0dde9` 用例语义 → `e5f3148` psql 插值修复 → `b0f04f0` 人数去重

## 0. 老板报的现象 → 实测根因

老板原话：「采购订单的审批有问题，现在到 laoliu 审批，但是不管是 admin 还是 laoliu 都显示不能审批自己发起的。」

生产只读实测（`PO-20261031-000002`）：

| 项 | 值 |
|---|---|
| 采购单 | `PO-20261031-000002`（`SUBMITTED`） |
| 制单人 | `laoliu`（老六） |
| 审批单 | `FLW-20261101-02524DBF`（`PENDING`） |
| 发起人 | `admin`（超级管理员） |
| 节点 | `n_dept`（部门主管审批，`approval_mode` 空 = 会签 ALL） |
| 审批人来源 | `ROLE` → `DEPT_LEADER`，**0 人绑定** → `empty_assignee=FALLBACK_ADMIN` 回退成 **4 个超管** |
| 4 张票 | `admin`(发起人) PENDING、`laoliu`(制单人) PENDING、`laoliu_test` PENDING、`henry.liao` **APPROVED** |

三重缺陷叠加 ⇒ 死锁：

1. **内控只在点的时候拦**（`guard_self_approval` → 403），但**待办照常生成** ⇒ 当事人看得见、点不动（L10/O5 不达标）；
2. **会签要求人齐** ⇒ 那两票永远出不来 ⇒ 节点永远过不去（L3）；
3. **审批岗位角色 0 人绑定** ⇒ `FALLBACK_ADMIN` 把 4 个超管全塞进审批人列表 ⇒ 必然包含制单人与发起人（配置缺陷）。

受影响单（生产 5 张，全部卡在 `n_dept`）：`PO-20261031-000002/000003/000009/000010/000011`。

## 1. 修复 1（代码）：内控闸改成「生成待办时排除」

### 新增 `backend/src/business/flow/internal_control.rs`

| 单元 | 作用 |
|---|---|
| `of(instance)` | 按实例算内控排除集：**制单人**（业务桥查 `create_by`）+ **发起人**（实例字段）；只对 `PO_APPROVE` / `STOCK_IN_POST` / `STOCK_IN_CANCEL` / `PAYABLE_PAY` 生效（`governed=true`） |
| `partition(approvers, control)` | **纯函数**：把审批人切成「该 PENDING」与「该 SKIPPED 留痕」 |
| `CREATOR_REASON` / `INITIATOR_REASON` | 留痕文案，写 `biz_flow_task.comment`（审批详情时间线直接展示 → F6） |

### 三个业务桥（硬红线 ②）

| 文件 | 新增 |
|---|---|
| `erp/purchase_bridge.rs` | `internal_control_creator(biz_id)` → `biz_purchase_order.create_by` |
| `erp/stockin_bridge.rs` | 同上 → `biz_stock_in.create_by`（过账 + 作废两类审批共用） |
| `erp/payable_bridge.rs` | 同上 → `biz_payment.create_by` |

### 引擎接线 `flow/engine.rs`（`NodeType::Approval | Countersign`）

```
assignee::resolve（框架 self_approval 策略，不动）
  → internal_control::of(&instance)                 ← 新增
  → internal_control::partition(approvers, control) ← 新增
  → 命中者落 SKIPPED 待办（completed_at + comment，**不删行**）
  → 其余落 PENDING
  → 若排除后一个人都不剩 → 节点按 empty_assignee 跳过 + tracing::warn + 知会发起人（写明「配置有问题」）
```

**没动的**（范围控制）：

- `guard_self_approval` 三个桥的 403 闸**一行未改**（转办 / 加签 / 老数据绕过仍被拦）；
- 节点 `self_approval` 仍是 `ALLOW`（内控 ≠ 框架自审策略：它还包含**制单人**）；
- 其它 `biz_type`（任务验收 / 调拨 / AI…）`governed=false`，行为与改动前逐字一致；
- **零迁移**（不新增表 / 列）。

## 2. 单测：14 条（卡面要求 ≥10，全绿）

```bash
P0FIX_DB_URL='postgres://p0fixrole:***@127.0.0.1:5432/cloudpivot_p0fix2' \
  cargo test --lib business::flow::internal_control -- --test-threads=1 --nocapture
```

```
running 14 tests
test business::flow::internal_control::db_tests::countersign_all_excluded_auto_passes ... ok
test business::flow::internal_control::db_tests::countersign_one_excluded_then_two_approve_passes ... ok
test business::flow::internal_control::db_tests::countersign_two_excluded_then_one_approves ... ok
test business::flow::internal_control::db_tests::excluded_user_cannot_bypass_by_calling_approve ... ok
test business::flow::internal_control::db_tests::remaining_approver_votes_normally ... ok
test business::flow::internal_control::db_tests::role_members_are_not_excluded ... ok
test business::flow::internal_control::db_tests::skipped_task_is_not_in_my_todo ... ok
test business::flow::internal_control::tests::business_roles_leave_nobody_excluded ... ok
test business::flow::internal_control::tests::creator_in_approvers_is_skipped_not_pending ... ok
test business::flow::internal_control::tests::initiator_in_approvers_is_skipped ... ok
test business::flow::internal_control::tests::nobody_excluded_keeps_everyone_pending ... ok
test business::flow::internal_control::tests::reasons_are_human_readable ... ok
test business::flow::internal_control::tests::same_person_keeps_one_reason ... ok
test business::flow::internal_control::tests::ungovened_business_is_untouched ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 1786 filtered out
```

| 卡面条目 | 对应用例 | 类型 |
|---|---|---|
| ① 制单人在审批人列表 → SKIPPED | `creator_in_approvers_is_skipped_not_pending` / `countersign_one_excluded_then_two_approve_passes` | 纯逻辑 + 真库 |
| ② 发起人同上 | `initiator_in_approvers_is_skipped` / `countersign_two_excluded_then_one_approves` | 纯逻辑 + 真库 |
| ③ 都不在 → 全 PENDING | `nobody_excluded_keeps_everyone_pending` | 纯逻辑 |
| ④ SKIPPED 不出现在「我的待办」 | `skipped_task_is_not_in_my_todo`（真库对照：当事人 0 条 / 能批的人 1 条） | 真库 |
| ⑤ 会签 3 人 1 人被 SKIP → 剩 2 人批完即通过 | `countersign_one_excluded_then_two_approve_passes` | 真库（引擎全链路） |
| ⑥ 会签 3 人 2 人被 SKIP → 剩 1 人批完即通过 | `countersign_two_excluded_then_one_approves` | 真库 |
| ⑦ 会签全被 SKIP → 节点自动通过 + warn | `countersign_all_excluded_auto_passes` | 真库 |
| ⑧ 绕过前端直接 approve（制单人）→ 仍 403 | `excluded_user_cannot_bypass_by_calling_approve` | 真库 |
| ⑨ 排除后能批的人正常批 | `remaining_approver_votes_normally` | 真库 |
| ⑩ 换成业务角色后无人被排除 | `business_roles_leave_nobody_excluded` / `role_members_are_not_excluded` | 纯逻辑 + 真库 |

**顺带改的旧用例**（内控语义变了，断言必须跟着变，不是绕过）：

- `purchase_handler::pflow3a_po_maker_cannot_approve_own_order`、`stockin_handler::pflow3a_si_maker_cannot_approve_own_post`、`payable_bridge::p17_creator_cannot_approve_own_payment`：原来断言「制单人**能看到**自己的待办但点不动」，改成「制单人拿到的是 **SKIPPED 留痕行**、403 兜底仍在、他人照常可批」。
- `payable_payment::d5_*`（4 条）：付款提交人由「admin 自己」换成普通账号 `d5.operator` —— 夹具库里只有一个超管，若 admin 自交自批，他就是唯一审批人、又被内控排除 ⇒ 节点自动通过，「配了流程必须走审批」的断言就失去意义（生产有 4 个超管，不会是这个形态）。

## 3. 全量回归

```
$ ERP_DB_URL=.../cloudpivot_p0fix2 cargo test --lib -j 2 -- --test-threads=1
test result: ok. 1800 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 503.12s
```

`cargo fmt -p cloudpivot -- --check` → 0 diff；`cargo clippy -- -D warnings` → 0 warning。

## 4. 不变量（干净迁移库 V0189，零夹具）

```
$ NIK_DATABASE_NAME=cloudpivot_p0fix3 python3 scripts/verify/erp_invariants.py
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

> 同一张库跑完 1800 条 lib 套件后再跑不变量会变 22/32（5 BLOCKER）—— 那是**测试夹具**留在库里的账实差（采购/收货/付款夹具各造各的账），与本次改动无关；验收口径按「干净库 32/32」。

## 5. 修复 2（数据）：解除存量死锁

脚本 `scripts/maintenance/p0fix_self_approval.sh`（默认 dry-run；`--apply` 才改；生产额外要 `--i-know-its-prod`）：

1. **整库 `pg_dump` 备份**（含 `MANIFEST.txt`；备份失败直接中止）；
2. 命中行**整行快照**进 `p0fix_self_approval_backup`（精确回滚用）；
3. 单事务：`status='SKIPPED'` + `completed_at=now()` + `comment='内控：制单人/发起人不可自审（P0-FIX 清理…）'`；
4. 按引擎口径重算节点 `approver_count / approved_count / rejected_count`（不含 SKIPPED）；
5. `--verify` 逐节点列出「还剩谁能批」+ 点名「ACTIVE 但 0 张 PENDING」的风险节点；
6. `--rollback --apply` 按行级快照精确回滚。

判据按**形态**（`assignee = 单据 create_by` 或 `= 实例 initiator_id`）而不是单号，覆盖采购 / 收货过账 / 收货作废 / 应付付款四类内控业务（复发也能修）。

### 5.1 演练（隔离库 cloudpivot_p0fix2，与生产同形的夹具）

```
$ P0FIX_DB=cloudpivot_p0fix2 bash scripts/verify/p0fix_deadlock_repair.sh
==> 汇总：PASS=20 FAIL=0
```

覆盖：dry-run 不改数据 / 精准命中 3 条内控待办（采购 2 + 付款 1）/ apply 生成整库备份 /
**待办行数不变（不删行）** / 制单人·发起人 → SKIPPED / 无关人仍 PENDING / 已批行不动 /
SKIPPED 必有 `completed_at` + 内控 comment / `approver_count` 重算 / verify 无风险节点 /
rollback 精确还原。

### 5.2 生产执行（144）

dry-run 命中清单（10 行 = 5 单 × {制单人, 发起人}，无一条误伤）：

```
 老六       | PO-20261031-000002 | PO_APPROVE | n_dept | 制单人
 超级管理员 | PO-20261031-000002 | PO_APPROVE | n_dept | 发起人
 ...（000003 / 000009 / 000010 / 000011 同形）
命中 PENDING 待办：10 条（APPROVED / REJECTED 一律不动，待办行不删）
```

apply：

```
---- ② 整库备份 → /root/datafix-backup-p0fix-20261102-001913 ----
备份完成：7.1M
---- ③ 修复（单事务：行级快照 → 标 SKIPPED → 重算节点人数）----
BEGIN / CREATE TABLE / COMMENT / SELECT 10 / INSERT 0 10 / UPDATE 10 / UPDATE 5 / COMMIT
修复完成。
```

`--verify`（生产 5 张单）：

```
 FLW-20261101-02524DBF | PO-20261031-000002 | PO_APPROVE | PENDING | n_dept | ACTIVE | 2 | pending=1 | skipped=2 | approved=1 | 老六测试号
 ...（5 张同形）
---- 遗留风险：节点 ACTIVE 但已无 PENDING 待办 ----
（没有 ⚠ 行 = 每个在跑节点都还有能审批的人）
```

## 6. 修复 3（配置）：审批岗位绑人

`bash scripts/maintenance/p0fix_bind_approver_roles.sh --audit`（生产）：

```
 role_code          | role_name | members | used_by_nodes
 DEPT_LEADER        | 部门主管  |       0 |             1
 GENERAL_MANAGER    | 总经理    |       0 |             1
 PURCHASE_MANAGER   | 采购经理  |       0 |             2
 WAREHOUSE_MANAGER  | 仓管主管  |       0 |             1
```

四个业务审批岗位**一个人都没有** ⇒ 每个节点都退化成 `FALLBACK_ADMIN`（全部超管）。已按
`--role <CODE> --users henry.liao,laoliu_test --apply --i-know-its-prod` 绑定（**只加不减**，幂等）：

```
DEPT_LEADER        henry.liao,laoliu_test      → po_approve.n_dept   可审批人数 = 2
PURCHASE_MANAGER   henry.liao,laoliu_test      → po_approve.n_pm=2 / n_board=2
GENERAL_MANAGER    henry.liao,laoliu_test      → po_approve.n_board 合并去重后仍 2 人
WAREHOUSE_MANAGER  henry.liao,laoliu_test      → stock_in_post.n_wh  可审批人数 = 2
```

> 「谁是采购经理 / 部门主管」是**组织数据**，不写进迁移（客户环境各不相同）；本卡把它做成
> 可重复执行的运维脚本 + 体检输出，代码侧的内控排除保证「即使仍走 `FALLBACK_ADMIN` 也不会死锁」。

## 7. 真机验收（生产库快照 + 部署的那只二进制 + 隔离实例）

生产账号口令无人知晓，所以 HTTP 走查在**生产库快照**（`cloudpivot_p0verify`，161 条待办 / 15 张采购单 /
迁移 190）上跑，二进制与随后部署到生产的**是同一个**（`md5 e3327684822450620f5052c497823564`），
生产服务全程未动（pid 不变）。口令只在副本里换成夹具口令。

```
============================== ④ 修复前：复现死锁 ==============================
  [PASS] laoliu 能看到 PO-20261031-000002 的待办（FLW-20261101-02524DBF / 部门主管审批）
  [PASS] laoliu（制单人）点自己不动的票 → HTTP 403
  [PASS] 报文点名内控原因
  [PASS] admin（发起人）点自己不动的票 → HTTP 403
  [PASS] 实例仍是 PENDING，节点上还有 3 张 PENDING 票（其中 2 张永远批不出来）
==> before 汇总：PASS=5 FAIL=0

============================== ⑥ 修复后 ==============================
  [PASS] laoliu（制单人）的「我的待办」里已经没有 PO-20261031-000002（不是「看得到点不动」）
  [PASS] admin（发起人）的「我的待办」里也没有 PO-20261031-000002
  [PASS] laoliu_test 能看到 PO-20261031-000002 的待办（FLW-20261101-02524DBF / 部门主管审批）
  [PASS] 审批详情（timeline）里能看到 2 条「内控被排除」的 SKIPPED 留痕
  [PASS] laoliu_test 审批成功
  [PASS] 会签满足 → 审批单 APPROVED
  [PASS] PO-20261031-000002 已变为 APPROVED（不再卡在审批中）
==> after 汇总：PASS=7 FAIL=0

============================== ⑧ 新提交（新代码行为）==============================
  用草稿 PO-20261101-000001 演示：admin 发起审批（制单人 = 老六测试号）
  实例 FLW-20261101-990AFEC8 的票：
    [{"who": "老六测试号", "status": "SKIPPED", "comment": "内控：单据制单人不可审批自己制单的单据（职责分离），已跳"},
     {"who": "Henry.Liao", "status": "PENDING", "comment": ""}]
  [PASS] 生成待办时就落 SKIPPED 留痕（内控排除生效，不是点的时候才报错）
  [PASS] 留痕写明内控原因
  [PASS] 还有能批的人拿到 PENDING 待办
  [PASS] 审批人「Henry.Liao」批票成功
  [PASS] PO-20261101-000001 批完即 APPROVED（不卡死）
==> new-flow 汇总：PASS=6 FAIL=0
```

### 7.1 生产部署（144）

```
== 0) 预检 ==  md5 e3327684822450620f5052c497823564 / 新代码特征串命中：7
== 1) 备份旧二进制 → cloudpivot.p0fix-backup-20261102-001905（md5 c4e906ec…）
== 3) 换二进制（.new + mv，避免 ETXTBSY）
== 5) 双验  health=200 / MainPID=3659850
   进程二进制 md5：e3327684822450620f5052c497823564
   磁盘二进制 md5：e3327684822450620f5052c497823564
   生产二进制里的新特征串：7
== 6) 数据库迁移完成: 应用 0 个迁移   （本次零迁移）
== 7) GET /api/business/flow/task/todo（无令牌）= 401（路由在，只是要认证）
```

### 7.2 生产终检（只读）

```
=== 5 张单的待办分布 ===
PO-20261031-000002  APPROVED  henry.liao   completed_at=true
PO-20261031-000002  PENDING   laoliu_test  （唯一还能批的人）
PO-20261031-000002  SKIPPED   admin        completed_at=true  内控：制单人/发起人不可自审（P0-FIX 清理…）
PO-20261031-000002  SKIPPED   laoliu       completed_at=true  内控：制单人/发起人不可自审（P0-FIX 清理…）
（000003 / 000009 / 000010 / 000011 同形）

=== 节点人数口径（approver_count 不含 SKIPPED，与 tally 一致）===
5 张单全部：approver_count=2 / tally_total=2 / skipped=2

=== 待办行数有没有变少 ===
PO 待办 20 行（5 单 × 4 票，**一行没删**）；行级快照 10 条；备份 /root/datafix-backup-p0fix-20261102-001913

=== 审批岗位角色成员 ===
DEPT_LEADER / GENERAL_MANAGER / PURCHASE_MANAGER / WAREHOUSE_MANAGER → henry.liao,laoliu_test

=== 服务与二进制 ===
active；进程 md5 == 磁盘 md5 == e3327684822450620f5052c497823564；/health=200；受保护路由无令牌=401
```

**生产现状**：5 张单各剩 1 张「老六测试号」的待办 —— 老板用该账号点一下即可通过（死锁已解除，
不需要 DBA 手工推流程）。

## 8. 设计自检（卡面关键项）

| # | 项 | 落点 | 证据 |
|---|---|---|---|
| L10 | 内外控：生成时就 SKIP | `internal_control::partition` + engine 落 SKIPPED | 单测 ①②；真机 new-flow 段 |
| L3 | 会签被 SKIP 的票不阻塞 | `Tally.total` 不含 SKIPPED（原有口径）+ 单测 | 单测 ⑤⑥⑦ |
| O5 | 被排除的人看不到待办 | `page_my_tasks` 只认 PENDING；其行是 SKIPPED | 单测 ④；真机 after 段 |
| F6 | 详情显示「谁因内控被排除」 | SKIPPED 行的 `comment` 落时间线（前端 `taskStatusText` 已有「跳过」） | 真机 after 段「2 条 SKIPPED 留痕」 |

## 9. 已知取舍 / 后续建议

1. **「全部审批人都被内控排除」时节点自动通过**（卡面 ⑦ 要求）。这在配置失误时意味着「没人签字
   就通过了」—— 已加 `tracing::warn` + 知会发起人（文案点名「审批人配置有问题」），运维可据此告警。
   若要更严的口径（fail-closed：发起时就 422「没有合法审批人」），是产品决策，建议单开一张卡。
2. `p0fix_self_approval_backup` 是**修复留痕表**，不要当业务表用；回滚后才可清理。
3. 生产 5 张单的最后 1 票留给老板点（本卡不代点生产单据）。

## 10. 交付物清单

| 类型 | 路径 |
|---|---|
| 代码 | `backend/src/business/flow/internal_control.rs`（新）、`flow/{engine,mod}.rs`、`erp/{purchase,stockin,payable}_bridge.rs` |
| 用例 | `flow/internal_control.rs`（14 条）、`erp/{purchase_handler,stockin_handler,payable_payment}.rs`（内控旧用例改新语义） |
| 运维脚本 | `scripts/maintenance/p0fix_self_approval.sh`、`scripts/maintenance/p0fix_bind_approver_roles.sh` |
| 验收脚本 | `scripts/verify/p0fix_deadlock_repair.sh`（20 项断言）、`scripts/verify/p0fix_e2e.py`（三段真机 HTTP） |
| 证据 | 本文件 |
