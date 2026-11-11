# COST-B2-11 · calc 第 6 段「约当产量与成本计算单」证据

卡：`t_4f45a691`（ARCH-9 §4.6 / 拆卡清单 §8.1 B2-11）
依赖：B2-6 料归集、B2-7 工归集、B2-10 分摊计算（都已在 main）
产出：`backend/src/business/erp/cost_period_item_{dto,model}.rs` + `cost_calc_item.rs` + 真库用例

## 1. 交付内容

| 文件 | 内容 |
|---|---|
| `cost_period_item_dto.rs` | 纯逻辑：约当产量 / 单位成本 / 完工转出 / **期末在制差额倒挤** / 期初在制三要素还原 / 五类数据校验 / 错误构造 |
| `cost_period_item_model.rs` | `biz_cost_period_item` 数据层：活跃批次 + 已结算行数、取对象与当期投入、上期已结算计算单、前期完工合计、上期红冲期间、整删重灌、回查 |
| `cost_calc_item.rs` | 段编排：闸 → 清 CALC → 取对象 → 期初结转 → 算 → 落库 → 回查（自开事务与 `in_tx` 两版） |
| `cost_calc_item_db_tests.rs` | 8 条真库用例（`ERP_DB_URL` 开关） |
| `shared/error_code.rs` | 新增 `66067`–`66071` 五个码 + 66xxx 码值稳定性数组 + 段说明 |

**无迁移**：`biz_cost_period` / `biz_cost_period_item` 由 V0373（B1-3）建好且已应用，一字未改
（`git status backend/migrations` = 0 行）⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
**无前端 / 无 HTTP 入口**：设计 §5.2 只有 `POST /cost-period/calc` #27（属 calc 编排卡）⇒
`pnpm typecheck && pnpm build` N/A。

## 2. 验收（卡面三条，逐条对应证据）

| 卡面验收 | 怎么证明 |
|---|---|
| 用 §4.6 算例 A 的数字做单测 | `01_unit_tests.out`：`example_a_matches_the_design_numbers` 逐字段核设计 §4.6.2 表格（约当 1000/900、单位 19/4.444444/4.444444、完工 22311.1104、在制 4688.8896、完工单价 27.888888）；`02_real_db_tests.out`：`example_a_lands_in_the_database` 用同一批数字**跑真库**，读回库里 18 列逐列相等 |
| 完工成本 + 期末在制 = 期初在制 + 本期归集（恒等式断言） | 纯逻辑：`identity_holds_on_example_a` + `identity_holds_across_a_grid`（5×3 组合全过）；库里读回自证：`identity_holds_and_recalc_is_idempotent`（用**字符串形态**的列值自己相加比对，不信内存）+ 回查函数 `verify_readback` 在每次落库后按库值复算 |
| 4 位小数尾差倒挤到最后一行 | `rounding_gap_is_absorbed_by_wip`：按公式反算在制 4688.8888，倒挤得 4688.8896，差的 **0.0008** 落在期末在制（若两边各算一遍，库层 `ck_biz_cost_item_cons` 会把结算打回，设计 §4.6.1 原话） |

## 3. 实测输出

```
cargo fmt -- --check                       FMT_EXIT=0
cargo clippy --all-targets -- -D warnings  0 warning / 0 error（EXIT=0）
cargo test --lib cost_period_item          21 passed / 0 failed
cargo test --lib cost_calc_item::tests     3 passed / 0 failed
ERP_DB_URL=… cargo test --lib cost_calc_item_db -- --test-threads=1
                                           8 passed / 0 failed（7.53s）
cargo test（全仓 33 目标）                 4916 passed / 0 failed / TEST_EXIT=0   ← 00_full_cargo_test.out
```

真库用例清单（`02_real_db_tests.out`）：

1. `example_a_lands_in_the_database` — 算例 A 逐列落库
2. `identity_holds_and_recalc_is_idempotent` — 恒等式 + 重算不翻倍（`purged=1`、金额收敛）
3. `beginning_wip_carries_over_from_prior_settled_item` — 上期已结算 wip 200 / 4688.8896 → 本期期初；
   本期完工 = 累计 1300 − 前期 800 = 500；期末在制 0 ⇒ 尾差进完工，完工 13688.8896
4. `missing_progress_is_rejected` — 在制 200 但进度 0 ⇒ 409 + `66067`，一行不写
5. `finished_without_material_is_rejected` — 有完工无材料 ⇒ 409 + `66068`
6. `reversed_prior_period_blocks_carryover` — 上期已红冲 ⇒ 409 + `66069`（点名期间）
7. `locked_period_keeps_old_rows` — 批次 SUBMITTED ⇒ 409 + `66070`，旧计算单金额原样保留
8. `missing_batch_is_409` — 该期间无批次 ⇒ 409 + `66071`

残留：`04_residue.out` 六项全 0（演练库 `costb26_drill_1625844` 跑完零残留，库保留供后续卡复跑）。

## 4. 已知偏差 / 判断（**要评审认一下**）

1. **本期完工数量 = 累计完工 − 前期已结算完工合计（推导，设计没写）**
   设计给的是 `biz_cost_object.completed_qty`（库注释「累计完工数量（结算回写）」、前端详情也写
   「累计完工」），但 §4.6 公式里的 FQ 是**本期**完工。单期对象两种读法一样（算例 A = 800）；
   跨月对象直接取累计会把前几个月的完工再转出一次（在制被抓成负数，库 CHECK 直接拒）。
   落口径：`FQ = completed_qty − Σ(前期 status ∈ SETTLED/CLOSED 的 period_item.completed_qty)`；
   差值 < 0 ⇒ 409 + `66067`（点名两个数）。真库用例 3 覆盖（累计 1300、前期 800 → 本期 500）。
2. **尾差吸收方随「有数量的一侧」变**：设计 §4.6.1 ④ 只写「在制 = 投入 − 完工」。若 `WQ = 0`
   （本期全部完工），在制侧吸收会留下「在制数量 0、在制成本 0.0006」的行 —— 对象关批后那 0.0006
   永久挂 4001 无人认领。落地：`WQ = 0 ⇒ 完工 = 投入合计、在制 = 0`；`WQ > 0 ⇒ 设计原文`。
   两条分支都让恒等式分毫不差（纯逻辑用例 2 条 + 真库用例 3 覆盖）。
3. **期初在制三要素还原的差额落在制造费用**：`BC_mat = round(u_mat×WQ,4)`、
   `BC_lab = round(u_lab×WQ×P/100,4)`、`BC_oh = 期末在制成本 − 前两项`。好处是三要素合计
   **一分不差**等于上期期末在制成本（跨期不断链）；代价是制造费用的期初可能带 −0.0001 量级负数，
   已在 `compose` 里拦成 409（`66067`），不外泄负数单价。
4. **本段不建批次、不更新 `biz_cost_period` 的 `total_*` / `object_count`**
   `period_id` 是外键，没有批次行就 409 + `66071`（文案指向「先建批次」）；批次号号段
   （`CS-yyyyMM-0001`）与建批次动作属编排卡 / B3-1。`total_*` 是结算事务（§4.8 ⑤）的落库项，
   本段只在 `ItemRunOutcome` 里回带合计供校验。**编排卡接线时注意**：calc 第 ⑥ 步之后如果
   界面上要显示「本期投入 / 完工 / 在制」合计，可以直接用 `ItemRunOutcome` 的三个 `total_*`。
5. **期初在制只认 `SETTLED` / `CLOSED`**：`SUBMITTED` / `APPROVED` 的数字还可能被驳回重算，
   拿它当期初等于把没定稿的数当事实。上期**被红冲**（`REVERSED`）⇒ 409 + `66069`（设计 §4.6.3 原话）。
6. **零成本完工的判据加了一条前提**：设计 §4.10 说「某对象有完工但没投料 ⇒ 报错」，但跨月对象
   完工既有在制时本期可以确实没有新领料（材料上期已一次投入）⇒ 判据是
   `完工 > 0 且本期材料 = 0 且期初在制没有材料`。真库用例 5 覆盖阻断路径，纯逻辑用例覆盖放行路径。

## 5. ★ 报出来的设计缺口（不在本卡修）

**「本期完工数量 / 期末在制数量 / 完工进度」目前没有任何录入路径。**

- 设计 §4.5.1 数据源矩阵：产量/进度取 `biz_cost_object.completed_qty / wip_qty / completion_pct`，
  车间主任**每月手填**；§6.2.2 缺在制数量的报错文案也写「请在成本对象页面填写本期完工数量与期末在制」。
- 但 B1-8 的成本对象四件套（`CostObjectCreateReq` / `CostObjectUpdateReq`）**没有这三个字段**，
  前端 `cost-object` 表单也没有（§6.2.1 字段清单里同样没有）。三列在库里恒为默认 0。
- 后果：本段逻辑齐全，但真实客户里程数永远是 0 —— 一跑就撞 `66067`「既无完工也无在制却有成本」。
- 建议：补一张卡（`COST-B2-14 / B1-8b`）给成本对象加「本期产量与在制」录入（建议独立接口
  `PUT /cost-object/{id}/output`，因为它是每期变化的量，不该混进主数据表单，且需要留「填写人 + 时间」，
  设计 §8.3 R-4 要求）。**本卡未擅自加接口**（范围纪律）。

## 6. 交给下游卡的口径（编排卡 / B3-*）

- 编排顺序：`第 ⑤ 步 cost_calc_alloc::allocate_overhead_in_tx` → **本段 `cost_calc_item::calc_items_in_tx`**
  （分摊不跑完，`element='OVERHEAD'` 的 ALLOC 归集行还没写，制造费用就进不了对象）。
- 本段入对象的制造费用 = `Σ biz_cost_collect(element='OVERHEAD', cost_object_id=对象, period_code)`，
  与 `Σ biz_cost_alloc_result.alloc_amount` 按对象分组一致（B2-10 回查保证）。
- 结算（B3-4/B3-5）取用：入库数量 = `completed_qty`、入库金额 = `finished_cost`、
  入库单价 = `unit_finished_cost`；完工结转凭证借方 1405 = `Σ finished_cost`；C-COST-3 对
  `Σ wip_end_cost` = 4001 期末余额。
- `stock_in_id` / `status='SETTLED'` 由结算事务写；本段只产 `status='CALC'`。
