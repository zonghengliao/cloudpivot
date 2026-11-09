# ERP 凭证辅助核算口径（DC-11 · P2-1 / P2-1b）

> 代码入口：`backend/src/business/erp/voucher.rs`（写入口径）、
> `backend/src/business/erp/ledger.rs`（辅助账查询）、
> `backend/src/business/erp/payable_payment.rs`（付款凭证的部门）。
> 真机验收：`scripts/verify/p21_aux_e2e.py`（56 项断言）、
> `scripts/verify/p21b_switch_point.py`（20 项切换点断言）。
> §4 是 **P2-1b 决议**（2026-11-04）：多物料单按**去重物料**拆分录 —— 决议口径即目标态，
> **已落地（切换日 2026-11-04）**：`voucher::purchase_in_lines` / `create_for_stock_out` 按物料出行，
> 切换日**之后**新建的收货 / 出库单按 §4.3 行数出行；切换日**之前**的存量单永久保持 §4.0 的
> 「一条汇总行 + 物料留空」（§4.7：不回填）。

## 1. 一句话口径

**每条分录只挂"这条分录自己说得清"的维度。** 说不清就留空 —— 留空是信息缺失，
记错是信息错误，后者财务发现不了、且会污染辅助账余额。

## 2. 哪些凭证写哪些维度（**当前实现**）

> ✅ **P2-1b 已落地（2026-11-04）**：「采购入库 / 出库」两行的物料维度从「仅单物料单」
> 改为**逐去重物料一行**（粒度 `MATERIAL`，不是明细行），行数见 §4.3。下表是**当前实现**；
> 切换日之前的存量凭证仍是「汇总行 + 物料留空」（§4.7 不回填）。

| 业务 | 凭证来源 | 借方行 | 贷方行 |
|---|---|---|---|
| 采购入库（收货过账） | `STOCK_IN` | **逐物料一行**：`aux_material_id` + `aux_warehouse_id`（收货仓） | `aux_supplier_id`（按 DOC 一条） |
| 期初 / 其他入库（序列号批量生成） | `SERIAL_BATCH` | 存货行：`aux_material_id` + `aux_warehouse_id`；对方 1901 行只有 `aux_material_id` | — |
| 序列号出库 / 报废 | `SERIAL_OUT` / `SERIAL_SCRAP` | 6401 / 1901 行：`aux_material_id` | 1405 行：`aux_material_id` + `aux_warehouse_id` |
| 出库 / 领用 / 报废单 | `STOCK_OUT` | 6401 / 1901 行：**逐物料一行** `aux_material_id`，不挂仓库 | 1405 行：**逐物料一行** `aux_material_id` + `aux_warehouse_id` |
| 盘盈盘亏 | `STOCK_TAKE` | 按物料拆行：每行 `aux_material_id`；盘亏带 `aux_warehouse_id` | 同左 |
| 付款 | `PAYMENT` | 2202 行：`aux_supplier_id` + **`aux_dept_id`** | 1002 行：`aux_supplier_id`（**不挂部门**） |
| 手工凭证 | `MANUAL` | **按科目声明**：`biz_account.aux_dimensions` 里有的维度，分录里必须选（见 §2.1） | 同左 |
| 期初 / 年结 / 红冲 | `OPENING` / `YEAR_END` | 不写维度 | 不写维度 |
| 红字（红冲） | 任意 | **原样复制原分录的四个辅助列**（技术架构 §3.1.7 第 4 条） | 同左 |

### 2.1 手工凭证的辅助核算（E2E04-FIX-1，2026-11-09）

手工凭证是财务补账 / 调整的唯一入口，**不可能只靠自动凭证覆盖辅助账**。因此
`POST /business/erp/voucher` 的每条分录支持四个可空辅助字段：

```jsonc
{
  "summary": "计提本月办公费",
  "entries": [
    { "account_id": "<5602.03 id>", "debit": "100.0000",
      "aux_dept_id": "<sys_dept id>" },      // 5602.x 声明了 DEPT → 必填
    { "account_id": "<1002 id>", "credit": "100.0000" }   // 1002 不声明维度 → 不用填
  ]
}
```

* 字段：`aux_dept_id` / `aux_supplier_id` / `aux_material_id` / `aux_warehouse_id`，
  与 `biz_voucher_entry` 的四个 `aux_*` 列、`biz_account.aux_dimensions` 的四个码一一对应
  （**没有**项目 / 人员维度，系统只支持这四个）。
* **校验**：科目 `aux_dimensions` 里有的维度，提交时对应的 `aux_*` 必须给值，
  否则 `400` + `entries[i] 科目 5602.03（管理费用—办公费）要求部门辅助核算，请选择部门（aux_dept_id）`。
  多个维度同时缺会一次列全（不让人改一轮撞一轮）。维度码解析大小写不敏感、逗号分隔；
  认不出的维度**忽略 + WARN**（不能因为科目配了未来维度就把正常凭证拦住）。
* **只卡新增 / 修改**：校验挂在写入路径（`create_manual_voucher`），不扫描既有分录，
  存量历史凭证的 `aux_*` 仍可以是 NULL（§6 不回填）。
* 不要求某维度的科目**传了也不报错**（多挂一个维度不会算错账）；前端只提交「该科目真的要求」的维度。
* 辅助账只算 `POSTED`：手工凭证落 `DRAFT`，**审核 + 记账之后**才会出现在
  `GET /ledger/aux-balance?aux_type=DEPT`。
* 前端：建凭证弹窗 → 分录行选科目后，按该科目的 `aux_dimensions` **动态显示**部门 / 供应商 /
  物料 / 仓库下拉（必填），换科目会清空上一科目残留的维度；详情页分录表新增「辅助核算」列。

## 3. 部门从哪来（DC-11 的"先定部门从哪来"）

```
付款凭证的部门 = 申请人所属部门
  申请人 = biz_payment.create_by（付款登记人 = 登录操作人）
  部门   = sys_user.dept_id 的**当前值**，写进凭证后即快照
```

* **为什么是快照**：与 `biz_task.dept_id`（"创建时负责人所属部门"）同口径。人调部门后，
  历史凭证的部门辅助账不跟着漂移 —— 凭证是审计证据，不允许事后变。
* **为什么采购 / 收货凭证不写部门**：`biz_purchase_order` / `biz_stock_in` **没有 `dept_id`**，
  "谁买的"在系统里没有权威字段（DC-11 原文指向 DC-16）。按"操作人部门"猜一个值等于
  给财务一张错的部门辅助账，所以**故意留空**。要让采购也归部门，先给采购单加 `dept_id`
  （那是 DC-16 的项目/部门归集设计，另一张卡）。
* **为什么银行行不挂部门**：1002 银行存款是公司级账户，挂了会让同一个账户按部门拆成多份，
  银行对账直接对不上。
* 系统账号（AI / 集成账号）没有部门 → 留空 + `WARN`，不阻断付款入账（部门是增强维度，
  不是记账前提）。

## 4. 多物料单：按物料拆分录（P2-1b 决议：**拆**）

### 4.0 原口径（P2-1，已上线，本决议之前的做法）

采购入库 / 出库的存货借方曾是**一条汇总行**（`purchase_in_lines` 用单头金额出一行 1405），
而分录只有一组辅助列，于是只在 `material_count > 1` 时把 `aux_material_id` 留空 + `WARN`。
留空是「信息缺失但不误导」，可接受只是过渡；**本决议把它改为按物料拆分录**。

### 4.1 决议：拆（理由按会计口径排序）

1. **辅助账要能按物料对账**（卡面给定的唯一理由）。`GET /business/erp/ledger/aux-balance`
   的 `MATERIAL` 维度是目前财务按物料查存货发生额 / 余额的**唯一出口**。不拆 ⇒ 真实业务
   （一次到货多品种）里这张单的存货发生额全部落进 `unassigned`（`ledger.rs` 的
   `AuxUnassigned`，见 §5），物料辅助账在主要场景下不可用。
2. **盘点的先例**：`stocktake_model.rs:1233-1296` 早已按明细行 `line_no` 逐物料出行（盘盈 /
   盘亏成对拆）。同一系统里两条主链口径相反，本身就是缺陷。
3. **不拆会连带科目错**：存货科目解析是「规则 `debit_account_id` → **物料**
   `inventory_account_id` → 1405」（`voucher.rs:582` / `voucher.rs:510`），而多物料单只
   解析**首个物料**的科目。规则没配 `debit_account_id` 且两种物料各有存货科目时，整笔会挂到
   首个物料的科目上 —— 这是**记错科目**，比辅助维度缺失更严重。
4. **与设计原意一致，且修正了设计里的一处自相矛盾**：批次 1 技术架构给 `biz_voucher_rule_entry`
   定的 `group_level` 枚举就是 `LINE / MATERIAL / ACCOUNT / DOC`
   （`docs/ERP-批次1-技术架构.md:937`），采购入库的种子规则里借方是
   `MATERIAL_INVENTORY + LINE_AMOUNT_EXCL + 聚合粒度 ACCOUNT + 辅助核算「物料 + 仓库」`
   （同文件 `:968`）—— 「按科目聚合」与「辅助核算挂物料」本身就冲突（聚合后无法单值挂物料），
   这正是今天这条缺口的来源。本决议把借方粒度钉为 **MATERIAL**（设计枚举里现成的一档），
   属于**修正种子规则的自相矛盾**，不是推翻设计。该表从未迁移（库里不存在，实测
   `biz_voucher_rule` 只有 debit / credit / tax 三个固定科目列），实现是硬编码 builder。
5. **按物料合并在本系统已是既定口径**：`docs/ERP-批次1-技术架构.md:1478` 的加锁约定要求
   「明细按 `material_id` 升序排序」「同一物料出现多行**先合并为一行**（数量金额相加）」。
   凭证的物料粒度与之同口径，财务与库房才对得上同一套物料维度。

**被明确否掉的四个替代方案**

| 方案 | 判定 | 理由 |
|---|---|---|
| 把整笔挂到 `first_material` | ✗ 记错 | 物料成本报表虚增（P2-1 已改判过） |
| 维持留空（P2-1 现状） | ✗ 长期不可用 | 物料辅助账在主要场景下全是 unassigned（本决议否定） |
| 按**明细行**（`LINE`）拆 | ✗ 行数膨胀 | 设计自己点过这个反例：`docs/ERP-批次1-技术架构.md:980`「借方会按明细行拆成一堆同级分录（可读性差、余额明细膨胀）」。200 行收货单会变 202 行凭证，而辅助账只需要物料粒度 |
| 只拆存货贷方、成本 / 税金侧不拆 | ✗ 破坏配对 | 同一条业务不该一边逐行一边汇总；盘点先例是**对称**拆 |

### 4.2 拆的粒度与金额来源

* **粒度 = 去重物料（`MATERIAL`），不是明细行**：同一 `material_id` 的多行明细**先合并**
  （金额相加），合并后每个物料出一条分录。排序按 `material_id` 升序（与技术架构 §「加锁顺序」
  的既定约定同口径，`docs/ERP-批次1-技术架构.md:1478`），保证同一张单两次生成的行序一致。
  * 为什么不到行级：辅助账只按**物料**对账，行级多出的可读性代价（行数膨胀）换不到更多对账
    能力；**行级金额本来就在 `biz_stock_in_item` / `biz_stock_out_item` 上可查**，凭证摘要
    写「物料名（第 k–j 行）」即可双向追溯。
  * 盘点凭证是**逐差异行**成对的（`stocktake_model.rs:1233-1296`），本决议**不动**它：差异凭证
    要指明「哪一行差了多少」，行级是它的对账单位，两种粒度各自服务于自己的对账目标。
* **金额来源 = 明细行金额相加，单头金额不重算**：
  * 收货：`biz_stock_in_item.amount`（不含税行金额，2 位小数，`stockin_dto.rs:469` 的
    `money_of(qty × price_excl)`）按物料相加；
  * 出库：`biz_stock_out_item.amount`（过账时**账上真实扣减金额**，4 位，
    `stockout_handler.rs` 的 `set_item_cost(applied.amount)`）按物料相加；
  * 单头 `total_amount` 仍是权威值：`Σ(分录存货行金额) ≡ 单头金额`。
* **末行吸收尾差**：`diff = 单头金额 − Σ(全部明细行金额)`，把 `diff` 加到**按 `material_id`
  升序的最后一个物料**那行；若吸收后该行金额 ≤ 0（异常数据），往前退一个物料；所有行都吸收
  不了 ⇒ 差额单独出一行并 `WARN` + 报错（**不许静默吞掉差额**）。
* **为什么尾差通常恰好是 0**（不是巧合，是取数结构决定的）：
  * 收货单三金额是**由明细派生**的 —— `stockin_dto.rs:405-416` fold 出 `amount/tax/with_tax`，
    写库统一走 `Totals::of(&lines)`（`stockin_handler_write.rs:74/221/368`），采购单带出的
    明细（`stockin_handler_purchase.rs:288` `pull_lines_from_purchase`）也走同一 `prepare`；
  * 出库单头金额是**逐行 `applied.amount` 累加**出来的（`stockout_handler.rs:592-599`）。
  * ⇒ `diff ≠ 0` 只可能来自非 API 写入（历史数据 / 手工 SQL / 测试夹具）。这时 WARN 是**数据
    质量信号**，不是正常路径。
* **实测依据**（本机可读库，2026-11-04）：收货单 `Σ item.amount − 单头 total_amount` 超过
  0.0001 的单据 —— `cloudpivot_test` 13 张 / `hyg_shared` 3 张 / `gate_d_erp` 2 张 /
  `cp_libtest_erp` 2 张，**0 张不符**。尾差吸收算法用 SQL 复算（明细 333.33×3、单头 1000.00）
  ⇒ 末行 333.34、合计 1000.0000、尾差 0.0100 被正确吸收（复算 SQL 见实施卡验收项）。
* **金额全为 0 的物料不出分录**（与现状「0 元行略过」一致）。赠品行在凭证上不占金额，但它的
  数量仍在 `biz_inventory_txn`（库存流水）里，不在凭证上重复。

### 4.3 借 / 贷两边各出几行

设一张单有 `M` 种物料（去重 `material_id`，`M ≥ 1`）：

| 来源 | 借方 | 贷方 | `entry_count` |
|---|---|---|---|
| 采购入库（`STOCK_IN`，含税） | `1405` **× M**（每行 `aux_material_id` + `aux_warehouse_id`）<br>`2221.01` **× 1**（不挂物料） | `2202` **× 1**（`aux_supplier_id`） | `M + 2` |
| 采购入库（税率 0） | `1405` **× M** | `2202` **× 1** | `M + 1` |
| 出库 / 领用 / 报废（`STOCK_OUT`） | `6401` / `1901` **× M**（每行 `aux_material_id`，不挂仓库） | `1405` **× M**（`aux_material_id` + `aux_warehouse_id`） | `2M` |
| 盘盈盘亏（`STOCK_TAKE`） | 现状已按**差异行**成对（**本决议不动**） | 同左 | `2 × 差异行数` |

* **贷方应付仍按 `DOC` 一条**：应付是对**供应商**对账的，按物料拆只会把同一供应商的应付切碎，
  没有会计意义，还会让红冲 / 核销的对齐变复杂。设计与本决议一致
  （`docs/ERP-批次1-技术架构.md:970` 的种子规则：贷方 `DOC_AMOUNT_INCL` + 聚合粒度 `DOC`）。
* **进项税仍按 `DOC` 一条**：进项税按发票 / 税额合计抵扣，不按物料；一单多税率也不拆
  （税额合计即可抵扣）。同一处设计种子（`:969`）就是 `LINE_TAX` → `ACCOUNT` 聚合、无辅助核算。
* **借贷行数不等是允许的**：凭证的硬约束是 `Σ借 = Σ贷`（`post_voucher` 校验 + `erp_invariants`
  A1 / A2），不是「两边行数相等」。
* **单物料单是 `M = 1` 的特例** ⇒ 行数与今天**逐字相同**（收货 3 行 / 出库 2 行），这是把回归
  面压到最小的保证，也是 `d3_generate_voucher_e2e.py` 的 B3 / B6 断言（3 行）能原样保留的原因。

### 4.4 拆之后存货科目按「该行物料」解析

存货行的科目解析顺序不变，但**逐行解析**：规则 `debit_account_id` → **该行物料**的
`inventory_account_id` → `1405`。实现要点：一次性批量取本单 `distinct material_id` 的
`inventory_account_id`（一条 SQL），**不要**每行查一次。
规则配了 `debit_account_id` 时（实测 `cloudpivot_test` / `hyg_shared` / `gate_d_erp` 三个库
的活跃 `biz_voucher_rule` 均 100% 有值）逐行解析与现在完全等价 —— 差异只出现在「规则没配 +
物料各有科目」这一种情况，而那正是现在会记错科目的情况。

### 4.5 影响面清单（实施前必读）

| 位置 | 影响 | 动作 |
|---|---|---|
| `voucher.rs:714` `purchase_in_lines` | 从「单头金额出一行 1405」改为「按物料出行 + 末行吸收尾差」 | 改 |
| `voucher.rs:1035` `VoucherBizCtx` / `:1077` `of_stock_in` | 增加按物料聚合的行（空 = 回退汇总行，付款等其它 `biz_type` 不受影响） | 改 |
| `voucher.rs:2073` `create_for_stock_out` / `StockOutVoucher` | 携带「按物料聚合的行」`(material_id, Σqty, Σamount)` | 改 |
| `voucher.rs:1769` `create_for_stock_in` | 签名带上「按物料聚合的行」 | 改 |
| `stockin_query.rs:109` `voucher_biz_ctx` | 取「按 `material_id` 聚合的物料行」（`material_id, Σamount, 行号区间`）替代 `first_material` + `material_count` | 改 |
| `stockin_handler_post.rs:741-766` | `material_count` 只用于日志，判定「是否留空」的逻辑作废 | 改 |
| `stockout_handler.rs:609-627` | `distinct_materials` 探测与 WARN 删除，改为把 `items` 传给 `create_for_stock_out` | 改 |
| `stocktake_model.rs:1233-1296` | 已逐行，**不动** | 不改 |
| `voucher.rs:1288` `snapshot_of` / `:1349` `compare_snapshots` | `lines` / `entry_count` **本来逐行比对** ⇒ 结构不动 | 不改 |
| `voucher.rs:772` `reversed_lines` | 逐行复制（借贷互换、辅助四列原样）⇒ 红冲自然保留拆分行数 | 不改 |
| `aux_tests.rs:334 / 371 / 391` | 「多物料留空」的 3 条断言与新口径冲突 | 改写（多物料 ⇒ `M` 条存货行、每行带物料、合计 = 单头） |
| `scripts/verify/p21_aux_e2e.py:356-369` | ③「多物料：存货行只有一条 + 物料为空」必须改写为「存货行 2 条（甲 160 / 乙 150）、各带物料、仓库照写、合计 310 = 单头」；并**新增**「Σ 1405 分录 = 单头金额」「按物料辅助账 = 该物料成本」两条 | 改写 |
| `scripts/verify/d3_generate_voucher_e2e.py:365/409/443` | fixture 是单物料 ⇒ `M = 1`，3 行断言**原样通过** | 不改（建议新增一条多物料 SHADOW 一致性断言） |
| `scripts/verify/erp_invariants.py` A1/A2/A3/A4/A5/B2/B3 | 全是**聚合 / 计数**校验，拆分不改任何合计，`entry_count` 仍由 `lines.len()` 写 | 不改 |
| 前端凭证详情 / 打印 | 行数从 3 → `M + 2`，注意分页；无 schema 变化、**无迁移** | 只做展示自检 |

**两条架构约束（违反任一条，本次改动就会把 D3 弄红）**

1. 拆分必须实现在**共享 builder**（`purchase_in_lines` / `create_for_stock_out`）里。旧逻辑路径
   与配置路径（`flow/linkage/actions.rs:576` `commit_voucher`）都调 `voucher::create_by_rule`，
   所以「同改同变」，D3 SHADOW 不会因拆分产生 `lines` / `entry_count` 差异。谁要是在
   `stockin_handler_post.rs` 里另写一套拆分录，SHADOW 会立刻报差异 —— 这是护栏，不是巧合。
2. **不要**给 `erp_invariants` 加「`Σ 1405` 按物料 = `biz_inventory` 按物料」的**全库**断言：
   切换日前的多物料凭证永远物料为空（§6 不回填），全库断言必然红。覆盖度用
   `aux-balance` 的 `unassigned` 自证，物料级对账只对「切换日后新建单」成立。

### 4.6 `aux_qty`：本批**不写**（继续留空），并写死触发条件

* 写它的唯一实际好处是「数量金额式明细账 / 账簿打印的数量列」。
* **现在没有消费方**：`aux-balance` 只出金额（`ledger.rs` `AuxBalanceRes`），报表与账簿没有数量列；
  数量账的权威来源是 `biz_inventory_txn`（按物料 + 仓库记 `qty` 与金额）。凭证再抄一份 = **第二个
  真相源**，两边一旦不一致反而更难查。
* **而且库里没有「哪些科目要数量」的开关**：技术架构设想的 `biz_voucher_rule_entry.aux_qty` /
  `biz_account.aux_qty` 都没迁移（实测 `biz_account` **没有** `aux_qty` 列，`biz_voucher_rule_entry`
  整张表不存在），现在写等于对所有行一律瞎写。
* **结论**：本批留空。**触发条件**（三者同时成立才做）：出现「数量金额式明细账 / 含数量列的账簿
  打印」需求 + 科目主数据上有了「是否数量核算」开关 + `VoucherLineSnapshot` 补上 `aux_qty`
  （现在快照不含 `aux_qty`，写了 D3 也**比不出来**）。落地时的取数：收货 / 出库按该物料的
  `Σqty` 写到 `1405` 两侧；盘点要给 `VoucherPair` 加 `diff_qty`。

### 4.7 存量与切换口径（不回填）

* 切换日（实施上线时点）**之前**的多物料凭证永久保持「一条汇总行 + 物料为空」；之后新建单按物料出多行。
  历史凭证是审计证据，不回填（§6）。
  * **P2-1b 的切换日 = 2026-11-04**（代码落地当天）。注意「生产生效时点」取决于 prod 重启部署 ——
    重启前 prod 仍在跑旧逻辑，重启后新建单才按物料出行；判断某张单属于哪一边，看它
    `biz_voucher_entry.aux_material_id` 是否为空（老单恒空），不要按凭证日期猜。
* ⇒ 按月看物料辅助账时，切换月会出现「有值的物料 + `unassigned` 里的历史部分」并存。**这正是
  `unassigned` 字段存在的理由**：不要用物料辅助账去对历史期间的存货成本，历史期间用库存流水（§6）。
* 实施卡必须在真机验收里留一条**切换点证据**（上线后新建的多物料单：分录行数 = `M + 2`、逐行物料、
  合计与单头一致），并在完成说明里点名切换日期，供财务解释口径变化。
  证据：`scripts/verify/p21b_switch_point.py`（3 物料收货 = 5 行 / 2 物料出库 = 4 行 / 逐行物料 /
  `Σ1405 = 单头` / 该单 1405 行 `aux_material_id` 为空的行数 = 0），输出见
  `docs/evidence/20261104-p21b-material-split/switch-point.txt`。

## 5. 辅助账接口

```
GET /business/erp/ledger/aux-balance
      ?aux_type=MATERIAL|WAREHOUSE|DEPT|SUPPLIER   # 必填，大小写不敏感
      &aux_id=<uuid>                               # 可选：不给 = 该维度全部辅助项
      &period_from=2026-11&period_to=2026-11       # 可选：yyyy-MM 或 yyyy-MM-dd

权限码：business:erp:ledger:aux-balance（V0214）
```

返回（节选）：

```json
{
  "aux_type": "MATERIAL",
  "aux_id": "…", "aux_name": "冷轧钢板 1.5mm",
  "date_from": "2026-11-01", "date_to": "2026-11-30",
  "rows": [{
    "aux_id": "…", "aux_name": "冷轧钢板 1.5mm",
    "account_code": "1405", "account_name": "库存商品",
    "opening_debit": "100.0000", "opening_credit": "0.0000",
    "period_debit": "800.0000", "period_credit": "80.0000",
    "closing_debit": "820.0000", "closing_credit": "0.0000"
  }],
  "total_period_debit": "800.0000", "total_period_credit": "80.0000",
  "unassigned": { "entry_count": 3, "period_debit": "1210.0000", "period_credit": "0.0000" }
}
```

口径要点：

* 只算 `status='POSTED'` 且凭证未软删；
* 红字凭证金额恒为正、借贷互换 ⇒ 净额天然为负，不需要特判；
* `closing = opening + 本期借 − 本期贷`，再按正负拆借 / 贷两侧；
* 一个辅助项在**不同科目上各占一行**（1405 / 2202 / 6401），不做科目间抵销；
* **不做 `Σ借 = Σ贷` 断言**：按辅助维度筛出来的子集本来就不平衡（物料的 1405 借方
  对应供应商的 2202 贷方，不在同一辅助项下）。全公司口径的平衡由 `trial-balance` 负责。
* **`unassigned` 是自证字段**：本期该维度为空的发生额。只回"有值的那些"会让人以为
  查到的就是全部（多物料汇总行、银行行、不挂该维度的手工凭证）。
  P2-1b 拆分上线后，**新建**单据不再产生多物料 unassigned；切换日前的历史部分永久留在
  这里（§4.7），按期间查询时应把它当成「口径切换前的存量」。

## 6. 存量不回填

历史凭证的辅助列保持为空，**没有任何回填脚本**：凭证是审计证据，事后改历史凭证的
辅助维度等于篡改证据。历史期间要按物料看成本，用「库存流水」
（`biz_inventory_txn`）而不是凭证辅助账。P2-1b 拆分录（§4）同样**只对新建单据生效**，
切换口径与财务解释义务见 §4.7。
