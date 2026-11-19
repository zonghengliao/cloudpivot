# M5-11 标准成本：与实际成本对比（逐物料）— 交付证据

卡号 `t_84d84186` · 2026-11-19 · **纯后端（零迁移 / 零新增权限码 / 零前端改动）**

## 一、做了什么（一句话）

新增一条只读报表 `GET /business/erp/report/cost-variance`（设计 §5.2 #36）：**一个物料一行**，
左标准（期间末生效且已发布的标准成本卡，三要素单位成本）、右实际（当期活跃批次的成本计算单，
完工数量 × 三要素单位成本），出**单位与总额双口径**差异、差异率、方向与三要素拆分，附整表合计与
「缺标准 / 缺实际」两类的**可执行提示**（去建卡发布 / 先做成本计算）。差异只出报表**不入账**（D-15）。

## 二、卡片清单

| 文件 | 说明 |
|---|---|
| `backend/app/src/business/erp/cost/cost_standard_compare_dto.rs`（新增 1325 行） | 契约 + **唯一算数入口** `calc_of`：三态判定 / 单位与总额双口径 / 差异率（基数为 0 ⇒ `None`）/ 方向（正=不利）/ 三要素拆分 / 尾差对平 / 合计 / 出参装配 / 19 条纯单测 |
| `backend/app/src/business/erp/cost/cost_standard_compare_model.rs`（新增 545 行） | 取数层：`act`（计算单按产出品聚合）+ `std`（已发布 + 覆盖期末）+ `draft`（未发布版本计数）三 CTE 按物料 `FULL OUTER JOIN`；分页走 `scope::paginate_unscoped`，合计走同模板聚合（与 Rust 逐行算数对拍）/ 9 条 SQL 形状单测 |
| `backend/app/src/business/erp/cost/cost_standard_compare_handler.rs`（新增 320 行） | 接口层：期间必填归一化 / 物料关键字 / 四档状态筛选 / 分页上限 200 / 编排（活跃批次 → 数据页 → 整表合计）/ 7 条入参单测 |
| `backend/app/src/business/erp/cost/cost_standard_compare_db_tests.rs`（新增 849 行） | 6 条真库用例（见下表） |
| `backend/app/src/business/erp/cost/mod.rs` | 4 个模块声明（含 `#[cfg(test)]` 的真库用例） |

**零迁移**：`biz_cost_standard` / `biz_cost_standard_item`（V0374）、`biz_cost_period` / `biz_cost_period_item`
（V0373）、`biz_cost_variance`（V0374，M5-12+ 落表用）都已存在，本卡只读，不新增表 / 列 / 索引。
**零新增权限码**：复用 `business:erp:cost-method:list`（与 M5-08/09/10 的 12 条标准成本接口同码，理由见 handler 模块头）。

## 三、接口

```
GET /api/business/erp/report/cost-variance
  ?period_code=2026-11          必填（yyyy-MM）
  &keyword=FG                   可选（物料编码 / 名称，通配符已转义）
  &state=all|comparable|no_standard|no_actual   可选（缺省 all）
  &page=1&page_size=20          可选（上限 200）
权限码 business:erp:cost-method:list   ·   只读，unscoped（全公司财务口径）
```

出参（节选，字段语义见 dto 文档注释）：

| 字段 | 说明 |
|---|---|
| `period_code / period_start / period_end` | 期间与**基准日**（标准按 period_end 取版本） |
| `standard_basis` | 取数口径一句话（前端直接展示） |
| `batch {id,batch_no,status}` | 当期活跃批次（实际侧出处；无 ⇒ `null` + `cost_ready=false`） |
| `list[].state` | `BOTH` 可比 / `NO_STANDARD` 缺标准 / `NO_ACTUAL` 缺实际（结构判定，不含金额） |
| `list[].standard` | 卡 id / 版本号 / 生效区间 / 发布时刻 / **换版标记** `switch_in_period` / 三要素单位成本 + 工时 + 工资率 |
| `list[].actual` | 批次数 / 完工数量 / 账面完工成本 / 三要素完工成本与单位成本 / 本期投入 / 期末在制 |
| `list[].variance` | `std_amount`（标准应发生 = 单位标准 × 完工数量）/ `act_amount` / `amount`（实际 − 标准，正=不利）/ `rate`（基数 0 ⇒ `null`）/ `direction` / 单位三件（标准 / 实际 / 差异）/ **三要素拆分** / `rounding_diff`（尾差，对平用非差异） |
| `list[].hint` | 缺一侧时给**具体动作**（「提交发布审批」/「先做成本核算」/「等完工」），可比行给换版 / 尾差说明 |
| `totals` | 按筛选后**全集**（不是当前页）的计数 + 可比行三要素金额 + 差异率（总差额 / 总标准额）+ 缺标准行的实际金额（`no_standard_amount`，不混进差异） |
| `note` | 口径说明（符号约定 / 不入账 / D-15 出处；无批次时追加「先做成本计算」的入口） |

## 四、验收（全部真跑）

| 证据文件 | 内容 | 结果 |
|---|---|---|
| `01_module_tests.out` | `cargo test --lib cost_standard_compare`（真库 `cp_m511_cmp`） | **43 passed / 0 failed**（37 纯 + 6 真库） |
| `02_cost_domain.out` | `cargo test --lib business::erp::cost:: -- --test-threads=1` | **946 passed / 0 failed** |
| `03_permission_audit.out` | `system::menu::permission_audit_tests` | **8 passed**（路由全带闸、零新增死码） |
| `04_menu_invariants.out` | `scripts/verify/menu_invariants.sh cp_m511_cmp` | **8 项全绿**（菜单 570，本卡未种菜单） |
| `05_static_checks.out` | `cargo fmt --check` / `cargo clippy -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel --lib --tests -D warnings` | rc=0 / rc=0 |
| 前端 | 无改动（本卡纯后端；标准成本页 = M5-18，差异分析页 = M5-19） | N/A |

**真库用例（`cost_standard_compare_db_tests`，6 条）**

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `comparable_row_matches_arithmetic_and_totals` | 标准 20 / 实际 23 / 完工 100 ⇒ 差异 300、差异率 15%（`0.150000`）、单位差异 3；**SQL 合计 = 逐行算数相加**（两处口径对拍） |
| 2 | `missing_sides_are_classified_with_actionable_hints` | 有实际没建卡 / 有实际但卡是草稿（未发布不算标准）/ 有标准没计算单 / 有标准但完工 0（全在制）四种形态各自归类；A 合计无可比行 ⇒ 差异率 `null`；缺标准的实际金额单列 300 |
| 3 | `unpublished_zero_status_and_switch_versions` | 期中换版 ⇒ 取**期末那版**（v2）+ `switch_in_period=true` + hint 说明；`status=0` 的卡不参与 |
| 4 | `filters_paging_and_keyword_narrow_the_set` | 四档筛选各自命中；关键字收窄；`page_size=1` 翻页 total 恒定、两页不同行、可比行排第一、合计不随分页变 |
| 5 | `period_without_batch_lists_standard_side_only` | 无批次 ⇒ `cost_ready=false` + 仍列标准侧（全 `NO_ACTUAL`）+ `note` 指向 `/business/erp/cost-period/calc` |
| 6 | `element_split_is_reported_per_element` | 三要素逐项：材料 +100（不利 / 20%）、人工 −50（有利 / −25%）、费用 0（持平 / 0%）；**要素相加 = 总差异** |

## 五、关键设计决策（含理由，别当 bug 改）

1. **标准取「期间末生效 + 已发布 + 启用」那一版**：与 M5-08 `period-sheet` 的期末口径逐字一致
   （差异分析的口径本来就是「期末产量 × 标准单位成本」）；期中换版不静默，`switch_in_period=true`
   且 `hint` 明说「按期末那一版对比」。M5-10 的取数闸（`publish_state='PUBLISHED'`）在 SQL 里显式写下，
   不依赖别的模块。
2. **实际取成本计算单，不取归集明细**：`biz_cost_period_item` 是唯一同时有「完工数量 + 三要素单位成本 +
   完工成本」的地方。`biz_cost_collect` 只有本期投入（含留在在制里的部分），拿它比会对不上标准单位成本。
   `actual.period_input` / `wip_end_cost` 一并给出，但**不参与差异**（只作「为什么没完工」的解释）。
3. **单位与总额双口径都给**：只看总额会把「产量少」误读成「成本低」；只看单位又看不到真金白银。
   `std_amount = 单位标准 × 完工数量`，`act_amount = 完工数量 × 实际单位成本`（逐要素相加，与计算单
   `finished_by_formula` 同源）。
4. **差异率基数为 0 时给 `null` 而不是 0**：标准额为 0 是「没有可比基数」，不是「完全达标」。
   合计口径 = **总差额 / 总标准额**（不是逐行差异率的算术平均），单测钉死。
5. **尾差单列 `rounding_diff`（账面完工成本 − 三要素相加）**：有期末在制时计算单把四位小数尾差吸进
   `finished_cost`，本报表以「单位成本 × 数量」为口径，差额必须显式给出并标注「不是差异」——否则
   财务对不上成本计算单会以为是漏算。
6. **三态只由结构判定**（`standard` 是否存在、`completed_qty` 是否 > 0），不含金额判据：这样 SQL 侧的
   筛选 / 排序与 Rust 侧状态判定能逐条对上，模型层不必重复实现算数。
7. **`{FILTER}` 注入位必写**：`scope::paginate_unscoped` 把权限片段**原位替换**在标记处；本 SQL 末尾是
   子查询收尾（`) v`），不写标记时框架会追加成 `) v AND 1 = 1` ⇒ 语法错（本卡踩过，已用单测锁死
   「四档各恰好一个标记且排在业务条件之前」）。
8. **权限码零新增**：`cost-method:list` 与标准成本一族同码；新开 `cost-variance:list` 在 M5-19 页面落地前
   没有菜单行承载 ⇒ 死码（n14 判红）；先插指向不存在组件的菜单行 ⇒ n08 判红。等 M5-19 一起解耦。
9. **路径用设计 §5.2 #36 的 `/report/cost-variance`**：本卡是该报表的**汇总层**；钻取（六类差异明细）由
   M5-12~16 落 `biz_cost_variance` 后另开子路径，**不改本路径出参结构**。

## 六、给下游卡的接缝

- **M5-12 / M5-13 / M5-14（价差 / 量差 / 效率差）**：本卡已把「物料 × 期间」两侧数值摊平；差异分解可直接
  复用 `cost_standard_compare_model::base_sql` 的 `act` / `std` 两个 CTE（同一份口径），不要再写一套取数。
- **M5-15 / M5-16（差异分摊 / 差异凭证）**：`biz_cost_variance`（六类 + `direction` 符号）已建表未用；
  本卡 `variance.elements[]` 的三要素拆分正是分摊与凭证的输入形状。
- **M5-19（差异分析页）**：`state` 四档 + `note` + `hint` 可直接渲染；列表默认排序已经是
  「可比行在前、差异额绝对值倒序、编码兜底」，前端不必再排。
- **M5-17（标准成本单测）**：本卡 43 条已覆盖「标准 vs 实际」的算数与取数；M5-17 只需补三种差异各自的恒等式。

## 七、已知边界 / 上报（不夹带）

1. **`cp_m510_full` 有两处夹具残留**：`sys_user` 里的 `m510.finance-manager` / `m510-finance-manager` 仍绑着
   `FINANCE_MANAGER`，会让 `cost_period_approval_db_tests` 的「角色成员只有种子 + 夹具」断言判红
   （报错原文：「夹具污染：FINANCE_MANAGER 上有夹具与演示种子之外的在职成员」）。本卡在演练库里清掉这两行后
   成本域 946 条全绿；**M5-10 的 publish 真库用例 teardown 没有解绑 `FINANCE_MANAGER`**，属上游夹具卫生问题，
   本卡不夹带修（另立卡或由 M5-10 补 teardown）。
2. **模板库 `cloudpivot_test` 落后到 V0357**（无 `publish_state`），本卡演练库从 `cp_m510_full`（V0465）克隆；
   下游卡若要跑 M5-10 之后的用例，同样别用 `cloudpivot_test` 当模板。
3. 页面未做浏览器 E2E（属 M5-19 / M5-34）。
4. 差异不入账（D-15）：不动存货计价、不出凭证、不落 `biz_cost_variance` 。
