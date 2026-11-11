# ARCH-9 成本核算升级设计（分批法 / 标准成本 + 差异分析 / 品种法）

> **卡**：ARCH9-3【L-4】 · **状态**：设计定稿（不含实现） · **作者**：backend-developer
> **关联**：评审报告 5.1 / 6.2 · ARCH-4（MES·D9 成本回写）· ARCH-6（财务扩展）· ARCH-9-1（多组织）· ARCH-9-2（人力资源/薪资）
> **证据**：`docs/evidence/2026-11-10-arch9-3-cost-ddl/`（DDL 在 PG 真库跑通 + 三个算例真库演算，整段 ROLLBACK 零残留）

**一句话结论**：存货计价继续**只有一本账**（移动加权平均，`biz_inventory.avg_cost`）；本卡新增的是**成本对象核算层**——把料工费归集到「生产批」这个筐里，用约当产量法算出完工单位成本，唯一出口是**完工入库金额 + 三张结转凭证**。这不是第二本库存账（守住 ARCH-4 D9），而是「钱从费用科目走到存货科目」的那段路现在有人记账了。

---

## 0. 结论前置（定稿决策）

> 老大的习惯是先看结论。18 条决策，每条带依据；不确定的标「待确认」并给默认取向。

| # | 决策 | 依据 / 理由 |
|---|---|---|
| **D-1** | **成本核算与存货计价分家**：存货计价不改，仍是移动加权平均一本账；新增「成本对象核算层」，唯一出口＝完工入库金额（`biz_stock_in.cost_price`）+ 结转凭证 | `biz_inventory.avg_cost`（V0137:400）是存货金额唯一真相；ARCH-4 **D9**（`ARCH-4-通用MES与委外架构设计.md:741-758`）明令「成本回写只写两处，不建第二本成本账」。本设计完全服从这条 |
| **D-2** | **不改 `ck_biz_material_costing`**（V0201 已收敛为仅 `MOVING_AVG`，代码 `dto.rs:73-77` 同口径）。不复活 FIFO / STANDARD | 复活值域＝每条出库路径都要改，而价值只体现在「可选」；品种法/分批法要的是**归集**，不是换计价法 |
| **D-3** | **一期只做分批法**（`object_type='BATCH'`）。`ORDER`/`PRODUCT` 值域预留，三期实现。**不引入工单**（MES 暂停） | 分批法所需的批次号在现有单据里**已经存在**（`lot_no` 三处），能立刻咬合；品种法缺的是「多步骤结转」，没有 MES 工序数据就做不出来 |
| **D-4** | 成本对象＝「生产批」本身，一张轻量主数据 `biz_cost_object`；按订单生产用 `source_doc_no` 记订单号即可覆盖，不建工单表 | 卡面说分批法适用「按订单生产」——一单一批是分批法的常规实现，不需要工单实体 |
| **D-5** | 新增批次主数据 `biz_lot`（卡面提到，但**实测库里没有这张表**）；三处 `lot_no` 文本列**不加物理外键** | 实测：只有 `biz_stock_in_item.lot_no`(V0145:225)、`biz_stock_out_item.lot_no`(V0203:141)、`biz_material_serial.batch_no`(V0138:80)。存量自由文本质量不可控，强 FK 会让历史单据改不动 → 改 handler 校验 + 巡检报告 |
| **D-6** | 料工费三路数据源固定：**料**＝领料出库明细（真实际成本）；**工**＝工时台账（手录/Excel 导入）；**费**＝折旧/报销单据 + 无单据的手工费用台账 | 这是「没有 MES 时成本靠什么算」的答案，详见 §4.5 |
| **D-7** | **不猜数据**：缺工时 / 缺完工数 / 缺在制进度时，结算**直接报错**并指出缺哪一行，**绝不**用「按产量比例」自动编造人工 | 编造出来的人工成本会让毛利假准，比算不出来更危险。产品取向（可推翻，但要产品书面拍板） |
| **D-8** | 分摊标准四值：`LABOR_HOURS` / `MACHINE_HOURS` / `OUTPUT_QTY` / `MATERIAL_COST`；**只做一次分配**（车间费用 → 成本对象）；二次分配（辅助车间）与分步结转不做 | 二次分配需要「辅助车间互相服务」的服务量矩阵，客户没有这个数据；一期做一次分配已覆盖 90% 场景 |
| **D-9** | 分摊尾差用**最大余数法**单行收口，`Σ分摊 = 待分摊总额` 是硬断言 | 真库演算：1000 元按 3 个等基数对象分摊 → `333.3333×2 + 333.3334`，尾差 0.0001 落在余数最大行（算例 B2） |
| **D-10** | 在制品用**约当产量法（加权平均法）**；材料按「开工一次投入」（一期唯一实现），加工按完工进度；期初在制自动从上期期末结转 | 材料一次投入是绝大多数离散/轻制造的实情；加权平均法免去「先进先出约当」的复杂度。完工进度是人工填的 → 见 D-7 |
| **D-11** | 完工入库新增 `biz_stock_in.in_type='PRODUCTION'`（现有 4 值里没有）；由**结算批次在同一事务内**生成入库单 + 库存 + 流水 + 凭证 | 词表 `TxnBizType::ProductionIn`（`doc_kind.rs:687`）早就存在且 `as_str` 已定义 `PRODUCTION_IN`（:761），但**没有任何写入口**（grep 实测）——框架预留了这条路，只差把 `in_type` 值域补上 |
| **D-12** | 一期出 **3 张结转凭证**：① 人工计提 借 `4001.02` / 贷 `2211.01`；② 制造费用分摊 借 `4001.03` / 贷 `4101.01~09`；③ 完工入库结转 借 `1405` / 贷 `4001.01~03`。**领料凭证改造现有路径**：出库单挂了 `cost_object_id` → 借方从 `6401` 改 `4001.01` | 4001 期末余额＝期末在制，天然对上「存货」报表项（V0183:192 已含 4001/4101）；领料改借方只在「挂了成本对象」时生效，存量行为零变化 |
| **D-13** | **人工来源二选一并排他**：`labor_source` 快照 `MANUAL`（一期工时台账）/ `PAYROLL`（ARCH9-2 薪资上线后）。同期不允许两种来源都出凭证 | ARCH9-2 定稿「工资凭证唯一出口是薪资单」；两边都计提＝双计提（ARCH9-2 handoff 已把这条列为跨卡冲突，本卡让路） |
| **D-14** | 科目补种：`4001.01~03`（直接材料/人工/制造费用）、`4101.01~04/09`（折旧/水电/修理/车间管理/其他）、`2211.01`（工资）；`4001`/`4101`/`2211` 翻非末级。**与 ARCH9-2 的 2211/5602 种子幂等共存** | 凭证闸只允许末级科目（`voucher.rs::post_voucher` 的 `non_leaf` 检查）；实测 4001/4101/2211 现在都是 `is_leaf=1` 且无子科目 |
| **D-15** | **二期标准成本只做差异分析，不改存货计价、差异不入账**。「标准成本计价 + 差异入 1404 分摊」属**推翻移动加权**的独立决策，本卡不做，另立 ADR | ARCH-4 D9 与 §8.2-7 把「库存成本改标准成本法」明确判为「另一个决策，本卡不碰」；同一逻辑适用于本卡。差异不入账时 1404 一分不动，报表零风险 |
| **D-16** | **品种法＝三期复用同一套表**（`object_type='PRODUCT'` + 分步结转的分摊规则 `biz_type='STEP_TRANSFER'`），不另建模块 | 品种法与分批法的差别只在「对象粒度」和「是否多步骤」，表结构 90% 相同；现在就把 `object_type`/`biz_type` 值域留出来 |
| **D-17** | 审批只加一处：**成本结算批次**（`biz_type='COST_SETTLE'`）两级审批（财务经理 → 财务总监）。成本对象/工时台账/费用台账**不审批** | 结算才动库存与凭证（高危）；台账类只是数据录入。复用流程引擎，不硬编码 |
| **D-18** | 数据权限：`biz_cost_object` / `biz_cost_labor` / `biz_cost_period_item` 登记 `SCOPED_TABLES`（`dept_col=workshop_dept_id`）；结算单头走 unscoped + 权限码（单头是期间合计，SELF 无意义）；新表只加 `org_id` 列不建 FK，但 `biz_cost_period` 用**哨兵默认值**让唯一键一次到位 | 与 ARCH9-1（多组织）/ ARCH9-2（数据权限口径）对齐：唯一键若只写 `period_code`，多组织那天必须换键，换键窗口并发就是双单 |

**跨卡热点（必须让总管知道）**：

- `biz_account` 的 `2211` 会被本卡与 ARCH9-2 **同时**翻非末级、同时种 `2211.01` → 两边都写成幂等（条件 `UPDATE` + `ON CONFLICT DO NOTHING`），自检断言写「结果形态」而不是「本卡刚改的」，否则并行上线时后跑的那张卡会误报红。
- `biz_account` 的 `5602` 只有 ARCH9-2 动，本卡不碰（一期人工不进 5602）。
- `biz_stock_in` 的 `in_type` 值域本卡加 `PRODUCTION`；V0330 的重设 `ck_biz_si_type` 与 `ck_biz_si_supplier_scope` 是**同一个对象**，后续任何卡再改这个约束必须与本卡的 `ck_biz_si_production` 一起处理。

---

## 1. 业务背景与目标

### 1.1 要解决的问题（都是实测出来的，不是「感觉」）

1. **料工费没有归集地**。现在领料出库直接「借 6401 主营业务成本 / 贷 1405 库存商品」（`stockout_base::debit_account_of:167` → `stockout_handler.rs:747`，出库过账时执行）。也就是说：**料一进车间就当期损益化了**，在制品（WIP）永远是 0，任何时点的利润表都被「还没卖出去的货」压着。
   - 实测印证：`4001 生产成本` / `4101 制造费用` 两个科目**存在但从未被记账**（科目种子 V0183:111-112 种了；`grep` 全仓 `backend/src`，这两个编码只出现在现金流量表的科目归类清单 `cash_flow.rs:1646,1751,2115` 与测试里，**没有一处** `post_voucher` 往它们记账）。
2. **一批货的成本算不出来**。`biz_inventory.avg_cost` 是「物料×仓库」的移动加权（V0137:400 注释明写「粒度=物料×仓库，不按库位分裂」）——它回答「这个物料现在值多少钱」，回答不了「这一批 800 件是花了 27000 还是 31000 做出来的」。分批生产、按订单报价的客户第一句就问这个。
3. **没有差异分析**。没有标准成本卡，就没有「材料超用 320 件多花 3840 元」这种结论，成本改善无从下手。

评审报告 5.1 / 6.2 的原话「现在只有移动加权平均」，与本卡实测完全一致。

### 1.2 不解决什么问题（边界，写死）

| 不做 | 理由 |
|---|---|
| 换存货计价法（FIFO / 标准成本计价） | 见 D-1/D-2/D-15；属独立 ADR |
| 建 MES（工单/工艺路线/工序过站/报工） | 已暂停；本卡只做成本侧，数据源见 §4.5 |
| 多步骤分步结转（上步半成品 → 下步） | 三期（品种法）；没有工序数据也做不实 |
| 定额成本法 / 作业成本法（ABC） | 客户没有作业动因数据；约当产量法足够 |
| 二次分配（辅助车间 → 生产车间） | 见 D-8 |
| 差异入账（借/贷 1404）与期末差异分摊 | 见 D-15；二期只出差异报表 |
| 成本对象级的预算控制 | ARCH-6 已有费用预算（`biz_budget`），本卡不碰 |
| 停工损失 / 废品损失的单独核算 | 一期并入「制造费用-其他」；专门核算需要废品单据，属 MES 侧 |
| 跨公司成本分摊 | 多组织（ARCH9-1）落地后按 `org_id` 隔离，本期不跨组织结转 |

### 1.3 成功的判据（上线后拿这几条验收）

| 判据 | 现在 | 一期上线后 |
|---|---|---|
| ① 任一成本对象能回答「这批货单位成本多少」 | 不能 | 成本计算单给出 `unit_finished_cost`，且能按要素（料/工/费）拆开复算 |
| ② `4001 生产成本` 期末余额 = 期末在制合计 | 恒为 0（从未记账） | 巡检 C-COST-3 每天校验，容差 0.0001 |
| ③ 领料不再直接进损益 | 全部进 6401 | 挂成本对象的领料进 `4001.01`；非生产领用行为不变 |
| ④ 制造费用能按车间分摊到批 | 不能 | 分摊结果落表，`Σ分摊 = 待分摊总额` 守恒（D-9） |
| ⑤ 期末结账前成本一定算完 | 无约束 | 结账预检新增 blocker `COST_UNSETTLED`（§4.11） |
| ⑥ 二期：任一成品能出「材料价差/量差、人工工资率差/效率差」四项 | 不能 | 四张差异报表，两因素恒等式库层可验（算例 C 已证） |

---

## 2. 与现状的关系

### 2.1 现有实现盘点（全部实测，标了位置）

| 物件 | 现状 | 本卡动作 |
|---|---|---|
| `biz_inventory`（`avg_cost`/`amount`） | 物料×仓库移动加权，唯一成本真相；写入口唯一：`inventory_writer.rs`（CI 闸 `scripts/verify/inventory_write_gate.py` 禁止别的文件写这三张表） | **不动**。结算入库也必须走 `inventory_writer::add_cost`（:153）/`add_location_qty`（:830）/`insert_txn`（:1244） |
| `biz_inventory_txn` | 流水含 before/after × qty/amount/cost；`biz_type` 词表 `TxnBizType`（`doc_kind.rs:677`） | 用已有的 `ProductionIn='PRODUCTION_IN'`（:761）；领用沿用 `IssueOut` |
| `biz_stock_out` | `out_type` 四值 `ISSUE/SCRAP/LOSS/OTHER`（V0203:60,81,116）；ISSUE 借 6401 | **改造**：加 `cost_object_id`；挂了对象 → 借 `4001.01` |
| `biz_stock_in` | `in_type` 四值 `PURCHASE/RETURN/OTHER/SOURCELESS`（V0330:53），**没有生产入库** | **改造**：加 `PRODUCTION` + 两列 `cost_object_id`/`cost_period_id` |
| `biz_stock_in_item.lot_no` / `biz_stock_out_item.lot_no` / `biz_material_serial.batch_no` | 批号是自由文本（V0145:225 / V0203:141 / V0138:80） | **不动结构**；新增 `biz_lot` 主数据，新单据必须选，存量只做报告式回填 |
| `biz_account` | 56 个科目；`4001`/`4101`/`2211` 均 `is_leaf=1` 且无子科目；`1404 材料成本差异` 已存在（V0183:76） | **改造**：翻非末级 + 种 9 个末级（D-14） |
| `biz_voucher` / `biz_voucher_entry` | `post_voucher`（`voucher.rs:1963`）是唯一收口：≥2 行、借贷平衡、**只允许末级科目**、期间已结账只读 | **不动**；三张结转凭证全部经它落库 |
| `biz_depreciation` / `biz_expense_report` | 折旧按资产分类的 `expense_account_id` 记借方（ARCH-6 §8.2-5 已判「A 默认进 5602.01，B 可配进 4101」） | **不动**；成本核算**读**它们的明细做费用归集（不重复记账） |
| `biz_accounting_period` + `period.rs` 结账预检 | 四项 blocker：`DRAFT_VOUCHER` / `DOC_UNFINISHED` / `UNBALANCED` / `INVARIANT`（`period.rs:151-160`） | **改造**（小）：新增第 5 项 `COST_UNSETTLED` |
| `biz_seq` | 号段表（`PZ`/`IN`/`OUT`/`TK`/`SN`…） | **种子**：`COST_OBJECT`（CB-）、`COST_PERIOD`（CS-） |
| `sys_menu` / `sys_role_menu` | 财务管理＝`…-e02`（子项 sort 10~50）、仓库管理＝`…-e01`（10~70） | **种子**：6 页 + 20 按钮，挂这两组 |
| `sys_dict_type`/`sys_dict_item` | 通用字典 | **种子**：`cost_expense_item` 7 项（唯一落字典的成本项，理由见 §7.1） |
| `biz_flow_def` / `biz_flow_def_version` | 流程定义 + 版本快照（V0332 是四重守卫的范本） | **种子**：`COST_SETTLE` 两级（实现卡落地） |
| `biz_status_log` | `entity_type` **无 CHECK**（V0219:44 已写明） | 直接用 `COST_PERIOD` / `COST_OBJECT` |

### 2.2 为什么「成本对象核算」不是第二本成本账（正面回答 ARCH-4 D9）

ARCH-4 D9 的原话是「成本回写只写两处：`biz_voucher` 与 `biz_inventory.avg_cost`，**不建第二本成本账**」，并列举了反例「`biz_mes_wip_cost` 表：与 `biz_inventory` 重复；每次领料/报工都要同步两处，必漂移」。

本设计的定位与那条红线**不冲突**，判据如下：

| 判据 | 本设计 |
|---|---|
| 有没有第二份「库存数量/金额」？ | **没有**。数量与金额仍然只有 `biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 三张表，且全部经 `inventory_writer` 写 |
| 新表存的是什么？ | 存**归集过程**（料工费从哪来、怎么摊、怎么约当、怎么结转），不存「库存结存」。它是成本计算的**工作底稿**，不是账 |
| 会不会两边需要同步？ | 只有一个方向：`biz_cost_period_item` → `biz_stock_in.cost_price` → `inventory_writer` 加法。**库存账永远是下游**，反方向没有写 |
| 与 `avg_cost` 的关系？ | 完工入库把「这一批的实际单位成本」作为入库单价喂给移动加权池（和采购入库一样）。移动加权仍然是唯一的库存计价法 |
| 有没有巡检保证不漂？ | 有，6 条不变量（§4.11），其中最硬的一条是 C-COST-2：`Σ完工入库金额 = 结转凭证贷方合计` |

**与 ARCH-4 真正的差别**：ARCH-4 一期不做成本归集（只把人工成本摊进 `stock_in.cost_price`），本卡把「归集与约当」补上。ARCH-4 的 `biz_mes_wo.actual_labor_cost` 等快照列在 MES 上线后成为**工时台账的一路来源**（`source_type='MES'`），不冲突。

### 2.3 新增 / 改造 / 不动 清单

**新增（13 张表）**

| # | 表 | 期 | 一句话 |
|---|---|---|---|
| ① | `biz_lot` | 一 | 批次主数据（物料级批号） |
| ② | `biz_cost_object` | 一 | 成本对象（生产批） |
| ③ | `biz_cost_collect` | 一 | 成本归集明细（料工费统一入口） |
| ④ | `biz_cost_labor` | 一 | 工时台账（人工数据源） |
| ⑤ | `biz_cost_overhead` | 一 | 手工制造费用台账 |
| ⑥ | `biz_cost_rate` | 一 | 工资率/机器费率（版本化） |
| ⑦ | `biz_cost_alloc_rule` | 一 | 分摊规则 |
| ⑧ | `biz_cost_alloc_result` | 一 | 分摊结果（含尾差调整） |
| ⑨ | `biz_cost_period` | 一 | 成本核算批次（期间结算单） |
| ⑩ | `biz_cost_period_item` | 一 | 成本计算单（成本对象×期间） |
| ⑪ | `biz_cost_standard` | 二 | 标准成本卡（卡头） |
| ⑫ | `biz_cost_standard_item` | 二 | 标准成本卡明细（标准用量/标准价） |
| ⑬ | `biz_cost_variance` | 二 | 差异明细（六类差异） |

**改造（4 类结构 + 5 类种子）**

| 对象 | 改法 | 存量影响 |
|---|---|---|
| `biz_stock_out` | `+ cost_object_id UUID`（可空）+ 局部索引 | 全 NULL，行为不变 |
| `biz_stock_in` | `+ cost_object_id`、`+ cost_period_id`（可空）；`ck_biz_si_type` 加 `PRODUCTION`；新增 `ck_biz_si_production` | 存量 `in_type` 值一个不改 |
| `biz_account` | `4001`/`4101`/`2211` 翻非末级；种 9 个末级 | 翻非末级前实测这三个科目**没有任何历史分录**（见 §7.2） |
| `period.rs` 结账预检 | 加第 5 项 blocker `COST_UNSETTLED` | 只有在存在未结成本批次时才拦 |
| 种子 | 科目 9 + 菜单 6 页/20 按钮 + 号段 2 + 字典 1 类 7 项 + 审批流 1 | 全部 `ON CONFLICT DO NOTHING` |

**不动**：`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 结构、`biz_material.costing_method` 值域、`biz_voucher` 结构、`biz_depreciation`、`biz_expense_report`、`sys_user`、`sys_dept`。

### 2.4 与 ARCH-1~10 的关系

| 设计 | 关系 | 说明 |
|---|---|---|
| ARCH-1/2 库存与订单基建 | **复用** | 库存三本账、`inventory_writer`、单据状态机、凭证引擎全部复用 |
| ARCH-3 勾稽审计 | **复用 + 增补** | DC-16（项目成本尚未接通）本卡**不解决**；本卡增加 6 条成本不变量到巡检 |
| ARCH-4 MES/委外 | **有条件让路** | D9「只写两处」服从（§2.2）；MES 上线后 `biz_work_log` → 本卡 `biz_cost_labor`（`source_type='MES'`）；委外加工费（`biz_outs_settlement`）三期并入 `4001.04`（本卡只留位置，不实现） |
| ARCH-5 流程引擎 | **复用** | `COST_SETTLE` 走引擎，两级审批；不改引擎 |
| ARCH-6 财务扩展 | **复用 + 一处冲突已解** | 折旧进 4101 只需改资产分类配置（ARCH-6 §8.2-5 的 B 选项）；**冲突点**：ARCH-6 假定折旧全进 5602.01，成本核算要求生产设备进 4101 —— 已由「分类级配置」解决，本卡不改代码 |
| ARCH-7 销售 | **不动** | 销售出库仍按移动加权结转 6401；分批法只影响**完工入库**单价，不影响销售出库路径 |
| ARCH-8 职责分离 | **复用** | 结算审批人与制单人分离（`separation::guard_self_approval`） |
| ARCH-9-1 多组织 | **前置依赖（软）** | 本卡所有表带 `org_id` 列；`biz_cost_period` 用哨兵默认值保证唯一键一次到位；不上多组织也能跑 |
| ARCH-9-2 人力资源/薪资 | **接口对齐** | 人工来源二选一（D-13）；`biz_cost_labor.employee_id` 弱引用 `biz_employee.id`，未上线时为空 |
| ARCH-10 产品演进 | **本卡即其「成本核算（分批法/标准成本）」条目**（`ARCH-10:145`） | 分期与 ARCH-10 一致 |

### 2.5 会不会破坏现有数据（逐项论证：不会）

| 风险点 | 论证 |
|---|---|
| 加列 | 全部 `ADD COLUMN IF NOT EXISTS` 且**可空**，不设 `NOT NULL` 默认值（避免全表重写锁） |
| `ck_biz_si_type` 加值 | 只**放宽**值域（4 → 5），存量行必然仍满足 |
| `biz_account` 翻非末级 | 翻前实测无历史分录（§7.2 给 SQL）；翻后 `post_voucher` 的末级闸只会**更严**（记账到 4001 会被拒 → 但本卡同时种了 4001.01~03 供记账，且没有任何存量凭证记 4001） |
| 报表口径 | 存货报表项已含 `1401,1402,1403,1404,1405,1408,4001,4101`（V0183:192）→ 4001/4101 有余额时**自动进存货**，报表映射一行不改 |
| 菜单/号段/字典 | 纯插入，`ON CONFLICT DO NOTHING` |
| 三处 `lot_no` | 结构零改动，只加校验（新单据校验存在性；存量单据不改不进校验） |

---

## 3. 数据模型

> 本节 DDL 与 `docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql` **逐字一致**，该脚本已在 PG 真库跑通（输出 `01_cost_ddl_check.out`：`13 表 / 4 类改造 / 6 类种子 / 周期唯一键口径` 全过，`ROLLBACK` 后零残留）。

### 3.1 一期 10 张表

#### ① `biz_lot` —— 批次主数据

```sql
CREATE TABLE IF NOT EXISTS biz_lot (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    lot_no          VARCHAR(64)   NOT NULL,                          -- 批号（业务键，物料级唯一）
    material_id     UUID          NOT NULL,                          -- 物料（弱引用 biz_material.id，不建 FK）
    source_type     VARCHAR(16)   NOT NULL DEFAULT 'MANUAL',         -- PURCHASE / PRODUCTION / OPENING / MANUAL
    source_doc_type VARCHAR(24),                                     -- STOCK_IN / COST_PERIOD / 空
    source_doc_id   UUID,
    source_doc_no   VARCHAR(32),                                     -- 来源单号快照（审计口径）
    supplier_id     UUID,                                            -- 采购批供应商（弱引用）
    prod_date       DATE,
    expire_date     DATE,
    qc_status       VARCHAR(12)   NOT NULL DEFAULT 'NA',             -- NA / PENDING / PASSED / REJECTED
    status          SMALLINT      NOT NULL DEFAULT 1,
    remark          TEXT,
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_biz_lot_source CHECK (source_type IN ('PURCHASE','PRODUCTION','OPENING','MANUAL')),
    CONSTRAINT ck_biz_lot_qc     CHECK (qc_status IN ('NA','PENDING','PASSED','REJECTED')),
    CONSTRAINT ck_biz_lot_status CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_lot_dates  CHECK (expire_date IS NULL OR prod_date IS NULL OR expire_date >= prod_date)
);
CREATE UNIQUE INDEX uk_biz_lot_material_no ON biz_lot (material_id, lot_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_lot_no     ON biz_lot (lot_no);
CREATE INDEX idx_biz_lot_source ON biz_lot (source_doc_type, source_doc_id);
```

#### ② `biz_cost_object` —— 成本对象（生产批）

```sql
CREATE TABLE IF NOT EXISTS biz_cost_object (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    object_no           VARCHAR(32)   NOT NULL,                      -- CB-yyyyMMdd-000001
    object_type         VARCHAR(12)   NOT NULL DEFAULT 'BATCH',      -- BATCH / ORDER / PRODUCT（一期只写 BATCH）
    product_id          UUID          NOT NULL,                      -- 产出品（弱引用）
    lot_no              VARCHAR(64),                                 -- 产出批号
    target_warehouse_id UUID          NOT NULL,                      -- 完工入库仓（弱引用）
    target_location_id  UUID,                                        -- 完工入库库位
    plan_qty            NUMERIC(18,4) NOT NULL,
    completed_qty       NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 累计完工数量（结算回写）
    wip_qty             NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 期末在制数量（结算必填）
    completion_pct      NUMERIC(5,2)  NOT NULL DEFAULT 0,            -- 期末在制完工进度%
    material_input_mode VARCHAR(16)   NOT NULL DEFAULT 'ONE_TIME',   -- ONE_TIME / AS_PROGRESS（一期只实现 ONE_TIME）
    source_doc_no       VARCHAR(32),                                 -- 上游订单号（按订单生产时手填）
    customer_id         UUID,                                        -- 按订单生产时的客户
    workshop_dept_id    UUID          NOT NULL,                      -- 生产车间（弱引用 sys_dept.id）
    workshop_dept_name  VARCHAR(120)  NOT NULL,                      -- 车间名快照（审计口径）
    start_date          DATE          NOT NULL,
    due_date            DATE,
    status              VARCHAR(12)   NOT NULL DEFAULT 'OPEN',       -- OPEN / CLOSED / CANCELLED
    closed_at           TIMESTAMPTZ,
    closed_by           UUID,
    remark              TEXT,
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_object_type   CHECK (object_type IN ('BATCH','ORDER','PRODUCT')),
    CONSTRAINT ck_biz_cost_object_status CHECK (status IN ('OPEN','CLOSED','CANCELLED')),
    CONSTRAINT ck_biz_cost_object_input  CHECK (material_input_mode IN ('ONE_TIME','AS_PROGRESS')),
    CONSTRAINT ck_biz_cost_object_qty    CHECK (plan_qty > 0 AND completed_qty >= 0 AND wip_qty >= 0),
    CONSTRAINT ck_biz_cost_object_pct    CHECK (completion_pct >= 0 AND completion_pct <= 100),
    CONSTRAINT ck_biz_cost_object_dates  CHECK (due_date IS NULL OR due_date >= start_date),
    CONSTRAINT ck_biz_cost_object_closed CHECK (status <> 'CLOSED' OR closed_at IS NOT NULL)
);
CREATE UNIQUE INDEX uk_biz_cost_object_no      ON biz_cost_object (object_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_cost_object_product       ON biz_cost_object (product_id, status);
CREATE INDEX idx_biz_cost_object_workshop      ON biz_cost_object (workshop_dept_id, start_date DESC);
CREATE INDEX idx_biz_cost_object_lot           ON biz_cost_object (lot_no) WHERE lot_no IS NOT NULL;
```

#### ③ `biz_cost_collect` —— 成本归集明细（**本卡核心表**）

```sql
CREATE TABLE IF NOT EXISTS biz_cost_collect (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cost_object_id UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,  -- 空=车间费用池（待分摊）
    period_code    VARCHAR(7)    NOT NULL,                           -- 归属会计期间 yyyy-MM
    element        VARCHAR(12)   NOT NULL,                           -- MATERIAL / LABOR / OVERHEAD
    source_type    VARCHAR(16)   NOT NULL,                           -- STOCK_OUT/LABOR_TICKET/OVERHEAD_TICKET/DEPRECIATION/EXPENSE/ALLOC/MANUAL
    source_id      UUID,                                             -- 来源行 id（MANUAL 为空）
    source_no      VARCHAR(32),                                      -- 来源单号快照
    line_no        INT           NOT NULL DEFAULT 0,                 -- 来源行号（幂等键组成）
    occur_date     DATE          NOT NULL,
    material_id    UUID,                                             -- 料行物料（弱引用）
    qty            NUMERIC(18,4),                                    -- 料行数量
    unit_price     NUMERIC(18,6),                                    -- 料行单价（移动加权出库单价快照）
    amount         NUMERIC(18,4) NOT NULL,                           -- 归集金额（不含税）
    dept_id        UUID,                                             -- 费行归属车间
    dept_name      VARCHAR(120),                                     -- 车间名快照
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_collect_element CHECK (element IN ('MATERIAL','LABOR','OVERHEAD')),
    CONSTRAINT ck_biz_cost_collect_src     CHECK (source_type IN
        ('STOCK_OUT','LABOR_TICKET','OVERHEAD_TICKET','DEPRECIATION','EXPENSE','ALLOC','MANUAL')),
    CONSTRAINT ck_biz_cost_collect_amount  CHECK (amount >= 0),
    CONSTRAINT ck_biz_cost_collect_qty     CHECK (qty IS NULL OR qty >= 0),
    CONSTRAINT ck_biz_cost_collect_period  CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    -- 料行必须有物料与数量；工/费行不许挂物料（防止「一张表两种语义」）
    CONSTRAINT ck_biz_cost_collect_mat     CHECK (
        (element = 'MATERIAL' AND material_id IS NOT NULL AND qty IS NOT NULL)
     OR (element <> 'MATERIAL' AND material_id IS NULL AND qty IS NULL)),
    -- 归属约束：只有「车间费用池」三类来源允许不挂成本对象（分摊前），其余必须挂
    CONSTRAINT ck_biz_cost_collect_obj     CHECK (
        cost_object_id IS NOT NULL
     OR (element = 'OVERHEAD' AND source_type IN ('DEPRECIATION','EXPENSE','OVERHEAD_TICKET')))
);
-- 幂等键：同一来源行只能归集一次
CREATE UNIQUE INDEX uk_biz_cost_collect_src
    ON biz_cost_collect (source_type, source_id, line_no)
    WHERE deleted_at IS NULL AND source_id IS NOT NULL;
CREATE INDEX idx_biz_cost_collect_obj    ON biz_cost_collect (cost_object_id, period_code, element);
CREATE INDEX idx_biz_cost_collect_period ON biz_cost_collect (period_code);
```

#### ④ `biz_cost_labor` —— 工时台账

```sql
CREATE TABLE IF NOT EXISTS biz_cost_labor (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code    VARCHAR(7)    NOT NULL,
    work_date      DATE          NOT NULL,
    cost_object_id UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    employee_id    UUID,                                             -- 弱引用 biz_employee.id（ARCH9-2）
    employee_name  VARCHAR(50)   NOT NULL,                           -- 姓名快照
    dept_id        UUID          NOT NULL,                           -- 工作中心
    dept_name      VARCHAR(120)  NOT NULL,                           -- 工作中心名快照
    hours          NUMERIC(10,2) NOT NULL,                           -- 人工工时
    machine_hours  NUMERIC(10,2) NOT NULL DEFAULT 0,                  -- 机器工时（另一分摊基数）
    rate           NUMERIC(18,6) NOT NULL,                           -- 工资率快照（元/小时）
    amount         NUMERIC(18,4) NOT NULL,                           -- = round(hours × rate, 4)
    source_type    VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',          -- MANUAL / IMPORT / MES
    status         VARCHAR(12)   NOT NULL DEFAULT 'DRAFT',           -- DRAFT / CONFIRMED（结算只取 CONFIRMED）
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_labor_hours  CHECK (hours > 0 AND machine_hours >= 0),
    CONSTRAINT ck_biz_cost_labor_rate   CHECK (rate >= 0),
    CONSTRAINT ck_biz_cost_labor_src    CHECK (source_type IN ('MANUAL','IMPORT','MES')),
    CONSTRAINT ck_biz_cost_labor_status CHECK (status IN ('DRAFT','CONFIRMED')),
    CONSTRAINT ck_biz_cost_labor_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    -- 金额必须等于工时×工资率（4 位四舍五入）：堵「改工时忘改金额」
    CONSTRAINT ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))
);
CREATE INDEX idx_biz_cost_labor_obj    ON biz_cost_labor (cost_object_id, period_code);
CREATE INDEX idx_biz_cost_labor_emp    ON biz_cost_labor (employee_id, work_date);
CREATE INDEX idx_biz_cost_labor_period ON biz_cost_labor (period_code, status);
```

#### ⑤ `biz_cost_overhead` —— 手工制造费用台账

```sql
CREATE TABLE IF NOT EXISTS biz_cost_overhead (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code         VARCHAR(7)    NOT NULL,
    cost_object_id      UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,  -- 空=车间级
    dept_id             UUID          NOT NULL,
    dept_name           VARCHAR(120)  NOT NULL,
    expense_item        VARCHAR(16)   NOT NULL,                      -- 字典 cost_expense_item
    debit_account_code  VARCHAR(30)   NOT NULL DEFAULT '4101.09',
    credit_account_code VARCHAR(30)   NOT NULL,                      -- 对方科目（1002/2241.xx/1801…）
    amount              NUMERIC(18,4) NOT NULL,
    source_type         VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',     -- MANUAL / IMPORT
    remark              VARCHAR(255),
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_oh_item   CHECK (expense_item IN
        ('DEPRECIATION','UTILITY','RENT','REPAIR','WORKSHOP_ADMIN','CONSUMABLE','OTHER')),
    CONSTRAINT ck_biz_cost_oh_src    CHECK (source_type IN ('MANUAL','IMPORT')),
    CONSTRAINT ck_biz_cost_oh_amount CHECK (amount > 0),
    CONSTRAINT ck_biz_cost_oh_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_oh_debit  CHECK (debit_account_code LIKE '4101.%')
);
CREATE INDEX idx_biz_cost_oh_period ON biz_cost_overhead (period_code, dept_id);
CREATE INDEX idx_biz_cost_oh_obj    ON biz_cost_overhead (cost_object_id) WHERE cost_object_id IS NOT NULL;
```

#### ⑥ `biz_cost_rate` —— 工资率 / 机器费率

```sql
CREATE TABLE IF NOT EXISTS biz_cost_rate (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rate_type      VARCHAR(12)   NOT NULL,                           -- LABOR / MACHINE
    scope          VARCHAR(12)   NOT NULL,                           -- COMPANY / DEPT / COST_OBJECT
    scope_id       UUID,                                             -- COMPANY 为空
    rate           NUMERIC(18,6) NOT NULL,
    effective_from DATE          NOT NULL,
    effective_to   DATE,
    status         SMALLINT      NOT NULL DEFAULT 1,
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_rate_type   CHECK (rate_type IN ('LABOR','MACHINE')),
    CONSTRAINT ck_biz_cost_rate_scope  CHECK (scope IN ('COMPANY','DEPT','COST_OBJECT')),
    CONSTRAINT ck_biz_cost_rate_rate   CHECK (rate >= 0),
    CONSTRAINT ck_biz_cost_rate_status CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_cost_rate_dates  CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_rate_scope_id CHECK (
        (scope = 'COMPANY' AND scope_id IS NULL) OR (scope <> 'COMPANY' AND scope_id IS NOT NULL))
);
CREATE UNIQUE INDEX uk_biz_cost_rate
    ON biz_cost_rate (rate_type, scope, coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid), effective_from)
    WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_cost_rate_lookup ON biz_cost_rate (rate_type, status, effective_from DESC);
```

#### ⑦ `biz_cost_alloc_rule` —— 分摊规则

```sql
CREATE TABLE IF NOT EXISTS biz_cost_alloc_rule (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_code        VARCHAR(32)   NOT NULL,
    rule_name        VARCHAR(120)  NOT NULL,
    biz_type         VARCHAR(16)   NOT NULL DEFAULT 'OVERHEAD',      -- 一期 OVERHEAD；三期 STEP_TRANSFER
    source_dept_id   UUID          NOT NULL,                         -- 被摊车间
    source_dept_name VARCHAR(120)  NOT NULL,                         -- 车间名快照
    account_code     VARCHAR(30)   NOT NULL,                         -- 被摊科目（4101.01~09）
    basis            VARCHAR(16)   NOT NULL,                         -- LABOR_HOURS/MACHINE_HOURS/OUTPUT_QTY/MATERIAL_COST
    priority         INT           NOT NULL DEFAULT 10,
    effective_from   DATE          NOT NULL,
    effective_to     DATE,
    status           SMALLINT      NOT NULL DEFAULT 1,
    remark           VARCHAR(255),
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_rule_biz     CHECK (biz_type IN ('OVERHEAD')),
    CONSTRAINT ck_biz_cost_rule_basis   CHECK (basis IN ('LABOR_HOURS','MACHINE_HOURS','OUTPUT_QTY','MATERIAL_COST')),
    CONSTRAINT ck_biz_cost_rule_status  CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_cost_rule_dates   CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_rule_account CHECK (account_code LIKE '4101.%')
);
CREATE UNIQUE INDEX uk_biz_cost_rule_code ON biz_cost_alloc_rule (rule_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_cost_rule_dept ON biz_cost_alloc_rule (source_dept_id, account_code, status);
```

#### ⑧ `biz_cost_alloc_result` —— 分摊结果

```sql
CREATE TABLE IF NOT EXISTS biz_cost_alloc_result (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code     VARCHAR(7)    NOT NULL,
    rule_id         UUID          NOT NULL REFERENCES biz_cost_alloc_rule(id) ON DELETE RESTRICT,
    cost_object_id  UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    dept_id         UUID          NOT NULL,
    dept_name       VARCHAR(120)  NOT NULL,
    account_code    VARCHAR(30)   NOT NULL,
    basis_value     NUMERIC(18,4) NOT NULL,                          -- 本对象基数
    basis_total     NUMERIC(18,4) NOT NULL,                          -- 全车间基数合计（快照）
    pool_amount     NUMERIC(18,4) NOT NULL,                          -- 待分摊总额（快照）
    alloc_amount    NUMERIC(18,4) NOT NULL,                          -- 摊到本对象（含尾差）
    rounding_adj    NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 尾差调整（最大余数法：仅一行非 0）
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    CONSTRAINT ck_biz_cost_alloc_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_alloc_num    CHECK (basis_value >= 0 AND basis_total >= 0
                                              AND pool_amount >= 0 AND alloc_amount >= 0),
    CONSTRAINT ck_biz_cost_alloc_base   CHECK (basis_total > 0 OR alloc_amount = 0)
);
CREATE UNIQUE INDEX uk_biz_cost_alloc_result
    ON biz_cost_alloc_result (period_code, rule_id, cost_object_id, account_code);
CREATE INDEX idx_biz_cost_alloc_result_period ON biz_cost_alloc_result (period_code, dept_id);
```

#### ⑨ `biz_cost_period` —— 成本核算批次（期间结算单）

```sql
CREATE TABLE IF NOT EXISTS biz_cost_period (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id               UUID          NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    period_code          VARCHAR(7)    NOT NULL,
    batch_no             VARCHAR(32)   NOT NULL,                     -- CS-yyyyMM-0001
    status               VARCHAR(12)   NOT NULL DEFAULT 'DRAFT',
    labor_source         VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',    -- MANUAL / PAYROLL（二选一，防双计提）
    object_count         INT           NOT NULL DEFAULT 0,
    total_material       NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_labor          NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_overhead       NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_cost           NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_finished       NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_wip            NUMERIC(18,4) NOT NULL DEFAULT 0,
    approval_instance_id UUID,
    labor_voucher_id     UUID,
    overhead_voucher_id  UUID,
    finish_voucher_id    UUID,
    settled_at           TIMESTAMPTZ,
    settled_by           UUID,
    reversed_at          TIMESTAMPTZ,
    reversed_by          UUID,
    reverse_reason       VARCHAR(255),
    cancel_reason        VARCHAR(255),
    remark               TEXT,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_period_code   CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_period_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','SETTLED','CLOSED','CANCELLED','REVERSED')),
    CONSTRAINT ck_biz_cost_period_labor  CHECK (labor_source IN ('MANUAL','PAYROLL')),
    CONSTRAINT ck_biz_cost_period_sum    CHECK (total_cost = total_material + total_labor + total_overhead),
    CONSTRAINT ck_biz_cost_period_cons   CHECK (total_cost = total_finished + total_wip),
    CONSTRAINT ck_biz_cost_period_appr   CHECK (status NOT IN ('SUBMITTED','APPROVED','SETTLED','CLOSED','REVERSED')
                                                OR approval_instance_id IS NOT NULL),
    CONSTRAINT ck_biz_cost_period_settle CHECK (status NOT IN ('SETTLED','CLOSED','REVERSED')
                                                OR (settled_at IS NOT NULL AND finish_voucher_id IS NOT NULL)),
    CONSTRAINT ck_biz_cost_period_rev    CHECK (status <> 'REVERSED'
                                                OR (reversed_at IS NOT NULL AND reverse_reason IS NOT NULL)),
    CONSTRAINT ck_biz_cost_period_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);
-- ★ 唯一键排除 REVERSED/CANCELLED：红冲/作废行留在表里（审计），但不再占位 ——
--   否则红冲后永远无法重新结算该期间（死单）。真库已实测两种情形（附录 A 断言⑦）。
CREATE UNIQUE INDEX uk_biz_cost_period
    ON biz_cost_period (org_id, period_code)
    WHERE deleted_at IS NULL AND status NOT IN ('REVERSED','CANCELLED');
CREATE UNIQUE INDEX uk_biz_cost_period_no     ON biz_cost_period (batch_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_cost_period_status       ON biz_cost_period (status, period_code DESC);
```

#### ⑩ `biz_cost_period_item` —— 成本计算单

```sql
CREATE TABLE IF NOT EXISTS biz_cost_period_item (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id          UUID          NOT NULL REFERENCES biz_cost_period(id) ON DELETE CASCADE,
    cost_object_id     UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    product_id         UUID          NOT NULL,                        -- 与 cost_object.product_id 冗余（防对象改品）
    wip_beg_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_beg_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,
    completed_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_end_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,
    completion_pct     NUMERIC(5,2)  NOT NULL DEFAULT 0,
    equiv_material     NUMERIC(18,4) NOT NULL DEFAULT 0,
    equiv_conversion   NUMERIC(18,4) NOT NULL DEFAULT 0,
    mat_cur            NUMERIC(18,4) NOT NULL DEFAULT 0,
    labor_cur          NUMERIC(18,4) NOT NULL DEFAULT 0,
    oh_cur             NUMERIC(18,4) NOT NULL DEFAULT 0,
    unit_material      NUMERIC(18,6) NOT NULL DEFAULT 0,
    unit_labor         NUMERIC(18,6) NOT NULL DEFAULT 0,
    unit_overhead      NUMERIC(18,6) NOT NULL DEFAULT 0,
    finished_cost      NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_end_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,
    unit_finished_cost NUMERIC(18,6) NOT NULL DEFAULT 0,             -- → stock_in_item.cost_price
    stock_in_id        UUID,
    status             VARCHAR(12)   NOT NULL DEFAULT 'CALC',        -- CALC / SETTLED
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_cost_item_status  CHECK (status IN ('CALC','SETTLED')),
    CONSTRAINT ck_biz_cost_item_equiv   CHECK (equiv_material >= 0 AND equiv_conversion >= 0),
    CONSTRAINT ck_biz_cost_item_units   CHECK (unit_material >= 0 AND unit_labor >= 0 AND unit_overhead >= 0),
    CONSTRAINT ck_biz_cost_item_money   CHECK (finished_cost >= 0 AND wip_end_cost >= 0 AND wip_beg_cost >= 0),
    CONSTRAINT ck_biz_cost_item_pct     CHECK (completion_pct >= 0 AND completion_pct <= 100),
    -- ★ 成本守恒（库层）：完工转出 + 期末在制 = 期初在制 + 本期投入
    CONSTRAINT ck_biz_cost_item_cons    CHECK (
        finished_cost + wip_end_cost = wip_beg_cost + mat_cur + labor_cur + oh_cur),
    CONSTRAINT ck_biz_cost_item_settled CHECK (status <> 'SETTLED' OR stock_in_id IS NOT NULL)
);
CREATE UNIQUE INDEX uk_biz_cost_item    ON biz_cost_period_item (period_id, cost_object_id);
CREATE INDEX idx_biz_cost_item_obj      ON biz_cost_period_item (cost_object_id);
```

### 3.2 二期 3 张表（标准成本 + 差异）

```sql
-- ⑪ 标准成本卡（卡头）：单位标准料/工/费，按生效期版本化。只用于差异分析，不参与存货计价（D-15）
CREATE TABLE IF NOT EXISTS biz_cost_standard (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id        UUID          NOT NULL,
    version_no         INT           NOT NULL DEFAULT 1,
    effective_from     DATE          NOT NULL,
    effective_to       DATE,
    std_material_cost  NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_labor_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_overhead_cost  NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_total_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_labor_hours    NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 单位标准工时
    std_labor_rate     NUMERIC(18,6) NOT NULL DEFAULT 0,             -- 标准工资率
    status             SMALLINT      NOT NULL DEFAULT 1,
    remark             TEXT,
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID, update_by UUID, deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_std_money CHECK (std_material_cost >= 0 AND std_labor_cost >= 0
                                            AND std_overhead_cost >= 0 AND std_total_cost >= 0),
    CONSTRAINT ck_biz_cost_std_sum   CHECK (std_total_cost = std_material_cost + std_labor_cost + std_overhead_cost),
    CONSTRAINT ck_biz_cost_std_dates CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_std_ver   CHECK (version_no >= 1),
    CONSTRAINT ck_biz_cost_std_labor CHECK (std_labor_cost = round(std_labor_hours * std_labor_rate, 6))
);
CREATE UNIQUE INDEX uk_biz_cost_std ON biz_cost_standard (material_id, effective_from) WHERE deleted_at IS NULL;

-- ⑫ 标准成本卡明细：材料量差需要「标准用量」，必须有 BOM 级明细
CREATE TABLE IF NOT EXISTS biz_cost_standard_item (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    standard_id           UUID          NOT NULL REFERENCES biz_cost_standard(id) ON DELETE CASCADE,
    line_no               INT           NOT NULL,
    component_material_id UUID          NOT NULL,
    std_qty               NUMERIC(18,6) NOT NULL,
    std_price             NUMERIC(18,6) NOT NULL,
    std_amount            NUMERIC(18,6) NOT NULL,
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_cost_std_item_num CHECK (std_qty > 0 AND std_price >= 0 AND std_amount >= 0),
    CONSTRAINT ck_biz_cost_std_item_cal CHECK (std_amount = round(std_qty * std_price, 6))
);
CREATE UNIQUE INDEX uk_biz_cost_std_item ON biz_cost_standard_item (standard_id, line_no);

-- ⑬ 差异明细：六类差异（§4.9 给公式与恒等式）
CREATE TABLE IF NOT EXISTS biz_cost_variance (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code    VARCHAR(7)    NOT NULL,
    cost_object_id UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    standard_id    UUID          REFERENCES biz_cost_standard(id) ON DELETE RESTRICT,
    variance_type  VARCHAR(12)   NOT NULL,
    element        VARCHAR(12)   NOT NULL,
    material_id    UUID,
    actual_qty     NUMERIC(18,6), std_qty    NUMERIC(18,6),
    actual_price   NUMERIC(18,6), std_price  NUMERIC(18,6),
    amount         NUMERIC(18,4) NOT NULL,                           -- 正=不利，负=有利
    direction      VARCHAR(12)   NOT NULL,                           -- FAVORABLE / UNFAVORABLE
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    CONSTRAINT ck_biz_cost_var_type CHECK (variance_type IN
        ('MAT_PRICE','MAT_QTY','LABOR_RATE','LABOR_EFF','OH_SPEND','OH_VOLUME')),
    CONSTRAINT ck_biz_cost_var_elem  CHECK (element IN ('MATERIAL','LABOR','OVERHEAD')),
    CONSTRAINT ck_biz_cost_var_dir   CHECK (direction IN ('FAVORABLE','UNFAVORABLE')),
    CONSTRAINT ck_biz_cost_var_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_var_sign  CHECK (
        (direction = 'UNFAVORABLE' AND amount >= 0) OR (direction = 'FAVORABLE' AND amount <= 0))
);
CREATE INDEX idx_biz_cost_var_period ON biz_cost_variance (period_code, variance_type);
CREATE INDEX idx_biz_cost_var_obj    ON biz_cost_variance (cost_object_id, period_code);
```

### 3.3 改造现有表（4 类，全部只加列 / 只放宽值域）

```sql
-- ① 领料挂成本对象（存量 NULL ⇒ 过账借方仍 6401，行为不变）
ALTER TABLE biz_stock_out ADD COLUMN IF NOT EXISTS cost_object_id UUID;
CREATE INDEX idx_biz_so_cost_obj ON biz_stock_out (cost_object_id) WHERE cost_object_id IS NOT NULL;

-- ② 完工入库：in_type 加 PRODUCTION + 两列
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS cost_object_id UUID;
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS cost_period_id UUID;

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_type
    CHECK (in_type IN ('PURCHASE','RETURN','OTHER','SOURCELESS','PRODUCTION'));

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_production;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_production
    CHECK (in_type <> 'PRODUCTION'
           OR (cost_object_id IS NOT NULL AND cost_period_id IS NOT NULL
               AND supplier_id IS NULL AND is_sourceless = FALSE));

CREATE INDEX idx_biz_si_cost_obj    ON biz_stock_in (cost_object_id) WHERE cost_object_id IS NOT NULL;
CREATE INDEX idx_biz_si_cost_period ON biz_stock_in (cost_period_id) WHERE cost_period_id IS NOT NULL;

-- ③ 科目：翻非末级 + 种末级
UPDATE biz_account SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP
 WHERE account_code IN ('4001','4101') AND is_leaf = 1;

INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status)
SELECT v.code, v.name, p.id, p.level + 1, p.account_category, p.balance_direction, 1, 1
  FROM (VALUES ('4001.01','生产成本-直接材料','4001'),
               ('4001.02','生产成本-直接人工','4001'),
               ('4001.03','生产成本-制造费用','4001'),
               ('4101.01','制造费用-折旧费','4101'),
               ('4101.02','制造费用-水电费','4101'),
               ('4101.03','制造费用-修理费','4101'),
               ('4101.04','制造费用-车间管理费','4101'),
               ('4101.09','制造费用-其他','4101')
       ) AS v(code, name, parent_code)
  JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;

-- 人工凭证贷方 2211.01（与 ARCH9-2 同键，幂等；见 D-14 / 跨卡热点）
UPDATE biz_account SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP
 WHERE account_code = '2211' AND is_leaf = 1;
INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status)
SELECT '2211.01','应付职工薪酬-工资', p.id, p.level + 1, p.account_category, p.balance_direction, 1, 1
  FROM biz_account p WHERE p.account_code = '2211' AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;
```

> 菜单 / 号段 / 字典种子与本期 `biz_seq` 实测口径，见 §7.1 迁移清单与证据脚本第三部分（已在真库断言）。

### 3.4 外键与弱引用约定

| 关系 | 做法 | 理由 |
|---|---|---|
| 成本模块内部（`biz_cost_collect` → `biz_cost_object` 等） | **物理 FK**，`ON DELETE RESTRICT`（`period_item` → `period` 用 `CASCADE`） | 同模块内一致性要求高；RESTRICT 防「删了对象留下孤儿归集」 |
| 成本模块 → 主数据（`biz_material` / `biz_warehouse` / `sys_dept` / `biz_employee` / `biz_customer`） | **弱引用**（只存 id，不建 FK） | 与全仓口径一致（V0137:400 起就是弱引用）；主数据软删/重建不该锁死历史成本 |
| 成本模块 → 单据（`biz_stock_out` / `biz_stock_in` / `biz_depreciation` / `biz_expense_report`） | **弱引用 + 单号快照** | 单据表分属不同事务路径；快照保证单据改名/作废后成本表可追溯（AGENTS 约定 13：审计/历史类存快照） |

### 3.5 数据量预估（每客户每年）

假设：50 个成本对象/月、每对象 30 条归集明细、20 条工时、3 个车间各 2 条分摊规则。

| 表 | 每月行数 | 每年 | 10 年 | 备注 |
|---|---|---|---|---|
| `biz_cost_object` | 50 | 600 | 6,000 | 主数据级 |
| `biz_cost_collect` | 50×(30) ≈ 1,500 | 18,000 | 180,000 | **最大表**；索引 `(cost_object_id, period_code, element)` 覆盖主查询 |
| `biz_cost_labor` | 50×20 ≈ 1,000 | 12,000 | 120,000 | 明细级；可按 `period_code` 归档 |
| `biz_cost_overhead` | 3 车间×5 ≈ 15 | 180 | 1,800 | 很小 |
| `biz_cost_alloc_result` | 50×2 规则 ≈ 100 | 1,200 | 12,000 | 小 |
| `biz_cost_period` | 1 | 12 | 120 | 极小 |
| `biz_cost_period_item` | 50 | 600 | 6,000 | 小 |
| `biz_lot` | 60（采购批+生产批） | 720 | 7,200 | 小 |
| 二期 `biz_cost_variance` | 50×6 ≈ 300 | 3,600 | 36,000 | 三期后可归档 |

总量 10 年 < 40 万行，单机 PG 毫无压力；真正的压力点是 `biz_cost_collect` 的「按期间重算全删重插」，所以重算只允许在 `DRAFT` 状态做，且按 `(period_code)` 分区条件删（索引 idx_biz_cost_collect_period 已备）。

---

## 4. 业务规则与流程

### 4.1 状态机

#### 成本对象 `biz_cost_object`

| 状态 | 含义 | 可做的动作 |
|---|---|---|
| `OPEN` | 在产（可领料、可报工、参与分摊与结算） | 改基本信息、关批、作废 |
| `CLOSED` | 已关批（不再领料/报工，历史数据全保留） | 只读（可被后续期间的成本计算读取期初在制） |
| `CANCELLED` | 作废（建错了） | 只读 |

合法边（只有这 3 条，实现按 `state_machine` 的边表走）：

```
OPEN ──关批──▶ CLOSED
OPEN ──作废──▶ CANCELLED      （前置：无归集行、无入库、无结算明细）
CLOSED ──✗ 不可回 OPEN        （成本已结转，重开会破坏已结算期间的归集基线；要复工就新建批）
```

**为什么 `CLOSED` 不可逆**：`CLOSED` 之后该对象的历史期间成本已生成入库与凭证。允许重开就等于允许「改已经进过账的成本基数」，而 `biz_cost_period_item` 的守恒式是**按期快照**的，一改就前后不一致。复工建新批是审计上更干净的表达（新批带 `source_doc_no` 指向同一订单）。

关批的前置检查（handler 里做，5 条）：

1. 该对象名下没有 `DRAFT` 状态的工时台账行（`biz_cost_labor.status='DRAFT'`）；
2. 该对象没有「已过账但未归集」的领料出库单（`biz_stock_out.cost_object_id = ? AND status='POSTED'` 且期间不在已结算批次内）；
3. 所属当期成本批次不是 `DRAFT/SUBMITTED`（即当期成本已结算或该对象当期无活动）；
4. 没有 `biz_stock_out` 未过账（草稿/待审）的领料单挂着该对象；
5. `wip_qty = 0` 或用户显式确认「带在制关批」（`force_wip=true`，两段确认）。

#### 成本核算批次 `biz_cost_period`

| 状态 | 含义 | 可做的动作 |
|---|---|---|
| `DRAFT` | 草稿（可反复重算） | 计算、改数、提交、作废 |
| `SUBMITTED` | 已提交待审 | 撤回 → `DRAFT` |
| `APPROVED` | 审批通过（锁定数据，等结算） | 结算 → `SETTLED` |
| `SETTLED` | 已结算（入库 + 凭证已生成） | 关期后 `CLOSED` / 红冲 → `REVERSED` |
| `CLOSED` | 期间已结账（随会计期间结账推进） | 只读 |
| `CANCELLED` | 作废（未结算就放弃） | 只读 |
| `REVERSED` | 已红冲（凭证红字 + 库存回冲） | 只读；同期可重新建新批次 |

```
DRAFT ──提交──▶ SUBMITTED ──审批通过──▶ APPROVED ──结算──▶ SETTLED ──期间结账──▶ CLOSED
  │                  │                                        │
  │                  └──驳回/撤回──▶ DRAFT                    └──红冲──▶ REVERSED
  └──作废──▶ CANCELLED
```

关键口径：

- **`DRAFT → APPROVED` 必须过审批流**（不是直接改状态），`APPROVED` 的库层 CHECK 要求 `approval_instance_id IS NOT NULL`；
- **`APPROVED` 之后数据冻结**：归集、分摊、计算单全部只读（handler 拦 + 前端禁用），要改就撤回或作废重建；
- **`SETTLED` 的库层 CHECK 要求 `settled_at` 与 `finish_voucher_id` 同时非空** —— 不允许出现「状态说结算了但没有凭证」；
- **红冲后同期间可重建**：`uk_biz_cost_period` 排除 `REVERSED/CANCELLED`，真库已实测（证据断言⑦）：两张 `REVERSED` 存在时仍可插入一张 `SUBMITTED`，而同期间插两张活跃批次必被唯一键拒。

### 4.2 审批流怎么接（复用引擎，不硬编码）

| 项 | 值 |
|---|---|
| `biz_type` | `COST_SETTLE` |
| `flow_code` | `cost_settle_approve` |
| 节点 | 开始 → **财务经理**（`FINANCE_MANAGER`，`…f0005`）→ **财务总监**（`FINANCE_DIRECTOR`，`…f0006`）→ 结束 |
| 驳回 | 打回 `DRAFT` 并把意见追加到 `remark`（与 V0332 采购退货同款） |
| 自审批 | `separation::guard_self_approval` 拦「制单人自己审」 |
| 免审 | **不允许**。`biz_flow_def.allow_auto_approve = FALSE` |

**为什么两级**：结算一次动三张凭证 + 一批完工入库，且把 4101 清零。只一级（财务经理）没有制衡；三级（再加总经理）在月结时必然卡流程（月结只有 5 个工作日）。

**为什么结算不硬编码状态跃迁**：`biz_cost_period` 走 `state_machine::transition` 同一套边表，审批通过的回调改状态（与 `stockin_bridge` / `payable_bridge` 同一模式），所以「谁审批、几级、条件分支」全在流程画布上改，不改代码。

### 4.3 归集时点与来源（料 / 工 / 费三路）

成本对象的三类成本要素，来源与归集时点全部固定：

| 要素 | 数据来源 | 归集时点 | `source_type` | 幂等键 |
|---|---|---|---|---|
| **料** | `biz_stock_out_item`（`out_type='ISSUE'`、`status='POSTED'`、`cost_object_id IS NOT NULL`） | `calc`：扫描当期已过账领料单 | `STOCK_OUT` | `(STOCK_OUT, item_id, 1)` |
| **工** | `biz_cost_labor`（`status='CONFIRMED'`、当期） | `calc`：逐条归集（一人一天一批一行） | `LABOR_TICKET` | `(LABOR_TICKET, labor_id, 0)` |
| **费·折旧** | `biz_depreciation`（`status='POSTED'`、当期、`expense_account_code LIKE '4101.%'`） | `calc`：进**车间费用池**（`cost_object_id IS NULL`） | `DEPRECIATION` | `(DEPRECIATION, dep_id, 0)` |
| **费·报销** | `biz_expense_report_item`（主单 `status IN ('APPROVED','PAID')`、当期、`expense_type.account_code LIKE '4101.%'`） | `calc`：进车间费用池 | `EXPENSE` | `(EXPENSE, item_id, 0)` |
| **费·手工** | `biz_cost_overhead`（当期） | `calc`：`cost_object_id` 非空 → 直接归集；为空 → 进池 | `OVERHEAD_TICKET` | `(OVERHEAD_TICKET, overhead_id, 0)` |
| **费·分摊** | `biz_cost_alloc_result`（当期） | `calc` 第 ⑥ 步之后 | `ALLOC` | `(ALLOC, result_id, 0)` |

三条口径说明：

1. **金额取「账上真实金额」，不重算**。料行金额取 `biz_stock_out_item.amount`（出库过账时写入的**账上真实差额**），不是 `qty × avg_cost` —— 与 `voucher::create_for_stock_out` 的硬红线同一条理由（多桶 + 4 位舍入下两者不等，重算必然对不上 1405）。
2. **期间归属**：料按 `biz_stock_out.out_date` 的所属期间；工按 `biz_cost_labor.period_code`（录入时选，跨月补录要选对期间）；费按单据期间。**结算批次的 `period_code` 必须与凭证 `voucher_date` 落入的期间一致**（`post_voucher` 会按 `voucher_date` 反查期间，对不上就被它拒）。
3. **重算是「清当期 + 重灌」**，只允许 `DRAFT`：`DELETE FROM biz_cost_collect WHERE period_code=? AND deleted_at IS NULL`（已 `SETTLED` 的期间禁止重算，走红冲）。

### 4.4 分摊规则怎么配 + 计算逻辑

#### 4.4.1 表结构（已在 §3.1 ⑦⑧ 给全）

规则的语义用一句话说清：**「X 车间的 Y 制造费用科目，按 Z 标准，摊给该车间当期在产的这批成本对象」**。

- `source_dept_id`：被摊车间（`sys_dept.id`），`source_dept_name` 存快照；
- `account_code`：只允许 `4101.*`（库层 CHECK），一条规则只摊一个费用科目 —— 这样「水电按工时分摊、折旧按机器工时分摊」可以并存；
- `basis`：四值，见 4.4.2；
- 生效期 `effective_from/to`：规则是版本化的，改规则不溯及历史（按期间取生效版本）。

#### 4.4.2 四种分配标准的基数定义

| `basis` | 基数公式（**该车间当期、该对象**） | 数据来源 | 适用 |
|---|---|---|---|
| `LABOR_HOURS` | `Σ biz_cost_labor.hours` | 工时台账（只算 `CONFIRMED`） | 劳动密集型、人工是主要驱动 |
| `MACHINE_HOURS` | `Σ biz_cost_labor.machine_hours` | 工时台账的机器工时列 | 设备密集型（折旧/电费尤其适用） |
| `OUTPUT_QTY` | `biz_cost_period_item.completed_qty`（本期完工数量） | 成本计算单 | 品种法（三期）/ 产量均一的产品 |
| `MATERIAL_COST` | `Σ biz_cost_collect.amount WHERE element='MATERIAL'` | 归集表 | 材料成本驱动的车间费用 |

#### 4.4.3 计算逻辑（`calc` 第 ⑤⑥ 步）

```
① 池总额：pool(dept, account, period) = Σ collect.amount
     WHERE element='OVERHEAD' AND cost_object_id IS NULL
       AND source_type IN ('DEPRECIATION','EXPENSE','OVERHEAD_TICKET')
       AND dept_id = dept AND account_code = account AND period_code = period

② 基数：basis_i = 第 i 个成本对象在该标准下的基数（见 4.4.2）
   basis_total = Σ basis_i   （只统计「参与分摊」的对象，见下）

③ 分摊：raw_i = pool × basis_i / basis_total
   alloc_i = trunc(raw_i, 4)        -- 先截断到分（4 位）
   shortfall = pool - Σ alloc_i     -- 尾差（正数，因为全截断）
   alloc_k = alloc_k + shortfall    -- k = 余数 (raw_i - trunc(raw_i,4)) 最大者；
                                    --   并列时取 basis 大者，再并列取 cost_object_id 字典序最小
   rounding_adj_i = (i = k ? shortfall : 0)

④ 守恒断言（硬）：Σ alloc_i = pool，且只有一个 i 的 rounding_adj ≠ 0
```

**「参与分摊的对象」判据**（避免把费用摊给没开工的批）：

```
cost_object.status IN ('OPEN','CLOSED')
AND 该对象当期有「料或工」的归集行   -- 有投入才算在产
```

一条都不满足（车间当期没有在产对象）时：**报错**「车间 X 科目 4101.01 有 4000.00 待分摊，但当期无在产成本对象」——不做「挂在池里留到下期」的静默处理（那正是上期漏摊的来源）。

#### 4.4.4 尾差为什么必须这么收（真库演算）

| 场景 | 池 | 基数 | 结果 | 断言 |
|---|---|---|---|---|
| B1（整除） | 4000.00 | 60 / 20 / 20（合计 100） | 2400.00 / 800.00 / 800.00 | Σ = 4000.00 ✅ |
| B2（除不尽） | 1000.00 | 1 / 1 / 1（合计 3） | 333.3334 / 333.3333 / 333.3333 | Σ = 1000.00 ✅，尾差 0.0001 落在**唯一一行**（`rounding_adj`） |

若按「各自四舍五入」，B2 会得到 `333.33×3 = 999.99`，池里永远剩 0.01 摊不掉 → 4101 有余額无法清零 → C-COST-1 巡检天天红。最大余数法从算法上消灭这类残差。

#### 4.4.5 幂等与重算

- 分摊结果唯一键 `(period_code, rule_id, cost_object_id, account_code)`：同规则同对象同科目只能一行；
- 重算 = 先 `DELETE FROM biz_cost_alloc_result WHERE period_code=?`，再重灌（`DRAFT` 才允许）；
- 分摊结果 → 归集行（`ALLOC`）在同一事务内生成，幂等键 `(ALLOC, result_id, 0)`。

### 4.5 没有 MES 时，成本核算靠什么数据（本卡最大不确定点，正面回答）

MES 本体已暂停，意味着**没有工单、没有工序过站、没有报工记录**。成本核算的三路数据来源因此必须能「不依赖 MES 独立跑」。答案是一个三层结构：

#### 4.5.1 数据源矩阵

| 成本要素 | 单据/表 | 谁维护 | 录入方式 | 频率 | 缺失时的行为 |
|---|---|---|---|---|---|
| **料**（直接材料） | `biz_stock_out`（`out_type='ISSUE'` + `cost_object_id`） | 仓库/车间领料员 | 现有领料单，**新增一个必选字段「成本对象」** | 每笔领料 | 出库单没挂成本对象 → 不进成本核算（按存量口径直接进 6401），并在「未归集领料」清单里列出来 |
| **工**（直接人工） | `biz_cost_labor` | 车间统计员 / 班组长 | ① 网页逐条录 ② Excel 批量导入 | 每天或每周 | 报错「成本对象 X 当期无工时」—— **不用产量比例编造**（D-7） |
| **费·折旧** | `biz_depreciation`（已有单据） | 财务（已有模块） | 已有（月度计提） | 每月 | 若资产分类的 `expense_account_id` 配的是 5602.x（默认）→ **不进成本**；要进成本就把生产设备分类改配 `4101.01`（ARCH-6 §8.2-5 已支持，零代码） |
| **费·报销** | `biz_expense_report`（已有单据） | 全员 | 已有 | 随时 | 费用类型的 `account_code` 不是 `4101.*` → 不进成本（进 5602 等）。要进成本新建费用类型「车间水电」绑 `4101.02` |
| **费·手工（无单据）** | `biz_cost_overhead` | 财务 | 网页 / 导入 | 每月 | 不填就是没有这笔制造费用（不会自动估算） |
| **产量/进度** | `biz_cost_object.completed_qty` / `wip_qty` / `completion_pct` | 车间主任 | 结算时手填（或导入） | 每月 | 报错「成本对象 X 期末在制数量/完工进度未填」 |

#### 4.5.2 三条硬规则

1. **不猜**。任何一项缺失都只报错 + 指出缺哪一行，**绝不**自动用「按产量比例」「按上月数」推算。编造的人工会让毛利假准，比算不出来危险得多（D-7）。
2. **可导入**。工时与费用都有 Excel 模板，因为客户现场大概率没有打卡机也没有 MES，只有 Excel。
3. **可追**。每条归集行都留 `source_type + source_no`，能点回原单；工时台账留 `employee_name` 快照（AGENTS 约定 13 的历史口径）。

#### 4.5.3 工时台账导入模板（8 列）

| 列 | 必填 | 校验 |
|---|---|---|
| 成本对象号 | ✓ | 必须存在且 `status='OPEN'`；不存在 → 该行报错（不跳过、不自动建） |
| 日期 | ✓ | 必须落在所选期间内 |
| 员工工号 | | 空＝按班组计（`employee_id` 为空）；填了则必须能解析到员工 |
| 员工姓名 | ✓ | 快照 |
| 工作中心（部门） | ✓ | 必须是 `sys_dept` 的现有部门；不同车间不能混在一行 |
| 人工工时 | ✓ | > 0，2 位小数 |
| 机器工时 | | ≥ 0，可空（按机器工时分摊时有意义） |
| 工资率 | | 空 → 按 §4.5.4 的优先级自动取 |

导入结果必须是「**全有或全无**」：任何一行校验失败 → 整批不入库，返回逐行错误清单（行号 + 列名 + 原因）。理由：半批导入会让「人工成本」少一块而没人发现。

#### 4.5.4 工资率取自哪（一期）

优先级：`COST_OBJECT`（该批专用费率）> `DEPT`（车间综合费率）> `COMPANY`（公司级）。都取不到 → **报错**，不默认 0。

一期**不读** ARCH9-2 的薪资表：
- ARCH9-2 未上线；
- 语义也不同：薪资表是「某个人拿了多少」，成本需要「这个车间一小时多少钱」（含社保、津贴的综合费率）。

ARCH9-2 上线后的切换（D-13）：把 `biz_cost_period.labor_source` 切成 `PAYROLL`，人工金额改为「薪资单该部门直接人工合计」，并**按工时的比重拆到各成本对象**（工时仍然是拆分依据，所以工时台账不能省）。切换后成本核算**不再生成人工计提凭证**（避免与薪资单双计提）。

#### 4.5.5 MES 上线后的对接（不改表结构）

| 一期字段 | MES 来源（ARCH-4） | 说明 |
|---|---|---|
| `biz_cost_labor.hours` | `biz_work_log.labor_minutes / 60` | ARCH-4 §3.9 明确「人工工时用 `labor_minutes`」 |
| `biz_cost_labor.machine_hours` | `biz_work_log.equip_minutes / 60` | 同上（OEE 与人工成本各用一列） |
| `biz_cost_labor.cost_object_id` | `biz_mes_wo.cost_object_id`（MES 上线时补的弱引用列） | 工单 ↔ 成本对象一对一 |
| `biz_cost_labor.source_type` | `'MES'` | 值域已预留 |
| 完工数量 | `biz_mes_wo.good_qty` | 不再手填 |

### 4.6 在制品与完工（约当产量法）

#### 4.6.1 公式（加权平均法）

```
设：BQ = 期初在制数量，BC = 期初在制成本
    FQ = 本期完工数量，WQ = 期末在制数量，P = 期末在制完工进度%
    M  = 本期投入材料，L = 本期投入人工，O = 本期投入制造费用

① 约当产量
   材料（一次投入）：EQ_mat = FQ + WQ            （在制按 100% 计材料）
   加工（人工/费用）：EQ_cnv = FQ + WQ × P / 100

② 单位成本（6 位）
   u_mat = round((BC_mat + M) / EQ_mat, 6)
   u_lab = round((BC_cnv + L) / EQ_cnv, 6)
   u_oh  = round((BC_cnv + O) / EQ_cnv, 6)

③ 完工转出（4 位，逐要素算出后相加）
   finished = round(FQ × u_mat, 4) + round(FQ × u_lab, 4) + round(FQ × u_oh, 4)

④ 期末在制 = 投入合计 − 完工转出（**差额倒挤**，保证库层守恒式成立）
   wip_end = (BC + M + L + O) − finished

⑤ 完工单位成本 = round(finished / FQ, 6)   → 写入完工入库明细 cost_price
```

**为什么 ④ 要倒挤而不是再算一遍**：`round(FQ×u,4) + round(WQ×P×u,4)` 与 `投入` 之间会有 0.0001–0.0004 的舍入差（见算例 A 的 444.4448 vs 444.4444）。两处各算一遍就会出现「完工 + 在制 ≠ 投入」，而 `ck_biz_cost_item_cons` 是**库层约束**，会直接把结算打回。差额倒挤进在制是标准做法（在制本来就是「剩下的」）。

#### 4.6.2 真库算例（`02_cost_calc_drill.sql` 算例 A，数字为库算结果）

输入：投料 A 1200 件 @12.50（15000）+ B 500 件 @8.00（4000）；工时 80 h @50.00（4000）；分摊制造费用 4000；完工 800 件；期末在制 200 件、进度 50%；材料一次投入。

| 项 | 值 |
|---|---|
| `equiv_material` | 1000.0000（800 + 200） |
| `equiv_conversion` | 900.0000（800 + 200×50%） |
| `unit_material` | 19.000000 |
| `unit_labor` | 4.444444 |
| `unit_overhead` | 4.444444 |
| `finished_cost` | **22311.1104** |
| `wip_end_cost` | **4688.8896** |
| `unit_finished_cost` | **27.888888**（＝22311.1104 / 800） |
| 守恒 | 22311.1104 + 4688.8896 = 27000.0000 = 19000 + 4000 + 4000 ✅ |

#### 4.6.3 期初在制怎么来（跨月）

- 上期 `biz_cost_period_item` 的 `wip_end_qty` / `wip_end_cost` → 本期 `wip_beg_qty` / `wip_beg_cost`（按 `cost_object_id` 取**上一个已结算批次**的同一对象）；
- 若上期批次被红冲 → 本期不得自动取数（报错「上期批次已红冲，请先重算上期」）；
- 首期（无上期记录）→ 期初在制为 0；
- 单位成本公式里 `BC` 按**要素**拆（材料的期初在制成本只进材料单位成本），所以 `biz_cost_period_item` 除了总额还要能拆要素 —— 一期实现里「期初在制按上期单位成本 × 期初数量还原到要素」（不额外存 3 列，避免列膨胀；若审计要求明细，二期加 `wip_beg_mat/labor/oh` 三列，属向后兼容的加列）。

### 4.7 成本结转凭证（借贷科目 + 与库存的勾稽）

一期一共 **4 类分录**，其中 1 类改现有路径、3 类由结算批次生成，全部经 `voucher::post_voucher`（唯一收口，自动过「≥2 行 / 借贷平衡 / 只允许末级科目 / 期间未结账」四道闸）。

#### 4.7.1 领料（改造现有出库过账路径）

| 场景 | 借 | 贷 |
|---|---|---|
| 出库单 `cost_object_id` 为**空**（存量行为） | `6401 主营业务成本`（按物料拆行） | `1405 库存商品` |
| 出库单 `cost_object_id` **非空**（生产领料） | **`4001.01 生产成本-直接材料`**（`aux_material_id` + `aux_warehouse_id` 保留） | `1405 库存商品` |

改动点唯一：`stockout_handler.rs:747` 的 `debit_account_of(&header.out_type)` 前加一个分支 —— 挂了成本对象就返回 `("4001.01", "生产成本-直接材料")`。其余（金额取账上真实差额、按物料拆分录、尾差吸收）**一行不改**。

**实现口径（B3-8 复核，无代码改动）**

| 项 | 落地事实 |
|---|---|
| 分支位置 | 不在 `debit_account_of` 里加参（它是纯 `out_type` 映射，`biz_type_of` 等 8 处共用），而是新增包装 `stockout_base::debit_account_of_doc(out_type, cost_object_id)`；`debit_account_of` 语义与输出零变化 |
| 调用点 | 唯一：`stockout_handler::post_out_tx` ⑤ 成本结转凭证（B1-10 已接入）。`git show 434aad42 -- stockout_handler.rs` 里该函数只有这一行改动（金额 / 拆行 / 尾差未动） |
| 非 ISSUE 兜底 | `out_type != ISSUE` 时**永不**返回 4001.01（挂对象也走 1901 / 6401）——写入侧 `ensure_cost_object_usable` 已拒非 ISSUE 挂对象，记账侧再兜一层 |
| 取证 | 真库两单对照：`OUT-20261111-000036`（挂对象）借 `4001.01 生产成本-直接材料` / 贷 `1405`；`OUT-20261111-000037`（不挂）借 `6401` / 贷 `1405`。见 `docs/evidence/2026-11-11-cost-b3-8-stockout-debit-split/` |

#### 4.7.2 人工计提（结算时生成，第 1 张凭证）

```
借 4001.02 生产成本-直接人工         Σ 当期 CONFIRMED 工时金额（按成本对象汇总后按车间合并）
                                      aux_dept_id = 车间
贷 2211.01 应付职工薪酬-工资        同额
```

`labor_source='PAYROLL'` 时**这张凭证不生成**（工资凭证由薪资单出，D-13）。

#### 4.7.3 制造费用分摊（结算时生成，第 2 张凭证）

```
借 4001.03 生产成本-制造费用         Σ 当期全部制造费用归集（池行 + 直接行，
                                     按 (车间, 科目) 分行，aux_dept_id = 车间）
贷 4101.01 制造费用-折旧费           该 (车间, 科目) 的归集金额
   4101.02 制造费用-水电费           …
   4101.09 制造费用-其他             …
```

**借方口径是「池行 + 直接行」，不是只算池**。`biz_cost_collect` 里 `element='OVERHEAD'` 的行有两类：

- **池行**：`cost_object_id IS NULL`（折旧 / 报销 / 未挂对象的手工台账，见 §4.4.3 ①）；
- **直接行**：`cost_object_id` 非空（§4.3「费·手工」：`biz_cost_overhead` 挂了成本对象 ⇒ 直接归集，**不进池、不参与分摊**）。

两类行的 4101 借方都真实存在，也都只能靠这张凭证清零 —— 只清池会让 ① 4101 期末余额 ≠ 0（C-COST-1 红）、② `Σ 4001.03 借方 ≠ Σ biz_cost_period_item.oh_cur`（C-COST-3 红）。这张凭证的目的已经写明是「把 4101 清零」，所以口径服从目的，不取「池」这一个子集。

- **`source_type='ALLOC'` 的分摊结果行不计入**：它是池行摊出去之后的另一半（§4.4.3 ③），池行与 ALLOC 行是**同一笔钱的两个投影**，一起加会翻倍。所以借方合计 ≡ 池行 + 直接行 ≡ `Σ item.oh_cur`（结算时**运行期硬断言**，不平即 409 + `66076`）。
- **行级科目取归集来源单据的科目**：手工台账取 `debit_account_code`、折旧取 `expense_account_code`、报销取费用明细的 `account_code`（`COALESCE` 口径）；按 (车间, 科目) 分行后逐行 `4101.xx` 贷方清零，与原借方逐组相等。
- 若某个 `(车间, 科目)` 有归集却没规则（池没摊完）→ **结算报错**（不允许留在 4101 里，否则存货含费用而产品不含）；归集行的来源单据已删（取不到科目）同样报错，要求先重算该期间。

**手工费用台账的凭证（第 4 张，即 §4.8 ④ 的 ④-b；条件触发 —— 不是「可选」）**：`biz_cost_overhead` 的行在结算时先生成「借 `4101.xx` / 贷 对方科目（`credit_account_code`）」，按 `(车间, 借科目, 贷科目)` 汇总；随后被上表这张分摊凭证清零。两张凭证同一事务，4101 在事务内「进→出」，期末余额为 0。

- **为什么不能省**：手工台账是**唯一**没有上游凭证的费用来源 —— 折旧（`biz_depreciation`）、报销（`biz_expense_report_item`）在上游过账时已经借过 4101，唯独手工台账没有。不先把它记进 4101，分摊凭证的贷方 4101 就没有对应借方，4101 会留下**负数余额**（C-COST-1 红）。
- **规则是纯条件式的**：当期 `biz_cost_overhead` 有行 ⇒ 必须发（有行不发 = 账不平）；没有行 ⇒ 完全不出现（`Option<Uuid>` = `None`，不造空凭证）。§4.8 ④ 那句「第 4 张，**按需**」的「按需」指的就是这个触发条件，**不是「可以跳过」**。
- **顺序固定**：台账凭证（④-b）必须排在分摊凭证（④-c）**之前**，否则分摊凭证先贷 4101 会把余额打成负数，同一事务内的 4101 明细账无法解释。
- **与巡检口径的接口（给 B3-10）**：§4.11 的 C-COST-1 原文写「`Σ biz_cost_collect(OVERHEAD, 池行)` = `4101.*` 借方发生额」，按本节口径应读作「**池行 + 直接行**」—— 台账凭证给直接行也借了 4101。只按池行取数，会在「手工台账挂了成本对象」的期间出现固定假差额（差额恒等于直接行金额）。

#### 4.7.4 完工入库结转（结算时生成，第 3 张凭证）

```
借 1405 库存商品                     按成本对象的完工成本分行（aux_material_id + aux_warehouse_id）
贷 4001.01 生产成本-直接材料          Σ 各对象材料部分完工转出
   4001.02 生产成本-直接人工          Σ 人工部分
   4001.03 生产成本-制造费用          Σ 费用部分
```

贷方按**要素**拆三行（不是一行 4001）——因为 4001 已被翻成非末级，记账必须到末级（`post_voucher` 的 `non_leaf` 闸）。

**这一张凭证同时就是完工入库单的凭证**：结算生成的 `biz_stock_in`（`in_type='PRODUCTION'`）的 `voucher_id` 指向它（多张入库单可以共指一张结算凭证，`biz_stock_in.voucher_id` 无唯一约束）。

#### 4.7.5 用算例 A 的数字验证四类凭证（`02_cost_calc_drill.sql` 输入）

> **对账前提（先读这条，否则一定以为算错）**：下表**含第 0 张领料凭证**（§4.7.1，借 `4001.01` **19000.00**，由 B3-8 落地）。结算凭证本身不生产这 19000 —— 没有它，4001.01 的余额是 **−15200.00**，而不是下面表里的 3800.00。
> 同理，4101.01 / 4101.02 的贷方 3000 / 1000 必须先有对应的**借方**才可能清零：折旧、报销在上游过账时已借 4101；若该费用来自**手工台账**，则由 §4.7.3 第二段那张台账凭证（④-b）先借进来。算例 A 的证据夹具用的是手工台账（借 4101.01 3000 + 4101.02 1000 / 贷 2241.01 4000），所以实测是 **5 张**（领料 + 人工 + 台账 + 结转 + 完工）；下表按**分录类别**列 4 类，台账凭证不单列。

| # | 凭证 | 借 | 贷 | 校验 |
|---|---|---|---|---|
| 0 | 领料（随出库单） | 4001.01 **19000.00** | 1405 19000.00 | 库存减少 19000 |
| 1 | 人工计提 | 4001.02 **4000.00** | 2211.01 4000.00 | ＝工时 80×50 |
| 2 | 制造费用分摊 | 4001.03 **4000.00** | 4101.01 3000.00 + 4101.02 1000.00 | 4101 清零 |
| 3 | 完工结转 | 1405 **22311.1104** | 4001.01 15200.00 + 4001.02 3555.5552 + 4001.03 3555.5552 | 三项和 = 22311.1104 ✅ |

费用来源不同，张数不同、余额不变：折旧 / 报销来源 ⇒ 4 张（上游凭证已借 4101）；手工台账来源 ⇒ 5 张（多 ④-b 那张）。两种情形下 4101 期末余额都是 **0**（C-COST-1 判据）。

结算后 4001 各末级余额（借 / 贷列为**发生额合计**，含第 0 张领料凭证的 19000 借方）：

| 科目 | 借 | 贷 | 余额（＝期末在制） |
|---|---|---|---|
| 4001.01 | 19000.00 | 15200.00 | **3800.00**（＝200 件 × 19.00） |
| 4001.02 | 4000.00 | 3555.5552 | **444.4448** |
| 4001.03 | 4000.00 | 3555.5552 | **444.4448** |
| 合计 | | | **4688.8896** ＝ `biz_cost_period_item.wip_end_cost` ✅ |

（4001.02 与 4001.03 各多出 0.0004，是 4 位舍入的必然结果，已由「差额倒挤进在制」吸收；三行合计与计算单**分毫不差**，这是巡检 C-COST-3 的判据。）

#### 4.7.6 完工入库与库存的勾稽

| 勾稽 | 内容 | 谁校验 |
|---|---|---|
| 库存数量 | 入库 `qty = completed_qty`；`biz_inventory.qty` 增加同额 | `inventory_writer::add_cost`（:153）+ `add_location_qty`（:830） |
| 库存金额 | 入库 `amount = finished_cost`；`biz_inventory.amount` 增加同额，`avg_cost` 重算 | 同上（移动加权加法） |
| 入库单价 | `stock_in_item.cost_price = round(finished_cost / qty, 6)`＝`unit_finished_cost` | 结算时写 |
| 流水 | `biz_inventory_txn.biz_type='PRODUCTION_IN'`（词表已有）、`doc_type='STOCK_IN'` | `inventory_writer::insert_txn`（:1244） |
| 账账 | 完工结转凭证借方 1405 合计 = 入库金额合计 | 巡检 C-COST-2 |

**注意**：完工入库**不走** `stockin_handler` 的过账路径（那条路径会生成采购凭证 + 应付单 `payable::create_for_stock_in`，`stockin_handler_post.rs:900-955`）。结算直接写 `biz_stock_in`（`status='POSTED'`、`posted_at/by` 有值、`in_type='PRODUCTION'`、无 `payable_id`），并调用 `inventory_writer` 写库存三本账 —— 库存写入仍然只有一个入口，不绕过闸。

### 4.8 结算事务与失败补偿（AGENTS 约定 11 落地）

结算是一次高危动作：生成 N 张入库单 + 三张凭证 + 一堆库存变动。事务边界设计成**一个事务**：

```
BEGIN
 ① SELECT * FROM biz_cost_period WHERE id=? FOR UPDATE        -- 锁批次行，防并发双结算
 ② 校验：status='APPROVED'；period 未结账（post_voucher 也会拦，提前给好人话）；无红冲中
 ③ 逐成本对象：
     a. INSERT biz_stock_in (in_type='PRODUCTION', status='POSTED', cost_object_id, cost_period_id)
     b. INSERT biz_stock_in_item (qty=completed_qty, cost_price=unit_finished_cost, amount=finished_cost, lot_no)
     c. inventory_writer::add_cost / add_location_qty / insert_txn  -- 库存三本账
     d. UPDATE biz_cost_period_item SET stock_in_id=?, status='SETTLED'
     e. 若 biz_cost_object.lot_no 有值 → upsert biz_lot(source_type='PRODUCTION')
 ④ 三张凭证（post_voucher×3）：人工计提（条件）/ 制造费用分摊 / 完工结转
     · 手工费用台账的「借 4101 / 贷 对方科目」凭证也在这一步（第 4 张，按需）
 ⑤ UPDATE biz_cost_period SET status='SETTLED', settled_at, settled_by,
       labor_voucher_id, overhead_voucher_id, finish_voucher_id,
       total_*, object_count
 ⑥ 校验 C-COST-1/2/3/4（在本事务内用自己的写读做断言；不平就整个回滚）
COMMIT
⑦ 提交后：只发通知（站内信 + 待办完成），**没有任何会失败的账务动作**
```

**为什么这样就满足约定 11**：约定 11 针对的是「`tx.commit()` 之后还要做可能失败的后续动作」。本设计把所有账务动作（入库 / 库存 / 凭证 / 状态）**全部放进同一个事务**，提交后只剩通知（失败只打 `warn`，不影响单据状态）。所以不存在「提交后失败留死单」的场景。

**反向场景（红冲）**也必须闭环：

```
BEGIN
 ① 锁批次行 FOR UPDATE；要求 status ∈ ('SETTLED','CLOSED')
 ② 对三张（或四张）凭证逐个 voucher::reverse_posted（红字凭证，独立 PZR 号段，原凭证保持 POSTED）
 ③ 逐对象回冲库存：inventory_writer::subtract_cost / subtract_location_qty / insert_txn(PRODUCE_CANCEL)
 ④ UPDATE biz_stock_in SET status='CANCELLED'（生产入库单作废，cancelled_at/by/reason）
 ⑤ UPDATE biz_cost_period SET status='REVERSED', reversed_at/by, reverse_reason
COMMIT
```

- 红冲后该对象回到「未结算」状态（`biz_cost_period_item.status` 回到 `CALC`，`stock_in_id` 清空）→ 可以改数重算，重建新批次（唯一键已排除 `REVERSED`，见 §4.1）；
- **库存回冲失败**（如货已发出、库存不足）→ 整个事务回滚，批次仍是 `SETTLED`，报错提示「该批货已出库，不能红冲，请走销售退货/盘亏流程」。这是明确的业务边界，不是死单。

**实现口径（B3-7 回填，2026-11-11）**

| 点 | 落地口径 | 理由 |
|---|---|---|
| 红冲可接受的起始状态 | 只接受 `SETTLED`（**不含 `CLOSED`**） | 状态机边表 `COST_PERIOD_EDGES` 只有 `SETTLED→REVERSED`；B3 内无代码置 `CLOSED`（关期卡未实施）。关期批次红冲要等关期/反关期卡补 `CLOSED→REVERSED` 边 |
| 红字凭证日期 | **被红冲期间的末日**（不是「今天」） | 红字必须与蓝字同期间，「本期净发生」才能立刻归零、同期间重结时 6 条断言才成立；跨期间会把两个期间都做错 |
| 期间已结账 | 开事务前先给 409 人话「请先反结账」 | `post_voucher` 有真闸，但冒泡上来是 500，用户看不出该做什么 |
| 单头结算痕迹 | `settled_at` 与 `labor/overhead/finish_voucher_id` **保留** | `ck_biz_cost_period_settle` 对 `REVERSED` 同样生效；且审计要能顺着指针找到「红了哪几张蓝字」 |
| 红冲回冲金额 | 取 `biz_inventory_txn` 里 `PRODUCTION_IN` **实际写入**的金额 | 期间内又进过货时 `avg_cost` 已变，按现价冲减会污染别的批次（与收货作废同口径） |
| 错误码 | 库存退不回 / 流水缺失 / 无蓝字可红 → 409 + `66078` | 三者都是「修数据后能从界面继续」的边界，不能是 500 |

### 4.9 差异分析（二期）

#### 4.9.1 六类差异公式

| 差异 | 公式 | 说明 |
|---|---|---|
| 材料价差 `MAT_PRICE` | `(实际单价 − 标准单价) × 实际用量` | 采购/领料价格波动 |
| 材料量差 `MAT_QTY` | `(实际用量 − 标准用量) × 标准单价` | 超用/浪费（标准用量 = 标准单耗 × 完工数） |
| 工资率差异 `LABOR_RATE` | `(实际工资率 − 标准工资率) × 实际工时` | 用工贵了 |
| 人工效率差异 `LABOR_EFF` | `(实际工时 − 标准工时) × 标准工资率` | 干慢了（标准工时 = 标准单耗工时 × 完工数） |
| 制造费用耗费差异 `OH_SPEND` | `实际制造费用 − 预算制造费用` | 花多了 |
| 制造费用产量差异 `OH_VOLUME` | `预算制造费用 − 标准分配额` | 产量没达标（标准分配额 = 标准费率 × 实际产量） |

**制造费用用「两差异法」**（耗费 + 产量），不做三差异法（三差异要把耗费再拆「耗费+效率」，需要「实际工时×标准费率」这一层，而一期工时数据的可信度不足以支撑二次拆分）。这是明确的取舍，不是遗漏。

#### 4.9.2 两因素恒等式（必须成立，真库已验）

```
价差 + 量差 = 实际材料成本 − 标准材料成本
率差 + 效差 = 实际人工成本 − 标准人工成本
```

真库演算（`02_cost_calc_drill.sql` 算例 C）：

| 材料 A | 计算 | 结果 |
|---|---|---|
| 标准 | 800 件 × 1.1 件/件 × 12.00 | 10560.00 |
| 实际 | 1200 件 × 12.50 | 15000.00 |
| 价差 | (12.50 − 12.00) × 1200 | **+600.00**（不利） |
| 量差 | (1200 − 880) × 12.00 | **+3840.00**（不利） |
| 合计 | 600 + 3840 | **4440.00** ＝ 15000 − 10560 ✅ |

| 人工 | 计算 | 结果 |
|---|---|---|
| 标准 | 800 件 × 0.10 h × 50.00 | 4000.00 |
| 实际 | 90 h × 48.00 | 4320.00 |
| 工资率差 | (48 − 50) × 90 | **−180.00**（有利） |
| 效率差 | (90 − 80) × 50 | **+500.00**（不利） |
| 合计 | −180 + 500 | **320.00** ＝ 4320 − 4000 ✅ |

两个恒等式在结算/分析时必须做成断言（`Σ两因素 = 实际 − 标准`），这是「公式写错了」唯一能自动抓到的办法（常见错法：量差用实际单价、率差用实际工时乘错方向——两种错法都会让恒等式破掉）。

#### 4.9.3 差异要不要入账（卡面 ④ 的正面回答）

**一期不做，二期也不做（D-15）**。理由三条：

1. **入账＝改存货计价口径**。「标准成本 + 差异入 1404 + 期末按差异率分摊」是**计划成本法**：材料按标准价进 1403/1405，差额挂 1404，月末按「差异率 = (期初差异 + 本期差异) / (期初计划成本 + 本期计划成本)」分摊到发出与结存。这会把 `biz_inventory.amount` 从「实际成本」变成「标准成本 + 分摊差异」，**推翻移动加权**——ARCH-4 §D9 与 §8.2-7 明确判为「另一个决策，本卡不碰」。
2. **风险不对等**。差异报表不会错账；差异入账一旦分摊率算错，存货与成本同时错，且月末才能发现。一期的价值是「让人看见差异」，不是「把差异做进账」。
3. **有替代方案**。真要考核，把标准总成本与差异额做成报表（`biz_cost_variance` 落表），管理会计口径够用；要进财务账，走「手工调整凭证」（已有能力）。

**所以要入账时会怎样**（给未来 ADR 的接口）：届时新增一张 `biz_cost_variance_alloc`（按期按物料记差异率与分攤额），并把完工入库 `cost_price` 改成标准价、差异记 `1404 材料成本差异`（科目已存在，V0183:76；资产负债表存货项已含 1404，V0183:192）。本卡只在 §8.4「待确认」里留了这一条。

#### 4.9.4 差异报表口径

- 按「期间 × 成本对象 × 差异类型」钻取，金额正数=不利、负数=有利（库层 `ck_biz_cost_var_sign` 锁死符号与 `direction` 的一致性）；
- 支持按物料聚合（找出「哪个料最贵了」）、按对象聚合（「哪批最亏」）；
- 与标准成本卡版本绑定（`standard_id`），改标准不重算历史。

### 4.10 特殊场景

| 场景 | 处理 |
|---|---|
| **跨期领料** | 领料单 `out_date` 落在 9 月、9 月成本批次已 `SETTLED` → 该笔归集进 10 月（按 `period_code` 归期，不按 `out_date` 回溯已结算期间）。10 月批次归集时按「出库单已过账且未归集」扫描，天然覆盖跨期补录 |
| **期间已结账** | `calc/settle` 一律拒（`post_voucher` 也会拦）；要改就反结账（已有能力，财务总监权限） |
| **红冲后重算** | §4.8 反向路径；唯一键允许同期间重建，红冲记录永久保留 |
| **并发** | `calc`：`SELECT … FOR UPDATE` 锁 `biz_cost_period` 行；`settle` 同；库存变动由 `inventory_writer::lock_cost`（:114）行锁串行；工时台账确认用 `UPDATE … WHERE status='DRAFT'` 乐观判影响行数 |
| **同一领料单改了成本对象** | 只允许在出库单 `DRAFT/SUBMITTED` 时改；已过账 → 作废重开。归集侧靠幂等键，改对象后重算自动纠正（旧行被清） |
| **完工数量超过计划** | 允许（超产常见），`completed_qty` 不设上限；但 `wip_qty + completed_qty > plan_qty` 时前端提示「超产 X 件」 |
| **多组织（ARCH9-1）** | 所有表带 `org_id`；`biz_cost_period` 唯一键已含 `org_id`，多组织上线不需改键。跨组织不结转、不分摊 |
| **成本对象改产品** | 禁止。`cost_object.product_id` 一经有归集行即只读（改品会让历史成本挂到别的产品上） |
| **结算时发现某对象 0 完工** | 允许（全在制）：`finished_cost = 0`、`wip_end_cost = 投入`，不生成入库单，但计算单照常落库（这是「在制」的正常形态） |
| **某对象有完工但没投料** | 报错「成本对象 X 有完工数量但无任何材料归集」——零成本完工是数据错误，不是业务 |

### 4.11 巡检不变量（6 条，进 `scripts/verify/erp_invariants.py`）

| 编号 | 断言 | 级别 | 口径 |
|---|---|---|---|
| **C-COST-1** | 当期 `Σ biz_cost_collect(OVERHEAD, 池行)` = 当期 `4101.*` 借方发生额（按 (车间, 科目) 分组） | BLOCKER | 池明细与总账逐组对得上；差一分就说明有人绕过成本模块记了 4101 |
| **C-COST-2** | `Σ biz_stock_in(in_type='PRODUCTION', cost_period_id=P).total_amount` = 该批次完工结转凭证借方 1405 合计 | BLOCKER | 入库金额与凭证必须同源 |
| **C-COST-3** | `Σ biz_cost_period_item.wip_end_cost`（最新已结算批次） = `4001.01+4001.02+4001.03` 期末余额 | BLOCKER | 在制与生产成本科目余额（这是「4001 有余额」这件事唯一的正确性判据） |
| **C-COST-4** | 每期 `Σ biz_cost_alloc_result.alloc_amount` = `Σ biz_cost_collect(池行)`；且每个 `(车间,科目)` 池在结算后余额为 0 | BLOCKER | 分摊守恒 + 无漏摊 |
| **C-COST-5** | `Σ biz_cost_collect(MATERIAL)` = `Σ biz_stock_out_item(cost_object_id IS NOT NULL, POSTED, 当期).amount` | WARN | 料归集完整（漏归集只影响成本准确性，不影响账） |
| **C-COST-6** | 每张 `biz_cost_period_item`：`finished + wip_end = wip_beg + mat + labor + oh`（库层已锁，巡检防历史数据被手工改库） | BLOCKER | 库层 CHECK 兜新数据，巡检兜老数据 |

**结账预检新增第 5 项 blocker**（改 `period.rs`，`BLOCKER_*` 常量在 `period.rs:151-160`）：

```
BLOCKER_COST_UNSETTLED = "COST_UNSETTLED"
判据：该期间存在 biz_cost_period 且 status NOT IN ('SETTLED','CLOSED','CANCELLED','REVERSED')
      → 409「成本核算未结算（批次 CS-202610-0001 状态 DRAFT），请先结算再结账」
      → items 给跳转（单据 id / 批次号 / 制单人 / 已等多久）
      → 该期间**没有**成本批次时**不拦**（未启用成本核算的客户不受影响）
```

#### 4.11.1 实现口径（B3-9 落地，2026-11-11）

| 项 | 实现 |
|---|---|
| 常量 / 判定 | `period.rs::BLOCKER_COST_UNSETTLED`；`period_items::cost_status_settled(status)` 与 SQL 字面量 `cost_settled_status_list()` **同源**（纯单测逐值比对，防「谓词改了 SQL 没改」） |
| 取数 | `period_items::cost_period_items(ex, period_code)` → `(COUNT(*), 明细 ≤50)`；三条 SQL（计数 / 单头 / 对象号）共用同一个 WHERE 片段 `cost_unsettled_where()`；对象号按计算单 join `biz_cost_object` 批量取，无 N+1 |
| 文案 | 「成本核算未结算（`期间码` 有 N 个批次：批次 `CS-…` 状态「中文态」，M 个成本对象（`CB-1`、`CB-2` 等））：请先结算或作废后再结账」；批次点 3 个、对象点 3 个，超出写「…」/「等」，总数照实报 |
| items 口径 | `doc_type=COST_PERIOD`（中文「成本批次」）、`doc_no=批次号`、`doc_id=批次 id`、`status_label` 按状态翻译、`action_label` 按状态给「去提交 / 去审批 / 去结算 / 去看批次」、`route=/business/erp/cost-period?batch_no=…`、`waiting_since=建单时间`、**`amount=None`**（`total_*` 只在结算事务回写，未结算恒 0，显示「0 元」会误导） |
| 处理人 | `owner_role/owner_name/owner_user_id` **留空 = 系统不指定**：批次下一步跨 `cost-period:submit` / `cost-period:settle` 两个权限码，且催办台账 `biz_reminder_log.doc_type` 的 CHECK（V0257，8 值）还没有 `COST_PERIOD`，填权限码会让催办通知发出、台账行被库层 CHECK 拒（审计少一行）。放宽那条 CHECK 后在这里补 `PERM_COST_PERIOD_*` 即可，明细结构不用动 |
| 前端 | `PeriodCloseBlocker.type` 联合类型加 `COST_UNSETTLED`；`blocker-logic.ts` 的 `BLOCKING_DOC_TYPE_LABELS` 加 `COST_PERIOD: '成本批次'`（与后端 `doc_type_label` 同口径） |
| 迁移 / 错误码 | 无（纯读校验，不改 schema、不新增错误码） |

#### 4.11.2 实现口径（B3-10 巡检落地，2026-11-11）

`scripts/verify/erp_invariants.py` 新增「成本」组（`group_cost`，`--group 成本`），六条不变量逐条实现；
配套自测 `scripts/verify/cost/cost_invariants_selftest.py`（只在演练库跑：装夹具 → 全绿 → 逐条造错报红 → 清场）。

| 项 | 实现 |
|---|---|
| 判据范围 | **只判 `status IN ('SETTLED','CLOSED')` 的批次所属期间**。原因：领料/折旧/报销在上游过账时已写分录，而人工计提、手工台账、费用结转、完工结转四张凭证是结算事务内才生成的 —— 正在做账的期间天然有「账上借了 4001.01、台账凭证还没发」的中间态，拿它对账必假红。已红冲（REVERSED）尚未重结的期间同样不判（蓝字被红字抵平，池由下次结算时对账）。库里没有任何已结算批次时整组 `INFO` 跳过（不是通过、也不报警） |
| C-COST-1 | 池侧 = `biz_cost_collect` 里 `element='OVERHEAD'` 且 `source_type IN (OVERHEAD_TICKET, DEPRECIATION, EXPENSE)` 的**池行 + 直接行**（§4.7.3「与巡检口径的接口」：只取池行会在「手工台账挂了成本对象」的期间恒差一笔）；`source_type='ALLOC'` 不计（同一笔钱的两个投影）。科目按来源单据反查（collect 表没有 `account_code` 列）。账面侧与 `cost_reconcile_model::LEDGER_DEBIT_SQL` 逐字同源（POSTED / 未软删 / 只算借方 / 排除红字与被红冲 / 只认 `4101.*`）。逐 (期间, 车间, 科目) 组对平，差 0.0001 即红 |
| C-COST-2 | 逐已结算批次：`Σ biz_stock_in(in_type='PRODUCTION', cost_period_id=批次, status<>'CANCELLED').total_amount` = 完工凭证（`finish_voucher_id`）分录里 `1405` 借方 = `Σ item.finished_cost`；并判该凭证自身平衡（分录 Σ借 = Σ贷、头 = 分录取和、状态 POSTED、非红字）。卡面要的「凭证平衡」即此项 |
| C-COST-2·账 | 卡面「三本账一致」：完工入库单 ↔ 它自己那条流水（`biz_type='PRODUCTION_IN'`、`doc_type='STOCK_IN'`）的数量/金额同源；且这些入库动过的 `物料×仓库` 上 `biz_inventory` = `biz_inventory_location` = `biz_inventory_txn` 合计（数量口径，与 S8 同款） |
| C-COST-3 | **全库余额版**（B3-6 明确留给巡检的那一条）：`4001.01/.02/.03` 净余额 = `Σ(每成本对象最新已结算批次的 wip_end_cost)` + **在途**。在途 = ① 已归集未结转（`element='MATERIAL'` 且该 (对象, 期间) 没有已结算批次）② 已过账未归集（挂了对象的已过账领料行还没有归集行）。两段互斥不重复计；在途项是「本月已领料、批次还没结算」这种正常中间态不假红的关键。余额形态才能抓到「跨期结转串期 / 有人手工记 4001」 |
| C-COST-4 | ① 每个 (期间, 车间, 科目) 的 `4101.*` 借贷相等（池清零）；② `Σ biz_cost_alloc_result.alloc_amount = Σ collect(池行)`（分摊守恒 / 无漏摊 —— 分摊结果不参与任何凭证，是「少写一行」唯一能抓到的判据） |
| C-COST-5 | WARN：料归集完整。逐行双向对账（漏归集 / 孤儿归集 / 金额不符），范围只看已结算期间（未结算期间 calc 还没跑，没有归集行是正常中间态）。逐行等价于 `Σ collect(MATERIAL) = Σ 已过账挂对象领料行`，但能直接点到出库单行 |
| C-COST-6 | 每张 `biz_cost_period_item`：`finished + wip_end = wip_beg + mat + labor + oh`（库层 `ck_biz_cost_item_cons` 兜新数据，巡检兜老数据 / 手工改库） |
| 静态同步闸 | 自测脚本第 [0] 步：巡检里的 `4001.01/.02/.03` / `1405` / `4101.` 与 `cost_settle_voucher.rs` / `cost_settle_run.rs` 的常量逐字比对；`C-COST-1~4` 要求两侧都有编号，`C-COST-5/6` 为巡检独有（5 只影响成本准确性、结算不许拦；6 由库层 CHECK 在写入时拦死） |
| 与事务内自检的关系 | 同一判据的两个时间点：事务内（`cost_settle_run::run_asserts`）是「此刻就能拦下」，巡检是「事后每天再验一遍」。C-COST-3 的事务内版取「本期净发生」形态（见 §4.8），巡检取全库余额形态 —— 两者互补，不是两套算法 |
| 迁移 / 错误码 / 后端代码 | 无（纯读校验；不改 schema、不新增错误码、不改 Rust 代码） |

> **B3-10 顺带发现（不属本卡范围，已报）**：`V0381__cost_flow_def.up.sql`（B3-2）的 `INSERT INTO biz_flow_def` 列清单漏写 `is_system`，
> 巡检 H6（`erp_invariants.py --group 审批`）与 H5 因此在**任何库**上都红（静态扫迁移，与库状态无关）。
> 按 AGENTS「已应用迁移不可改」只能新开一条迁移补偿：`UPDATE biz_flow_def SET is_system = TRUE WHERE flow_code = 'cost_settle_approve'`（参照 V0220/V0233 的修法）。

---

## 5. 接口设计

### 5.1 通用约定

| 项 | 口径 |
|---|---|
| 路径前缀 | 宏路径**不含** `/api`（由 `[router] prefix` 注入，AGENTS 约定 8）；前端必须写全 `/api/...`（约定 9） |
| 错误出口 | handler 用 `ResResult<T>`，model 用 `DbResult<T>`；`DbError/io::Error` 直接 `?`（约定 5） |
| `*_id` 必带 `*_name` | 所有出参里的 `product_id` / `workshop_dept_id` / `cost_object_id` / `material_id` 都同时返回 `*_name`（AGENTS 约定 13）；批量 join，防 N+1 |
| 分页出参 | **`{ list, total, page, page_size }`** |
| 单据详情出参 | 详情 + `next_statuses` + `legal_edges`（前端按钮跟配置走，同 `stockout_dto` 的做法） |
| 金额出参 | 一律**字符串**（`Decimal.to_string()`），防止前端 JS number 精度丢失（仓库既有口径，`stockout_dto.rs:299-301`） |
| 权限码 | `business:erp:{资源}:{动作}`，与 `sys_menu.auth_code` 一一对应；`SUPER_ADMIN` 短路 |

> ⚠️ **与卡面的偏差（必须记录）**：卡面写「出参口径（header/items/meta）」，但**本仓库实际没有这个口径** —— 实测 ERP 列表接口统一 `{list,total,page,page_size}`（`stockout_dto.rs:482-492` 的 `StockOutPageRes`，注释原文「与其它列表接口同形」）。本设计**服从仓库现状**，避免造出第二套出参形状。若总管要求统一改成 header/items/meta，那是一次横切重构，应单独立卡，不在本卡里做。

### 5.2 API 清单

#### 成本对象（生产批）

| # | Method | Path | 权限码 | 入参关键字段 | 出参关键字段 |
|---|---|---|---|---|---|
| 1 | GET | `/business/erp/cost-object` | `cost-object:list` | `keyword` `status` `product_id` `workshop_dept_id` `start_from` `start_to` `page` `page_size` | `list[]: id/object_no/object_type/product_id/product_name/workshop_dept_id/workshop_dept_name/plan_qty/completed_qty/wip_qty/status/lot_no/source_doc_no/cost_total`，`total/page/page_size` |
| 2 | GET | `/business/erp/cost-object/{id}` | `cost-object:list` | — | 单头 + `collect_summary{mat,labor,oh,total}` + `period_items[]` + `next_statuses` + `legal_edges` |
| 3 | POST | `/business/erp/cost-object` | `cost-object:create` | `product_id` `target_warehouse_id` `target_location_id?` `plan_qty` `workshop_dept_id` `start_date` `due_date?` `source_doc_no?` `customer_id?` `lot_no?` `material_input_mode?` | 详情（新建后直接可用，免二次 GET） |
| 4 | PUT | `/business/erp/cost-object/{id}` | `cost-object:update` | 同上（`product_id` 有归集后只读） | 详情 |
| 5 | DELETE | `/business/erp/cost-object/{id}` | `cost-object:delete` | — | 空（软删，仅 `OPEN` 且无归集/无入库） |
| 6 | POST | `/business/erp/cost-object/{id}/close` | `cost-object:close` | `force_wip?` `remark?` | 详情 |

#### 批次主数据

| # | Method | Path | 权限码 |
|---|---|---|---|
| 7 | GET | `/business/erp/lot` | `lot:list` |
| 8 | POST | `/business/erp/lot` | `lot:create` |
| 9 | PUT | `/business/erp/lot/{id}` | `lot:update` |

#### 工时台账

| # | Method | Path | 权限码 | 备注 |
|---|---|---|---|---|
| 10 | GET | `/business/erp/cost-labor` | `cost-labor:list` | 筛选：`period_code` `cost_object_id` `dept_id` `employee_id` `status` |
| 11 | POST | `/business/erp/cost-labor` | `cost-labor:create` | 单条录入；`rate` 为空时按 §4.5.4 自动取 |
| 12 | PUT | `/business/erp/cost-labor/{id}` | `cost-labor:update` | 仅 `DRAFT` |
| 13 | DELETE | `/business/erp/cost-labor/{id}` | `cost-labor:delete` | 仅 `DRAFT` |
| 14 | POST | `/business/erp/cost-labor/{id}/confirm` | `cost-labor:update` | `DRAFT → CONFIRMED` |
| 15 | POST | `/business/erp/cost-labor/import` | `cost-labor:import` | multipart xlsx；返回 `{success:0, failed:n, rows:[{line, column, message}]}`（全有或全无） |
| 16 | GET | `/business/erp/cost-labor/template` | `cost-labor:list` | 下载 8 列模板 xlsx |

#### 手工费用台账

| # | Method | Path | 权限码 |
|---|---|---|---|
| 17 | GET | `/business/erp/cost-overhead` | `cost-overhead:list` |
| 18 | POST | `/business/erp/cost-overhead` | `cost-overhead:create` |
| 19 | DELETE | `/business/erp/cost-overhead/{id}` | `cost-overhead:delete` |

#### 分摊规则

| # | Method | Path | 权限码 | 备注 |
|---|---|---|---|---|
| 20 | GET | `/business/erp/cost-alloc-rule` | `cost-alloc-rule:list` | |
| 21 | POST | `/business/erp/cost-alloc-rule` | `cost-alloc-rule:create` | |
| 22 | PUT | `/business/erp/cost-alloc-rule/{id}` | `cost-alloc-rule:update` | |
| 23 | DELETE | `/business/erp/cost-alloc-rule/{id}` | `cost-alloc-rule:delete` | 软删 |
| 24 | POST | `/business/erp/cost-alloc-rule/preview` | `cost-alloc-rule:preview` | 入参 `{period_code, rule_ids?}`；出参逐对象 `basis_value/basis_total/pool_amount/alloc_amount/rounding_adj`（**只算不写**，前端「试算」按钮） |

#### 成本核算批次

| # | Method | Path | 权限码 | 备注 |
|---|---|---|---|---|
| 25 | GET | `/business/erp/cost-period` | `cost-period:list` | 筛选 `period_code` `status` |
| 26 | GET | `/business/erp/cost-period/{id}` | `cost-period:list` | 单头 + 统计 + `items[]`（成本计算单）+ `next_statuses` |
| 27 | POST | `/business/erp/cost-period/calc` | `cost-period:calc` | 入参 `{period_code}`；不存在则建 `DRAFT`，存在 `DRAFT` 则重算；出参批次详情 |
| 28 | POST | `/business/erp/cost-period/{id}/submit` | `cost-period:submit` | 起流程 |
| 29 | POST | `/business/erp/cost-period/{id}/settle` | `cost-period:settle` | 要求 `APPROVED`；一次事务（§4.8） |
| 30 | POST | `/business/erp/cost-period/{id}/reverse` | `cost-period:reverse` | 入参 `{reason}`（≥10 字） |
| 31 | POST | `/business/erp/cost-period/{id}/cancel` | `cost-period:submit` | 仅 `DRAFT` |
| 32 | GET | `/business/erp/cost-period/{id}/cost-sheet?format=xlsx` | `cost-period:list` | 成本计算单导出（对象 × 要素 × 约当 × 单位成本） |
| 33 | GET | `/business/erp/cost-period/{id}/uncollected` | `cost-period:list` | 「未归集领料 / 缺工时对象 / 未分摊池」三类缺口清单（结算前自查） |

#### 二期（标准成本 + 差异）

| # | Method | Path | 权限码 |
|---|---|---|---|
| 34 | GET | `/business/erp/cost-standard` | `cost-standard:list` |
| 35 | POST | `/business/erp/cost-standard` | `cost-standard:create` |
| 36 | GET | `/business/erp/report/cost-variance` | `cost-variance:list` |

### 5.3 权限码清单（与菜单 `auth_code` 一一对应）

```
business:erp:cost-object:list | create | update | delete | close
business:erp:cost-labor:list  | create | update | delete | import
business:erp:cost-overhead:list | create | delete
business:erp:cost-alloc-rule:list | create | update | delete | preview
business:erp:cost-period:list | calc | submit | settle | reverse
business:erp:lot:list | create | update
business:erp:cost-standard:list | create | update      （二期）
business:erp:cost-variance:list                        （二期）
```

真库已种 6 个页面菜单 + 20 个按钮菜单（`01_cost_ddl_check.sql` 第三部分 ④，断言「按钮权限应有 20 个」通过）。

### 5.4 分页 / 筛选 / 导出 / 导入

- **分页**：默认 20，上限 200（与仓库既有 `page_size` 上限一致）；
- **筛选**：成本对象列表支持产品 / 车间 / 状态 / 开工日期区间 / 关键字（单号 + 订单号）；成本计算单支持成本要素下钻；
- **导出**：成本计算单 xlsx（后端 `xlsx` feature 已启用）+ 差异报表 xlsx（二期）；
- **导入**：工时台账 xlsx（16）；费用台账二期再开导入（一期表单够用）；
- **数据权限**：`cost-object` / `cost-labor` / `cost-period-item` 走 `scope::ScopeTarget`（`dept_col='workshop_dept_id'`），`cost-period` 单头 unscoped + 权限码（与 ARCH9-2 的 `salary_sheet` 同口径：单头是期间合计，SELF 无意义）。

---

## 6. 前端设计

### 6.1 页面清单

全部在 `frontend/src/views/business/erp/` 下新建目录，`api/business/erp/` 下对应 api 文件；菜单挂载与真库种子的 id 一致（`01_cost_ddl_check.sql` 第三部分 ④）。

| # | 页面 | 路由 path | component | 菜单挂哪 | title（≤12 字） | 权限码 |
|---|---|---|---|---|---|---|
| 1 | 成本对象 | `/business/erp/cost-object` | `business/erp/cost-object/index` | 财务管理（`…e02`）sort 60 | **成本对象** | `business:erp:cost-object:list` |
| 2 | 工时台账 | `/business/erp/cost-labor` | `business/erp/cost-labor/index` | 财务管理 sort 62 | **工时台账** | `business:erp:cost-labor:list` |
| 3 | 费用台账 | `/business/erp/cost-overhead` | `business/erp/cost-overhead/index` | 财务管理 sort 64 | **费用台账** | `business:erp:cost-overhead:list` |
| 4 | 分摊规则 | `/business/erp/cost-alloc-rule` | `business/erp/cost-alloc-rule/index` | 财务管理 sort 66 | **分摊规则** | `business:erp:cost-alloc-rule:list` |
| 5 | 成本核算 | `/business/erp/cost-period` | `business/erp/cost-period/index` | 财务管理 sort 68 | **成本核算** | `business:erp:cost-period:list` |
| 6 | 批次管理 | `/business/erp/lot` | `business/erp/lot/index` | 仓库管理（`…e01`）sort 80 | **批次管理** | `business:erp:lot:list` |

路由注册在 `frontend/src/router/index.ts` 里按现有 ERP 写法逐条加（参考 `business/erp/stock-out` 那一段，`router/index.ts:643-650`）；菜单由迁移驱动，前端不写死。

> 排期顺序即「数据流顺序」：先能建成本对象 → 才能录工时/费用 → 才能配规则 → 才能跑核算。

### 6.2 关键交互

#### 6.2.1 成本对象（生产批）

表单字段（必填标 ✓）：

```
产品 ✓（物料选择器，只列 material_type IN ('FINISHED','SEMI')）
计划数量 ✓ | 生产车间 ✓（DeptTreeSelect）
产出批号（可空；填了自动建 biz_lot，source=PRODUCTION）
目标仓 ✓ | 库位（可空，跟仓库联动，复用 StockPicker 的仓-库位联动）
开工日期 ✓ | 计划完工日期
上游订单号（按订单生产时填）+ 客户
投料方式：一次投入（默认，一期唯一可选，另一项置灰并提示「三期支持」）
```

按钮态：

| 状态 | 可点 | 置灰并提示 |
|---|---|---|
| `OPEN` 且无归集 | 保存、关批、作废、领料（跳转新建出库单并带 `cost_object_id`） | — |
| `OPEN` 且有归集 | 保存（限字段）、关批、领料 | 作废（「已有归集，不能作废，请关批」） |
| `CLOSED` | 只读 | 全部写操作（「已关批，要复工请新建批」） |

#### 6.2.2 成本核算（5 步单向导）

```
① 选期间 → ② 自查缺口 → ③ 计算 → ④ 提交审批 → ⑤ 结算
```

| 步 | 界面 | 交互要点 |
|---|---|---|
| ① | 期间选择器（只列未结账期间；已存在批次则显示状态并可继续） | 期间已结账 → 置灰 + 「已结账，需先反结账」 |
| ② | 「未归集清单」三块：未挂成本对象的领料单 / 无工时的在产对象 / 未分摊的池 | 每块给出条数与跳转；**不阻断**（可以带着缺口算，但结算前必须有解释） |
| ③ | 计算结果表：对象 × 料工费 × 约当 × 单位成本 × 完工/在制 | 「重算」按钮只在 `DRAFT` 亮；重算二次确认（会清空当期归集） |
| ④ | 提交 → 弹流程预览（两级审批人） | 提交后整页只读 |
| ⑤ | 结算 → 危险确认框（列出「将生成 N 张入库单 + 3 张凭证 + 金额合计」） | 需输入「结算」二字确认（高危动作与仓库既有口径一致） |

**错误提示必须带「怎么修」**（每条都在 handler 里给具体文案）：

| 场景 | 文案 |
|---|---|
| 缺工时 | 「成本对象 CB-20261101-000003 本期没有工时记录，无法计算人工成本。请录入工时台账（每天或每周）后重算」 |
| 缺在制数量 | 「成本对象 … 的期末在制数量/完工进度未填。请在成本对象页面填写本期完工数量与期末在制后重算」 |
| 池无规则 | 「车间一车间 科目 4101.01 有 3,000.00 待分摊，但没有生效的分摊规则。请到「分摊规则」新建规则或指定该科目不进成本」 |
| 池无对象 | 「车间一车间 科目 4101.01 有 3,000.00 待分摊，但本期没有在产成本对象。请检查是否漏建成本对象」 |
| 4101 有非成本记账 | 「科目 4101.02 本期发生 5,000.00，但成本模块只归集到 4,000.00（差 1,000.00）。差额来自凭证 V-202610-000123，请改成通过成本模块归集」 |
| 期间已结账 | 「会计期间 2026-10 已结账，不能重算。请先反结账（财务总监）」 |
| 结算前置 | 「批次状态是 SUBMITTED，需审批通过后才能结算」 |

#### 6.2.3 分摊规则

表单：规则编码 ✓ / 规则名 ✓ / 车间 ✓ / 费用科目（只列 `4101.*`，下拉显示「4101.01 制造费用-折旧费」）/ 分配标准（四选一，带一句解释：工时=按人工工时占比）/ 生效期（默认当月 1 日起，无终止日）。

「试算」按钮（`cost-alloc-rule:preview`）：选期间 → 出「待分摊池 + 各对象基数 + 分摊额」，**不落库**；表尾显示 `合计 = 池` 的勾（守恒可视化）。

#### 6.2.4 工时台账

- 列表支持「按成本对象分组」视图（车间统计员按批填）；
- 单条录入时，工资率字段空着 → 后端按 §4.5.4 自动取，返回时把「实际取到的费率 + 来源（车间/公司/对象）」显示在字段右侧（`.` 用户能验收）；
- 导入：拖 xlsx → 全有或全无 → 失败时表格逐行红标（行号 + 列 + 原因），可下载错误清单。

### 6.3 复用哪些现有组件

| 组件 | 用途 |
|---|---|
| `views/business/flow/components/FlowProgress.vue` | 成本批次审批进度（引用方式见 `payable/index.vue:719`） |
| `views/business/erp/shared/DocActionBar.vue` | 成本对象 / 成本批次的动作条（`state_machine` 边表驱动按钮） |
| `views/business/erp/shared/DocAttachmentPanel.vue` | **二期**标准成本卡附件（工艺文件/标准成本卡）；一期成本核算无附件需求，故本卡不加 `DocKind` |
| `components/ImportEntryButton.vue` | 工时导入入口（已有导入框架） |
| `components/BatchActionBar.vue` | 工时台账批量确认 |
| `components/DeptTreeSelect.vue` | 车间选择 |
| `components/DictSelect.vue` | 费用项目（`cost_expense_item` 字典） |
| `components/ListMemoryBar.vue` + `FormMemoryHint.vue` | 列表筛选记忆（与其它 ERP 页一致） |
| `views/business/erp/shared/money.ts` | 金额展示（4 位/6 位分别处理） |

**不新增前端通用组件**：一期所有交互都能用现有组件拼出来。唯一新增的是 `cost-period/steps` 下的三个页内组件（缺口清单 / 成本计算单 / 结算确认框），属页面私有。

---

## 7. 迁移与上线

### 7.1 迁移文件清单

`backend/migrations/business/` 当前最大版本 **V0367**（实测：`ls | sed 's/_.*//' | sort -u | tail`），本设计占用 **V0368 ~ V0379**，每个版本一对 `up/down`。

| 版本 | 文件 | 内容 | 幂等策略 |
|---|---|---|---|
| V0368 | `cost_batch_master` | `biz_lot` + `biz_cost_object` | `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` |
| V0369 | `cost_collect_tables` | `biz_cost_collect` + `biz_cost_labor` + `biz_cost_overhead` + `biz_cost_rate` | 同上 |
| V0370 | `cost_alloc_tables` | `biz_cost_alloc_rule` + `biz_cost_alloc_result` | 同上 |
| V0371 | `cost_period_tables` | `biz_cost_period` + `biz_cost_period_item`（含排除 REVERSED 的唯一键） | 同上 |
| V0372 | `cost_standard_tables` | `biz_cost_standard` + `_item` + `biz_cost_variance`（二期表，先建好） | 同上 |
| V0373 | `stock_out_cost_object` | `biz_stock_out + cost_object_id` + 局部索引 | `ADD COLUMN IF NOT EXISTS` |
| V0374 | `stock_in_production` | `biz_stock_in` 两列 + `ck_biz_si_type` 加 `PRODUCTION` + `ck_biz_si_production` | `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（PG 无 `ADD CONSTRAINT IF NOT EXISTS`） |
| V0375 | `cost_accounts` | 翻 `4001/4101/2211` 非末级 + 种 9 个末级 + 自检 | 条件 `UPDATE … WHERE is_leaf=1`（第二遍 0 行）+ `ON CONFLICT (account_code) DO NOTHING` + `DO $$` 自检 |
| V0376 | `cost_menu_and_perm` | 6 页 + 20 按钮 + 角色绑定（`SUPER_ADMIN/ADMIN/FINANCE_MANAGER/FINANCE_DIRECTOR/WAREHOUSE_MANAGER`） | `ON CONFLICT (id) DO NOTHING` / `ON CONFLICT (role_id, menu_id) DO NOTHING` |
| V0377 | `cost_seq_and_dict` | `biz_seq` 两条（CB/CS）+ `sys_dict_type`/`sys_dict_item`（`cost_expense_item` 7 项） | `ON CONFLICT (id)` / `ON CONFLICT (dict_code) WHERE deleted_at IS NULL` |
| V0378 | `cost_flow_def` | 审批流 `cost_settle_approve`（两级）+ 版本快照 | 四重守卫（与 V0332 同款：已有 flow_code / 已有同 biz_type 默认流 / 角色缺失 → 整条跳过） |
| V0379 | `cost_lot_backfill` | 存量 `lot_no` → `biz_lot` 回填（三处来源） | `INSERT … SELECT DISTINCT … ON CONFLICT DO NOTHING`；第二遍 0 行 |

**为什么字典只种一个**（`cost_expense_item`）：费用项目随行业扩展（客户会加「模具摊销」「烤版费」），落字典能免改代码；而「成本要素」「分摊标准」是**算法分支**（代码必须认识才算得出来），落字典只会制造「配了但算不了」的坑 —— 与 V0201「选了不会算错」的教训同源。

### 7.2 存量数据处理

| 数据 | 处理 | 是否 downtime |
|---|---|---|
| 13 张新表 | 建空表 | 否 |
| `biz_stock_out/in` 新列 | 加可空列，不设默认值（避免全表重写） | 否 |
| `ck_biz_si_type` 重建 | 需要 `ACCESS EXCLUSIVE`；`biz_stock_in` 当前规模小（实测库级别）→ 毫秒级 | 否（秒级锁） |
| `biz_account` 翻非末级 | **先检查历史分录**（下面给 SQL），0 行才翻 | 否 |
| `biz_lot` 回填 | 三条 `INSERT … SELECT DISTINCT`（V0379） | 否 |
| `biz_cost_*` 业务数据 | **没有 backfill**（新模块，无存量） | — |

`biz_account` 翻非末级前的检查 SQL（V0375 里用 `DO $$` 写成自检，不通过就让迁移失败）：

```sql
-- 4001 / 4101 / 2211 是否已有历史分录（非 0 就不能翻末级）
SELECT a.account_code, count(e.id) AS entry_rows
  FROM biz_account a
  LEFT JOIN biz_voucher_entry e ON e.account_code = a.account_code
 WHERE a.account_code IN ('4001','4101','2211')
 GROUP BY a.account_code;
```

本卡在设计阶段实测：三科目**历史分录 0 行**（理由也很直白 —— 代码里从来没有一处记账到 4001/4101，grep 实测；2211 只有 ARCH9-2 上线后才会有）。所以翻末级零风险；但迁移里仍保留该检查，因为客户环境可能被手工改过账。

`biz_lot` 回填 SQL（V0379 核心，三处来源、幂等）：

```sql
WITH src AS (
    SELECT material_id, btrim(lot_no) AS lot_no, 'STOCK_IN' AS from_tbl FROM biz_stock_in_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
    UNION
    SELECT material_id, btrim(lot_no), 'STOCK_OUT' FROM biz_stock_out_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
    UNION
    SELECT material_id, btrim(batch_no), 'SERIAL' FROM biz_material_serial WHERE batch_no IS NOT NULL AND btrim(batch_no) <> '' AND deleted_at IS NULL
)
INSERT INTO biz_lot (lot_no, material_id, source_type, source_doc_type, remark)
SELECT DISTINCT ON (material_id, lot_no) lot_no, material_id, 'MANUAL', 'BACKFILL',
       '存量批号回填（来源：' || from_tbl || '），请确认来源类型与效期'
  FROM src
ON CONFLICT (material_id, lot_no) WHERE deleted_at IS NULL DO NOTHING;
```

同时输出**待清理清单**（不阻断迁移，进运维报告）：

```sql
-- 不合规批号：超长 / 含空格 / 与物料不匹配（物料已被软删）
SELECT material_id, lot_no, length(lot_no) FROM biz_lot
 WHERE deleted_at IS NULL AND (length(lot_no) > 64 OR lot_no <> btrim(lot_no));
```

### 7.3 回滚方案

| 层 | 回滚方式 | 边界 |
|---|---|---|
| 结构 | 每个迁移配套 `.down.sql`：删索引 → 删表（新表）/ 删列与约束（改造表）/ 科目改回 `is_leaf=1` 并删子科目 / 菜单与号段按 id 删 | 逆序执行 V0379 → V0368 |
| 数据 | `biz_lot` 回填行可整表删（`deleted_at IS NULL AND source_doc_type='BACKFILL'`）；`biz_cost_*` 全空表直接删 | — |
| **已结算批次** | **必须先红冲所有 `SETTLED/CLOSED` 批次，再执行 down**。down 里带检查：`SELECT count(*) FROM biz_cost_period WHERE status IN ('SETTLED','CLOSED')` 非 0 → `RAISE EXCEPTION` 让 down 失败 | 因为 down 只删表，删不掉已经进过账的凭证与库存；硬删会造成凭证孤儿 |
| 科目 | down 里 `4001/4101/2211` 翻回末级前，同样检查「子科目无分录余额」 | 有余额就报错，不允许静默丢余额 |

**回滚的最大风险不是结构，是数据**：`biz_cost_period` 一 `SETTLED` 就已经动了库存和凭证，`down` 不做业务回滚。所以**上线顺序上，一期必须先在测试库跑完一个完整「calc → 审批 → 结算 → 红冲」链路**（§7.4 第 4 批的门槛）。

### 7.4 分几批上线（能否灰度）

**能灰度**：新增一个 `sys_config` 开关 `erp.cost.enabled`（`BOOLEAN`，默认 `false`，`config_level=2` 业务级）。

| 批 | 内容 | 谁能用 | 门槛 |
|---|---|---|---|
| **B1** 地基 | V0368~V0374 结构迁移 + 成本对象/批次主数据页 + 领料挂成本对象 | 全客户（开关关闭时页面隐藏，领料字段可选） | 迁移零残留 + `menu_invariants.sh` 5 条全绿 |
| **B2** 归集 | 工时台账 + 费用台账 + 归集（calc 到「成本计算单」为止，**不结算**） | 试点客户（开关打开） | 一个真实客户的 1 个月数据能算出计算单，与手工 Excel 差 ≤ 0.01 |
| **B3** 结算 | 审批流 + 结算（入库 + 三张凭证）+ 红冲 | 试点客户 | 演练：完整跑「calc → 审 → 结算 → 报表对平 → 红冲 → 重算」两轮 |
| **B4** 收口 | 巡检 6 条 + 结账 blocker + 成本计算单导出 | 全客户 | 巡检连续 3 天全绿（含 B3 试点数据） |

二期（标准成本 + 差异）单独排期，不动一期结构（V0372 已把表建好，二期只加代码与菜单）。

---

## 8. 工作量估算与风险

### 8.1 拆实现卡清单（一期 42 张 + 二期 14 张，每张 ≤1.5 小时）

> 依赖列写「←」表示必须先完成哪几张。批次与 §7.4 对应。

#### B1 地基（12 张）

| # | 卡 | 内容 | 依赖 |
|---|---|---|---|
| B1-1 | 迁移 V0368 | `biz_lot` + `biz_cost_object` + 自检 | — |
| B1-2 | 迁移 V0369 | 归集四表 + 自检 | B1-1 |
| B1-3 | 迁移 V0370/0371 | 分摊两表 + 期间两表（含排除 REVERSED 唯一键） | B1-2 |
| B1-4 | 迁移 V0372 | 二期三表（先建，代码后写） | B1-3 |
| B1-5 | 迁移 V0373/V0374 | 出库加列；入库加 `PRODUCTION` + 两列 | B1-3 |
| B1-6 | 迁移 V0375 | 科目翻末级 + 9 末级 + 分录为 0 自检 | B1-5 |
| B1-7 | 迁移 V0376/0377/0379 | 菜单权限 + 号段字典 + 批号回填 | B1-6 |
| B1-8 | 后端成本对象四件套 | `mod/handler/dto/model`：CRUD + 关批（5 条前置） | B1-1 |
| B1-9 | 后端批次主数据四件套 | `biz_lot` CRUD + 「按物料取批号」内部接口 | B1-1 |
| B1-10 | 领料单挂成本对象 | `stockout_dto` 加字段 + 校验「对象 OPEN」+ 复算出库借方科目 | B1-8、B1-5 |
| B1-11 | 前端成本对象页 | 列表 + 表单 + 关批 | B1-8 |
| B1-12 | 前端批次管理页 + 领料单字段 | 列表 + 出库单加「成本对象」选择器 | B1-9、B1-10 |

#### B2 归集（13 张）

| # | 卡 | 内容 | 依赖 |
|---|---|---|---|
| B2-1 | 工时台账后端 | 四件套：CRUD + 确认 + 金额 CHECK 错误映射 | B1-2 |
| B2-2 | 工时导入 | xlsx 解析 + 全有或全无 + 逐行错误 | B2-1 |
| B2-3 | 费率取数 | `biz_cost_rate` 优先级（对象>车间>公司）+ 缺费率报错 | B2-1 |
| B2-4 | 费用台账后端 | 四件套 + 科目合法性（4101.*） | B1-2 |
| B2-5 | 分摊规则后端 | 四件套 + 生效期版本选择 + 试算（preview） | B1-3 |
| B2-6 | calc 第 1 段：料归集 | 扫当期已过账领料 → 幂等写 collect | B1-10 |
| B2-7 | calc 第 2 段：工归集 | 工时 CONFIRMED → 逐条写 collect | B2-1 |
| B2-8 | calc 第 3 段：费归集 | 折旧/报销/手工 → 池行 + 直接行 | B2-4 |
| B2-9 | calc 第 4 段：池与 4101 对账 | 按 (车间,科目) 对账，差额列凭证号 | B2-8 |
| B2-10 | calc 第 5 段：分摊计算 | 四标准基数 + 最大余数法 + 守恒断言 | B2-5、B2-9 |
| B2-11 | calc 第 6 段：约当产量与计算单 | 公式 + 差额倒挤 + 期初在制结转 | B2-6、B2-7、B2-10 |
| B2-12 | 「未归集清单」接口 | 三类缺口查询 | B2-11 |
| B2-13 | 前端 工时/费用/规则 三页 | 三个列表页 + 试算弹窗 | B2-1~B2-5 |

#### B3 结算（11 张）

| # | 卡 | 内容 | 依赖 |
|---|---|---|---|
| B3-1 | 结算四件套骨架 | `biz_cost_period` CRUD + 状态机边表 + 权限码 | B1-3、B2-11 |
| B3-2 | 审批流种子 V0378 | `cost_settle_approve` 两级 + 四重守卫 | B1-7 |
| B3-3 | 提交/撤回/驳回接线 | 走引擎 + 驳回打回 DRAFT | B3-1、B3-2 |
| B3-4 | 结算：完工入库 | 生成 `stock_in(PRODUCTION)` + 明细 + `inventory_writer` 三本账 | B3-1 |
| B3-5 | 结算：三张凭证 | 人工计提 / 台账（条件必备）/ 分摊（借方含池行+直接行）/ 完工结转（`post_voucher`，实测 3–4 张） | B3-4、B2-11 |
| B3-6 | 结算：状态回写 + 事务内 6 条断言 | 一次事务 + C-COST-1~4 事务内自检 | B3-5 |
| B3-7 | 红冲 | 逐凭证 `reverse_posted` + 库存回冲 + 状态 → `REVERSED` | B3-6 |
| B3-8 | 领料凭证借方分流 | `debit_account_of` 前加分支（挂对象 → 4001.01） | B1-10 |
| B3-9 | 结账 blocker `COST_UNSETTLED` | `period.rs` 加第 5 项 + items 口径 | B3-1 |
| B3-10 | 巡检 6 条 | `erp_invariants.py` 加 C-COST-1~6 | B3-6 |
| B3-11 | 前端成本核算页 | 5 步向导 + 试算确认 + 审批进度 + 红冲 | B3-1~B3-7 |

#### B4 收口（6 张）

| # | 卡 | 内容 | 依赖 |
|---|---|---|---|
| B4-1 | 成本计算单导出 xlsx | 后端 `xlsx` + 前端下载 | B3-11 |
| B4-2 | 成本对象 ↔ 出库/入库反查 | 详情页「这个批的料从哪来、货去哪了」 | B3-11 |
| B4-3 | 一键领料跳转 | 成本对象 → 新建出库单预填 | B1-11、B1-10 |
| B4-4 | 开关 `erp.cost.enabled` | 前后端按开关隐藏/放行 | B3-11 |
| B4-5 | 端到端演练脚本 | `scripts/verify/cost_e2e.sh`：calc→审→结算→对平→红冲 | B3-10 |
| B4-6 | 文档与二开指引 | `docs/后端二开指南.md` 增「成本核算」章 + `skill/SKILL.md` 速查 | B4-5 |

#### 二期（14 张，单独排期）

| # | 卡 | 内容 |
|---|---|---|
| S-1 | 标准成本卡四件套 | 表头 + 明细 CRUD |
| S-2 | 标准成本卡前端 | 列表 + 明细编辑（BOM 式） |
| S-3~S-6 | 四类差异计算 ×4 | 材料价差/量差、人工率差/效差（各一张，含恒等式断言） |
| S-7~S-8 | 制造费用两差异 ×2 | 耗费差 / 产量差 |
| S-9 | 差异落表与幂等 | `biz_cost_variance` 写入 |
| S-10 | 差异报表接口 + 钻取 | 期间×对象×类型 |
| S-11 | 差异报表前端 | 表格 + 图表 |
| S-12 | 差异报表导出 xlsx | |
| S-13 | 三期预研：`object_type='PRODUCT'` + 分步结转 | 设计稿 |
| S-14 | 二期回归 | 与一期不冲突验证（混用标准/实际对象） |

### 8.2 人日估算

| 阶段 | 卡数 | 人时 | 人日（按 8h） |
|---|---|---|---|
| B1 地基 | 12 | ≈ 16 | 2.0 |
| B2 归集 | 13 | ≈ 19 | 2.4 |
| B3 结算 | 11 | ≈ 18 | 2.3 |
| B4 收口 | 6 | ≈ 8 | 1.0 |
| **一期小计** | **42** | **≈ 61** | **≈ 7.6** |
| 回归 + 演练 + 文档 | — | ≈ 16 | 2.0 |
| **一期合计** | | **≈ 77** | **≈ 9.6 人日**（1 人 ≈ 2 周；2 人并行 ≈ 6 工作日） |
| 二期 | 14 | ≈ 24 | ≈ 3.0（含差异报表打磨 ≈ 4 人日） |

**诚实说明会溢出的 4 张**（按经验一定超 1.5h）：

1. **B2-10 分摊计算**：四种基数 + 最大余数法 + 与试算共用一套逻辑，逻辑本身不难，但「基数口径」的边界（哪些对象参与、池为 0 怎么办、基数全为 0 怎么办）会反复改，实测一般要 3h。
2. **B3-6 结算事务**：涉及 `inventory_writer` 三种调用 + 三张凭证 + 事务内 6 条断言，任何一个反序列化/类型不对都会来回调，2.5~3h。
3. **B3-7 红冲**：红字凭证 + 库存回冲 + 生产入库单作废 + 状态回退，四段各自有前置校验，3h。
4. **B2-11 约当产量**：公式不难，难在「期初在制按要素还原」与 4 位尾差倒挤的边界（本卡的算例已经把数字钉死，能省一半时间）。

### 8.3 已知风险（12 条）

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R-1 | **数据源质量**：客户不填工时，人工成本恒为 0（或算不出来） | 成本「算出来但不准」→ 决策失真 | D-7 不猜 + 导入模板 + 「未归集清单」+ 报表标注「人工来源：手工/未录」 |
| R-2 | **折旧/报销科目配错**：资产分类还指 5602，报销类型还指 5602 | 4101 池永远是 0，制造费用不进成本 | 上线检查清单 + 「未分摊池」为空时前端提示「本期无制造费用，请确认资产分类/费用类型是否已配 4101」 |
| R-3 | **4101 有旁路记账**（有人手工凭证记 4101） | C-COST-1 红，结算被拦 | 错误列凭证号，引导改走成本模块；巡检每日跑 |
| R-4 | **完工进度拍脑袋**（`completion_pct` 没有客观依据） | 在制与完工成本被转移，可能操纵利润 | 前端强制填写并留「填写人 + 时间」；报表展示「进度来源=手工」；二期接 MES 后自动采集 |
| R-5 | **移动加权的固有局限**：同一物料多批混用一个 `avg_cost`，分批法算出的批成本与库存账面单价不一致 | 财务对不上账会来问 | 明确口径：批成本是「成本对象的制造口径」，库存是「物料×仓库的计价口径」，两者本来就不该相等（采购价差已经在 avg_cost 里被平均掉了）。文档 §2.2 已写；报表上加口径说明 |
| R-6 | **期间唯一键并发** | 双结算 → 重复入库与双凭证 | 排除 REVERSED 的唯一键（真库已验）+ `SELECT … FOR UPDATE` + 结算幂等（`period_item.stock_in_id` 非空即拒） |
| R-7 | **跨期补录归集**：9 月的领料单在 10 月才过账 | 9 月成本少算 | 明确口径「按过账期间归集」（§4.10）；月结流程要求「月末最后一件事是过账完所有单据」（结账 blocker `DOC_UNFINISHED` 已兜住未完成单据） |
| R-8 | **工时台账被批量改**（没有审计） | 成本被悄悄改动 | 工时确认后 `status=CONFIRMED` 只读；已结算期间的归集行只读；改数必须走红冲 |
| R-9 | **`biz_cost_collect` 重算全删**在数据量大时锁表 | 重算慢 | 只允许 `DRAFT` 重算 + `DELETE … WHERE period_code=?`（有索引）+ 单期间数据量 < 3000 行 |
| R-10 | **与 ARCH9-2 的 2211/薪资双计提** | 应付职工薪酬虚增 | D-13 二选一 + `labor_source` 快照 + 结算前检查「同期是否已有薪资单」（有则强制 `PAYROLL` 或报错） |
| R-11 | **ARCH9-1 多组织落地后键变更** | 唯一键冲突/双单 | `org_id` 现已进唯一键（哨兵默认），不改键 |
| R-12 | **BOM 缺失**：一期料归集靠实际领料，不校验 BOM | 领错料也算得进去 | 明确边界（一期不做 BOM 定额控制）；差异分析（二期）需要标准用量时才要求录入标准 BOM |

### 8.4 待确认（每条带默认取向与确认方式）

| # | 待确认 | 默认取向（不改也能上线） | 确认方式 |
|---|---|---|---|
| Q-1 | **制造费用是否要「二次分配」**（辅助车间→生产车间） | 不做（D-8） | 试点客户实际车间结构是否 ≥2 层且辅助车间有费用；问财务总监 |
| Q-2 | **差异入账（标准成本计价 + 1404 分摊）** | 不做（D-15），只出差异报表 | 需要产品书面拍板；触发条件：≥3 家客户明确要求「按标准成本记账」 |
| Q-3 | **`CLOSED` 成本对象是否需要「重开」** | 不可逆（§4.1） | 试点期观察是否出现「关早了」的诉求；若高频则二期加「重开 + 重算当期」 |
| Q-4 | **人工来源切换时机**（MANUAL → PAYROLL） | 一期 MANUAL；ARCH9-2 上线后按客户选择 | 与 ARCH9-2 的上线卡对齐，切换卡由 HR 侧发起 |
| Q-5 | **完工进度是否有更客观的数据源**（如工序数量） | 手工填（R-4） | 三期接 MES 后取工序过站数据；过渡期可要求车间每日报「完成件数」 |
| Q-6 | **`biz_stock_in.in_type='PRODUCTION'` 是否要单独一张「完工入库单」** | 不要（结算直接生成，§4.7.6） | 问财务/审计：是否需要车间签字确认的入库单。若要，二期加单据类型（不影响本卡表结构） |

---

## 附录 A：证据索引（都能重跑）

| 证据 | 路径 | 内容 | 结论 |
|---|---|---|---|
| DDL 真库验证脚本 | `docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql` | 13 表 + 4 类改造 + 6 类种子 + 8 组断言，整段 `ROLLBACK` | 全过 |
| DDL 运行输出 | 同目录 `01_cost_ddl_check.out` | 102 行完整日志 | `✅ 全部断言通过：13 表 / 4 类改造 / 6 类种子 / 周期唯一键口径`；`✅ 零残留：ROLLBACK 干净` |
| 算例真库演算 | 同目录 `02_cost_calc_drill.sql` | 约当产量 / 分摊守恒 / 差异恒等式 三个算例 | 全过 |
| 算例运行输出 | 同目录 `02_cost_calc_drill.out` | 59 行 | 完工 22311.1104（单位 27.888888）/ 在制 4688.8896；分摊 2400/800/800 与 333.3333×2+333.3334；差异 600+3840=4440、−180+500=320 |

复跑命令：

```bash
PGPASSWORD=<pwd> psql -h 127.0.0.1 -U <user> -d <db> -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql
PGPASSWORD=<pwd> psql -h 127.0.0.1 -U <user> -d <db> -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-3-cost-ddl/02_cost_calc_drill.sql
```

**真库跑通过程中改掉的 3 个真错误**（记录下来，说明「文档 DDL 必须跑」不是形式主义）：

1. **`uk_biz_cost_period` 写成 `period_code` 全表唯一** → 红冲后该期间永远无法重新结算（死单）。改成排除 `REVERSED/CANCELLED` 的部分唯一索引，并加了两段反向断言（两张红冲行存在时能建活跃批次；同期间两张活跃批次必被拒）。
2. **`biz_cost_collect.cost_object_id` 写成 NOT NULL** → 车间费用池（分摊前不属于任何对象）无处安放，只能挂在某个对象上再摊出来，等于把「待分摊」伪装成「已归属」，池与 4101 永远对不平。改成可空 + `ck_biz_cost_collect_obj` 限定只有三类池来源可为空。
3. **`source_type` 值域漏了 `DEPRECIATION`/`EXPENSE`/`OVERHEAD_TICKET`** → 折旧与报销的归集行写不进去（只有 `STOCK_OUT/LABOR_TICKET/ALLOC/MANUAL`）。补齐为 7 值。

## 附录 B：本设计依据的实测事实（不是推断）

| 事实 | 来源 |
|---|---|
| `biz_inventory` 只有 `avg_cost`（移动加权），粒度 = 物料×仓库 | `V0137__erp_master_data_and_warehouse.up.sql:400-403` |
| 成本法值域已收敛为仅 `MOVING_AVG` | `V0201__erp_material_costing_moving_avg_only`；`erp/dto.rs:73-77,152,3583` |
| `biz_stock_out.out_type` 四值，`ISSUE` 借 6401 | `V0203__erp_stock_out.up.sql:60,81,116`；`stockout_base.rs:167`；`stockout_handler.rs:747` |
| `biz_stock_in.in_type` 四值，**无 PRODUCTION** | `V0330__inventory_biz_type_and_sourceless_in_type.up.sql:53` |
| 词表里 `ProductionIn='PRODUCTION_IN'` 已定义但无写入口 | `shared/doc_kind.rs:687,761,818` |
| 库里有 3 处自由文本批号、**没有 `biz_lot`** | `V0145:225`、`V0203:141`、`V0138:80` |
| 凭证唯一收口 + 只允许末级科目 + 已结账期间只读 | `erp/voucher.rs:1963-2050`（`post_voucher`） |
| 库存三本账唯一写入口 + CI 闸 | `erp/inventory_writer.rs:1-25`；`scripts/verify/inventory_write_gate.py` |
| `4001/4101/2211` 均 `is_leaf=1`、无子科目、无历史分录 | 真库查询 `biz_account`（56 行全表）+ `grep`（`backend/src` 里这三个编码只出现在 `cash_flow.rs` 的科目归类清单与测试，无一处记账） |
| 存货报表项已含 `1401..1405,1408,4001,4101` | `V0183__erp_balance_sheet.up.sql:192` |
| `1404 材料成本差异` 科目已存在 | `V0183:76` |
| 结账预检四项 blocker 常量 | `erp/period.rs:151-160`；文档头 `period.rs:25-40` |
| 折旧凭证借方按部门写 `aux_dept_id` | `erp/depreciation_model.rs:72,89,1068` |
| 折旧费用科目分类级可配（可改指 4101） | ARCH-6 §8.2-5（`ARCH-6-财务扩展设计.md:3175,3183`） |
| ARCH-4 D9「成本回写只写两处、不建第二本成本账」 | `ARCH-4-通用MES与委外架构设计.md:741-758` |
| ARCH-4 预留 `biz_work_log.labor_minutes/equip_minutes` | 同上 §3.9（`:787`） |
| 批次主数据「本批不做，作为独立 ADR」 | `通用勾稽规则与业务链映射表.md:566`（BR-4） |
| 菜单挂载点与 sort 现状 | 真库 `sys_menu` 查询（财务管理 `…e02` 子项 10~50；仓库管理 `…e01` 10~70） |
| 号段表现状（PZ/IN/OUT/TK/SN…） | 真库 `biz_seq` 查询 |
| 列表出参是 `{list,total,page,page_size}` | `stockout_dto.rs:482-492` |
| 迁移号段现状最大 V0367 | `ls backend/migrations/business \| sed 's/_.*//' \| sort -u \| tail` |
| 流程种子四重守卫范本 | `V0332__purchase_return.up.sql:269-300` |

## 附录 C：术语表

| 术语 | 本文含义 |
|---|---|
| 成本对象 | 料工费归集的筐。一期 = 生产批（`BATCH`） |
| 分批法 | 按批（订单/生产批）归集与结转成本 |
| 品种法 | 按产品归集料工费（不区分批次），适合流程型连续生产 |
| 约当产量 | 把期末在制按完工进度折算成「相当于多少完工品」 |
| 制造费用池 | 车间级、尚未归属到具体成本对象的费用（折旧/报销/手工费用） |
| 一次分配 / 二次分配 | 车间费用 → 成本对象 / 辅助车间 → 生产车间 |
| 最大余数法 | 分摊尾差按「余数最大者」单行收口，保证合计等于池总额 |
| 两差异法 | 制造费用只拆「耗费差异 + 产量差异」 |
| 沉没在制 | 期末在制成本（4001 期末余额） |
| 完工单位成本 | 完工转出成本 / 完工数量，写入完工入库单价 |

---

**设计到此为止，未写任何实现代码、未改任何现有代码、未建迁移文件**（唯一新增文件是本设计文档与 `docs/evidence/` 下两个验证脚本及其输出）。DDL 与算例均已在 PG 真库跑通并 `ROLLBACK`，可复跑验证。
