# M5-15 差异分摊到成本对象 —— 证据（2026-11-19 · t_732c79d7）

一句话：新增 `cost_variance_alloc_dto.rs`（1666 行 / 40 条纯单测，零迁移、零路由、零权限码），
把「六类差异 × 期间 × 物料」的差异池按规则基数摊到成本对象，最大余数法单行收尾差，
`Σ分摊 = 待分摊额` 与 `已归属 + 已摊 = 差异合计` 双恒等式硬断言；算数复用 B2-10 的
`cost_alloc_rule_dto::allocate`（最大余数法全仓唯一实现），本文件只做符号处理 / 已归属扣除 / 期间批量守恒。

## 交付物

| 文件 | 行数 | 内容 |
|---|---:|---|
| `backend/app/src/business/erp/cost/cost_variance_alloc_dto.rs` | 1666 | 四种基数值域 + 六类差异的默认基数映射 + 元素配对 + `VariancePool` / `VarianceAllocRule` / `VarianceAllocObject` / 结果四件套 + `allocate_variance`（单池）+ `allocate_period`（按期批量）+ 守恒断言 + 40 条单测 |
| `backend/app/src/business/erp/cost/mod.rs` | +5 | 模块声明（带口径注释） |

## 口径（写死在模块头，改之前先看）

| 问题 | 口径 | 为什么 |
|---|---|---|
| 摊什么 | 只摊 `待分摊额 = 差异合计 − 已归属`（已挂 `cost_object_id` 的差异行不再摊） | 再摊一遍就是双计 |
| 默认基数 | 价差/量差 → `ACTUAL_QTY`；率差/效差 → `LABOR_HOURS`；费用两差 → `STD_AMOUNT` | 权重必须与差异同量纲 |
| 规则覆盖 | 规则给 `basis` 就用（四值域），拼错 400，不静默退化默认 | 「按工时摊价差」是另一种管理口径，但错字不能当成没填 |
| 负数池 | 按 `\|池\|` 分再整体翻符号 | 最大余数法要求非负池；直接拒会让「整体节约」的期间摊不出结果 |
| 尾差 | 最大余数法**单行**收口（余数最大 → 基数大 → id 小） | ARCH-9 §4.4.3 ③ / D-9；重算两次必须同答案 |
| 基数 0 对象 | 留在结果里、分摊 0、`participates=false` | 报表要能看见「这批本月没做」 |
| 基数全 0 / 无对象 | 409（复用 `66062` / `66063`）+ 怎么修 | 池里的钱摊不出去＝账不平；同一失败模式不新开码 |
| 分摊比例 | 出参给 8 位，**只展示**，用它算钱必差一分（单测钉死） | 闭合只能靠最大余数法 |
| 入账 | 不入账（D-15） | 差异是报表结论；差异凭证是 M5-16 |

## 真跑输出（同目录）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_module_tests.txt` | `cargo test --lib business::erp::cost::cost_variance_alloc_dto` | **40 passed / 0 failed** |
| `02_contract_drill_145.txt` | 145 真库（`cp_m515_drill`，整段 ROLLBACK，残留 0 行）契约演练 | 见下 |
| `03_cost_domain_regression.txt` | `cargo test --lib business::erp::cost::` | **1067 passed / 0 failed**（M5-14 时 1027，+40 本卡，零回归） |
| `contract_drill.sql` | 演练脚本原文（可重跑） | — |

### 145 真库演练钉住的 6 件事（`02_contract_drill_145.txt`）

1. **六类差异类型全部能落库**：`MAT_PRICE/MAT_QTY/LABOR_RATE/LABOR_EFF/OH_SPEND/OH_VOLUME` × 三要素，全部通过 `ck_biz_cost_var_type` / `ck_biz_cost_var_elem` / `ck_biz_cost_var_sign`。
2. **符号一致性是硬的**：负金额 + `UNFAVORABLE`、正金额 + `FAVORABLE` 都被库拒绝（模块 `direction()` 口径与库一致）。
3. **零差异行的方向**：`ck_biz_cost_var_sign` 的判据是 `amount >= 0` / `amount <= 0`，所以 0 金额两个方向都合法；模块统一写 `UNFAVORABLE`（与 M5-12 的 `db_direction_of` 同口径），零基数对象的分摊行照此落库。
4. **4 位标度是硬的**：插 `0.00005` 被 PG 静默改成 `0.0001`（半数远离零）—— 这正是模块对入参 >4 位直接 400 的理由（否则「分摊合计 ≠ 池」且查不出原因）。
5. **分摊守恒在 PG numeric 上精确相等**：`333.3334 + 333.3333 + 333.3333 = 1000.0000` 与池 `1000.0000` 相等（不是「差不多」）；负数池（有利差异）同样精确。
6. **trunc 口径同源**：`trunc(1000/3, 4) = 333.3333`，与 Rust `trunc4` 同值。

## 守门

| 门 | 命令 | 结果 |
|---|---|---|
| 格式化 | `cargo fmt -p cloudpivot --check` | rc=0 |
| clippy | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | rc=0（零 warning，3m21s） |
| 权限审计 | `cargo test --lib system::menu::permission_audit_tests` | 8 passed（本卡零路由 / 零权限码，理应不变） |
| 前端 | — | N/A（纯后端纯函数卡，无前端改动） |
| 迁移 | — | 无（`biz_cost_variance` 在 V0374 已建，本卡不建表不改列） |

## 给下游卡的口径

- **M5-16（差异凭证）**：分录金额用 `VarianceAllocLine::alloc_amount`（含尾差），**不要**用 `share_rate` 乘回去；每行方向用该行金额的符号（0 行写 `UNFAVORABLE`）。
- **M5-19（前端差异分析页）**：`share_rate` 只做展示列；`participates=false` 的行显示「未参与（基数 0）」而不是隐藏。
- **落表（若后续卡要写 `biz_cost_variance`）**：池行 `cost_object_id` 留空，分摊行填对象；同一 `(期间, 类型, 物料)` 的 `Σ amount` 必须等于池行金额（`allocate_period` 已断言）。
