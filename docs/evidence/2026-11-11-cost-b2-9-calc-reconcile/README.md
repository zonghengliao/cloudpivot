# COST-B2-9【成本】calc 第 4 段：池与 4101 对账 —— 证据

- 卡：`t_59cae68b`（COST-B2-9），批次 B2，前置 B2-8（费归集，`t_89836f59`）
- 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
  - §4.11 C-COST-1：当期 `Σ biz_cost_collect(OVERHEAD, 池行)` = 当期 `4101.*` **借方发生额**（按 (车间, 科目) 分组），BLOCKER
  - §4.4.3 ① 池口径；§4.8 ④ 结算事务（手工台账的「借 4101 / 贷 对方科目」凭证在**结算时**生成）
  - §8.1 B2-9 行：`calc 第 4 段：池与 4101 对账 / 按 (车间,科目) 对账，差额列凭证号`
  - §8.5 R-3：4101 旁路记账 → C-COST-1 红、结算被拦、错误列凭证号、引导改走成本模块
- 范围：**只有**「池 ↔ `4101.*` 借方发生额逐 (车间, 科目) 对账 + 不平 409 列凭证号」。
  不做分摊（B2-10）、不做约当与计算单（B2-11）、不做未归集清单接口（B2-12）、不改任何已应用迁移。

## 1. 改了什么

| 文件 | 类型 | 内容 |
|---|---|---|
| `backend/src/business/erp/cost_reconcile_dto.rs` | 新增（纯逻辑，无库） | `Stage`（Calc/Settle 与「该来源此刻该不该有凭证」判据）、`PoolSideRow` / `LedgerSideRow`（两侧输入行）、`ReconcileGroup` + `GroupKind`（Balanced / Pending / LedgerOnly / PoolOnly / Mismatch）、`ReconcileOutcome`（池 / 应已入账 / 待结算过账 / 账面 / 差额 / 不平组数 / 差额凭证号）、`reconcile()` 纯函数、`ensure_balanced()`、`mismatch()`（409+`66065` 报文）；**14 条单测** |
| `backend/src/business/erp/cost_reconcile_model.rs` | 新增（数据层） | `POOL_BY_SOURCE_SQL`（池行按 (车间,科目,来源) 汇总，科目从三张来源表反查）、`LEDGER_DEBIT_SQL`（当期 `4101.*` 借方逐条分录，带凭证号与 `aux_dept_id`）、`pool_by_source()` / `ledger_debits()`；**5 条 SQL 形状单测** |
| `backend/src/business/erp/cost_calc_reconcile.rs` | 新增（段编排） | `reconcile_pool_with_ledger(period, stage)`（自开事务，不平 409）+ `reconcile_in_tx(ex, period, stage)`（供 calc / B3-6 结算事务内复用，不提交不报错，交回结论）；**3 条单测** |
| `backend/src/business/erp/cost_calc_reconcile_db_tests.rs` | 新增（真库用例，`ERP_DB_URL` 开关） | **10 条**：逐组对平 / 旁路记账点凭证 / 无车间 / 红冲与红字与 DRAFT 与 5602.* 不计 / 上期不串 / 金额不符 / 总额相抵仍报错 / 与费归集段池互校 / 空期间 / 阶段口径 |
| `backend/src/shared/error_code.rs` | 改动 | 新增 `ERP_COST_POOL_LEDGER_MISMATCH = 66065`（409）+ 为什么不复用 66062/66064 的理由 |
| `backend/src/business/erp/mod.rs` | 改动（登记） | 4 个新模块 + 注释（6 行） |

**无迁移**：`biz_cost_collect`（V0371）、`biz_voucher` / `biz_voucher_entry`（V0149）全部已应用，
本卡一字未改 ⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
**无 HTTP 入口、无前端**：设计 §5.2 只有 `POST /business/erp/cost-period/calc`（#27，属 calc 编排卡），
段卡与 B2-6/7/8 同构只交付段函数 ⇒ `pnpm typecheck/build` N/A。

## 2. 本卡的口径（全部规则）

1. **池侧**：`biz_cost_collect` 里 `element='OVERHEAD'` + `cost_object_id IS NULL` + 未软删的
   **三种费来源**池行，按 (车间, 科目, 来源类型) 汇总。`collect` 表没有 `account_code` 列
   （V0371 结构事实），科目从三张来源单据反查：`biz_cost_overhead.debit_account_code` /
   `biz_depreciation.expense_account_code` / `biz_expense_item.account_code`。
   `ALLOC`（分摊结果）是池的**去向**不是来源，不进池（否则自我循环）。
2. **账面侧**：`biz_voucher_entry` 的 `4101.*` **借方**分录，条件四条 —— `v.period=?`
   （期间口径）、`v.status='POSTED'`、`v.deleted_at IS NULL`、`v.is_red=0 AND v.reversed_by IS NULL`。
   - 只算**借方**：C-COST-1 判据就是「借方发生额」。用净额（借-贷）会在结算阶段被分摊凭证的
     4101 贷方清零，变成「结算后池有数、账面 0」的永久假差。
   - 排除**红字凭证**（其 4101 行落贷方，本就不进借方）与被**红冲的原凭证**（`reversed_by` 非空）：
     折旧红冲后归集行已软删，若原凭证借方还在，就成永久假差。
3. **逐 (车间, 科目) 分组求差，不看总额**：总额相等是假绿最常见的来源（A 车间多 100、B 车间少 100
   会互相抵掉）。真库用例 [`offsetting_groups_do_not_cancel_out`] 专门造了总额 0、两组不平的场景。
4. **凭证没挂部门（`aux_dept_id IS NULL`）单独成组**：那正是「绕过成本模块记账」最典型的形态，
   报文写「（无车间）」并点名凭证号。
5. **不平 ⇒ 409 + `66065`**，报文必须回答三件事：差多少（总额 + 逐组，正负号含义写明）、
   差在哪几张凭证（凭证号，去重升序）、怎么修（改走费用台账/报销，或改记别的科目；
   账面少的那部分提示查来源单据是否已过账）。

### 2.1 ★ 阶段口径（本卡唯一的判断取舍，已具名）

设计 §4.3 的费三路里，**折旧 / 报销的凭证随来源单据过账生成，手工台账的「借 4101 / 贷 对方科目」
凭证在结算时才生成**（§4.8 ④，与分摊凭证同一事务）。因此 calc 时刻三路池里必然有一段
账上还没有的手工台账金额：

- 把它当差错 ⇒ 每一张有手工台账的期间都红，财务只能去删台账；
- 把它当不存在 ⇒ 结算时 C-COST-1 形同虚设。

落地办法是把它**显式命名**为「待结算过账」（`ReconcileOutcome::pending_amount`），并用
`Stage` 分开两个时点：

| 阶段 | 谁调用 | 要求账面背书的来源 | 手工台账池行 |
|---|---|---|---|
| `Stage::Calc` | calc 第 ④ 步（本卡） | 折旧 + 报销 | 计 `pending_amount`，**不算差错** |
| `Stage::Settle` | B3-6 结算事务内自检 | 全部来源（含 `OVERHEAD_TICKET`） | 必须有账面背书，否则 `PoolOnly` 报错 |

两个阶段**共用同一份代码与同一条判据**（逐组差额为 0），不存在两套算法。真库用例
[`settle_stage_requires_the_manual_ticket_to_be_booked`] 证明了同一批数据「Calc 平 / Settle 不平 /
补上第四张凭证后平」。

## 3. 实测输出

### 3.1 纯逻辑单测（`01_unit_tests.out`）

```
=== cargo fmt -- --check ===        FMT_EXIT=0
=== cargo test --lib cost_reconcile ===        19 passed; 0 failed
=== cargo test --lib cost_calc_reconcile ===   13 passed; 0 failed   （含 10 条真库用例无 ERP_DB_URL 时 SKIP）
```

### 3.2 真库用例（`ERP_DB_URL=…/costb26_drill_1625844`，`--test-threads=1`，`02_real_db_tests.out`）

```
running 10 tests
... every_booked_route_is_balanced_and_manual_ticket_is_pending ... ok   # 池 1450（应已入账 1300 + 待过账 150），账面 1300，差额 0，三组
... bypass_voucher_is_flagged_with_its_voucher_no ... ok                 # 旁路 200 ⇒ 409+66065，报文点名凭证号与 4101.04
... voucher_without_department_is_flagged ... ok                         # 无车间 4101.09 ⇒ LedgerOnly，标签「（无车间）」
... reversed_red_draft_and_non_4101_vouchers_are_ignored ... ok          # 红冲/红字/DRAFT/5602.* 账面 0、组 0
... previous_period_voucher_does_not_leak ... ok                         # 上期 500 不进本期
... partial_booking_is_a_mismatch ... ok                                # 1000 vs 800 ⇒ 差额 200
... offsetting_groups_do_not_cancel_out ... ok                           # 总额 0、两组不平 ⇒ 仍报错
... pool_by_source_matches_the_collect_segment_pool ... ok               # 本段池侧 = 费归集段 pool_of_period（逐组金额 + 组数 + 笔数）
... empty_period_is_balanced ... ok                                      # 0 组、平、不报错
... settle_stage_requires_the_manual_ticket_to_be_booked ... ok          # Calc 平 / Settle 差 150 / 补凭证后平
test result: ok. 10 passed; 0 failed; ... finished in 13.79s
```

跑完演练库**零残留**（`04_residue.out`：`biz_voucher` / `biz_voucher_entry` / `biz_cost_collect` /
`biz_depreciation` / `biz_depreciation_batch` / `biz_expense_report` / `biz_cost_overhead` /
`biz_fixed_asset` 计数全部为 0）。

真实报文（用例 ② 打印，`--nocapture`）：

```
费用池与总账 4101.* 借方发生额对不平（期间 2100-01，计算口径）：池 1000，应已入账 1000，账面 1200，
差额 -200（= 应已入账 − 账面，负数表示账面多），共 1 组不平。
· 总部 / 4101.04：池 0（应已入账 0，待结算过账 0），账面 200，差额 -200｜凭证：B29-E73C082E9B84-2100010｜账面多（旁路记账）
账面多出来的 4101.* 分录是绕过成本模块直接记的制造费用，成本核算看不见它们，结算后 4101 清不掉。
请把这类费用改走「费用台账（无单据）」或「费用报销」后重算本期成本；若该笔本来就不是制造费用，
请改记对应费用科目。账面少的那部分说明费用已在成本模块归集、4101 凭证却没生成，请检查来源单据是否已过账。
```

### 3.3 门禁（`03_gates.out`）

| 命令 | 结果 |
|---|---|
| `cargo fmt -- --check` | 干净（`FMT_EXIT=0`） |
| `cargo clippy --all-targets -- -D warnings` | 0 warning / 0 error（`CLIPPY_EXIT=0`） |
| `cargo test`（全仓） | **33 targets / 4851 passed / 0 failed**（`01_full_cargo_test.out`，EXIT=0；基线 B2-8 为 4819，本卡新增 32 ⇒ 4851） |

## 4. 已知偏差 / 未做

1. **阶段口径**（§2.1）：Calc 阶段不要求手工台账有账面背书。这不是漏做，是设计 §4.8 ④ 的时序事实
   （第四张凭证在结算时才过账）。若产品要求「calc 时就必须把手工台账凭证打出来」，那是结算事务
   提前到 calc —— 属于编排卡（#27）的范围，本卡不擅自改时序。
2. **期间归属不一致会被判为不平**（有意）：池按 `biz_expense_report.period_code` 归期（B2-8 口径），
   而报销凭证的 `biz_voucher.period` 取**审批通过日**的期间。跨月审批（9 月单 10 月审）会让池在 9 月、
   凭证在 10 月 ⇒ 两边各报一组不平。这符合设计 §4.3 口径说明 ②「结算批次的 period_code 必须与凭证
   `voucher_date` 落入的期间一致」，报文里已提示「检查来源单据是否已过账」，但**没有**自动按凭证日期
   反推归期（那会掩盖真问题）。
3. **账面侧只认 `4101.*` 末级借方**：有人把制造费用记在 4101 一级（非末级）时，`LIKE '4101.%'` 取不到 ——
   但凭证闸（`leaf_required`）本来就不允许在非末级科目上记账，这条是死路。
4. **不提供「差额明细」的对外接口**：设计 §5.2 没有这个入口，报文与 `ReconcileOutcome` 已带全部明细
   （逐组 + 凭证号）。若前端要一张「对账差异表」，属 B2-12/编排卡的新接口。
5. **没有迁移、没有 HTTP、没有前端**：见 §1。
6. **本卡未改 B2-8 的池 SQL**：`cost_overhead_collect_model::POOL_SQL` 按 (车间,科目) 汇总后丢掉了
   `source_type`（对账要按来源判断阶段），因此本卡自带 `POOL_BY_SOURCE_SQL`。两份 SQL 的口径漂移由
   真库用例 `pool_by_source_matches_the_collect_segment_pool` 兜住（逐组金额 + 组数 + 笔数互校）。

## 5. 给下游卡的交接

- **calc 编排卡（#27）**：第 ④ 步调 `cost_calc_reconcile::reconcile_pool_with_ledger(period, Stage::Calc)`；
  不平就整笔停（不许继续分摊 —— 继续会把「账上多出来的费用」摊到成本对象上，错的不是成本而是全部存货）。
  在已有事务里编排放第 ④ 步时用 `reconcile_in_tx(ex, period, Stage::Calc)` + `cost_reconcile_dto::ensure_balanced(&outcome)?`。
- **B3-6（结算事务内 6 条断言）**：C-COST-1 用 `reconcile_in_tx(ex, period, Stage::Settle)`，
  在**自己的事务内**（第四张手工台账凭证已过账之后）断言，不平就整个回滚。
- **B3-10（巡检 `erp_invariants.py` 的 C-COST-1）**：SQL 口径与 `LEDGER_DEBIT_SQL` 逐条对齐
  （`4101.%` + POSTED + 未删 + `is_red=0` + `reversed_by IS NULL` + 借方；池侧按来源反查科目）；
  巡检是「事后兜底」，calc 时已经拦过一次。
- **B2-12（未归集清单）**：本卡的「账面多（旁路记账）」与「账面少（凭证缺失）」是**两类不同的缺口**，
  `GroupKind::LedgerOnly` / `PoolOnly` 可直接映射成清单的两类行。

## 6. 复跑

```bash
# 纯逻辑（不连库）
cd backend
cargo test --lib cost_reconcile          # dto 14 + model 5
cargo test --lib cost_calc_reconcile     # 段 3 条（真库 10 条 SKIP）

# 真库（演练库 costb26_drill_1625844，B2-7 起保留）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/costb26_drill_1625844' \
  cargo test --lib cost_calc_reconcile_db -- --test-threads=1

# 门禁
cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```
