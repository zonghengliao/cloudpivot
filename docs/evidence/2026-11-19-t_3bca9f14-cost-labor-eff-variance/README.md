# M5-14 差异：效率差（工时）— 证据与口径

- 卡片：`t_3bca9f14`（M5-14【M5·成本】差异：效率差（工时））
- 上游：`t_6fcd5583`（M5-13 材料量差，提交 `36f1e697`）
- 交付物：`backend/app/src/business/erp/cost/cost_labor_eff_variance_dto.rs`（新增，820 行含 33 条单测）
  + `backend/app/src/business/erp/cost/mod.rs`（+4 行模块声明）
- 零迁移、零路由、零权限码、零前端改动（纯函数卡）

## 1. 公式与三个入口

```
pivot = std_amount_at_actual_qty = money(标准工资率 × 实际工时)   ← M5-12 工资率差的右端也是它
率差  = money(实际工资率 × 实际工时) − pivot                     ← M5-12（LABOR_RATE）
效差  = pivot − money(标准工资率 × 标准工时)                     ← 本卡（LABOR_EFF）
────────────────────────────────────────────────────────────
率差 + 效差 = money(实际工资率 × 实际工时) − money(标准工资率 × 标准工时)
```

| 入口 | 作用 | 说明 |
|---|---|---|
| `labor_eff_variance(实际工时, 标准工时, 标准工资率)` | 本卡公式 | 守卫按**工时口径**重写（报文说「实际工时」而不是「实际用量」），算数**委托** M5-13 的 `qty_variance(LABOR_EFF, LABOR, …)` |
| `std_hours_of(单位标准工时, 完工数)` | 标准工时的**唯一**算法 | `biz_cost_standard.std_labor_hours × 完工数`，先 round 到落库列标度 6 位**再算钱** |
| `labor_two_factor(实际费率, 标准费率, 实际工时, 标准工时)` | 人工两因素配对 | 四个入参只收一次 ⇒ 两条线不可能各拿一个实际工时；`residual()` / `closes_exactly()` 把 §4.9.2 恒等式做成可断言对象 |

## 2. 口径决定（逐条有理由）

1. **乘标准工资率，不乘实际工资率** —— 用实际费率会把工资率差重复计入，§4.9.2 点名的错法。
2. **金额按项取整**（两端各 `money()` 后相减），pivot 与 M5-12 字面同源；单测 `pivot_is_the_same_number_as_the_wage_rate_line` 直接比两条线的该字段相等。
3. **标准工时先 round 到 18,6 再算钱** —— 否则「算金额用的工时 ≠ 落表的 std_qty」，报表上恒等式差一厘且查不出来。单测 `stored_std_hours_are_the_same_number_used_in_the_amount` 盯死。
4. **入参最多 6 位小数**（= `biz_cost_variance.actual_qty/std_qty/std_price` 的 `NUMERIC(18,6)`），多了 400 —— 多行台账的加权平均费率动辄十几位小数，让库静默截断就会「同一条工时在两条路径上算出不同金额」（与 `cost_labor_dto` 同一条纪律）。
5. **算数只有一份**：`delegates_to_the_qty_family_arithmetic` 拿 `labor_eff_variance(...)` 与 `qty_variance(VARIANCE_LABOR_EFF, …)` 的返回值逐字段比相等 —— 有人手抄公式就红。
6. **不新增业务码**：纯入参校验走 400 + `PARAM_INVALID`(10001)，同 M5-12 / M5-13。

## 3. 两个合法边界（不当错误拦）

| 边界 | 结果 | 为什么 |
|---|---|---|
| 标准工时 = 0（完工数 0 / 单位工时没填） | 实际工时全算超用 | 诚实结果；跳过它报表就少一块差异 |
| 标准工资率 = 0（未定薪） | 效差恒 0，差异全归率差 | 两个半区各归各位 |

另有一条**需下游注意**：`biz_cost_labor.hours` 库层 `CHECK (hours > 0)`，所以「实际工时 0 而标准工时 > 0」在数学上是全额有利，实际语义是**一条台账都没有**。本卡不拦（数学合法），M5-15 应把它归进「未归集清单」而不是当成绩效优良入表（单测 `zero_actual_hours_is_favorable_and_means_missing_ledger`）。

## 4. 两处会静默算错的地方（都写进模块头）

**① 多行台账不许先平均再算。** `biz_cost_labor.amount` 被库层
`ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))` 逐行锁死 ⇒「实际人工成本」在库里是 **Σ 逐行 round**，不是 `round(Σ工时 × 平均费率)`。多行必须**逐行算差再相加**（跨行汇总属 M5-15）。

**② `money()` 是银行家舍入，PG `round()` 是半数远离零。** 两者在「恰好中点」上差 0.0001：

```
$ psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test -c "SELECT 2.00005::numeric, round(2.00005::numeric,4)"
 2.00005 | 2.0001        ← PG：半数远离零
money(2.00005) = 2.0000  ← rust_decimal round_dp 默认：进位到偶
```

差异表金额沿用 `money()`（M5-11/12/13 全仓同一处舍入，恒等式才闭合）；与台账 `Σ amount` 对平时若差 0.0001 的整数倍，**概率就是这两个策略的分歧，不是公式错**。单测 `ledger_rounds_half_up_while_money_rounds_to_even` 把这一分钱钉成可复现断言。

## 5. 真跑输出（本目录）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01-module-tests.txt` | `cargo test --lib business::erp::cost::cost_labor_eff_variance_dto` | **33 passed / 0 failed** |
| `02-cost-domain-regression.txt` | `cargo test --lib business::erp::cost::` | **1027 passed / 0 failed**（M5-13 时 994，+33 = 本卡，零回归） |
| `03-permission-audit.txt` | `cargo test --lib system::menu::permission_audit_tests` | **8 passed** |
| `04-fmt-clippy.txt` | `cargo fmt -p cloudpivot --check` / `cargo clippy --lib --tests -- -D warnings` | **rc=0 / rc=0，零 warning** |
| `05-pg-round-semantics.txt` | 145 真库核实 PG `round` 语义 | 见上（2.00005 ⇒ 2.0001） |

## 6. 给下游的交接

- **M5-15（差异分摊到成本对象）**：落 `biz_cost_variance` 时按 `QtyVarianceLine` 的列映射填 ——
  `actual_qty ← 实际工时`、`std_qty ← std_hours_of(...)`、`std_price ← 标准工资率`、**`actual_price 写 NULL`**（实际费率在 `LABOR_RATE` 那一行）、`amount + direction()`。
  多行台账逐行算再相加；`hours=0 且 std_hours>0` 的行归「未归集」。
- **M5-16（差异凭证）**：用 `amount` + `direction()`；与台账 `Σ amount` 对平差 0.0001 的整数倍先看第 4 节②，别改公式。
- **M5-17（三差异单测）**：人工两因素的恒等式断言可直接用 `LaborTwoFactor::closes_exactly()`；材料两因素沿用 M5-12/13 的 pivot 对拍。
