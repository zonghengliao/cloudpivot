# 审批与审计口径（签核 / 意见回写 / 姓名快照 / 凭证摘要）

> 本页是 FIX-9（P4 可读性）落地后的**口径说明**：四件看起来像 bug、实际要写清「设计是什么」的事。
> 所有实现指针都在文中给出；改代码请同步改本页。

## 1. 角色类审批节点多成员：默认**会签**（ALL），可配成**或签**（ANY）

### 现状（为什么 demo_lead + lead_drill 都签才推进）

审批节点（`node_type = APPROVAL`）绑的是**角色**（`assignee_type = ROLE`）时，引擎把该角色**全部成员**
展开成**多条待办**（每人一票），然后在节点级按 `approval_mode` 判票：

| `approval_mode` | 语义 | 通过条件 | 判死条件 |
|---|---|---|---|
| `ALL`（**默认**，列 NULL 时按它） | 全员同意（会签） | 同意票 = 应到票 | 没有待审票仍不满足 |
| `RATIO` | 按比例（`approve_ratio` ∈ (0,1]） | 同意/应到 ≥ 比例 | 「已同意 + 未表态」也追不上比例 |
| `ANY` | 任一人同意（或签） | 同意 ≥ 1 | 没有待审票仍无人同意 |

判定实现：`backend/src/business/flow/engine.rs::judge`（一条纯函数，`tally` 每次 `COUNT(*)` 现算，不信任累加器）。

**所以「多成员要全员签」是设计（会签），不是 bug** —— 默认 `approval_mode` 为空 ⇒ 按 `ALL` 走。

### 想改成或签：把该节点 `approval_mode` 配成 `ANY`

- 配置入口：流程定义编辑器 → 选中节点 → **「审批模式」**（下拉：全部同意（会签）/ 按比例通过 / 任一人同意（或签））。
  前端 `frontend/src/views/business/flow/components/NodeProps.vue`（`supportsCountersign` 对 `APPROVAL` 与 `COUNTERSIGN` 都显示）。
- 落库：`biz_flow_node.approval_mode`（`varchar(8)`，`ALL` / `RATIO` / `ANY`），发布新版本后对新实例生效；
  **已发起的实例走的是发起时快照**（`biz_flow_instance_node.approval_mode`），改配置不影响在跑的审批单。
- 引擎对 `APPROVAL` 与 `COUNTERSIGN` **读同一列**（`engine.rs` 的 `judge(ApprovalMode::parse(node.approval_mode.as_deref().unwrap_or("ALL")))`）；
  发布校验 `R9` 只对 `COUNTERSIGN` **强制必填**，`APPROVAL` 上留空即 `ALL`。
- 现网实测（拷库 `biz_flow_node`）：`APPROVAL + ROLE` 的节点绝大多数 `approval_mode` 为 NULL（= 会签），
  也有一个真实配了 `ANY` 的 `n_finance 财务经理审批` ⇒ 这条配置链路是通的。

### 两个配套开关（别忽略）

- **一票否决** `reject_veto`（默认 `true`）：即使 `ANY`，只要有 1 张驳回票就整节点驳回、其余待办作废。
  想「有人同意就过、别人驳回不算」的极端或签，必须同时关 `reject_veto`——那等于放弃内控，慎用。
- **空审批人 / 自审**策略（`empty_assignee` / `self_approval`）：角色没人时按节点配置回退（`SKIP` / `FALLBACK_ADMIN` / …），
  回退后**待办数 = 回退集合人数**，会签人数随之变化 —— 写演练断言时别假设「恒为 1 条待办」。

## 2. 审批意见回写实例 `result_comment`（FIX-9 ①）

**背景**：引擎一直把「同意 / 驳回」的意见写在**待办行** `biz_flow_task.comment`，实例行
`biz_flow_instance.result_comment` 从来没人写。而审批中心 → 实例详情读的是实例行 ⇒ 用户看不到审批人说了什么。

**现在的口径**（`backend/src/business/flow/engine.rs::vote` 第 ⑦ 步）：

```text
实例落终态（APPROVED / REJECTED / …）时：
  result_comment = 「该实例上最新一条非空的、状态为 APPROVED / REJECTED 的待办 comment」
中间态（PENDING，例如驳回到上一节点 / 退回发起人）→ 不写（还在走，没有结论）
```

- 「最新一条非空」而不是「本次这一票」：会签最后一票常常只点同意不写话，严格取本票会让实例又空掉；
- **排除 `CANCELLED`**：会签提前通过 / 驳回时其余待办会被置 `CANCELLED` 并写系统文案
  （「该节点已满足通过条件，无需再投」），那不是人的意见，不能盖掉真话；
- 实现：`backend/src/business/flow/flow_model_instance.rs::latest_verdict_comment`，写入走 `bump_instance` 的
  `COALESCE(?, result_comment)`（不传 = 保留原值，重复审批幂等）；
- 撤回（`WITHDRAWN`）与人工终止仍写入各自的原因文案（「发起人撤回」等），口径不变。

**查库验证**：

```sql
select status, result_comment from biz_flow_instance where id = '<实例 id>';
select status, comment from biz_flow_task where instance_id = '<实例 id>' order by completed_at;
```

## 3. `biz_status_log.operator_name`：写状态日志时补姓名快照（FIX-9 ③）

**背景**：绝大多数状态变更走的是事务体 / model 路径，手上只有 `operator_id`（`Operator::of(Some(id))`），
`operator_name` 一路写空。用户改名 / 销号之后，审计只剩一串 uuid。

**口径**（`backend/src/business/shared/state_machine.rs::insert_log`）：

```text
operator.name 有值（API 路径：令牌里的 username）        → 直接落
operator.name 为空、operator.id 有值                    → 主键查 sys_user 取快照：
                                                          COALESCE(NULLIF(btrim(real_name),''), username)
查不到（历史脏数据 / 号被硬删）                           → 留空，**不阻断**状态变更（日志是旁路）
```

- **快照**语义：今天查到的名字写死在这一行；之后用户改名不会回改历史日志（审计事实不漂移）。
- 查库验证：`select entity_type, count(*), count(operator_name) from biz_status_log group by 1;`
  —— `count(operator_name)` 应等于总行数（除历史遗留行）。
- 联动配置审计（`FLOW_LINKAGE_CFG`）走另一条写入路径（`flow/linkage/model.rs::write_config_audit`），本来就带 `claims.username`。

## 4. 凭证分录摘要里的物料：写**物料编码**，不写 uuid 前缀（FIX-9 ②）

**背景**：采购入库按物料拆分录时，摘要长这样：「采购入库 IN-… 存货 物料 01a1b5d2（第 1 行）」——
`01a1b5d2` 是物料 uuid 前 8 位，人根本认不出是哪个料。

**根因**：`material_inventory_accounts` 把「物料编码」和「该物料的存货科目」绑在同一个 `Option` 里
（`account.map(|a| (code, a))`）。物料**没配** `inventory_account_id`（演示数据 `DEMO-P101/P102` 就是这样，
科目最终退到规则 / 默认 1405）时，编码被一起丢掉 ⇒ 摘要退化。

**现在**：编码与科目解耦——**只要物料行在就一定写编码**，科目缺席只影响科目退化链。

```text
✅ 采购入库 … 存货 物料 DEMO-P101（第 1 行）
✅ 出库 / 销售成本 … 存货 物料 DEMO-P101（第 1 行）
⚠️ 物料已软删 / 不存在 ⇒ 仍出分录（金额在明细上），摘要退回「物料 <uuid 前 8 位>」
```

- 实现：`backend/src/business/erp/voucher.rs::material_inventory_accounts`（入库存货行）与
  `material_label` / `material_codes`（出库 / 收入行，本来就是按编码取的）。
- 回归用例：`voucher::tests::fix9_voucher_summary_uses_material_code_without_inventory_account`（真库，`ERP_DB_URL` 开关）。

## 5. 折旧凭证「同部门多台汇总一行」

口径见 `docs/api-contract-quickref.md` **附录 B**：凭证分录粒度到「部门 × 费用科目」，单台金额只查 `biz_depreciation`。

---

## 6. 审批实例卡住时的标准处置流程（E2E01-FIX-3）

> 「卡住」= 实例还是 `PENDING`，但**再也批不完、也撤不回**：最典型的是会签节点缺人
> （3 人会签，其中 2 人离职/停用），其次是审批人注销、角色被清空、子流程挂死。
> 这类单在界面上没有任何可点的按钮，以前**只能改库**。

### 谁可以处理

`business:flow:admin`（流程管理员）。默认只授 `SUPER_ADMIN`（V0239 §③），
可在「角色管理 → 审批中心 → 终止」勾给其它角色。**不是**发起人，也**不是**审批人。

### 处置顺序（照这个顺序走，别跳步）

| 步 | 动作 | 接口 / 位置 | 说明 |
|---|---|---|---|
| 1 | 能撤回就先撤回 | `POST /api/business/flow/instance/{id}/withdraw` | 只允许**发起人**且**零人已审**；有人投过票会回 `409 + 65023`「已有人审批，不能撤回；请联系流程管理员终止」 |
| 2 | 缺人但还没人投票 → 补齐人 | 流程定义编辑器改节点审批人 / 给角色加成员 → 发布新版本 | 只影响**新发起**的实例；在跑的实例走发起时的快照，改配置救不了它 |
| 3 | 撤不回 → **管理员终止** | `POST /api/business/flow/instance/{id}/terminate`，body `{"reason":"…"}` | 原因必填。实例落 `CANCELLED`、未处理待办全部撤销、**关联业务单据退回草稿** |
| 4 | 单据没跟着退 → 提缺陷 | — | 按 ARCH-8 约定 11，「流程已终止、单据停在 `PENDING_*`」是**代码缺陷**，不许改库绕过 |

### 终止接口到底做了什么（`flow::handler::terminate_instance`）

1. 校验：实例存在（否则 `404 + 65002`）、仍在跑（否则 `400 + 65024`）、原因非空（否则 `400 + 65005`）、
   调用者有 `business:flow:admin`（否则 403）；
2. 事务内 `engine::terminate_by_admin`：实例 `PENDING → CANCELLED` + `ended_at` + `result_comment = 原因`，
   `PENDING` 待办 → `CANCELLED`，`ACTIVE` 节点 → `CANCELLED`，已投过的票**原样保留**（审计要看「谁批过」）；
3. 事务提交后逐条跑**全部业务桥**（`CANCELLED` 分支 = 单据退回），这是「单据不许停在中间态」的落点；
4. 通知：**发起人 + 已审批过的人**（排除操作人）；子流程实例还会回调父流程，避免父节点干等一张死单；
5. 操作日志：`operation_logs` 里一行「终止审批实例」（`request_params` 带原因、`target_id` 是实例 id）。

### 为什么实例状态是 `CANCELLED` 而不是 `TERMINATED`

`biz_flow_instance.status` 的 CHECK 只允许 `PENDING / APPROVED / REJECTED / CANCELLED / WITHDRAWN`，
全仓 15 个业务桥的回调判据都把 `CANCELLED` 当「行政终止」。新增 `TERMINATED` 字面量要改 DDL +
每个桥的判据 + 前端状态表，收益为零、回归面很大。前端 `flow-instance` 域把它显示成**「已取消」**，
详情页底部「结束说明」显示终止原因。

### 已知仍会「卡住」的形态（终止是兜底，不是修复）

- **会签 `ALL` + 角色里有人永远不签**：终止能收掉单子，但根因是流程配置。发布校验 R28（`65048`）
  要求 `ALL` 必须有 `timeout_hours` 或 fallback；`scripts/verify/flow_countersign_invariants.sh` 是常驻体检。
- **`APPROVAL + ROLE + ALL` 错配**（一人签字即可的节点被冻成委员会）：见 V0359 系列迁移的修法。

---

## 附：这几条各自的验收脚本指向

| 口径 | 真库用例 |
|---|---|
| 会签 / 或签投票判定 | `flow::engine::tests`（`judge` 纯逻辑）+ `flow::edge_admin_tests::approval_comment_written_back_to_instance_result_comment` |
| 意见回写实例 | `flow::edge_admin_tests::approval_comment_written_back_to_instance_result_comment` |
| 姓名快照 | `shared::state_machine::tests::operator_name_snapshot_filled_when_only_id_given` |
| 凭证摘要物料编码 | `erp::voucher::tests::fix9_voucher_summary_uses_material_code_without_inventory_account` |
| 审批实例终止（管理员） | `flow::terminate_db_tests`（真库，`ERP_DB_URL` 开关）+ `scripts/verify/e2e01fix3_terminate.py`（145 真机） |
