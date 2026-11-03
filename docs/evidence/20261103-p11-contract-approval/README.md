# P1-1 合同审批流（合同没人审就生效）+ 审批终态 → 单据必须前进巡检

卡片：`t_9df27086`（后端部分）。依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §6.2 R-P1-1
（断链 **DC-02** 合同无审批 + **DC-04** 审批 APPROVED 后单据可能不前进）。

## 1. 问题（实测，改前）

```text
biz_contract 没有 approval_instance_id
→ 合同 DRAFT 点一次「登记签订」直接变 SIGNED（可被采购单引用、可用于付款），全程无人审批
→ 而采购单自己有 po_approve 复核 —— 「给合同付钱」这件事只审了单、没审合同
→ DC-04：审批实例 APPROVED 后单据可能停在待审批态，没有任何巡检能发现
```

## 2. 做了什么（照 ARCH-3 R-P1-1 的四步）

### ① `biz_contract` 加 `approval_instance_id`（迁移 `V0209`，号段先复核过）

后台最大迁移当时是 `V0208`（YEAR-END），卡面写的 `V0207` 已被 period-remind 占用，故本卡用 **`V0209`**。

| 结构变更 | 说明 |
|---|---|
| `biz_contract.approval_instance_id uuid` + `idx_biz_contract_approval`（部分索引） | 提交审批的实例弱引用 |
| `ck_biz_contract_status` 5 值 → **6 值**（加 `PENDING_APPROVAL`） | 状态**值**一个没改，只多一个「审批中」中间态 |
| `ck_biz_contract_approval`（新） | `PENDING_APPROVAL ⟺ approval_instance_id IS NOT NULL`；软删行豁免。挡「审批中却没挂实例」与「非审批中却挂实例」（后者会撞 `uk_biz_flow_instance_biz_active`） |

### ② 预置 `contract_approve` 流程定义（同一条迁移，种子）

```text
START → c_amt(CONDITION: form.amount < 100000)
          ├─ 是 → n_pm   (APPROVAL, ROLE=PURCHASE_MANAGER)  ─────────────→ END   ← < 10 万单级
          └─ 否 → n_pm_hi(APPROVAL, ROLE=PURCHASE_MANAGER) → n_gm(APPROVAL, ROLE=GENERAL_MANAGER) → END  ← ≥ 10 万两级（串行两层，非会签）
```

* 审批人用**角色**（`PURCHASE_MANAGER` / `GENERAL_MANAGER`，V0182 已种），不用固定人；
* `empty_assignee = FALLBACK_ADMIN`（角色里 0 人回退超管，**不静默自动通过**）；
* `self_approval = ALLOW`（内控否决放在审批入口 403 + 待办生成前 SKIPPED 留痕，不是发起时 422）；
* 阈值与审批人全在流程定义里，客户改阈值不用改代码；
* 种子自带 4 层 `WHERE NOT EXISTS` 守卫（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符 → 整条跳过）；
* `graph_hash = 95270c46…75768`，快照 canonical JSON 嵌在迁移里；**行 ↔ 快照 ↔ hash 三方对账全部 OK**
  （见 `raw/03-graph-hash-crosscheck.txt`，口径与 D4d 每日自检 `flow::maintain::run_once` 相同）。

### ③ 合同状态机：5 条边 → **7 条**（`shared/state_machine.rs::CONTRACT_EDGES`）

```text
改前（5 条）                          改后（7 条）
DRAFT   → SIGNED        ✗ 删掉        DRAFT            → PENDING_APPROVAL   （提交审批）
SIGNED  → ACTIVE                      PENDING_APPROVAL → SIGNED             （审批通过 = 登记签订）
SIGNED  → TERMINATED                  PENDING_APPROVAL → DRAFT              （驳回 / 撤回 / 终止审批）
ACTIVE  → COMPLETED                   SIGNED           → ACTIVE
ACTIVE  → TERMINATED                  SIGNED           → TERMINATED
                                      ACTIVE           → COMPLETED
                                      ACTIVE           → TERMINATED
```

* **没有任何边能从 `DRAFT` 一步到 `SIGNED` / `ACTIVE`** —— 「没人审就生效」在状态机层面就不成立；
* `SIGNED` 的唯一入口是审批通过边，`ACTIVE` 的唯一入口是 `SIGNED → ACTIVE`，
  所以「合同可被采购单引用」⟺「它一定被批过」；
* `CONTRACT_LOCKED` 收进 `PENDING_APPROVAL`（审批中不许改字段，杜绝「审 A 改 B」）；
* 单测：`contract_legal_edges_all_pass` / `contract_draft_cannot_reach_signed_or_active_directly` /
  `contract_forbidden_edges_all_rejected`（23 条禁止边 + 36 种组合全扫描）/ 自环 / 终态 / reason 边 / locked。

### ④ `erp_invariants.py` 新增 H4 巡检（卡面写 H3，号段已被「待办可追溯」占用）

`flow_instance.status='APPROVED'` 而单据仍在**待审批态** → **BLOCKER（P0）**。按 `biz_type` 逐类给真实状态值：

| biz_type | 单据待审批态 |
|---|---|
| `PO_APPROVE` | `biz_purchase_order.status = SUBMITTED` |
| `STOCK_IN_POST` | `biz_stock_in.status = PENDING_POST` |
| `STOCK_IN_CANCEL` | `biz_stock_in.status = PENDING_CANCEL` |
| `PAYABLE_CANCEL` | `biz_payable.status = PENDING_CANCEL` |
| `CONTRACT_APPROVE` | `biz_contract.status = PENDING_APPROVAL`（本卡新增） |
| `VOUCHER_REVERSE` | 凭证 status 恒 `POSTED`，待审批由 `pending_reverse_instance_id` 表达 |

## 3. 服务端改动清单

| 文件 | 改什么 |
|---|---|
| `business/shared/state_machine.rs` | `CONTRACT_EDGES` 7 条、`CONTRACT_BASE` 7 条、`CONTRACT_LOCKED` 加审批中、`CONTRACT_PENDING_APPROVAL` 常量；单测同步 |
| `business/erp/contract_bridge.rs`（**新** 500 行） | 审批回调（APPROVED → SIGNED / 驳回撤回 → DRAFT）、`published_contract_flow`、`guard_self_approval`（403+65019）、`internal_control_creator`、`cancel_approval_of` |
| `business/erp/contract_handler.rs` | `PATCH /status` 目标态 `PENDING_APPROVAL` = **提交审批**（起流程 + 回写实例，同事务）；`DRAFT→SIGNED/ACTIVE` 驳回文案点名「先提交审批」；审批中撤回先终止实例；软删时终止在跑审批；详情/列表下发 `approval_instance_id` / `approval_instance_no` |
| `business/erp/contract_model.rs` | 6 状态常量、`LEGAL_EDGES` 7 条、`approval_instance_id` 进列清单/行结构、`contract_apply_approval_transition`（状态+实例同写，CAS） |
| `business/erp/contract_dto.rs` | 响应加 `approval_instance_id` / `approval_instance_no`；`ContractStatusReq` 混入 `SkipApprovalReq` |
| `business/flow/handler.rs` | 撤回 / 同意 / 驳回三个出口都调 `contract_bridge::on_contract_settled`；审批入口加合同内控闸 |
| `business/flow/internal_control.rs` | `CONTRACT_APPROVE` 分支：制单人从待办候选人里排除（落 SKIPPED 留痕） |
| `business/erp/contract_approval_tests.rs`（**新** 9 条真库用例） | 提交/单级/两级/驳回/撤回/跳签被拒/缺日期/流程下线/收尾 |
| `migrations/business/V0209__erp_contract_approval.{up,down}.sql` | 见上文 ①②（up 自检失败即迁移失败） |
| `scripts/verify/erp_invariants.py` | 新增 H4 |
| `scripts/verify/p11_contract_approval_e2e.py`（**新**） | 真机验收脚本（8 个 phase / 30 条断言） |

**前端一个文件都没改**（审批块由下一张卡做）。副作用：合同页会多出一个 `PENDING_APPROVAL` 去向按钮
（后端 `next_statuses` 直接下发），文案走 `contractStatusLabel` 兜底为「PENDING_APPROVAL」灰色徽标 ——
这是预期的过渡态，下一张卡换成正式的「提交审批 / 审批过程」区块。

## 4. 验收结果（全部本卡实测）

| 验收项 | 结果 | 证据 |
|---|---|---|
| 迁移 up/down/up 三轮 | ✅ 3 轮全过（幂等；终态 def=1 / nodes=6 / edges=6 / 两个 CHECK 都对） | `raw/04-migration-up-down-up.txt` |
| 合同状态机单测覆盖新边 + 非法边 | ✅ 7 条边逐条 + 23 条禁止边 + 36 种组合全扫描 + 自环 + 「DRAFT 到不了 SIGNED/ACTIVE」专项 | `cargo test --lib contract_` → 70 passed / 0 failed |
| 真机：新建 → 提交审批 → 审批通过 → 才 ACTIVE | ✅ 30/30 断言通过（HT-20261103-000051 走完 DRAFT→审批中→已签订→已生效） | `raw/01-e2e-real-machine.txt` |
| 真机：不审批直接点生效 → 被拒 | ✅ `409 + 66015`，报文点名「先提交审批（target_status=PENDING_APPROVAL）」 | 同上 phase ② |
| 不变量新增 H4 能抓到脏数据 | ✅ 手工把实例改 APPROVED 后 → H4 列为 BLOCKER 并点名单据；清掉后回到 37/37 全绿 | 同上 phase ⑧ |
| cargo test 全绿 | ✅ **2137 passed / 0 failed / 0 ignored**（EXIT=0，967s，`--test-threads=1`，干净库 cp_p11_full）。本卡新增 10 条用例：9 条合同审批真库用例（`contract_approval_tests`）+ 1 条状态机专项（`contract_draft_cannot_reach_signed_or_active_directly`）；上一卡基线 2125，本卡实测 2137（差额 12 含 10 条新增用例与父卡基线口径差 2，按实测报告） | `raw/05-full-lib-tests.txt` |
| 不变量全绿 | ✅ **37/37**（原 36 条 + 新 H4），BLOCKER 0 / WARN 0 | `raw/02-invariants-37.txt` |
| 前端合同页面不改 | ✅ `git diff --stat` 里 frontend/ 0 个文件 | |

其它门禁：`cargo check --lib` / `--tests` 零警告；`cargo clippy --lib` 零告警；
本卡改动的 9 个 .rs 文件 `rustfmt +1.94.1 --edition 2024 --check` 干净。

## 5. 与卡面/文档的偏差（都按「以代码与 DDL 为准」处理，逐条说明）

1. **迁移号 `V0207` → `V0209`**：卡面写 V0207 并要求「先复核号段」，实测 V0207/V0208 已被
   period-remind / year-end-remind 占用。
2. **H3 → H4**：`erp_invariants.py` 的 H3 已是「待办必能追溯到任务」，新巡检排到 H4（判据与卡面一致，
   只是编号不撞）。原 36 条 + H4 = 37 条（卡面写「原 32 条」，那是更早的基线）。
3. **「不审批直接点生效 → 400」实测是 409**：合同模块既有的非法边约定是 `409 + 66015`
   （A2 卡面写 400 时也被改成 409 并在文件头记录）。本卡沿用 409，但**报文专门点名**
   「必须先提交审批（target_status=PENDING_APPROVAL）」；真机断言按「被拒」口径（409）验证。
4. **审批通过落 `SIGNED` 而不是 `ACTIVE`**：卡面写「DRAFT → ACTIVE（或 SIGNED）」。选 SIGNED 的理由：
   ① 合同的「已签订」是法律事实，审批通过 = 同意签订；② `ck_biz_contract_signed_dates` 要求 SIGNED 三件套；
   ③ ACTIVE 的唯一入口保持 `SIGNED → ACTIVE`（生效），审批链不被绕过。真机 phase ⑥ 证明了
   「审批通过 → SIGNED → 点生效 → ACTIVE」整条链，且**未审批时两步都进不去**。
5. **状态枚举多了一个 `PENDING_APPROVAL`**：卡面说「不改 A1 定义的枚举值（只加边）」，但 ARCH-3 §6
   同时要求「加一条 DRAFT → 待审批过渡」。实现上**原有 5 个值的语义与字符串一个没动**，只新增
   「审批中」中间态并把 `ck_biz_contract_status` 扩成 6 值白名单（与收货单 `PENDING_POST` 同款）。
6. **合同不提供「跳过审批」旁路**：采购单/收货单没流程时可用 `business:erp:skip-approval` + 理由放行，
   因为它们的边表里有「不走审批也能推进」的边；合同删掉这条边正是本卡的目的，所以没流程时
   一律 `409 + 65020`（点名 `contract_approve`）。种子迁移保证正常库不会走到这里。

## 6. 用到的库 / 清理

| 库 | 用途 |
|---|---|
| `cp_p11` | 真机验收（隔离实例 127.0.0.1:18097）：合同审批全链 + H4 造脏数据 |
| `cp_p11_mig` | V0209 up/down/up 三轮 |
| `cp_p11_full` | 全量 lib 单测（`--test-threads=1`）+ 不变量 37/37（跑前是干净库） |

`144 / 生产库`：零触碰。共享工作区只做加法（本卡所有改动都在 `backend/src/business/{erp,shared,flow}`、
`backend/migrations/business/`、`scripts/verify/`、`docs/evidence/` 之下）。
