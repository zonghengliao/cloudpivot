# ADR-20261031 · ERP 序列号「批量生成」的取号语义与账务口径

- 状态：**已拍板（总管 manong），待产品经理补业务话术、架构师补实施规格**
- 日期：2026-10-31
- 触发：`t_5d52a72e`（测试侧升级）／`t_d75b02e7`（prod 夹具事故取证）／
  `docs/datafix-20261031-p11-m-w1a-004-recur.md` §6 遗留 ②
- 涉及代码：`backend/src/business/erp/serial_model.rs`、`backend/src/business/erp/serial_handler.rs`、
  `backend/src/business/erp/stockin_model.rs`、`frontend/src/views/business/erp/serial/*`、
  `frontend/e2e/12-erp-serial-batch-walkthrough.spec.ts`
- 库里现状（HEAD）：`e1b5ce0`

---

## 1. 事实（代码 + 库，逐条可复核）

1. `serial_model::batch_generate()`（`serial_model.rs:750`）一次调用做三件事：
   ① `take_numbers` 取号 → ② 一条 `INSERT … SELECT` 把号落库，**状态写死 `'IN_STOCK'`、`inbound_at = now()`**
   （`:781`）→ ③ `link_inventory()`（`:715`）先 `recompute_quantity()` 再把 `qty` 记成一条 `SERIAL_IN/SERIAL_BATCH` 流水。
2. `recompute_quantity()`（`:592`）把 `biz_inventory.qty` 重算成「该 物料×仓库 下 `status='IN_STOCK'` 的件数」，
   落进 **`location_id IS NULL` 的仓级汇总桶**，并写 `amount = qty × avg_cost`；
   行是新建时 `avg_cost` 插入为 **0** ⇒ 合法操作一次就得到 `qty>0 / amount=0 / avg_cost=0`。
3. `batch_generate()` **一行库位账（`biz_inventory_location`）都不写**。
4. 不变式套件（`scripts/verify/erp_invariants.py`）：
   - **B1（BLOCKER，:190）**`SUM(库位账 qty) = 成本账 qty`（按 物料×仓库 全外连）⇒ 事实 3 必然打红；
   - **C3（WARN，:289）**`qty > 0 且 avg_cost = 0` ⇒ 事实 2 必然打红。
   ⇒ **这两个红不需要任何脚本、任何越权：任何合法用户在生产上点一次「批量生成」就能造出来。**
5. 对照的「正确路径」早就存在：收货入库 `stockin_model` 过账时**同事务两笔账**
   （`apply_cost` 成本账 + `apply_location_qty` 库位账，`:1059` / `:1176`，均 `pub` 可复用），
   成本走移动加权（`new_amount = old_amount + in_amount`，`new_avg = round(new_amount/new_qty, 6)`）。
   它**故意不调用** `batch_generate`（`stockin_model.rs:51-70`：调了会双写流水、且 `recompute_quantity`
   会把过账算出的金额改写成 `qty×avg_cost` 而静默抹账），而是复用取号原语自己落号（回填 `source_type/source_id`）。
6. 夹具残留判据（`scripts/datafix/p10/20_residue_report.up.sql`）第 ④ 道是「库存零价值」——
   **有价值 = 有账载来源，一律不碰**。所以本 ADR 落地后，「批量生成」产生的库存**不再被判为夹具**。

---

## 2. 候选方案与评估

| 方案 | 内容 | 评估 |
|---|---|---|
| ① 必须录成本 | 请求体加必填 `unit_cost`，同事务写成本账 + 流水带单价/金额 | **缺一半**：只录成本仍不写库位账 ⇒ B1 照红。必须打包「写两本账」 |
| ② 取号≠在库 | 生成号落 `GENERATED`，收货/入库才 `IN_STOCK` | 语义最干净，但 `serial_model` 现在只有 `IN_STOCK/OUTBOUND/SCRAPPED` 三态（`:117-124`），要加状态 + 状态机 + 入库单「引用已生成号段」的新能力 + 前端两套列表；**改动大、会打断客户当前正在演示的路径**（点完批量生成看到库存为 0 会被当成 bug） |
| ③ 只在隔离环境暴露入口 | 前端隐藏按钮 | 等于对客户关闭功能；客户 11:34 的走查就在用这个按钮，不可接受 |

**补充事实（决定权重）**：目前库里 `biz_stock_in`（收货单）是唯一会正确写两本账 + 成本的入口，
而它自己生成号；「批量生成」在业务上实际承担的是 **期初 / 无来源入库（打码、建账）** 这个角色，
而不是「先打码后收货」。所以把它按「无来源入库」补齐账务（录成本 + 写两本账），是最小改动且业务语义自洽的路径。

---

## 3. 拍板（decision）

**主口径 = ①′：批量生成改判为「无来源入库（期初/其他入库）」，必须录成本，并同事务写两本账。**

1. **请求体新增必填字段** `unit_cost`（不含税单价，`> 0`，`numeric(18,6)`），缺失或 ≤ 0 ⇒ `400`，
   报文「批量生成即入库，必须录成本单价（期初/其他入库口径）」。
2. **同事务写两本账**：
   - 成本账：走 `stockin_model::apply_cost`（移动加权，加法口径，`new_avg` 在 SQL 里 `round(…,6)`，
     禁止 Rust 侧自己舍入）——**不要**再调 `recompute_quantity` 加第二次数量（会双计）；
   - 库位账：走 `stockin_model::apply_location_qty`（有 `location_id` 按库位行，无则落 `NULL` 仓级汇总桶，
     锁顺序与 `apply_cost` 一致：`biz_material.id` 升序）。
3. **不变式①仍须成立**：`biz_inventory.qty == count(IN_STOCK 号)`。落库后要断言（单测 + 巡检），
   允许实现方式为「先插号再按件数记账」，但**只允许一处加数量**。
4. **流水**：保留 `SERIAL_IN / SERIAL_BATCH` 形态（残留判据第 ① 道依赖它），但必须带上
   `unit_cost` 与 `amount`（当前写 0）；`remark` 保留「批量生成 N 件（首号）」并追加单价。
5. **语义定位写进对客文档**：批量生成 = 无来源入库，成本由操作人负责，属于会计上的「其他入库/期初」，
   需在 remark/来源字段留业务依据（不新增审批流，P0 不做）。
6. **② 记入需求池（P2，不实现）**：若客户业务确为「先打码后收货」，再立项做 `GENERATED` 态 +
   入库单引用已生成号段；届时本 ADR 第 3 条的账务口径直接复用。
7. **否决 ③**：功能对客户关闭，与客户正在进行的验收冲突。

### 影响面（必须同步改的地方）

| 位置 | 改动 |
|---|---|
| `POST /business/erp/serial/batch-generate` 契约 | 新增必填 `unit_cost`；**breaking**，须进 release note |
| 前端「批量生成」表单 | 增加「成本单价（必填）」+ 本次入库金额预览；错误文案 |
| `frontend/e2e/12-erp-serial-batch-walkthrough.spec.ts` | 断言从「生成 → 零成本 → B1/C3 红」改为「生成 → 有成本 → 两本账相符 → B1/C3 绿」 |
| `scripts/verify/erp_invariants.py` | 不改判据（B1/C3 是对的），但 e2e 的期望值改 |
| `scripts/datafix/p10/20_residue_report.up.sql` | 不改判据：落地后「批量生成」不再产生零价值夹具（第 ④ 道自然不命中）；**但要在文档里写明**：此后走查点出来的库存是**真实入库**，撤销要走红冲/盘点，不是跑 P10 就能清 |
| 存量数据 | **不补账**（增量修复，与 P6 同口径）；已软删的号段不回拨（P10 口径） |
| 迁移 | 预期 **0 迁移**（`unit_cost/amount` 列在 `biz_inventory_txn` 已存在）；若实现需要新枚举值再单开 |

### 验收判据（后端卡/前端卡/测试卡共同遵守）

1. 隔离实例上「物料须一物一码 + 指定仓库 + 带单价」生成 N 件后：`biz_inventory.qty = N`、
   `amount = N × unit_cost`、`avg_cost = unit_cost`、库位账合计 = N ⇒ `erp_invariants.py` **32/32，B1/C3 全绿**。
2. 缺 `unit_cost` ⇒ 400，且**不落任何数据**（无号、无流水、无账）。
3. 同一物料二次生成（不同单价）⇒ 移动加权正确：`new_avg = round((old_amount + N×cost) / (old_qty + N), 6)`。
4. 既有 `W2b/W2c` 链路（收货入库、出库、报废、调拨）单测全绿，不变式①不破。
5. 前端：不填单价不能提交；提交成功后页面显示的库存数量/金额与库一致。

---

## 4. 与「E2E 不许打 prod」的关系（两件事，别混）

仓库侧已把自动化挡住（`frontend/e2e/target-guard.mjs` 加载期抛错 + 隔离实例脚本 + 跑完必查）。
本 ADR 修的是**人点按钮**这条路：修完后业务方在 prod 点「批量生成」不再污染账务的唯一硬伤
（B1/C3），但那笔库存就是**真实入库**——要撤得走业务单据。所以「业务方在 prod 走查」这件事
仍需业务侧确认（走查前后跑 `bash scripts/e2e-postrun-check.sh`，见对客验收手册条款）。

---

## 5. 未决（不在本 ADR 内）

- `henry.liao`（SUPER_ADMIN，2026-10-28 由 admin 建）是客户还是内部业务同学：需老大确认。
- prod 遗留 `TK-20261031-000007`（PENDING，admin 申请，3 件 M-W1A-004，会签 4 人：
  admin/henry.liao 已同意、`laoliu`/`laoliu_test` 待办）：撤销还是继续审批，需业务确认。

---

## 6. 总管决策补充（2026-10-31 15:1x，manong）— 规格 v3 §11 的 R1 / R2 已批准

架构卡 `t_1daca7f4` 交规格 v3（bare main `2317418`）时，单列了两处**比本 ADR 字面更大**的口径需总管点头。
现批准，作为本 ADR 的执行补充（方向不变，只是把「不做就必然红」的部分补齐）：

- **R1（批准）出库 / 报废侧同样生成凭证**：生成 借 1405 / 贷 1901；出库 借 6401 主营业务成本 / 贷 1405；
  报废 借 1901 / 贷 1405。理由（实测）：只补生成侧凭证时，出库会让 `biz_inventory.amount` 减少而 1405 不动
  ⇒ `scripts/verify/erp_invariants.py` 的 **B2（BLOCKER，`:201-212`）反向变红**，验收判据 ①「32/32、B1/C3 全绿」不可能成立。
  **金额一律取账上真实差额**（不在 Rust 侧重算），差额 ≤ 0 跳过（`post_voucher` 在 `debit<=0` 会报错）。
- **R2（批准）科目取 `1901` 待处理财产损溢 + `6401` 主营业务成本**：生成按「存货盘盈」口径贷 1901，出库结转借 6401。
  种子走 **`V0160`**（`V0159` 已被 P10 的 `V0159__biz_status_log`（`07400af`）占用；动工前按「现取最大号 + 1」复核）。
  口径变更成本低：科目映射集中，若客户财务要求换科目，只改映射常量 + 一条种子迁移，事务步骤不动。

**R3（调拨结算不写库位账 ⇒ B1 必红）不在本 ADR 范围**：架构卡已单开 `t_270b472e`（backend-developer）收口；
后端卡验收库判据加一条「副本库 `biz_inventory_txn` 无 `STOCK_TRANSFER` 流水」。

**只等业务侧（非技术）确认的三件**不改本 ADR 口径：`henry.liao` 身份与 prod 走查方式、prod 测试超管号是否摘权、
`TK-20261031-000007` 撤销或继续审批 —— 登记在 `docs/待决策.md`（产品卡 `t_064fa4fd`）；
**未获书面确认前 prod 数据与账号一律不动**。
