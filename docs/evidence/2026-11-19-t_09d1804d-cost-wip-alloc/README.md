# M5-22【M5·成本】在制品：料工费在完工与在制间分配 · 证据

卡片：`t_09d1804d`（父卡 `t_46c1745d` M5-21 完工率录入）
交付物：**一条只读报表接口** `GET /api/business/erp/cost-wip-alloc`

---

## 1. 这张表回答什么

期间内**逐成本对象（生产批）× 逐要素（料 / 工 / 费）**：钱怎么在完工与在制之间分。

```
① 成本池 = 期初在制（上期单位成本还原到三要素）+ 本期投入（biz_cost_collect.element）
② 分母   = 材料 → 材料约当；人工 / 制造费用 → 加工约当     ← 直接调 M5-20 的 equiv_of
③ 分配率 = 池 ÷ 分母                                        （6 位，与 calc 的 unit_* 同一分母）
④ 完工   = 本期完工数量 × 分配率                             （4 位）
⑤ 在制   = 池 − 完工                                        （4 位，差额倒挤）
```

④⑤ 合起来恒等于①（每个要素、每行、整表都一样）。本期期末在制为 0 时走 calc 的
「整池进完工」分支：尾差留在完工，不在在制里挂一笔无主尾差。

## 2. 交付清单

| 文件 | 内容 |
|---|---|
| `backend/app/src/business/erp/cost/cost_wip_alloc_dto.rs`（新增 1856 行） | 契约 + 纯逻辑：`Element`（料/工/费 + 分母口径）、期初还原（调 calc 的 `beg_cost_of`）、`alloc_of` 一行分钱（三道「分不出来」的闸 + 与计算单对拍 + 整表合计）、29 条纯单测 |
| `backend/app/src/business/erp/cost/cost_wip_alloc_model.rs`（新增 334 行） | 取数层：一条 SQL 取回对象 + 最近一期已结算计算单（期初来源）+ 前期已结算汇总 + 本期三要素归集 + 本期计算单快照（无 N+1）；数据范围复用 `ScopeTarget::BIZ_COST_OBJECT`；占位符/参数一致性硬校验；8 条单测 |
| `backend/app/src/business/erp/cost/cost_wip_alloc_handler.rs`（新增 282 行） | 接口层：入参归一化（期间必填 + 关键字 + 五档状态 + 分页上限 200）、编排（取数 → 算 → 筛 → 分页 → 整表合计 → 页脚说明）、`#[nik::permission]` + `#[nik::get]`；6 条单测 |
| `backend/app/src/business/erp/cost/cost_wip_alloc_db_tests.rs`（新增 934 行） | 10 条**真库**用例（`ERP_DB_URL` 开关），计算单用真 `compose` 算好再落库 |
| `backend/app/src/business/erp/cost/mod.rs`（+17 行） | 挂三个 `pub mod` + 一个 `#[cfg(test)] mod`，每个都写明为什么在这里 |
| `scripts/lint/scope_gate.py`（+1 行） | 新增测试面文件登记进 `SKIP_MANIFEST`（闸② 的逐条断言要求） |

**零迁移、零新增权限码、零菜单、零其它模块改动、零前端改动**（前端页是 M5-25）。

## 3. 权限与数据范围

| 项 | 决定 | 理由 |
|---|---|---|
| 权限码 | **复用** `business:erp:cost-object:list` | 读的是同一批 `biz_cost_object`，与 `cost-wip-equiv`（M5-20）/ `cost-collect-sheet`（M5-02）/ `wip-balance`（M5-05）同码。新开码没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；页面码/接口码解耦（PERM-01）只在 HR 试点 |
| 数据范围 | 行由 `biz_cost_object.workshop_dept_id` 裁剪（`ScopeTarget::BIZ_COST_OBJECT`） | 与约当产量表 / 归集表同一锚点。**表别名必须写 `t`**：锚点里写死 `alias = "t"`，换字母只有非超管 500（超管走 `AND 1 = 1` 看不出来）—— 已加单测钉死 |
| 看不见的对象 | 不出现在表里，不报 403 | 列表没有「探测口」问题 |

## 4. 四个不算账的决定（都不是 bug）

1. **分不出来就给 `null`，绝不给 0**。金额写 0 会被读成「这行没有成本」，而真相是「缺一个输入」。
   问题码：`WIP_PCT_MISSING` / `NO_OUTPUT` / `COMPLETED_NEGATIVE` / `PCT_OUT_OF_RANGE` /
   `MATERIAL_MODE_UNSUPPORTED`（这五个来自 M5-20 的同一段判定，不重写）+ 本卡新增两条：
   `BEG_COST_INVALID`（上期单位成本为负，期初还原不出来）、`NEGATIVE_ALLOC`（分出负数）。
   每条都带「怎么修」。
2. **池与分母照给，只有「完工 / 在制」两列给空**。池（期初 + 投入）与分母（约当）是事实，
   不会因为缺完工率就消失；把整行清空反而让人看不出「这行有多少钱待分」。
3. **合计分两套口径**：数量合计（完工 / 在制）算所有行；金额合计**整行跳过 blocked 行** ——
   只加一半会给出读不出含义的数，加 0 会让合计看起来像丢了钱。合计旁边写着「有 N 行算不出来」。
   恒等式：材料池 + 人工池 + 制造费用池 = 成本池 = 完工 + 在制（整表级）。
4. **分配率按未舍入的约当算**（与 calc 的 `unit_*` 完全同源），表里约当显示 4 位。
   所以拿显示值回算分配率可能在**第 6 位**差 1 —— 这是计算单本身的口径，页脚写明了。

## 5. 与计算单对拍（11 个字段）

有当期活跃批次时，把本表算出的结果与 `biz_cost_period_item` 的快照逐字段比：
本期完工数量 / 期末在制数量 / 完工率 / 材料约当 / 加工约当 / 期初在制成本 /
单位材料成本 / 单位人工成本 / 单位制造费用 / 完工转出成本 / 期末在制成本。
任一不等 ⇒ `recalc_required = true` + `drift[]` 逐字段给「表里 X / 计算单 Y」，
`state=drift` 可一屏筛出。已结算的行提示「红冲后重算」而不是「直接重算」。

## 6. 验收（全部真跑）

| 项 | 命令 | 结果 | 证据 |
|---|---|---|---|
| 纯单测 + 真库用例 | `ERP_DB_URL=…/cp_m522_dev cargo test --lib business::erp::cost::cost_wip_alloc -- --test-threads=1` | **53 passed / 0 failed**（43 纯单测 + 10 真库，10 条真库全跑，不是 SKIP） | `raw/01-unit.txt` / `raw/02-db.txt` |
| 成本域回归 | `cargo test --lib business::erp::cost` | **1262 passed / 0 failed**（本卡 +53） | `raw/03-cost-domain.txt` |
| changelog | `cargo test --lib business::changelog` | **54 passed / 0 failed** | `raw/04-changelog.txt` |
| 权限审计 | `cargo test --lib system::menu::permission_audit_tests` | **8 passed / 0 failed** | `raw/05-permission-audit.txt` |
| 格式化 | `rustfmt --edition 2024 --check`（本卡 4 个新文件 + `mod.rs`） | rc=0 | `raw/06-fmt.txt` |
| lint | `cargo clippy --lib -- -D warnings` | rc=0（零警告） | `raw/06b-clippy.txt` |
| 工程检查 | `bash scripts/check.sh --fast`（L0 + L1，含 `pnpm typecheck`） | 全部通过 | — |
| 闸②（scope_gate） | `python3 scripts/lint/scope_gate.py --root .` | 本卡 1 条登记后**已不在失败清单**；仍有 **13 条 HEAD 既有红**（别卡的测试面文件未登记，见下「已知欠账」） | `raw/07-scope-gate.txt` |

真库演练库：`cp_m522_dev`（clone 自 `cloudpivot_test`，145 台），跑完 10 条用例后按对象号清场、用例自带 `zz_teardown_leaves_zero_residue` 断言零残留，最后整库 drop。

### 已知欠账（不属于本卡，未顺手改）

`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 缺 13 条**别卡**测试面登记
（`cost_batch_collect_db_tests` / `cost_batch_finish_db_tests` / `cost_batch_method_tests` /
`cost_batch_wip_db_tests` / `cost_collect_sheet_db_tests` / `cost_standard_compare_db_tests` /
`cost_standard_db_tests` / `cost_standard_import_db_tests` / `cost_standard_publish_db_tests` /
`cost_standard_variance_tests` / `cost_variance_voucher_db_tests` / `cost_wip_equiv_db_tests` /
`stockin_handler_tests_zz_shared_roles`）。这些文件都是纯 `#[cfg(test)]` 挂载，
登记一行即绿；但该文件是多卡共用的碰撞点（父卡 M5-21 已报 `hotspot`），本卡只补自己那一条，
避免把别卡的工作树改动扫进本次提交。建议单开一张「测试面登记补齐」的小卡一次收口。

## 7. 边界（本卡不做）

- **期末在制余额台账**（M5-23）：本表已经把「在制」那一列的金额算出来，M5-23 应消费同一口径；
- **在制品单测大卡**（M5-24）：本卡的 10 条真库用例可直接作起点；
- **前端在制品页**（M5-25）：本卡的 `formula_*` / `totals.note` / `state_options` 就是给那一页现成吃的。
