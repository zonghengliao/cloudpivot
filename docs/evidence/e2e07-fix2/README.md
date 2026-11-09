# E2E07-FIX-2 · CONTRACT 零联动规则（全系统扫描 + 预置规则补齐 + 不变量）

卡：`t_a7152992`（P3，E2E-07 发现）
分支：`origin/main`（本卡提交 `0dad364d` + 证据补丁）
环境：145 测试机（`cloudpivot_dev`，真后端 `127.0.0.1:8033`）+ 99 全新空库（迁移链 / 不变量牙齿）

## 问题

E2E-07 演练发现：`LNK-07` 把合同状态变更接进了联动引擎（`contract_linkage.rs` 三个状态推进点
真的在调 `engine::run_excluding` / `run_deferred`），`spec::TRIGGER_SPECS` 也标了 `wired = true`，
**但 `biz_flow_linkage_rule` 里 CONTRACT 规则 0 条** ⇒ 合同签订 / 终止**一条通知都不发**，
且没有任何报错。卡面那句「两道审批路径只触发一次联动」在默认配置下**无从体现**。

按卡面要求做了**全系统扫描**（不只合同）：16 个 wired 触发点里只有 4 个有存活规则
（`STOCK_IN×2` / `FIXED_ASSET×3` / `PURCHASE_RETURN×1` / `TASK×1`），其余 12 个都是
「接线了但零规则 = 能力不存在」，其中两个是**存量被破坏**：

| 触发点 | 145 扫描时的状态 | 说明 |
|---|---|---|
| `CONTRACT` | 0 条 | 本卡主角（LNK-07 接线了，规则 0 条） |
| `STOCK_OUT` / `STOCK_TRANSFER` / `PURCHASE_ORDER` / `PAYABLE` / `VOUCHER` / `ACCOUNTING_PERIOD` / `ACCOUNTING_YEAR` / `STOCK_TAKE` / `DEPRECIATION` / `EXPENSE_REPORT` | 0 条 | 与 FIX-3 的 `FIXED_ASSET` 同病：从来没人种过预置规则 |
| `TASK` | 0 条 | V0254 的 `TASK/DOING` 基线行在演练清场时被**硬删** |
| `FLOW_INSTANCE` | 1 条（**软删**） | LNK-12 取证收口时把 `APPROVED` 那行软删了 ⇒ enabled = 0 |

扫描原始输出：`coverage_before_145.txt`（17 行，含未接线的 `SALES_RETURN` 作对照）。

## 修复

### ① 迁移 `V0360__linkage_rules_coverage`：补 15 条预置规则

| # | 规则名 | 触发点 | 说明（为什么是这个状态） |
|---|---|---|---|
| 1 | 合同签订 → 通知制单人 | `CONTRACT/SIGNED` | 审批通过的唯一目标态（两条审批路径都 CAS 到它） |
| 2 | 合同终止 → 通知制单人 | `CONTRACT/TERMINATED` | 需要 `reason` 的那条边（会动已生效承诺） |
| 3 | 出库过账 → 通知制单人 | `STOCK_OUT/POSTED` | 出库只在 `post_out_tx` 过账时接引擎 |
| 4 | 调拨完成 → 通知制单人 | `STOCK_TRANSFER/DONE` | 六态都接，选终态（中间态各发一条就是刷屏） |
| 5 | 采购订单全部收货 → 通知制单人 | `PURCHASE_ORDER/RECEIVED` | 业务终态；`APPROVED` 由审批流程节点负责 |
| 6 | 应付结清 → 通知制单人 | `PAYABLE/SETTLED` | 付款事务只在这条边上接引擎 |
| 7 | 凭证记账 → 通知制单人 | `VOUCHER/POSTED` | 凭证自身状态流转（来源单据路径不重复触发） |
| 8 | 会计期间结账 → 通知操作人 | `ACCOUNTING_PERIOD/CLOSED` | 开账是批量补行，挂上就是启动扇出 N 条通知 |
| 9 | 会计年度结转 → 通知操作人 | `ACCOUNTING_YEAR/CLOSED` | 年结唯一终态 |
| 10 | 盘点过账 → 通知制单人 | `STOCK_TAKE/POSTED` | 过账 / 作废两态，选过账 |
| 11 | 折旧过账 → 通知制单人 | `DEPRECIATION/POSTED` | `CALCULATED` 是同事务中间态、`SKIPPED` 是空跑，都没有触发源 |
| 12 | 报销付清 → 通知制单人 | `EXPENSE_REPORT/PAID` | 钱付出去了；`SUBMITTED/APPROVED` 由审批流程负责 |
| 13 | 任务开工 → 通知负责人与创建人 | `TASK/DOING` | V0254 基线键（被清场硬删，这里种回） |
| 14 | 审批通过 → 通知发起人 | `FLOW_INSTANCE/APPROVED` | 145 那行是取证残留（软删），`DO NOTHING` 不复活 |
| 15 | 审批驳回 → 通知发起人 | `FLOW_INSTANCE/REJECTED` | 新键：保证「审批结论至少有一种会通知」 |

口径（与 V0254 / V0323 / V0351 一致）：

* 动作 **只用 `SEND_NOTICE`**（这些触发点上动账 / 凭证动作由各自专用路径落账，其余动作是占位）；
* 参数 **`{"mode": "TAKEOVER"}`**：`SHADOW` 只做影子对比、**不投递**，而这些触发点没有旧代码在发通知，
  写成 SHADOW 等于一条不发；**不写 `recipient`** = 沿用 `*_linkage.rs` 算好的收件人（零重复真相），
  标题 / 正文同理不覆盖（草稿里已带人读文案 + 详情页链接）；
* 参数键只用 `SEND_NOTICE` 声明的 `mode / title / content / force / recipient`
  （卡面示例里的 `template` / `to` 不在动作契约里，会被第 2 层校验 400 —— 已按真实契约改写）；
* `ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING`：运维手工配的 / 停用的 /
  软删的行**原样保留**；`down` 不删 `TASK/DOING`（属 V0254 的基线能力）。

### ② 不变量 `scripts/verify/linkage_rule_invariants.sh`（4 条断言，rc=1 逐条点名）

| # | 断言 |
|---|---|
| ① | 每个 wired 触发点至少 1 条 `enabled AND deleted_at IS NULL` 规则（**接线了但零规则 = 能力不存在**） |
| ② | enabled 规则不能挂在**未接线**触发点上（配得上、永远不触发） |
| ③ | `(触发点, 状态)` 必须是该触发点的真实状态（状态写错 = 永远等不到） |
| ④ | 动作必须已接管（占位动作配了只被 Skipped），且不是该实体结构性被禁的（合同出凭证 / 凭证生凭证 / 审批实例落账） |

脚本里写死的 `WIRED_TRIGGERS` / `STATUS_TABLE` / `IMPLEMENTED_ACTIONS` / `FORBIDDEN_PAIRS`
由单测 `linkage::tests_presets_coverage::invariants_script_*` **双向**钉住（脚本过期 ⇒ 单测红）。

### ③ 代码侧漂移守卫（15 条单测）

`backend/src/business/flow/linkage/tests_presets_coverage.rs`：

* `every_wired_trigger_has_a_preset` —— **本卡的核心闸门**：`spec::wired_triggers()` 与全部预置迁移
  原文对拍，新接一个触发点却忘了种规则 ⇒ 立刻红；
* `presets_only_for_wired_triggers` —— 不许给未接线 / `TRIGGER_EXEMPT` 实体种规则；
* 其余：状态真实 / 动作已接管 / 动作未被结构性禁止 / 参数过第 2 层 / TAKEOVER / 键唯一 /
  迁移文本含每行 / down 删同一批键（TASK 例外）/ 脚本四张表双向一致。

### ④ 真库夹具适配

各模块真库用例的 `add_rule` 夹具改成 `ON CONFLICT … DO UPDATE`：预置规则可能已占用同一键，
裸 `INSERT` 会撞 `uk_flow_linkage_rule`。**不是放宽断言** —— 夹具接管该键、收场仍按自己的前缀清理。

## 验证（全部真跑）

| 验收 | 证据 | 结果 |
|---|---|---|
| ① 触发点规则覆盖表 | `coverage_before_145.txt`（补前） / `rules_and_notices_145.txt`（补后） | 补前 12 个触发点 enabled=0；补后 16/16 都有 ≥1 条 |
| ② CONTRACT 补 2 条规则 | `rules_and_notices_145.txt` 首两行 + probe 第 1 节 | `CONTRACT/SIGNED`、`CONTRACT/TERMINATED` = `enabled + TAKEOVER` |
| ③ 实测：合同签订 → 产生通知 | `contract_notice_145.txt`（真 token / 真 HTTP，PASS=20 FAIL=0） | 审批通过 → 合同 `SIGNED`，`sys_notice` 落 1 条 `LNK:CONTRACT:SIGNED:<id>`，收件人 = 制单人；重复审批 `idempotent=true` 且通知仍 1 条；终止 → 再落 1 条 `LNK:CONTRACT:TERMINATED:<id>`；合同名下 0 张凭证 |
| ④ 其他零规则触发点的处置 | 迁移 V0360 头部表格 + 本文「修复①」表格 | 12 个全部补上（每个落在真实状态上）；未接线的 `SALES_RETURN` 等**不种**（配了永远不触发） |
| ⑤ 不变量能发现「wired 但零规则」 | `invariants_teeth.txt`（一次性库逐条弄红→还原复绿） | 停用 CONTRACT 两条 → ① 点名 CONTRACT 报红；未接线触发点插规则 → ②③ 报红；状态改成 `PAID` → ③ 报红；占位 / 被禁动作 → ④ 报红；还原后 4 绿 |
| ⑥ cargo test + push | 见卡内汇报 | 新增 15 条单测全绿；全量 `cargo test --lib` 见汇报的实测数字 |

迁移链与往返：

* 99 全新空库 `mig_replay_fix2a/fix2b`：**282/282 条迁移按版本号升序全部通过**（含 V0360 自检）；
* `V0360 down → up` 往返：down 后 7 条存活、不变量 ① 点名 12 个缺口；up 后 21 条复活、4 绿；
* 145：`git pull 0dad364d` → `cargo build` → `systemctl restart cloudpivot-test` → health 200 →
  V0360 自动应用（21 行规则，`FLOW_INSTANCE/APPROVED` 的取证残留按 `DO NOTHING` 保留）→ 不变量 4 绿。

## 残留 / 已知取舍

* `FLOW_INSTANCE/APPROVED` 在 145 上仍是软删状态（LNK-12 取证残留，`DO NOTHING` 不复活）；
  `FLOW_INSTANCE` 的覆盖率由新增的 `REJECTED` 规则兜住。全新库上 `APPROVED` 是启用的。
* 预置规则是**新增通知**：这些触发点以前一条通知都不发。客户若嫌吵可在
  「流程配置 → 跨单据联动」停用（停用后不变量 ① 会点名 —— 那是配置事实，不是 bug；运维需知情）。
* 144 生产未部署：本卡含新迁移 **V0360**，下次 `scripts/deploy/deploy.sh` 随发布应用。

## 真库联动用例（本卡改过夹具的 11 个模块）

99 工作区里有另一张卡未提交的 `V0360__receipt_refund_approval_instance`（**撞号**，起实例直接
`FATAL: duplicate up migration for version 360`），所以真库用例改成跑
**145 dev 库的副本**（`pg_dump cloudpivot_dev → 99 cloudpivot_copy_fix4`，不是原库）：

```
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_copy_fix4   # SYS/TASK 同库
cargo test --lib linkage -- --test-threads=1
→ test result: ok. 192 passed; 0 failed; 0 ignored  （见 db_linkage_suite.txt）
```

覆盖：`contract_linkage_tests` / `period_linkage_tests` / `voucher_linkage_tests` /
`expense_linkage_tests` / `payable_linkage_db_tests` / `purchase_linkage_db_tests` /
`transfer_handler::db_linkage` / `depreciation_linkage_tests` / `year_end_linkage_tests` /
`stocktake_tests` / `instance_linkage_db_tests` + 全部纯逻辑用例（`tests` / `tests_presets` /
`tests_wiring` / `tests_presets_coverage`）。

**两处用例口径修正**（不是把断言放宽成假绿）：`payable_linkage_db_tests::
excluded_payable_balance_action_does_not_double_deduct` 与 `purchase_linkage_db_tests::
excluded_received_qty_action_is_not_executed_by_the_generic_engine` 原来断言
「带排除清单跑一遍 ⇒ `runs` 与 `deferred` **全空**」。预置规则补齐后，同一个触发点上还合法地
有一条 `SEND_NOTICE`（提交后动作）⇒ 整个 run 不再为空。断言改成
「**被排除的那些动作**没有执行记录」（`runs` + `deferred` 里逐条按排除清单过滤），
「钱只扣一次 / 累计量不被二次推进 / 流水只有一条」这些真正的判据一条没动；
两个用例下面原有的「不传排除清单 ⇒ 引擎真的会跑它」的反证也保留。

## 已知问题（不是本卡引入，但会挡住所有人）

99 共享工作区里 `backend/migrations/business/V0360__receipt_refund_approval_instance.{up,down}.sql`
是**另一张卡未提交**的 WIP，与已上 main 的 `V0360__linkage_rules_coverage` 撞号 ⇒ 在被测树上
起实例直接 FATAL（迁移器要求 version 全局唯一）。已在看板留 `hotspot:` 记录，
**需要那张卡改用 V0361+**（V0360 已被 145 应用，改名会破坏已应用迁移的指纹校验）。
