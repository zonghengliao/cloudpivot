# M5-17 标准成本：单测（三种差异，含正负差异）— 交付证据

卡号 `t_f43d485b` · 2026-11-19 · **纯后端测试卡（零迁移 / 零路由 / 零权限码 / 零前端改动 / 零生产代码改动）**

## 一、做了什么（一句话）

新增 `cost_standard_variance_tests.rs`（**1464 行 / 29 条纯单测**）：把 M5-12 价差、M5-13 量差、
M5-14 效率差三个生产入口放进**同一张标准成本卡**里跑，钉住「标准量派生 → 三种差异 → 两条恒等式
→ 与 M5-11 报表对拍」这条链，**正负差异各成一组用例**（每种差异都有「不利 / 有利 / 持平」三态，
外加整卡的多物料正负混排）。

## 二、卡片清单

| 文件 | 说明 |
|---|---|
| `backend/app/src/business/erp/cost/cost_standard_variance_tests.rs`（新增 1464 行） | 夹具（`StandardCard` / `Actuals` / `ThreeVariances` / `VarianceRow`）+ 29 条纯单测（A–J 十组，见下表） |
| `backend/app/src/business/erp/cost/mod.rs`（+5 行） | `#[cfg(test)] mod cost_standard_variance_tests;` 声明与模块说明 |

**没有改任何生产代码**：三个入口（`mat_price_variance` / `mat_qty_variance` / `labor_eff_variance`
与 `labor_two_factor` / `std_hours_of`）都原样调用；本卡不新增函数、不改签名、不动 DDL。

## 三、29 条用例（按组，全部真跑通过）

| # | 组 | 用例 | 钉住什么 |
|---|---|---|---|
| 1 | A 标准卡派生 | `standard_card_derives_the_quantities_the_variances_consume` | 单位单耗 1.1 × 800 件 ⇒ 标准量 880；单位工时 0.10 × 800 ⇒ 80；两个差异吃到的就是卡上这两个数 |
| 2 | A | `standard_card_unit_cost_times_completed_equals_the_variance_baseline` | `StandardSpec::amounts()`（卡上三要素）× 完工数 == 差异的「标准材料/人工额」（两条路径不许各算一份） |
| 3 | A | `std_hours_are_rounded_to_the_column_scale_before_they_are_priced` | 0.333333 h/件 × 3 件 ⇒ 0.999999 h；算金额用的工时 == 落表的工时 |
| 4 | B 算例 C | `drill_example_c_reproduces_the_three_variances` | §4.9.2 算例 C：价差 **+600**、量差 **+3840**、效率差 **+500**（类型/要素逐项） |
| 5 | B | `drill_example_c_closes_both_two_factor_identities` | 600+3840 = 15000−10560 = **4440**；(−180)+500 = 4320−4000 = **320**；两条残差 0 |
| 6 | B | `drill_example_c_mixes_unfavorable_and_favorable_lines` | 同一卡里 3 条不利 + 1 条有利（率差 −180），方向/文案各归各的 |
| 7 | C 三态 | `material_price_variance_covers_unfavorable_favorable_and_even` | 价差 +600 / −600 / 0（符号 + `direction` + `direction_label` 三重断言） |
| 8 | C | `material_qty_variance_covers_unfavorable_favorable_and_even` | 量差 +3840 / −2160 / 0，附 `qty_delta`（多用了 320 件） |
| 9 | C | `labor_efficiency_variance_covers_unfavorable_favorable_and_even` | 效差 +500 / −500 / 0，附多干 10 小时 |
| 10 | D 符号对称 | `swapping_actual_and_standard_flips_each_sign_exactly` | 三个入口把实际与标准对调 ⇒ 金额**精确反号**（一厘不漂） |
| 11 | E 库层符号闸 | `direction_mapping_always_satisfies_the_db_sign_check` | 整卡 4 行逐行满足 `ck_biz_cost_var_sign`，且样本里**必须真有**一条有利行 |
| 12 | F 恒等式网格 | `material_identity_holds_across_a_sign_grid` | 9 组输入（涨/跌/只动一边/6 位价/4 位取舍相反/极小/极大）：分水岭同一 + 恒等式闭合 + 金额 ≤4 位 |
| 13 | F | `labor_identity_holds_across_a_sign_grid` | 8 组人工输入（含 6 位费率、零工时、有利为主）：`closes_exactly()` 逐组为真 |
| 14 | F | `identity_closes_when_only_one_factor_moves` | 只动价 / 只动量 / 只动工时：另一半**恰好为 0**，差额全落在动的那一侧 |
| 15 | F | `the_identity_rejects_the_classic_wrong_formulas` | **反证**：量差用实际价、效差用实际费率这两种错法都让恒等式破（证明断言有牙） |
| 16 | G 正负混排 | `mixed_material_lines_keep_their_own_signs_when_summed` | 三物料：+220/+200、−280/−200、+40/−80，逐行符号不被汇总吃掉，净额 −100 |
| 17 | G | `realistic_month_with_mixed_signs_keeps_every_line_straight` | 一期真实形态：料一路有利一路不利 + 人工率差有利/效差不利 ⇒ 合计 +130 与恒等式自洽 |
| 18 | H 与 M5-11 对拍 | `compare_report_row_matches_the_material_and_labor_variances` | 报表标准/实际额 == 差异用的两端；要素差异 == 价差+量差 / 率差+效差；总差异 4760 |
| 19 | H | `compare_report_row_matches_a_favorable_variance_sum` | 全有利的一期：三差异 −2766、报表 −2846（含费用 −80），方向 `FAVORABLE` |
| 20 | H | `compare_total_beyond_the_three_variances_is_the_overhead_variance` | 报表总差异 − 三差异 **恰等于**制造费用差异（三差异不含费用两差，口径写死） |
| 21 | I 边界 | `no_completion_makes_all_actual_usage_and_hours_unfavorable` | 完工 0 ⇒ 标准量/标准工时 0 ⇒ 实际用量与工时全算超支（诚实结果，不跳过） |
| 22 | I | `zero_standard_usage_puts_the_whole_pivot_into_the_qty_side` | 标准单耗 0 ⇒ 量差吃掉全部 pivot，价差不受影响 |
| 23 | I | `zero_standard_price_collapses_the_qty_side_to_zero` | 标准价 0 ⇒ 量差恒 0，差异全归价差（两个半区不互相串） |
| 24 | I | `zero_variance_lines_are_flagged_and_still_carry_a_writable_direction` | 完全达标：四行 `is_zero()` / 「持平」/ 仍可写库 `UNFAVORABLE` |
| 25 | I | `labor_efficiency_is_independent_of_the_actual_rate` | 实际费率从 48 改到 55，效差**逐字段相同**，差额只进率差 |
| 26 | J 落表口径 | `the_three_entry_points_cover_exactly_the_three_types` | 三个入口 ⇔ `MAT_PRICE/MAT_QTY/LABOR_EFF` + 要素配对 + 三种人话名互不相同 |
| 27 | J | `every_line_carries_what_biz_cost_variance_needs` | 金额 ≤4 位、类型/要素在六值枚举内、方向两值内、率差族 `std_qty` 为空；并与 `V0374` 的 DDL 文本对拍（类型字面值 + 符号 CHECK） |
| 28 | J | `six_decimal_inputs_never_leak_more_than_four_decimals` | 6 位输入下金额仍 ≤4 位、分水岭同一、恒等式精确闭合 |
| 29 | J | `all_three_entry_points_reject_negative_inputs_with_field_wording` | 三个入口 + `std_hours_of` 的负入参一律 400/`10001`，消息指到具体字段 |

## 四、验收（全部真跑，原始输出在 `raw/`）

| 证据文件 | 命令 | 结果 |
|---|---|---|
| `raw/01-tests.txt` | `cargo test -p cloudpivot --lib cost_standard_variance -- --test-threads=1` | **29 passed / 0 failed**（0.01s，纯逻辑不连库） |
| `raw/02-clippy.txt` | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | **rc=0**（零 warning，1m10s） |
| `raw/03-cost-domain.txt` | `cargo test -p cloudpivot --lib business::erp::cost:: -- --test-threads=1` | **1122 passed / 0 failed**（M5-16 时 1093：+29 本卡，**零回归**） |
| `raw/04-permission-audit.txt` | `cargo test -p cloudpivot --lib system::menu::permission_audit_tests` | **8 passed**（无新增路由/权限码） |
| `raw/05-fmt.txt` | `cargo fmt -p cloudpivot -- --check` | **rc=0**（空输出即无 diff；只动本卡两个文件） |
| 前端 | — | **N/A**（纯后端，无前端改动；标准成本页 = M5-18，差异分析页 = M5-19） |
| 数据库 | — | **N/A**（零迁移；用例不连库，见下「为什么不做真库用例」） |

## 五、关键设计决策（含理由，别当 bug 改）

1. **本卡只写测试，不动生产代码。** 三个差异的算数已经在 M5-12/13/14 落地且各自有单测；
   M5-17 的价值是**跨卡组装验证**，不是再实现一遍公式。测试里凡是需要金额都调用生产入口
   （`mat_price_variance` / `mat_qty_variance` / `labor_eff_variance` / `labor_two_factor` /
   `std_hours_of`），没有一个手算字面量代替被测函数 —— 否则测试与实现一起错也照样绿。
2. **标准卡用生产的 `StandardSpec` 而不是手搓结构。** 「卡上单位成本 × 完工数 == 差异的标准额」
   这条断言（用例 2）把 M5-08 的标准卡算数与 M5-12~14 的差异基准绑在一起；两条路径哪天分叉就红。
3. **材料标准量在测试侧取整，人工标准量走 `std_hours_of`。** M5-13 明确「标准量由调用方传」，
   生产侧没有材料标准量的公共算法；测试按差异表列口径 `NUMERIC(18,6)` + 半数远离零取整，
   与 `std_hours_of` 同策略，避免两套取整在恒等式上分叉（用例 3 钉住人工那一侧）。
4. **「正负差异」不是加一两条反向断言，而是每类差异都成三态 + 一张混排整卡。**
   只测正数会漏掉「金额对了但方向填反」，只测单类型会漏掉「有利行在汇总时被取绝对值」。
   所以用例 7–9 各测三态，用例 10 测对调反号，用例 11 测库层符号闸，用例 16–17 测多物料正负混排。
5. **恒等式必须有「反证」用例（15）。** ARCH-9 §4.9.2 明说恒等式是「公式写错了唯一能自动抓到的办法」；
   本卡把两种经典错法（量差用实际价、效差用实际费率）显式算一遍并断言其**破** ——
   这证明其它恒等式用例不是「恰好相等」，而是真的能抓住公式被改错。
6. **与 M5-11 报表对拍放在纯逻辑层（用例 18–20）。** `calc_of` 是 M5-11 的唯一算数入口，
   它吃 `CompareCore`（model 层产出）。测试直接构造这份取数行，断言「报表要素差异 == 三差异之和」，
   并显式指出**报表总差异 − 三差异 = 制造费用差异**（三差异不含 `OH_SPEND`/`OH_VOLUME`，这是设计取舍不是遗漏）。
7. **不做真库用例（本卡口径）。** 三张卡的算数入口全是纯函数，恒等式不依赖任何取数；
   落表（`biz_cost_variance` 写入）目前**没有生产写入口**（M5-15/16 只出分摊与凭证），
   用测试直插模拟写库只会重复 V0374 迁移自检已做的「算例 C 两行 + 符号 CHECK」。
   本卡改为**对 DDL 文本对拍**（用例 27：六值枚举 + `ck_biz_cost_var_sign` 两侧），
   成本更低且不会因为模板库迁移落后而假绿/假红。真库写入路径由落表卡建立后另开用例。
8. **零新增文件以外的改动**：模块声明加在 `mod.rs` 的 `#[cfg(test)]` 段，生产构建不编译本文件。

## 六、给下游卡的接缝

- **M5-18（前端：标准成本页）**：本卡的夹具形状（`StandardCard` → 期间标准量 → 三差异）就是页面
  需要的输入/输出形状；前端只需展示 `amount` + `direction_label`，**不用再翻符号**。
- **M5-19（前端：差异分析页）**：三态颜色（正=不利/负=有利）与「持平」文案已由
  `direction_label_of` 统一，页面直接用；混排用例 16 可作为列表排序/汇总的对照数据。
- **落表卡（`biz_cost_variance` 写入）**：用例 27 已把「一行差异要带哪些列」写死
  （金额 4 位、类型/要素六值枚举、率差族 `std_qty` 为空、方向两值且符号一致），照着填即可。
- **M5-34（标准成本 E2E）**：本卡的算例 C 与混排整卡可直接作为 E2E 的期望值基线。

## 七、已知边界 / 上报（不夹带）

1. **三种差异不含制造费用两差**（`OH_SPEND` / `OH_VOLUME`）—— 设计 §4.9.1 明确用两差异法，
   属另两张卡；本卡在用例 20 用「报表总差异 − 三差异 = 费用差异」把这条边界显式写死。
2. **材料标准量取整策略在测试侧**（生产由调用方传）。若将来 M5-15 的落表方引入统一的
   「材料标准量」算法，应把本文件的 `std_qty` 夹具方法改成调用它（否则两处取整可能分叉）。
3. **未做真库用例**（理由见决策 7）；卡面给的 `cloudpivot_test` 模板库在 145 上迁移落后
   （M5-16 已上报），本卡不依赖模板库，因此不受影响。
