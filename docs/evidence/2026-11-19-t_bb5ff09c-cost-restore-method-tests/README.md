# M5-29 成本还原：单测（含三级还原）— 证据

- 卡：`t_bb5ff09c`（父卡 M5-28 `t_3e57dec1` / 提交 `02393235`；同级链 M5-26 结构表、M5-27 还原表）
- 日期：2026-11-19
- 结论：**只加测试，零业务改动**。新增 `cost_restore_method_tests.rs`（6 条纯逻辑 + 6 条真库，共 12 条），
  用**一个四层链夹具**同时喂 M5-26 结构表 / M5-27 还原表 / M5-28 报表，钉住三张表之间的缝；
  `max_depth` 1→4 逐档验**三级还原**（第 1 / 2 / 3 / 4 层卡住时的金额）。
  零迁移 / 零新码 / 零菜单 / 零前端 / 零路由（本卡不碰业务代码）。

## 1. 这张卡补什么（前面三卡都没测的缝）

M5-26 / M5-27 / M5-28 各自有单测，但都是**各测各的**：结构表测「层长得对不对」，还原表测
「钱拆得对不对」，报表测「分组加得对不对」。三张表**在同一个对象上是否说同一组数**、以及
`max_depth` 一变**三张表是否同步改口**，没人钉。本卡的 12 条用例只做这件事。

```text
  M5-26 结构表   depth 4 / layers[1..4] / 第 1 层金额 = 本期材料投入 1000
       │  同一棵树（同一批取数）
       ▼
  M5-27 还原表   flows[1..4] / 第 k 层流入 = 第 k-1 层拨下来的材料
       │  同一批行（rollup_rows_of）
       ▼
  M5-28 报表     group_by=object 的一组 == 明细表同一行（8 个金额字段逐字对拍）
```

## 2. 夹具：四层链（全项目最深的还原路径）

```text
 F 批（P4）：材料 1000 = S2 600 + 外购 R2 400；工 200 / 费 200        → 完工 1400
  S2 批（P3）：材料 S3 600；工 200 / 费 200                          → 结构 600/200/200
   S3 批（P2）：材料 S4 600；工 200 / 费 200                         → 结构 600/200/200
    S4 批（P1）：材料 外购 R 600；工 200 / 费 200                    → 结构 600/200/200
 P4 还有一个对照对象 G：外购 R 500 + 工 100 + 费 100 = 700（全外购料 ⇒ no_semi）
```

每层结构都是 60% / 20% / 20%，往下拨的材料逐层 ×0.6：`1000 → 600 → 360 → 216 → 129.6`，
最后 129.6 落在**外购料 R** 上。期望值是按这条链手推的，不是抄输出：

| 层级 | 流入 | 外购料 | 人工 | 制造费用 | 往下拨 | 不彻底 |
|---|---|---|---|---|---|---|
| 1 | 1000 | 400（R2） | 120 | 120 | 360 | 0 |
| 2 | 360 | 0 | 72 | 72 | 216 | 0 |
| 3 | 216 | 0 | 43.2 | 43.2 | 129.6 | 0 |
| 4 | 129.6 | 129.6（R） | 0 | 0 | 0 | 0 |

成品行：还原前 1000 / 200 / 200 = 1400 → 还原后 **529.6 / 435.2 / 435.2** = 1400，不彻底 0。

## 3. 三级还原：`max_depth` 1 → 4

| `max_depth` | 卡在第几层 | 不彻底 | 外购料 | 人工 | 制造费用 | 结构表 state | 还原表 state |
|---|---|---|---|---|---|---|---|
| 1 | S2 | 360 | 400 | 320 | 320 | `truncated` | `partial` |
| 2 | S3 | 216 | 400 | 392 | 392 | `truncated` | `partial` |
| 3 | S4 | 129.6 | 400 | 435.2 | 435.2 | **`semi`** | `partial` |
| 4 | 到底 | 0 | 529.6 | 435.2 | 435.2 | `semi` | `restored` |

两条要记住的结论（用例逐条断言）：

1. **截断只把「材料」挡在上一层，已拆出的工 / 费一分不动**：卡住那层自己的完工成本结构是齐的，
   拆得动的部分照拆。所以 `max_depth=3` 与 `4` 的差别**只有**外购料 129.6 ↔ 不彻底 129.6 互换，
   `人工 / 制造费用 / 三要素合计 + 不彻底` 完全不变。
2. **结构表「结构到底」与还原表「仍差一层」是口径差，不是 bug**：`max_depth=3` 时
   `probe_depth_cut` 探查发现 S4 下面全是外购料 ⇒ 结构表**不报截断**（`state=semi`、`issues` 空），
   但还原要把材料变成**外购料叶子**才算确认，S4 没有子层 ⇒ 那 129.6 只能记「还原不彻底」。
   结构问「是什么」，还原问「钱变成了什么」，两者本来就不是同一个问题。

## 4. 12 条用例清单

### 4.1 纯逻辑（不连库，合成结构树；`cost_restore_method_tests::pure`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `four_levels_are_convolved_to_purchased_material` | 四层链 529.6 / 435.2 / 435.2；四层台账逐层自洽；层间「流入 = 上层往下拨」；还原后材料 = 各层外购料之和 |
| 2 | `three_level_cut_stops_after_the_third_level` | **三级还原**：停在第 3 层 ⇒ 不彻底 129.6；与四级相比只差「外购料 ↔ 不彻底」；半成品明细最后一层 `stopped` |
| 3 | `every_cut_depth_moves_exactly_the_material_part_into_unresolved` | `max_depth` 1–4 四档金额逐档对上（360/216/129.6/0）+ 每档守恒 + 台账层数 = `max_depth` |
| 4 | `semi_details_carry_the_imputed_amount_of_their_own_level` | 半成品明细与台账对得上（第 1 层 = 半成品 + 外购料；第 2 层起流入全来自上一层材料）；每条明细「还原后 + 不彻底 = 被卷积金额」 |
| 5 | `branching_bom_allocates_by_sibling_amount_and_conserves` | 分叉 BOM（两个半成品各自下钻）兄弟按金额分配、第 2 层同一层里既确认外购料又继续下钻 |
| 6 | `every_depth_keeps_the_before_total` | 任何截断深度都守恒、每个 flow 自洽 |

### 4.2 真库全链路（`ERP_DB_URL` 开关；`cost_restore_method_tests::db`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `e1_four_level_chain_agrees_across_the_three_tables` | 三张表在同一个对象上：结构 depth 4 / layers 4 / 逐层金额 1000-600-600-600；还原表 flows 4 / semis 3 / 逐层台账；跨表对拍（第 1 层金额 = 第 1 层流入 = 还原前材料；更深层流入 = 上一层往下拨且 ≤ 结构层金额）；报表 `group_by=object` 一组 == 明细行 8 个字段；占比 37.83 / 31.09 / 31.09 / 0.00、`share_sum=100.01`、pp −33.60 / +16.80 / +16.80、主导要素 material、`check=0` |
| 2 | `e2_depth_limit_stops_the_chain_at_the_same_layer_in_all_tables` | `max_depth` 1–4：三张表 depth 同步、金额四档、结构表 state（1/2 层 `truncated`、3 层起 `semi`）、还原表 state、页脚提示；报表同档对拍 + 不彻底在报表里占一格（四项合计 100.00±0.01） |
| 3 | `e3_states_line_up_between_rollup_and_report` | 三档状态两表一致（F `restored` / G `no_semi`）、G 不被还原、报表三档行数 = 明细表三档行数、`state` 筛选两表同步收窄 |
| 4 | `e4_report_groupings_sum_to_the_whole_table_on_the_chain` | 四档维度（dept/product/object/all）分组之和 = 整表合计、对象不重不漏、报表合计 = 明细表合计（逐字段） |
| 5 | `e5_other_dept_sees_nothing_in_any_table` | 数据范围：别车间三张表都是 0 行（不是 403）、空表不画假 `0.00%`；本车间能看到 2 行 |
| zz | `zz_teardown_leaves_zero_residue` | 清场零残留 |

## 5. 实现（1 个新文件 + 1 行注册）

| 文件 | 作用 |
|---|---|
| `backend/app/src/business/erp/cost/cost_restore_method_tests.rs` | 12 条用例（纯逻辑 + 真库）。**不新增任何生产代码**：三张表都调各自 handler 的 `*_table_of` / `report_of`（与 HTTP 入口同一条代码路径，只跳过认证） |
| `backend/app/src/business/erp/cost/mod.rs` | `#[cfg(test)] mod cost_restore_method_tests;` + 模块文档 |

夹具纪律（与既有卡一致）：期间槽独占 `1600 + slot`（M5-26 用 1900+/M5-27 1800+/M5-28 1700+/
在制分批 2000+）；对象号 / 批号 / 物料编码 / 领料单号全带 `M529` 前缀 + 唯一后缀；
计算单用 `cost_period_item_dto::compose` 算好再落库（不手抄数字）；最后一条断言清场零残留。

## 6. 测试证据（本次实测）

| 项 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt && cargo fmt --check` | rc=0 |
| clippy（lib） | `cargo clippy --lib -- -D warnings` | rc=0（零警告） |
| clippy（全 target） | `cargo clippy --all-targets -- -D warnings` | rc=0（零警告） |
| 本卡 12 条 | `ERP_DB_URL=…/cp_m529_head cargo test --lib cost_restore_method_tests -- --test-threads=1` | **12 passed / 0 failed**（6 纯逻辑 + 6 真库），日志 `cargo-test-method.log` |
| 成本还原三卡回归 | `ERP_DB_URL=…/cp_m529_head cargo test --lib cost_restore -- --test-threads=1` | **133 passed / 0 failed** = 121（结构 54 + 还原 43 + 报表 24）+ 12（本卡），日志 `cargo-test-restore.log` |
| 权限审计 | `cargo test --lib permission_audit_tests` | **8 passed / 0 failed**（零新增码 ⇒ n14/n16/n18 不动），日志 `cargo-test-permission-audit.log` |
| cost 域回归 | `ERP_DB_URL=…/cp_m529_head cargo test --lib business::erp::cost -- --test-threads=1` | **1462 passed / 4 failed** = 1450（M5-28 基线）+ 12（本卡）；4 条红全是既有 `cost_standard_db_tests`（空库无演示成员，见 §6.1），日志 `cargo-test-cost-domain.log` |
| 空库首启 | 空库 `cp_m529_head` 直接跑二进制 | **应用 400 个迁移 / 203 张表 / `/health` 200**，日志 `empty-db-first-boot.log` |

### 6.1 既有 4 条红（与 M5-29 无关，且不是本卡引入）

全在 `cost_standard_db_tests`：报错原文是「节点「财务经理审批」的审批人全部是制单人 / 发起人本人…
（标准成本卡保持草稿、没有生成审批单）」。空库首启的库里没有任何「非制单人」能当审批人，
M5-27（`cp_m527_head`）与 M5-28（`cp_m528_head`）在同一个空库首启口径下跑出的是同样这 4 条。
本卡不碰 `cost_standard_*` / 审批链任何一行，只加了一个 `#[cfg(test)]` 模块。

### 6.2 库与环境

- 运行位置：**99（开发机）**，PG 走 `172.18.10.145:5432`（按卡上场景 A 的硬约束）；
- 库：`cp_m529_head`（**空库首启**，不用漂移的 `cloudpivot_test` 模板库 —— 缺 `is_foreign_currency`
  列会让 cost 域出 138 条假红）；验证完 `dropdb` 清理；
- 实例：`NIK_DATABASE_* + NIK_SERVER_LISTEN=127.0.0.1:18035 + 三把密钥` 环境变量注入，
  **未新建任何 config profile 文件**；迁移完成后停实例（验收不依赖常驻服务）。

## 7. 与上游口径的交叉自查

- M5-27 的守恒式 `after.total + unresolved = before.total` 在四层链上逐层成立（本卡在
  `max_depth` 四个档位各断言一次）；
- M5-28 的 `share_sum`「2 位舍入差如实暴露」在四层链上出现实例：37.83 + 31.09 + 31.09 + 0.00 =
  **100.01**，用例把这个字符串钉住（凑整就是错的）；
- M5-26 的 `probe_depth_cut`「下面全是外购料就不报截断」在本卡第一次被跨表钉住（§3 结论 2）；
- 本卡**零生产代码改动** ⇒ 前面三卡的用例（121 条）一行未改仍全绿。
