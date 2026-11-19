# M5-13 · 差异计算：材料量差 =（实际量 − 标准量）× 标准价

卡号 `t_6fcd5583` · 2026-11-19 · 纯函数 + 单测（零 DB / 零路由 / 零迁移 / 零权限码）

## 交付了什么

| 文件 | 内容 |
|---|---|
| `backend/app/src/business/erp/cost/cost_qty_variance_dto.rs`（新增 641 行） | 契约层与纯逻辑：`QtyVarianceLine` 结果结构 + `mat_qty_variance()`（本卡入口）+ `qty_variance()`（用量差一族公共算数，M5-14 效率差同形复用）+ 要素/类型配对护栏 + 输入域护栏 + **27 条纯单测** |
| `backend/app/src/business/erp/cost/cost_variance_dto.rs`（+17/−8） | 两处**复用**（不改语义）：`max_money()` 提为 `pub(crate)`（金额列上限只留一份）；新增 `direction_label_of()` 自由函数，`VarianceLine::direction_label()` 改为委托它 —— 量差与价差共用同一份「不利/有利/持平」文案逻辑，防两卡各写一份后分叉 |
| `backend/app/src/business/erp/cost/mod.rs`（+3 行） | 模块声明 |

**没有做的事**（本卡范围外）：不出路由、不动 `biz_cost_variance` 落表（M5-15/16）、不算差异率、不取标准量（纯函数只收不算，取数属报表/落表层）、不新增错误码、不动前端、零迁移。

## 口径（改之前先看 ARCH-9 §4.9）

| 问题 | 本卡口径 | 为什么 |
|---|---|---|
| 用哪个价乘 | **标准价**，不是实际价 | 量差回答「多用了多少 × 本该多少钱」。用实际价乘会把价差重复计入，§4.9.2 明列的两大常见错法之一 |
| 金额取整 | **按项取整**：`amount = money(标准价×实际量) − money(标准价×标准量)`，不是 `money((实际量−标准量)×标准价)` | 左端必须与 M5-12 的 pivot **字面相同**（同函数、同入参、同取整）；先差后乘会让恒等式差一厘 |
| 分水岭 | `std_amount_at_actual_qty = money(标准价 × 实际量)` —— 与 M5-12 价差行的右端是**同一个数** | 闭合点只能有一个。本卡绝不自己再 round 一遍；单测 `shared_pivot_is_literally_the_same_number` 直接比两卡的该字段相等 |
| 符号 | 正 = 不利（多用）/ 负 = 有利（少用） | 与 §4.9.4、`ck_biz_cost_var_sign` 同号 |
| 0 差异 | 库层字面值 `UNFAVORABLE`，展示文案「持平」 | `ck_biz_cost_var_dir` 只有两值；文案走共用的 `direction_label_of` |
| 输入域 | 实际量 / 标准量 / 标准价 ≥ 0，负数 400 + `PARAM_INVALID`(10001) | 负用量是上游算错，不是「倒退生产」；静默取绝对值 = 藏错账 |
| 要素与类型配对 | `MAT_QTY` ⇔ `MATERIAL`、`LABOR_EFF` ⇔ `LABOR`，**配错直接 400** | 两者公式同形但 `element` 决定差异汇入哪项三要素合计；放任配错 = 造一行永远对不上合计的差异 |
| 标准量 0 | **合法**（BOM 缺项 / 新品未定单耗）⇒ 实际用量全算超用 | 把 0 当「无标准」跳过，报表里就少一块差异 |
| 标准价 0 | **合法**（免费料 / 未定价）⇒ 量差恒 0，差异全归价差那一半 | 两个半区各归各位，正是分水岭的语义 |
| 金额列上限 | 差异额超 `NUMERIC(18,4)` 在算数层就 400 | 把「改库才能修」的 500 前移成「改输入就能修」的 400 |
| 尾差 | `rounding_diff = amount − raw_amount`（4 位），标注「不是差异」 | 财务对平差的那 0.0001 得有个名字（同 M5-11 / M5-12 用法） |
| 入账吗 | **不入账** | ARCH-9 §4.9.3 / D-15：本期只让差异被看见 |

## 验收（真跑，输出在同目录）

| 证据 | 命令 | 结果 |
|---|---|---|
| `01_module_tests.out` | `cargo test -p cloudpivot --lib business::erp::cost::cost_qty_variance_dto -- --test-threads=1` | **27 passed / 0 failed**（RC=0） |
| `02_cost_domain.out` | `cargo test -p cloudpivot --lib business::erp::cost:: -- --test-threads=1` | **994 passed / 0 failed**（成本域全量；M5-12 时 967，+27 本卡） |
| `03_permission_audit.out` | `cargo test -p cloudpivot --lib system::menu::permission_audit_tests` | **8 passed**（零新增权限码，页面码未误入后端） |
| `04_fmt_check.out` | `cargo fmt -p cloudpivot --check` | RC=0 |
| `05_static_checks.out` | `cargo clippy -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel --lib --tests -- -D warnings` | **RC=0，零 warning** |

单测覆盖：§4.9.2 算例 C 量差（+3840）、卡面公式 `raw_amount == (实际量−标准量)×标准价`、**与价差共用分水岭（字面相等）**、两因素恒等式闭合（600 + 3840 = 15000 − 10560）、6 位标准价下恒等式仍闭合、工资率差/效率差合为 320、零量差 / 少用 / 标准量 0 / 标准价 0 / 双 0、4 位取整与 12 位精确值、两端取舍相反的尾差 0.0001、三类负数、率差类型 / 制造费用类型 / 拼错类型 / 未知要素 / 要素配错、金额列超限、`ck_biz_cost_var_sign` 逐值对拍、库层枚举镜像与 `V0374` DDL 对拍（`include_str!` 读迁移原文）。

## 样本（单测断言原文，节选）

```
mat_qty_variance(1200, 880, 12.00)
  std_amount_at_actual_qty = 14400.0000      // 分水岭：1200 × 12.00（与价差行右端同一个数）
  std_amount               = 10560.0000      // 标准 880 × 12.00
  amount                   =  3840.0000      // 量差（正 = 不利）
  qty_delta                =       320       // 多用了 320 件
  direction / label        = UNFAVORABLE / 不利
  rounding_diff            =    0.0000
价差 600.0000（mat_price_variance 给出） + 量差 3840.0000 == 实际 15000.0000 − 标准 10560.0000 ✅
```

## 下游怎么用

- **M5-14 效率差**：`qty_variance(VARIANCE_LABOR_EFF, "LABOR", 实际工时, 标准工时, 标准工资率)` 即 §4.9.1 第 4 行；与 M5-12 的 `price_variance(VARIANCE_LABOR_RATE, …)` 共用同一 pivot，单测 `labor_efficiency_variance_reuses_the_same_arithmetic` 已证 −180 + 500 = 320。
- **M5-15 落表**：`QtyVarianceLine` → `biz_cost_variance` 的映射写在结构体文档的表格里（`actual_price` 这一列**写 NULL** —— 量差公式不吃实际单价，写标准价会让人以为实际价=标准价）。
- **M5-16 凭证 / M5-17 三差异单测**：`amount` / `direction()` 已是库层列口径（4 位金额 + 两值方向）。
- **标准量取数**（标准单耗 × 完工数）不在本卡：由报表/落表层在 SQL 里算好再传进来。

## 环境备注

- 本卡为纯逻辑，无真库用例 ⇒ 未建演练库（不走 `ERP_DB_URL` 那套）。
