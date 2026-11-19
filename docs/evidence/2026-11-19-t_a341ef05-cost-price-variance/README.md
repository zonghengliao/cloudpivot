# M5-12 · 差异计算：材料价差 =（实际价 − 标准价）× 实际量

卡号 `t_a341ef05` · 2026-11-19 · 纯函数 + 单测（零 DB / 零路由 / 零迁移 / 零权限码）

## 交付了什么

| 文件 | 内容 |
|---|---|
| `backend/app/src/business/erp/cost/cost_variance_dto.rs`（新增 546 行） | 契约层与纯逻辑：`VarianceLine` 结果结构 + `mat_price_variance()`（本卡入口）+ `price_variance()`（率差一族公共算数，M5-14 工资率差同形复用）+ 输入域护栏 + 21 条纯单测 |
| `backend/app/src/business/erp/cost/mod.rs`（+3 行） | 模块声明 |

**没有做的事**（本卡范围外，已在卡内声明）：不出路由、不动 `biz_cost_variance` 落表（M5-15/16）、不算差异率（报表层口径同 M5-11）、不新增错误码、不动前端、零迁移。

## 口径（改之前先看 ARCH-9 §4.9）

| 问题 | 本卡口径 | 为什么 |
|---|---|---|
| 金额取整 | **按项取整**：`amount = money(实际价×实际量) − money(标准价×实际量)`，不是「先算差再乘再取整」 | 两因素恒等式 `价差+量差 = 实际−标准` 要在 **4 位金额**上精确闭合（`ck_biz_cost_var_sign` / 报表对平都按 4 位）；先差后乘两端各自留尾数，恒等式就差一厘 |
| 分水岭 | `std_amount_at_actual_qty = money(标准价 × 实际量)`：价差的右端 = 量差的左端 | 闭合点只能有一个。M5-13 的 `量差 = pivot − money(标准价×标准量)`，谁都不许自己再 round 一遍 |
| 符号 | 正 = 不利（超支）/ 负 = 有利（节约） | 与 ARCH-9 §4.9.4、`ck_biz_cost_var_sign` 同号 |
| 0 差异的方向 | 库层字面值 `UNFAVORABLE`（`amount >= 0` 那支），展示文案「持平」 | `ck_biz_cost_var_dir` 只有两值，写 `EVEN` 会被库层打回；但人不该看到「0.0000 不利」 |
| 输入域 | 单价 / 用量 ≥ 0，负数 400 + `PARAM_INVALID`（10001），不静默取绝对值 | 负用量不是「倒退生产」，是上游算错；静默纠正是藏错 |
| 金额列上限 | 差异额超 `NUMERIC(18,4)` 在算数层就 400，不等库层 500 | 把「改库才能修」的 500 前移成「改输入就能修」的 400 |
| 尾差 | `rounding_diff = amount − raw_amount`（4 位），标注「不是差异」 | 财务拿报表对计算单，差的那 0.0001 得有个名字（同 M5-11 `rounding_diff` 用法） |

## 验收（真跑，输出在同目录）

| 证据 | 命令 | 结果 |
|---|---|---|
| `01_module_tests.out` | `cargo test -p cloudpivot --lib business::erp::cost::cost_variance_dto -- --test-threads=1` | **21 passed / 0 failed**（RC=0） |
| `02_cost_domain.out` | `cargo test -p cloudpivot --lib business::erp::cost:: -- --test-threads=1` | **967 passed / 0 failed**（成本域全量，含 M5-12 的 21 条；M5-11 时为 946） |
| `03_permission_audit.out` | `cargo test -p cloudpivot --lib system::menu::permission_audit_tests` | **8 passed**（零新增权限码，页面码未误入后端） |
| `04_fmt_check.out` | `cargo fmt -p cloudpivot --check` | RC=0 |
| `05_static_checks.out` | `cargo clippy -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel --lib --tests -- -D warnings` | **RC=0，零 warning** |

单测覆盖：ARCH-9 §4.9.2 算例 C（+600）、两因素恒等式闭合（600 + 3840 = 15000 − 10560）、工资率差同形算数（−180，与效率差 500 合为 320）、零用量 / 零价差 / 标准价 0 / 实际价 0、4 位取整与 12 位精确值、两端取舍相反的尾差 0.0001、三类负数入参、非率差类型（`MAT_QTY`）/ 拼错类型 / 未知识别要素、金额列超限、`ck_biz_cost_var_sign` 逐值对拍、库层枚举镜像与 `V0374` DDL 对拍（`include_str!` 读迁移原文）。

## 样本（单测断言原文，节选）

```
mat_price_variance(12.50, 12.00, 1200)
  actual_amount          = 15000.0000        // 实际 1200 × 12.50
  std_amount_at_actual_qty = 14400.0000      // 分水岭：1200 × 12.00
  amount                 =   600.0000        // 价差（正 = 不利）
  direction / label      = UNFAVORABLE / 不利
  rounding_diff          =     0.0000
  std_qty                = None              // 价差公式不吃标准用量（量差才吃）
价差 600.0000 + 量差 3840.0000 == 实际 15000.0000 − 标准 10560.0000 == 4440.0000 ✅
```

## 下游怎么用

- **M5-13 量差**：`量差 = line.std_amount_at_actual_qty − money(标准价 × 标准用量)`，同一 `VarianceLine` 的 `std_qty` 由该卡填。
- **M5-14 效率差**：`price_variance(VARIANCE_LABOR_RATE, "LABOR", 实际工资率, 标准工资率, 实际工时)` 即工资率差；效率差从同一个 `std_amount_at_actual_qty` 出发。
- **M5-15/16 分摊与凭证**：`VarianceLine` 的 `amount` / `direction` 已是库层列口径（4 位金额 + 两值方向），维度列（期间 / 成本对象 / 物料）由写库方补。
- **落差提醒**：若实际金额是「账面金额反算单价」得来的（`单价 = 金额 / 量`），二次相乘会引入 0.0001 级偏差 —— 届时由 M5-15 决定是传账面金额原值还是走单价口径，本卡不预设。

## 环境备注

- 本卡为纯逻辑，无真库用例 ⇒ 未建演练库（不走 `ERP_DB_URL` 那套）。
- 99 本机 `cargo clippy --tests` 3m04s；`cargo test --lib` 复用同一 target，耗时 <1s。
