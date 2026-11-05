# ERP 库位策略（ARCH-3 §5 DC-22 · P2-4）

> 卡：`t_72b6a4d4`【P2-4】库位做完整（收货永远进「未分配」桶，库位管理等于没有）
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-22**（P2）
> 迁移：`backend/migrations/business/V0218__erp_p2_4_stock_in_item_location_dict.{up,down}.sql`
> 代码：`backend/src/business/erp/stockin_location.rs`（库位解析原语）、
> `stockin_handler_common.rs`（建单校验 + 详情提示）、`stockin_handler_post.rs`（过账落位）、
> `stockout_model.rs` / `stockout_handler.rs`（出库 FIFO）

## 0. 一句话结论

| 动作 | 库位从哪来 | 不指定会怎样 |
|---|---|---|
| 收货 | 用户在明细里选 `location_dict_id`（库位字典）；建单只存**意图** | 用该仓默认库位（`biz_location.is_default = 1`）；没有默认 → 进「未分配」桶 + 详情 `location_warnings` 提示 |
| 出库 | 指定库位账行 → 精确扣该桶 | **FIFO 自动分配**（P1-FIX-2）：建单即按库位账行建账时间升序预填，跨库位够量时自动拆成多行；手填库位可覆盖。库存真不够 → 400/409 点名「差多少 + 现存哪些库位 + 怎么办」 |
| 调拨 | （既有）`from_location_id` / `to_location_id` 已强制成对 | 本卡不动 |

## 1. 两个 id 的分工（最容易搞混的一处）

```text
biz_stock_in_item.location_dict_id  = biz_location.id           ← 用户选的库位（P2-4 新增，可为空）
biz_stock_in_item.location_id       = biz_inventory_location.id ← 库位账行（过账时 find-or-create 后回写）
biz_inventory_location.location_id  = biz_location.id           ← 库位账行自己的库位列
biz_inventory.location_id           = biz_inventory_location.id ← 成本账按库位账行分桶（V0139 口径）
biz_material_serial.location_id     = biz_inventory_location.id ← 同上
```

为什么必须分两列：

- `biz_inventory_location` 是 `(物料, 仓库, 库位)` 三元组的**库存账**，只在过账后存在。
  如果收货明细只能填它，用户在表单上就**没法选一个还没建账的库位** —— 这正是改之前
  「前端 0 处 location 字段 / 收货明细 0/1 填了库位 / 库位账 7/11 行是未分配桶」的根因。
- 下游（成本账 / 出库 / 调拨 / 序列号）三处 `location_id` 的既有语义**一个字节都没动**，
  过账时把「解析出来的库位账行 id」回写到明细，链路保持一致。

> 兼容：老客户端仍然可以只传 `location_id`（库位账行 id，P6 口径），服务端**原样保留**，
> 不改写成库位字典（把它改写成「(物料, 字典)」会让「拿别的物料的库位账行来收货」这种
> 调用方 bug 在过账前悄悄自愈掉，掩盖问题）。两种口径都给且指向不同库位 → 400
> 「指向不同的库位」（不许「用户选 A、账落 B」）。

## 2. 收货：建单与过账各做什么

| 阶段 | 做什么 | 为什么 |
|---|---|---|
| 建单 / 改单 | 校验库位：存在、未软删、`status = 1`、`warehouse_id` 与本单仓库一致；不传则解析默认库位并**存下** | 跨仓库/停用/已删的库位必须在门口拒；否则过账动了一半账才发现 |
| 过账 | 按 `(物料, 仓库, location_dict_id)` find-or-create 库位账行 → 回写 `location_id` → 成本账 + 库位账 + 流水都按它落 | 草稿建空账行会把「库位库存」Tab 撑出 0 数量空行；过账才建既干净又不丢用户意图 |

过账后的动作（作废 / 红冲）按明细上的 `location_id` 原路退回，与库位选择一致。

## 3. 出库：FIFO 自动分配 + 手动指定并存（P1-FIX-2 改口径）

```text
① 建单 / 改单：明细没填库位 → 按 FIFO 预填
   - 够量  → 按 created_at ASC, id ASC 逐桶贪心取，**一条明细自动拆成 N 行**（每行一个库位账行）
   - 不够量 → 保持「未指定库位」单行（草稿可存），详情 stock_warnings 给缺口 + 库位分布
② 明细填了库位 → 原样尊重（覆盖自动值），只扣那一桶
③ 过账：明细已有库位 → 直接扣；仍为空（老单据 / 手清库位）→ 单个够量的 FIFO 桶兜底
④ 库存真不够 → 409 点名「第几行、要出多少、该仓可用合计、差多少、现有分布、三条处理办法」
```

**为什么「自动拆行」而不是「一行跨桶扣减」**：`biz_stock_out_item` 一行只挂一个
`location_id`，过账是「一行明细 ↔ 一笔库存流水 ↔ 一个成本账桶」；一行跨桶扣会打断这个
对应关系（流水没有 location 列），作废原路退回也会失去依据。拆成 N 行后每行仍然一对一，
下游（过账 / 作废 / 红冲 / 行号）不需要知道「这一行是系统拆出来的」。

**这是对旧口径的修正**：改之前「没指定库位」只挑**单个够量**的库位，都不够量就 409
「请把这一行拆成多行并分别指定库位」—— 货明明够（分散在 2 个库位），用户却得手工拆行
（评审实测 `05_chains_run3.txt` ③ 段的 3 分主因）。

**同一张单多行同一物料不会重复分配**：建单期按 `(物料, 库位账行)` 记「已预占量」，
后一行只看到剩下的量；用户手填库位的那一行也登记占用量。

**FIFO 的排序键是 `biz_inventory_location.created_at`**，即「该库位第一次收到这个物料的时间」
（并列按行 id）。这是当前库位账能做出的最近似口径：逐笔入库时间没有落在库位账上
（`biz_inventory_txn` 没有 location 列），真正的**批次库龄** FIFO 要等批次/库龄卡，这里不假装它是。

出库过账会把 FIFO 选中的库位账行**回写到明细**（`biz_stock_out_item.location_id`），
所以作废时按同一行原路退回 —— 不会退进「未分配」桶把账又搞歪。

一个必须写下来的实现细节：**「未分配」桶在成本账里是 `location_id IS NULL`，不是它的账行 id**
（`biz_inventory` 只有「具体库位」才用账行 id）。所以 FIFO 命中 NULL 桶时，回给成本账 / 库位账
的 key 必须是 `None`，否则会去 `location_id = <账行 id>` 找一行不存在的成本账
（DC-13 的负库存 / 全链用例会当场 409）。「未分配」桶按普通桶参与 FIFO 排序，不是特例。

## 4. 容量（`biz_location.capacity`）：做预警，不拦截

卡面 §三 给了二选一（预警 / 从 UI 移除并注明预留），本卡选**预警**：

- 详情接口现算：`SUM(该库位所有物料 qty) + 本单待过账数量 > capacity` → 写进 `location_warnings`；
- 草稿态才把本单数量算进来，已过账单据数量已在 `used` 里，**不会重复计一遍**；
- **只提示不拦截**（过账照常成功）——原因：`capacity` 的单位是「同基础单位数量」，
  同一个库位混放不同计量单位的物料时这个数字是粗粒度的，拿它硬拦会误伤；
- 前端：表单保存后 toast 提醒、详情抽屉告警条逐条列出。

## 5. 「未分配」桶（不删、不洗历史）

- **是什么**：`biz_inventory_location.location_id IS NULL` 的那一行（每物料每仓最多一行），
  代表「这批货没指定库位」。它是**必要的历史形态**（老单据、期初导入都没有库位），
  不是设计缺陷；本卡只保证**新收货不再默认全进这里**。
- **不动历史数据**：迁移只回填 `location_dict_id` 这一列元数据（老明细从库位账行反查库位字典），
  库存数量 / 未分配桶的存量一分不动。
- **可见性**：只要有行仍会进未分配桶（仓库没有默认库位），详情就给出
  `location_warnings`：「第 N 行未指定库位，仓库 X 也没有默认库位：过账后会记进未分配库位桶…」

## 6. 验收对照（卡面 §四）

| 验收项 | 证据 |
|---|---|
| 收货可指定库位，库位账写到对应库位 | `stockin_handler_tests_location_dict.rs::p24_receipt_with_dict_location_writes_that_location` |
| 不指定 → 自动用默认库位 | `p24_receipt_without_location_uses_default` |
| 无默认库位 → 进未分配 + 提示 | `p24_receipt_without_default_falls_back_to_unassigned_with_warning` |
| 跨仓库 / 停用 / 已删库位被拒 | `p24_cross_warehouse_location_is_rejected`、`p24_disabled_and_deleted_location_are_rejected` |
| 两个口径打架被拒 | `p24_conflicting_dict_and_ledger_row_rejected` |
| 改单可换库位 | `p24_update_draft_can_move_location` |
| 容量预警（不拦截） | `p24_capacity_overflow_is_warning_only` |
| 出库指定 / FIFO 扣减正确 | `p24_stockout_with_location_deducts_that_one`、`p24_stockout_without_location_deducts_fifo` |
| 出库跨库位自动拆行、建单预填、手改覆盖、缺口文案（P1-FIX-2） | `stockout_fifo_tests.rs`（14 条：`fifo_allocate_*` 纯函数 5 条 + 真库 9 条） |
