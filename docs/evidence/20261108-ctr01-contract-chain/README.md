# CTR-01 合同链演练证据（签订 → 审批 → 变更 → 履约 → 到期提醒 → 归档/终止）

- 演练日期：2026-11-09（北京时间）
- 环境：145 测试机、**隔离空库 `cloudpivot_ctr01`**（首启自动跑完全部迁移）、独立实例 `127.0.0.1:18226`
- 代码版本：`762c1c50486104a3830599986077ac095aee4f91`（含本轮修复）
- 二进制：`sha256=00b10a0793daf1ff075c9ee864ecd33493dc5c46d79babba92f0d11699da8303`（`cargo build --bin cloudpivot`，mtime 2026-11-09 00:21:13）
- 迁移数：**283**（空库首启，不克隆模板库）
- 结果：**101 条断言全绿（PASS=101 / FAIL=0）**，原始输出 `ctr01_drill_out.txt`、`raw/` 56 个响应与库快照
- 未部署：145 只做 `git pull --ff-only` + `cargo build`，**未重启 cloudpivot-test**（按铁律由老六手工执行）；未触碰 144 生产

> 一句话结论：**主链是通的**（签订必须先审批、审批后自动落 SIGNED、额度按累计现算并卡边界、到期提醒真会发且幂等、归档后不能再挂单），
> 但卡面写的「四类合同 / 合同变更 / CLOSED」三处与实现有实质差距，另有 1 处文档承诺的终止留痕没实现（本轮已修）。

---

## 1. 演练步骤与实测结果

| # | 卡面步骤 | 实测动作 | 结果 |
|---|---|---|---|
| ① | 建合同（四类各一个） | `POST /api/business/erp/contract` × 4 | PURCHASE ✅ / SALES ✅ / **SERVICE ❌ 400** / **LEASE ❌ 400**（详见 §3 F1） |
| ① | 合同扫描件 | `POST /api/business/attachment/upload`（`doc_type=CONTRACT` `file_role=CONTRACT_SCAN`） | ✅ `biz_doc_attachment` 1 行 |
| ② | 提交审批 → 通过 | `PATCH /contract/{id}/status {target_status:PENDING_APPROVAL}` → 审批人 `POST /flow/task/{id}/approve` | ✅ 10 万采购合同走 **n_pm_hi → n_gm 两级**；5.5 万销售合同走 **n_pm 单级**；终态 SIGNED，再 `SIGNED→ACTIVE` |
| ③ | 合同变更（金额/期限） | 草稿 `PUT`；审批中 `PUT`；已签 `PUT` | 草稿 ✅ 可改；**审批中 409/66016**；**已签 409/66016**；无变更记录表（§3 F2） |
| ④ | 履约跟踪（关联订单 + 金额勾稽） | 采购单 `purchase_type=CONTRACT`、销售单 `contract_id`；额度累计现算 | ✅ 供应商从合同乙方派生、合同号快照落单；33900+66100=100000 **恰好等于合同额放行**，再下 1 元 → **409/66019** |
| ⑤ | 到期提醒 | `sys_job` 有没有任务？手动触发两次 | ✅ 任务存在（status=1）；`POST /business/erp/reminder/contract_expire/run` → `sent=1`，台账+通知+2 个收件人；同日第二次 `sent=0 / skipped_reminded=1` |
| ⑥ | 归档/关闭 | `ACTIVE→COMPLETED`；再挂新单；删除；`ACTIVE→TERMINATED` | ✅ COMPLETED 成功、归档后挂单 **400/66014**、删除 **409/66018**；终止无原因 **400**、有原因 ✅ 且原因落 `biz_status_log.reason` + `remark` |

### 1.1 合同号与状态机（实测）

```
HT-20261109-000001  PURCHASE  DRAFT → PENDING_APPROVAL → SIGNED → ACTIVE → COMPLETED
HT-20261109-000002  SALES     DRAFT → PENDING_APPROVAL → SIGNED → ACTIVE → TERMINATED（原因：客户要求提前终止框架合同）
HT-20261109-000003  PURCHASE  ACTIVE（到期日 2026-12-09 = 业务日 +30，用于提醒）
HT-20261109-000004  PURCHASE  DRAFT（同到期日，用于证明「草稿不提醒」）
```

- 合同号格式 `HT-yyyyMMdd-000001`（`biz_seq(CT)` 取号，与 `biz_contract` 的 `ck_biz_contract_no_charset` 一致）✅
- 状态机 7 条边（`state_machine::CONTRACT_EDGES`）：`DRAFT→PENDING_APPROVAL`、`PENDING_APPROVAL→SIGNED`、`PENDING_APPROVAL→DRAFT`、`SIGNED→ACTIVE`、`SIGNED→TERMINATED`、`ACTIVE→COMPLETED`、`ACTIVE→TERMINATED`
- **没有** `DRAFT→SIGNED` / `DRAFT→ACTIVE`：实测直接打这两条边都是 **409 + 66015**，文案点名「必须先提交审批」（R-P1-1 的内控设计，不是 bug）
- 状态日志（`biz_status_log`）逐条落库：`DRAFT→PENDING_APPROVAL|提交审批`、`PENDING_APPROVAL→SIGNED|合同审批通过（已签订）`、`SIGNED→ACTIVE`、`ACTIVE→COMPLETED`、`ACTIVE→TERMINATED|原因`
- `SIGNED` 之后 **保留** `approval_instance_id`（详情显示「审批通过 FLW-xxx」，审计用）✅

### 1.2 审批（含内控负例）

| 场景 | 实测 |
|---|---|
| 制单人 admin 批自己提交的合同 | **403 + 65019**「不能审批自己提交的合同…（内控开关 `erp.separation_of_duties.approve` 已开启）」 |
| 金额分级（合同 10 万） | 两级：`n_pm_hi`（采购经理）→ `n_gm`（总经理），两轮都 PENDING→APPROVED |
| 金额分级（合同 5.5 万） | 单级：`n_pm`（采购经理） |
| 待办与意见落库 | `biz_flow_task` 两行，`assignee_name` + `comment` 都有快照 |

### 1.3 履约勾稽（卡面重点 ①）

采购合同 `HT-20261109-000001`，合同额 **100000.00**：

| 单据 | 含税金额 | 累计已下单 | 结果 |
|---|---|---|---|
| PO-20261109-000001（100×300，13%） | 33900.00 | 33900.00 | ✅ 建单成功 |
| （尝试 200×300，13% = 67800） | — | 101700.00 | ❌ **409 + 66019**（文案给足四个数：合同额 / 已下单 / 本单 / 下单后累计） |
| PO-20261109-000002（661×100，0%） | 66100.00 | **100000.00** | ✅ **边界相等放行** |
| （尝试 1×1，0% = 1 元） | 1.00 | 100001.00 | ❌ **409 + 66019** |

- 供应商**从合同乙方派生**：`biz_purchase_order.supplier_id` = 合同 `party_b_id`，`contract_no_snapshot` = `HT-20261109-000001` ✅
- 销售侧同口径：`SO-20261109-000001` 33900.00（合同额 55000.00），第二张同额 → **409 + 66019**；销售单详情回带 `contract_no`（SO-27）✅
- 跨类型/跨期拦截：采购单挂 SALES 合同 **400/66014**；销售单挂 PURCHASE 合同 **400/66014**；下单日 2028-01-01 晚于合同到期日 2027-11-01 **400/66014**（文案点名「请续签或改用新合同」）✅
- 额度是**现算 SUM**（`purchase_model::committed_amount` / `sales_order_model::committed_amount`，`status <> 'CANCELLED' AND deleted_at IS NULL`），不是缓存列 → 作废/改单不会漂移

### 1.4 到期提醒（卡面重点 ③）

| 检查 | 实测 |
|---|---|
| `sys_job` 有没有这个任务 | 有：`erp_contract_expire_scan`，`status=1`，`cron=0 30 7 * * *`，`invoke_target=erp_contract_expire_scan` |
| 手动触发（业务接口） | `POST /api/business/erp/reminder/contract_expire/run` → `scanned=1 sent=1 skipped_reminded=0 failed=0` |
| 台账 `biz_reminder_log` | 1 行：`CONTRACT / CONTRACT_EXPIRE / D-30 / 2026-11-09` |
| 通知 `sys_notice` | 1 条：`biz_type=contract_expire`，标题「**合同 30 天后到期：HT-20261109-000003**」 |
| 收件人 | **2 人**（制单人 admin + 负责人/采购经理角色成员 ctr01_approver） |
| 同日第二次触发 | `scanned=1 sent=0 skipped_reminded=1`，台账仍 1 行（幂等真的生效） |
| 草稿合同（同到期日） | 台账 0 行 —— 只有 `SIGNED`/`ACTIVE` 才提醒 ✅ |
| 定时任务页「立即执行」 | `POST /api/support/job/{id}/trigger` → `{"code":0,"msg":"ok"}`（摘要落在 `sys_job_log`，响应体不带） |

档位是 **30 / 15 / 7 / 1 天**（`reminder_contract::STAGES`），本轮实测 D-30 档；四档共用同一段 SQL，差别只是日期参数。

### 1.5 归档 / 终止（卡面重点 ②）

| 场景 | 实测 |
|---|---|
| `ACTIVE → COMPLETED` | ✅ 成功（**注意：此时合同下 2 张采购单仍是 DRAFT、额度已用满 100000 —— 见 §3 F3**） |
| 归档后再挂采购单 | ❌ **400 + 66014**「当前是「已完成」（COMPLETED），只有已签署（SIGNED）/ 生效（ACTIVE）的合同可以下单」 |
| 删除被引用的合同 | ❌ **409 + 66018**，文案列出全部引用单号 `PO-20261109-000001 / 000002` |
| `ACTIVE → TERMINATED` 无原因 | ❌ **400 + 10001**「从 ACTIVE 流转到 TERMINATED 必须给出原因（reason）…」 |
| `ACTIVE → TERMINATED` 带原因 | ✅ 状态 TERMINATED；原因落 `biz_status_log.reason` **且** 落 `biz_contract.remark`（本轮修复，见 §2） |
| 终止后再挂销售单 | ❌ **400 + 66014** |

### 1.6 合同链不产生账务

`biz_voucher` 0 张、`biz_inventory_txn` 0 条（合同与未过账的订单都不记账）。合同状态联动里 `GENERATE_VOUCHER` 被 `linkage::spec::CONTRACT_EXCLUDED_ACTIONS` 显式排除，本轮实测吻合。

---

## 2. 本轮修复（1 处，2 文件）
**F4：终止原因只写了 `biz_status_log.reason`，没按 ARCH-2 §2.1 V-18 写 `biz_contract.remark`。**

- 规格原文（`docs/architecture/ARCH-2-合同表数据模型与校验规则.md:362`）：
  「V-18 终止必须给原因：`to='TERMINATED'` ⇒ `reason` 非空且 ≤ 200 字，**写入 `biz_status_log.reason` 与 `remark`**」
- 修复前实测：`biz_contract.remark` 仍是建单时那条（`CTR-01 销售合同：草稿阶段改金额…`），终止原因只出现在状态日志里；列表页「备注」看不到为什么终止。
- 修复（commit **`762c1c5`**）：
  - `backend/src/business/erp/contract_model.rs`
    - `termination_remark(existing, reason)`：空备注 → `【终止】<reason>`；已含同一段 → 原样返回（幂等）；否则**追加**一行，**不覆盖**签约时写的备注（那是审计事实）
    - `contract_remark_of` / `contract_write_termination_remark`：取数与写库
  - `backend/src/business/erp/contract_handler.rs`
    - `contract_change_status` 在状态 CAS 成功之后、**同一事务内**补写；只在 `CONTRACT_REASON_EDGES`（`SIGNED→TERMINATED`、`ACTIVE→TERMINATED`）上触发；写失败整笔回滚，不留「状态已终止、备注没留原因」的半截单
  - 单测：`contract_model::tests::termination_remark_merges_without_losing_old_text`（覆盖空 / 纯空白 / 追加 / 幂等 / trim 五种口径）
- 修复后实测：`HT-20261109-000002` 的 `remark` =
  ```
  CTR-01 销售合同：草稿阶段改金额 5 万→5.5 万，到期日延到 2027-07-31
  【终止】CTR-01 演练：客户要求提前终止框架合同
  ```
  （`raw/s7_final.txt` + 断言「终止原因回写 remark（V-18：追加且保留原备注） = yes」）

---

## 3. 与卡面 / 文档的出入（findings）

### F1（P2）SERVICE / LEASE 合同**建不了**，卡面的「四类各一个」做不到

- 卡面要求：采购 / 销售 / 服务 / 租赁四类各建一个。
- 实测：`SUPPORTED_TYPES = ["PURCHASE", "SALES"]`（`contract_dto.rs:72`），`SERVICE` / `LEASE` 被**类型闸**拦在建单入口：
  `HTTP 400 code=10001`，文案「contract_type `SERVICE` 本批次未开放：只支持 PURCHASE / SALES」（`raw/s2_srv.json`、`raw/s2_lse.json`）。
- 库层是允许的（`ck_biz_contract_type` 有四个值），DDL 注释也写「采购/销售/服务/租赁」—— 是**应用层闸**没放开。
- 影响：服务类 / 租赁类合同只能被迫归到采购或销售口径，`contract_type` 的统计与「四类合同」报表失真。
- 建议：单开卡（要动乙方主档类型、审批人映射、额度口径），不在本演练卡里顺手开。

### F2（P2）「合同变更」不存在：签后不可改、草稿改单无留痕、无变更记录表

| 环节 | 实测 |
|---|---|
| 草稿阶段改金额/期限 | ✅ 可改（50000→55000，到期日 2027-06-30→2027-07-31） |
| 审批中改金额（「审 A 改 B」） | ❌ 409 + 66016，金额未被改动 ✅（内控是对的） |
| 已签订改金额 | ❌ 409 + 66016，文案：「已签合同的关键字段要改，只能作废（软删）后重签」 |
| 变更记录表 | **没有**（`information_schema` 里 `biz_contract*` 之外无合同相关表） |
| 草稿改单留痕 | **没有**：`biz_status_log` 行数不变（字段变更不是状态变更），`sys_change_log` **0 行**（changelog 模块未接 ERP），只剩 `updated_at` / `update_by` 被覆盖，**改前值永久丢失** |
| 变更是否重新审批 | 无此概念 —— 只有「撤回重提」：`PENDING_APPROVAL→DRAFT` 有边，`SIGNED` 之后**没有任何边回到 DRAFT** |

- 结论：卡面预期的「变更金额/期限 + 重新审批 + 变更留痕」在实现里是**用「作废重签」替代的**；而「作废（软删）」又被 66018 引用守卫挡住（有在途采购单时删不了），实务上只能 `TERMINATED` 后新签一份。
- 这是可辩护的内控设计（不给已签合同开后门），但**草稿改单零留痕**是实打实的审计缺口（谁把 8 万改成 5.5 万、什么时候改的，事后查不到）。

### F3（P2）归档（COMPLETED）不校验履约完成，在途采购单也能归档

- 实测：`HT-20261109-000001` 名下 2 张采购单（`PO-…000001` 33900 + `PO-…000002` 66100，**额度已用满**、**均未提交/未收货**）仍在途时，`ACTIVE→COMPLETED` **直接成功**。
- 归档后新单被拦（✅ 66014），但「合同已完成」与「订单已履约」不是同一件事：按 `status='COMPLETED'` 统计「已履行合同数/金额」会高估。
- 建议：`ACTIVE→COMPLETED` 增加守卫（存在未收货/未关闭的关联订单 → 409 并点名单号），或明确文档写「COMPLETED = 归档，不代表履约完成」。

### F4（P3，**已修**）终止原因未回写 `remark` —— 见 §2

### F5（P3）销售合同仍由**采购经理**审批

- `contract_approve` 流程只有一套（`biz_flow_def.flow_code='contract_approve'`，`biz_type=CONTRACT_APPROVE`），**与 contract_type 无关**。
  节点审批人（只读体检 `tools/probe_assignee.txt`）：
  ```
  n_pm(采购经理审批（< 10 万）)      ROLE=PURCHASE_MANAGER
  n_pm_hi(采购经理审批（≥ 10 万·第一级）) ROLE=PURCHASE_MANAGER
  n_gm(总经理审批（≥ 10 万·第二级）)    ROLE=GENERAL_MANAGER
  条件边：c_amt -[form.amount < 100000]-> n_pm ；c_amt -[default]-> n_pm_hi ；n_pm_hi -> n_gm
  ```
- 实测：销售合同（`HT-20261109-000002`，5.5 万）的待办落到 `n_pm`，`assignee_name` = 采购经理角色成员。
- 影响：销售合同的商务复核落在采购口，销售主管（`SALES_SUPERVISOR`）全程不参与；金额分级阈值（10 万）也是采购口径。

### F6（P3）合同没有「已执行金额」字段/接口

- `ContractDetailRes` 不含 `executed_amount` / `used_amount` / `committed_amount`（实测 `has(...) = false`）。
- 履约进度只能由前端/报表自己 `SUM(biz_purchase_order.total_with_tax)` + `SUM(biz_sales_order.total_with_tax)`；而额度守卫用的是现算 SUM（口径在 `*_model::committed_amount`）。
- 建议：详情补 `committed_amount` / `remaining_amount`（一次 SQL 现算，别加缓存列），前端「履约进度」才有唯一口径。

### F7（P4）到期提醒会双跑：内置 cron 与 `sys_job` 同时启用

- `sys_job.erp_contract_expire_scan.status = 1`（V0245 把 R-P1-2 的种子从「停用视图」改成了启用），同时 `Scheduler::register_builtin` 也注册了同一条 cron。
- 不会重复推送（台账 `biz_reminder_log` 兜底，实测同日第二次 `sent=0`），但每天 07:30 白扫一遍；四类提醒都这样。

### F8（P4）状态名与卡面不一致：实现是 `COMPLETED`，不是 `CLOSED`；也没有 `CANCELLED`

- 卡面「合同状态变 CLOSED」→ 实现 `COMPLETED`；草稿作废走软删而不是 `CANCELLED`（`state_machine.rs` 有专门说明）。
- 不是 bug，但写验收脚本 / 报表时按卡面写 `CLOSED` 会查不到数据。

### F9（P4）66019 文案把「剩余额度」说成「超出合同额」

- 实测文案：「…下单后累计 100001.0000 将超出合同额 **0.0000**」（最后那个数其实是**剩余额度**）。数值都对，语义别扭，容易被当成四舍五入错误。

---

## 4. 单元测试

| 命令 | 结果 |
|---|---|
| `cargo test --lib business::erp::contract`（**真库**：`ERP_DB_URL` → `cloudpivot_ctr01`，`--test-threads=1`） | **69 passed / 0 failed / 0 ignored**（含 `contract_approval_tests` 的提交→两级审批→驳回回位、`contract_linkage_tests`、`contract_pdf` 真库用例），日志 `tests_cargo_contract.txt` |
| `cargo test --lib`（全量，无 `ERP_DB_URL`，DB 用例自跳过） | **3799 passed / 3 failed**，3 条是 `business::flow::linkage::{admin,tests_wiring}` 的 **既有红**（缺 `PURCHASE_RETURN` 触发点，9cf6cc1 引入未同步白名单，与合同无关；RET-01 卡已报同 3 条），日志 `tests_cargo_full.txt` |
| `cargo check --lib` / `cargo clippy --lib` | ✅ 零新增告警（仓库既有 2 条：`receivable.rs` doc 缩进、`sales_order_model.rs` 参数过多，均非本卡） |
| `rustfmt --check --edition 2024` 两个改动文件 | ✅ |

## 5. 复现方式

```bash
# 145 上（需要 /etc/cloudpivot/cloudpivot-dev.env；二进制由 backend/target/debug 提供）
cd /opt/cloudpivot && git pull --ff-only          # 762c1c5
cd backend && cargo build --bin cloudpivot

# 演练（会 drop + create 隔离库 cloudpivot_ctr01，起 127.0.0.1:18226，不碰 8033 的服役实例）
bash docs/evidence/20261108-ctr01-contract-chain/tools/00_env.sh
bash docs/evidence/20261108-ctr01-contract-chain/tools/01_drill.sh   # 期望 PASS=101 FAIL=0
bash docs/evidence/20261108-ctr01-contract-chain/tools/00_env.sh down
```

证据脚本约定：`tools/01_drill.sh` 每步落 `raw/<步名>.json`（响应体，token 已打码）+ `raw/<步名>.txt`（库快照），
断言计数 `PASS/FAIL` 打在末尾；`tools/00b_*`~`00e_*` 是只读体检（种子角色、流程节点与审批人、条件边、`sys_job`、权限码），
输出见 `tools/probe.txt` / `probe2.txt` / `probe_flow.txt` / `probe_assignee.txt`。

## 6. 本轮未覆盖 / 边界

- **SERVICE / LEASE 无法演练**（F1：应用层类型闸未开放），只验到「400 + 点名放行集」。
- 到期提醒只跑 **D-30** 档；15/7/1 档共用同一段 SQL（`end_date IN (?,?,?,?)`），差别仅在日期参数。
- 履约只建单（DRAFT），**没有走收货/收款/开票** —— 那是采购链与 INV-01 发票链的范围（合同链本身不记账，实测 0 凭证）。
- 未验证「审批驳回后重提」「审批中撤回」两条回位边（`PENDING_APPROVAL→DRAFT` 由 `contract_approval_tests` 单测覆盖）。
- 提醒的「多实例并发抢跑」只由台账 + `uk_sys_notice_idem` 保证，本轮单实例未构造并发。
