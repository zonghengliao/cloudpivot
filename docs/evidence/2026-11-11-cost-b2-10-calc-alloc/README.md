# COST-B2-10【成本】calc 第 5 段：分摊计算 —— 证据

- 卡：`t_d856d29d`（COST-B2-10），批次 B2，前置 B2-5（分摊规则 + 试算，`t_1be97cdf`）、
  B2-9（池与 4101 对账，`t_59cae68b`）
- 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
  - §4.4.2 四种分配标准的基数定义（工时 / 机时 / 产量 / 直接材料）
  - §4.4.3 ①②③④ 计算逻辑（池 → 基数 → 最大余数法 → **守恒断言**）
  - §4.4.5 幂等与重算（`uk_biz_cost_alloc_result` + 「按期间整删重灌，只允许 DRAFT」）
  - §3.1 ③⑧ 表结构（`biz_cost_collect` V0371 / `biz_cost_alloc_result` V0372）
  - §8.1 B2-10 行：`calc 第 5 段：分摊计算 / 四标准基数 + 最大余数法 + 守恒断言`
- 范围：**只有**「按当期规则把车间费用池摊到成本对象，并落库结果 + `ALLOC` 归集行」。
  不做约当产量与计算单（B2-11）、不做未归集清单接口（B2-12）、不做结算与凭证（B3）、
  不改任何已应用迁移。

## 1. 改了什么

| 文件 | 类型 | 内容 |
|---|---|---|
| `backend/src/business/erp/cost_alloc_calc_dto.rs` | 新增（纯逻辑，无库） | `AllocLineView` / `RuleOutcome` / `ZeroPoolRule` / `UncoveredPool` / `AllocRunOutcome`；`rule_outcome()`（`dto::allocate` 产出 → 段产出）、**`ensure_conservation()`**（逐规则 `Σalloc = pool` + 尾差最多一行 + 尾差合计 = 池 − 截断合计 + 无负分摊）、`uncovered_pools()`（有池无规则诊断）、**`verify_readback()`**（落库后按 (车间,科目) 回查逐组相等、多出的组也算错）、错误构造（409+`66066` 期间锁定 / 500 守恒或回查破裂）；**14 条单测** |
| `backend/src/business/erp/cost_alloc_result_model.rs` | 新增（数据层） | `biz_cost_alloc_result` 行结构 + 读列；`period_state()`（重算闸的输入）、`results_of_period()`、`alloc_collect_of_period()`、**`purge_period()`**（先删 `ALLOC` 归集行再删结果行）、`insert_result()`、`insert_alloc_collect()`（`ON CONFLICT DO NOTHING`，幂等键 `(ALLOC, result_id, 0)`）；**5 条单测**（钉 `element='OVERHEAD'` 而不是 `'ALLOC'` 等口径） |
| `backend/src/business/erp/cost_calc_alloc.rs` | 新增（段编排） | `allocate_overhead(period, operator)`（自开事务）+ `allocate_overhead_in_tx(ex, period, operator)`（供 calc 编排复用）：**① 闸 → ② 清当期 → ③ 选规则版本 → ④ 池 → ⑤ 基数（每车间只查一次）→ ⑥ 逐规则算 + 守恒断言 → ⑦ 落库 → ⑧ 回查**；**3 条单测** |
| `backend/src/business/erp/cost_calc_alloc_db_tests.rs` | 新增（真库用例，`ERP_DB_URL` 开关） | **8 条**：四标准逐对象核数 / 尾差只落一行 / 池 0 跳过 / 无在产对象 409 / 基数合计 0 409 / 重算幂等且换池生效 / 已提交批次拒重算且旧结果保留 / 有池无规则诊断 |
| `backend/src/business/erp/cost_alloc_rule_model.rs` | 改动（共 124 行） | ① 新增 `active_rules_in(ex)`（事务内版本）；② 新增**纯函数 `select_versions(rows, from, to)`** —— 每 `(车间,科目)` 族取覆盖该期间且生效最晚的一条，priority→rule_code 排序；③ `participants` / `labor_basis` / `material_basis` / `completed_qty` 四个取数函数改收 `ex: &impl SqlExecutor`（事务内读同一快照，避免「算池用事务、算基数用另一条连接」）；④ 新增 `select_versions` 3 条单测 |
| `backend/src/business/erp/cost_alloc_rule_handler.rs` | 改动（−37 行重复代码） | 试算 `preview_allocations` 的选版逻辑改为调用 `model::select_versions`（与 calc 第 5 段**共用同一份选版口径**），4 处取数改传 `db()` |
| `backend/src/shared/error_code.rs` | 改动 | 新增 `ERP_COST_ALLOC_PERIOD_LOCKED = 66066`（409）+ 不复用 10001/10003 的理由；顺带把 B2-9 的 `66065` 补进 66xxx 码值稳定性数组（上卡漏登记，数组里本该有它） |
| `backend/src/business/erp/mod.rs` | 改动（登记） | 4 个新模块 + 注释 |

**无迁移**：`biz_cost_collect`（V0371）、`biz_cost_alloc_result` / `biz_cost_alloc_rule`（V0372）全部已应用，
本卡一字未改（`git status backend/migrations` = 0 行）⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
**无 HTTP 入口、无前端**：设计 §5.2 只有 `POST /business/erp/cost-period/calc`（#27，属 calc 编排卡），
段卡与 B2-6/7/8/9 同构只交付段函数 ⇒ `pnpm typecheck/build` N/A。

## 2. 本卡的口径（全部规则）

1. **池**：`cost_overhead_collect_model::pool_of_period(ex, period)`（与 B2-8 同一份 SQL，
   B2-9 用 `pool_by_source` 逐组互校过）—— `element='OVERHEAD' + cost_object_id IS NULL +
   未软删` 的三类来源池行，科目从来源单据反查。`ALLOC` 行有对象，**不会**进池（否则自我循环）。
2. **规则版本**：`select_versions` 每 `(车间, 科目)` 族取「与期间有交集 + 启用 + `effective_from` 最晚」，
   同日并列按 `rule_code`；排序 `priority` 升序 → `rule_code`。**试算与真摊共用这一个函数**
   （两份实现会在换版那个月分叉）。
3. **参与分摊的对象**：`biz_cost_object.status ∈ (OPEN, CLOSED)` + 对象属该车间 + 当期有
   `element=MATERIAL|LABOR` 的归集行**或** `CONFIRMED` 工时台账行（B2-5 放宽的那一处，原样继承）。
   `ALLOC` 行是 `OVERHEAD` 且挂对象，**不会**把对象「喂」成参与对象（重算不会自我强化）。
4. **基数**：`LABOR_HOURS/MACHINE_HOURS` ← 当期该车间 `CONFIRMED` 工时（一次查两列）；
   `OUTPUT_QTY` ← `biz_cost_period_item.completed_qty`（排除 REVERSED/CANCELLED 批次）；
   `MATERIAL_COST` ← `Σ biz_cost_collect.amount WHERE element='MATERIAL'`。
5. **分摊**：`dto::allocate`（B2-5 已落地并单测）—— `trunc4` 后尾差全额落**唯一一行**
   （余数最大 → 基数大 → id 字典序小）。
6. **守恒断言三处**：`allocate` 内部 `debug_assert`；段内 `ensure_conservation`（出参层逐规则）；
   落库后 `verify_readback`（**从库里读回来**按 (车间,科目) 汇总再比，多出/少写都报 500）。
7. **`ALLOC` 归集行的形态**：`element='OVERHEAD'`（V0371 的 `ck_biz_cost_collect_element` 只允许
   MATERIAL/LABOR/OVERHEAD，`'ALLOC'` 是 **`source_type`** 的枚举值）、`source_type='ALLOC'`、
   `source_id=分摊结果 id`、`line_no=0`、`occur_date=期间末日`、`amount=alloc_amount`。
   **一条结果 = 一行归集**（0 元也写，保住「行数相等 + 金额相等」这条可断言的形态）。
8. **重算**：`purge_period` 先删 `ALLOC` 归集行（**硬删**：它的 `source_id` 指向被删的结果行，
   留着就是孤儿）再删结果行；料/工/费归集行不动（它们指向业务单据，走软删）。
9. **期间闸**：`biz_cost_period` 该期间活跃批次状态 ∉ `{DRAFT, CANCELLED, REVERSED}` ⇒ **409 + 66066**，
   报文带批次号与修法（撤回 / 红冲后退回 DRAFT）。闸在**清当期之前**，被拒时旧结果原样保留。

### 2.1 边界行为（卡面验收项③，三条都写死）

| 场景 | 行为 | 为什么 |
|---|---|---|
| 池 = 0（或当期没这个科目的池行） | **跳过**该规则，进 `AllocRunOutcome::zero_pool`（带人话原因），不写任何行 | 没有钱可摊；写一堆 0 元结果只会让计算单变脏，且无法与「真的摊了 0」区分 |
| 池 > 0 且当期无在产对象 | **409 + `66062`**，报文点名车间 / 科目 / 金额 | 设计 §4.4.3：不许「挂池里留到下期」（上期漏摊的根因） |
| 池 > 0 且该标准基数合计 0 | **409 + `66063`**，报文点名标准与缺什么数据 | 除零不可能守恒；按 0 摊会让 4101 清不掉 |
| 池 > 0 但没有规则覆盖 | **不报错**，进 `AllocRunOutcome::uncovered`（金额 + 「补一条规则后重算」） | 这是**配置缺失**不是数据错误，由未归集清单（B2-12）在界面列出来；与上面两条的修法不同 |

### 2.2 调用顺序契约（写给 calc 编排卡 #27）

第 5 段**必须**在第 4 段（`cost_calc_reconcile::reconcile_in_tx`，`Stage::Calc`）**之后**调用 ——
账上多出来的费用（旁路记账）先摊下去，错的不是成本而是全部存货。本段不重复跑对账
（编排方一次事务只该跑一次），但把池合计放进 `AllocRunOutcome::total_pool` 供编排方与对账结果互校。

## 3. 实测

### 3.1 单测（`01_unit_tests.out`）

```
cargo test --lib cost_alloc_calc_dto      → 14 passed; 0 failed
cargo test --lib cost_alloc_result_model  →  5 passed; 0 failed
cargo test --lib select_versions          →  3 passed; 0 failed   （选版口径与试算共用）
cargo test --lib cost_calc_alloc          → 11 passed; 0 failed   （3 段内 + 8 真库用例无 ERP_DB_URL 时 SKIP）
```

### 3.2 真库用例（`ERP_DB_URL=…/costb26_drill_1625844`，`--test-threads=1`，`02_real_db_tests.out`）

```
running 8 tests
four_bases_allocate_per_object ... ok       # 工时 600/200/200、机时 100/300/600、产量 500/300/200、料 600/200/200（Σ 各 = 1000）
remainder_lands_on_one_stored_row ... ok    # 1000/1:1:1 → 库里 333.3334 / 333.3333 / 333.3333，rounding_adj 只有一行
zero_pool_rule_is_skipped ... ok            # 池 0：进 zero_pool，0 行落库
pool_without_participant_is_409 ... ok      # 409 + 66062，报文点名 4000 / 科目
zero_basis_total_is_409 ... ok              # 409 + 66063，报文点名「机器工时」
recompute_is_idempotent_and_repools ... ok  # 跑 2 次行数不翻倍；池 +500 后重算 → 900/600（整删重灌生效）
locked_period_cannot_be_recomputed ... ok   # SUBMITTED → 409 + 66066，旧结果 800.0000 原样保留
uncovered_pool_is_diagnosed ... ok          # 有池无规则 → uncovered 1 条（250），有规则的照常摊
test result: ok. 8 passed; 0 failed; 0 ignored; finished in 12.80s
```

### 3.3 门禁（`03_gates.out`）

```
cargo fmt -- --check                     → clean
cargo clippy --all-targets -- -D warnings → 0 warning / 0 error
cargo test（全仓 33 目标）               → 4884 passed / 0 failed（EXIT=0）
```

### 3.4 残留（`04_residue.out`）

演练库 `costb26_drill_1625844` 的 13 项计数全 0（分摊结果 / 规则 / 批次 / 计算单 / 归集 / 工时 /
费用台账 / 对象 / 科目 / 物料 / 仓库）。

## 4. 已知偏差与取舍（逐条都有理由）

1. **`ALLOC` 归集行的 `element` 用 `'OVERHEAD'` 不用 `'ALLOC'`**：V0371 的
   `ck_biz_cost_collect_element` 值域只有 `MATERIAL/LABOR/OVERHEAD`（`'ALLOC'` 是 `source_type`）。
   语义也对：摊进对象的就是制造费用，只是来源是分摊。已写进 model 模块头与单测。
2. **0 元分摊结果也写一行 `ALLOC` 归集行**（B2-8 费归集是「0 元不写」）：这里一对一是可断言的
   形态（行数相等 + 金额相等），而 0 元归集行不会影响计算单汇总。
3. **重算闸用「状态 ∈ {DRAFT,CANCELLED,REVERSED}」而不是只认 DRAFT**：CANCELLED/REVERSED 的批次
   已不占位（`uk_biz_cost_period` 排除了它们），该期间的旧结果必须能重算；若只认 DRAFT，
   红冲过的期间就永远改不了（死单）。
4. **`purge_period` 硬删 `ALLOC` 归集行**：它的 `source_id` 就是被删的结果 id，软删只会留下孤儿行、
   让「结果表 vs 归集表」两处口径对不上。
5. **`select_versions` 从 handler 提到 model（纯函数）**：属于本卡必要的重构 —— 若保留两份选版实现，
   「试算看到的数」与「真摊下去的数」在换版那个月会分叉。handler 因此少 37 行重复代码，
   B2-5 的 58 条用例（含 7 条真库）改后全绿。
6. **`participants/labor_basis/material_basis/completed_qty` 改收 `ex`**：本段在一个事务里做
   「读池 → 读基数 → 写结果 → 回查」，四条读必须走同一快照；否则并发过账会让「摊的池」与
   「算的基数」来自两个时点（这正是「Σalloc = pool 断言通过但账面不对」的隐蔽来源）。
7. **`biz_cost_period` 的 `ck_biz_cost_period_appr`**：真库用例把批次改成 SUBMITTED 时必须同时给
   `approval_instance_id`，否则触库层 CHECK（不是本卡口径，夹具已按库约束补）。
8. **未覆盖池不报错**（见 §2.1 第四行）：设计只对「无在产对象」写了报错。若将来要求「有池必摊」，
   应是 calc 编排卡的整段策略，不属本段。
9. **数据权限**：`biz_cost_alloc_result` 不在设计 D-18 登记表里（登记的是成本对象 / 工时 / 计算单），
   本段是财务全公司口径计算，`unscoped`（与 B2-5 规则表同口径）。
10. **没有对外接口**：设计 §5.2 无「分摊结果查询」入口；结果由 calc 编排卡 / 计算单（B2-11）与
    结算（B3）消费。若前端要「本期分摊明细」页，属 B2-12/编排卡新接口。

## 5. 给后续卡的接口

```text
// calc 编排卡（#27，第 ⑤ 步）
let alloc = cost_calc_alloc::allocate_overhead_in_tx(&tx, period_code, operator).await?;
// └ 同一事务内：第 ② 步清当期已含 ALLOC 行 → 不要在外面再清一次

// B2-11（第 6 段：约当产量与计算单）
//   分摊额 = biz_cost_collect(element='OVERHEAD', source_type='ALLOC', cost_object_id=?) 的 amount 之和
//   或直接 sum(biz_cost_alloc_result.alloc_amount) group by cost_object_id（两者由本段回查保证一致）

// B3-5 / B3-6（结算凭证与事务内自检）
//   分摊凭证：借 4001.03 制造费用 / 贷 4101.* —— 按 biz_cost_alloc_result 的 (dept, account) 汇总
//   C-COST-2 自检可直接用 AllocRunOutcome.total_pool 与 total_alloc 相等作为断言之一
```

## 6. 复现命令

```bash
cd /opt/cloudpivot/backend
export TMPDIR=/root/.hermes/profiles/backend-developer/cache/scratch

cargo fmt -- --check && cargo clippy --all-targets -- -D warnings
cargo test --lib cost_alloc_calc_dto cost_calc_alloc cost_alloc_result_model   # 逐个跑

# 真库（演练库 costb26_drill_1625844，B2-7 起保留；必须 --test-threads=1）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/costb26_drill_1625844' \
  cargo test --lib cost_calc_alloc_db -- --test-threads=1

cargo test    # 全仓
```
