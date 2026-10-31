# 云枢 ERP 第 2 批 — 技术架构设计

> **批次**：ERP 批次 2 · 销售 / 出库 / 应收 / 收款 / 客户信用
> **卡号**：P3-D2（重派 `t_70b2338e`；原始设计卡 `t_7384c8f3`，本轮在原稿上复核并补齐落盘）
> **前置**：批次 1 已交付（42 张 `biz_` 表 / 100 条已应用迁移〔`cloudpivot_p2e2`〕/ 1278 后端测试）
> **落盘状态**：本轮已实体落盘到仓库 `docs/ERP-批次2-技术架构.md`（`wc -l` 与验收命令见 §9.6、§9.7）
> **本文档**：`docs/ERP-批次2-技术架构.md`
> **参考**：`docs/ERP-批次1-技术架构.md`（2895 行，风格/密度对齐）、`docs/ERP-批次2-产品需求PRD.md`（3160 行，**业务规则以它为准**）
> **勘误**：本仓库中 `docs/ERP-仓库管理-技术架构.md` **不存在**（W0 设计稿未入库），仓储域一律以批次 1 的 V0137–V0140 迁移与 `\d` 实测为准（见 §1.3）；不要引用那份不存在的设计稿。
> **性质**：**只做设计，不写业务代码**。§2 的 DDL、§9.1 的 POC 输出均为**真库实际执行**结果，不是纸面推演。

---

## 目录

| 章 | 内容 | 关键产出 |
|----|------|---------|
| §0 | 决策摘要 | 十条硬结论 / 六处与任务书的偏差 / 交付边界 / **三条最易错口径速查（§0.4）** |
| §1 | 现状核查 | 迁移号实测 / 42 张表实测 / 9 张关键表 `\d` 原文 / 三处实测纠偏 |
| §2 | 数据模型 | **13 张表完整 DDL**（CHECK / 索引 / 注释）+ 9 条迁移 V0160–V0168 |
| §3 | 核心引擎 | 出库成本 / 信用占用 / 核销 / 序列号 / 凭证复用 / 事务边界 / 状态机 / **§3.12 七条硬结论方案对比总表** |
| §4 | API 契约 | 41 个接口（路径 / 请求体 / 响应 / 错误码 / 权限码） |
| §5 | 前端架构 | 页面与路由 / 扫码方案 / 金额契约 / 信用提示 |
| §6 | 测试策略 | 单测估算 / 真库集成场景 / e2e 脚本 / 验收标准 |
| §7 | 实施计划 | **8 步交付（E1–E8，≈24.5 人日）** / 里程碑检查点 |
| §8 | 风险清单 | 会计正确性 / 并发 / 与批次 1 的分歧点 / 易错清单 / 工期 |
| §9 | 附录 | POC 真实输出 / ADR / 待确认问题 / 自检对照 |

---

## 0. 决策摘要（先看这一页）

全部结论都基于**对生产库与批次 1 源码的实测**（证据见 §1、§9.1），不是通用 ERP 教科书。

**本设计已通过两类真实验证**（详见 §9.1，输出原样粘贴）：

1. **DDL 真库验证**：本文 §2 的 13 张表**原样提取**后在 PostgreSQL 18.6 执行（库 `erp_p3d2_poc1/2/3`，从批次 1 生产 schema 克隆），一次通过；
2. **业务场景验证**：10 组场景（成本结转 / 尾差倒挤 / 负库存 / 库位拆分 / 超卖 / 信用并发 / 核销超额 / 序列号 CAS / 状态机 / 真两会话并发）全部按设计拦住或放行，过程中**发现并修正 3 个真实缺陷**（§1.8）。

### 0.1 十条硬结论

| # | 决策点 | 结论 | 一句话理由 |
|---|--------|------|-----------|
| **D1** | 出库成本结转的并发安全 | **条件扣减**：`UPDATE … SET qty=qty-?, amount=amount-ROUND(?*avg_cost,4) WHERE id=? AND qty>=?` + `RETURNING`，0 行 = 库存不足 | 与批次 1 调拨（`transfer_model::deduct_inventory_row`）**同一段实现**；入库保留 `FOR UPDATE`（要读-算-写三步）——**入用悲观、出用条件，边界由"是否重算均价"划分** |
| **D2** | 负库存 | 用**已存在**的 `biz_warehouse.allow_negative`（不加表不加列）；允许时成本取**该物料该仓最近一条 `IN` 流水的 `unit_cost`**，绝不取 0；后续入库用移动加权**自然冲平**，不追补历史凭证 | 均价 0 会让出库成本为 0、毛利虚高，且把正金额摊到负数量上污染成本链；追补历史 = 悄悄改已生成的成本凭证，会计上不可接受 |
| **D3** | 出库即生成应收 + 凭证 | **两张凭证**（收入 3 行 / 成本 2 行），**同一事务**；`source_type` 用 `STOCK_OUT` / `STOCK_OUT_COST` 区分；出库单挂 `voucher_id` + `cost_voucher_id` 两列 | 批次 1 的 `create_auto` 只支持"一规则一模板"（`purchase_in_lines` 内联在 866–877 行）；混成 5 行一张要改引擎 = 违反"复用不重写"；且红冲粒度、利润表行次都要求分开 |
| **D4** | 信用额度的并发占用 | **下单提交时占用**（`order_used`）+ **出库时平移**（`order_used→ar_used`，总额不变）+ **核销时释放**（`ar_used`）；占用用**单语句条件 UPDATE**，超限返回 0 行 → 409 | 单语句无"读-判-写"窗口，锁持有时间最短；平移设计让出库/收款**不需要再判额度**（总额不变），把并发判断收敛到唯一一处 |
| **D5** | 核销的原子性 | 单事务 + 按 `receivable_id` **升序**逐张条件 UPDATE（`AND unreceived_amount>=?`）+ 明细表 `before/after` 快照 CHECK 自证 + 表级 `write_off_total<=amount` | 三步任一失败整笔回滚；`ck_biz_receipt_item_after`（`after = before - amount`）让"金额对不上"在 DB 层就插不进去，不靠应用层自觉 |
| **D6** | 序列号出库 | **状态 CAS**：`UPDATE … SET status='OUTBOUND' WHERE id=ANY(...) AND status='IN_STOCK'`，`RETURNING` 行数 ≠ 期望数即为冲突；明细数量必须 == 序列号个数；`location_id` 指向 **`biz_inventory_location.id`**（不是 `biz_location.id`） | 状态机不可逆（`IN_STOCK→OUTBOUND→SCRAPPED`），CAS 是唯一能挡住"两单抢同一个码"的写法；库位语义沿用批次 1 既定口径（§1.8 ①） |
| **D7** | 与批次 1 库存表共存 | `biz_inventory` 是**唯一成本账**；`biz_inventory_location` 是**库位数量账**；出库以"该物料该仓是否存在库位行"为库位管理开关，存在则必须落库位（可自动分配），不存在则只扣整仓汇总桶（`location_id IS NULL`） | 唯一键 `(material_id, warehouse_id, COALESCE(location_id, 零 uuid))` 决定"一行 = 一个成本桶"；同时动桶与库位行会让巡检不变式 I2（Σ库位 = 仓级）当场判假（§3.3） |
| **D8** | 迁移号段 | 批次 2 占 **V0160–V0168**（9 条）；批次 1 补丁只剩 **V0157–V0159** | 实测仓库文件最大 **V0156**、库最大 150（`p2e1`）/151（`p2e2`）；从 160 起与任何在途补丁都不冲突 |
| **D9** | 凭证引擎复用 | **不重写**：把 `create_auto` 内联的分录构造抽成 `lines_for(biz_type, …)` 模板分派（+3 个纯函数），新增 `create_auto_with(ex, req, lines)`；`create_auto` **签名不变**、`PURCHASE_IN` 行为逐字不变 | 批次 1 已有 1278 个测试钉住 `create_auto`；抽模板是纯内部重构，新增能力而不改既有契约 |
| **D10** | 命名与金额口径 | 严格沿用：表头 `total_amount/total_tax/total_with_tax`、明细 `price/price_incl/amount/tax_amount/with_tax_amount`、数量 `qty`、流水 `txn_type IN('IN','OUT')` | 实测 `biz_stock_in_item` 的单价列是 **`price`**（不是任务书写的 `price_excl`）——已因猜字段名错过 6 次，本次逐列取自 `\d` |

**一处有意偏离命名惯例并在此声明**：应收的"已收/未收"列命名为 `received_amount` / `unreceived_amount`，而不是镜像 `biz_payable` 的 `paid_amount` / `unpaid_amount`。理由：`paid` 在中文语境读作"已付"，放在应收表里会被一眼读反；语义镜像用 `received` 更安全。全表其余列名与批次 1 逐字一致。

### 0.2 与任务书假设的六处偏差（必须先对齐）

| # | 任务书假设 | 实测事实（证据） | 处理 |
|---|-----------|-----------------|------|
| 1 | 明细列名 `price_excl` / `price_incl` | `\d biz_stock_in_item`：单价列是 **`price`** 与 `price_incl`，没有 `price_excl`（§1.3） | 本批明细一律 `price` / `price_incl`（D10） |
| 2 | 迁移号"生产已到 V0151" | 本轮复核：`cloudpivot_p2e1` = `150/99`，`cloudpivot_p2e2` = **`151/100`**（V0151 已应用）；仓库文件已到 **V0156** | 不影响排期：从 V0160 起最安全；补丁空号只剩 V0157–V0159（§1.1） |
| 3 | `biz_customer` 若批次 1 没建才建 | 42 张 `biz_` 表里**没有** `biz_customer`（§1.2 全清单） | 本批 V0160 新建 |
| 4 | 权限码迁移 V0160 / 科目 V0161 / 规则 V0162 | 任务书这三行是**按"三类种子"列举**，但依赖顺序不允许：权限码挂在 `sys_menu` 上，必须先有表（客户/订单/出库/应收/收款/对账）才能建菜单；且权限码在 `sys_menu.auth_code`（**本仓无 `sys_permission` 表**），ID 段 1009/1010 已被 V0151 占 | 实际排布：表 V0160–V0164 → **权限码 V0165**（ID 1011 起）→ **科目 V0166** → **凭证规则 V0167** → 序列 V0168。三类种子的**内容**一条不少，只是号≠任务书示例号；硬红线"从 V0160 起"仍满足（§2.7） |
| 5 | 科目只需补 1122 / 6001 / 2221.02 / 6401 | 这 4 个实测确实缺（库里只有 1405 / 2202 / 2221 / 2221.01）；但**收款凭证还需要 1002 银行存款**，任务书没列 | V0166 补 **5 个**科目，多出的 1002 在文档中标注来源 |
| 6 | 表清单只列了 `biz_customer_credit` | 信用额度需要"占用/释放"可审计，单表无法自证"这 8 万是谁占的、什么时候释放的" | 拆为 `biz_customer_credit`（当前值，热点行）+ `biz_customer_credit_txn`（追加写流水，`idem_key` 幂等） |

### 0.3 本批交付边界（明确不做）

- ❌ 销售发票（开票 / 红字发票 / 税控对接）——只存 `invoice_no` / `invoice_date` 占位
- ❌ 销售退货单（`out_type='RETURN'` 枚举已留，流程与凭证模板留批次 3）
- ❌ 委托代销 / 分期收款 / 多币种汇率换算（只存 `currency_code`，汇率表批次 3）
- ❌ 客户对账单的客户侧在线确认（只做内部生成 + 确认状态，不做门户）
- ❌ 信用保险 / 客户评级模型（只做"额度 + 观察/冻结"三态）
- ❌ 拣货波次 / 装箱 / 物流面单（扫码只到"序列号/物料码"粒度）
- ❌ 期末结转销售成本（批次 4 结账；本批是**逐单实时结转**）

### 0.4 三条最容易搞错的口径（开发必读，一页速查）

本章是给开发看的**硬约束速查**：三条口径只要错一条，数据就是错的，且不会报错——只会算错钱。全部数字都来自本轮重派时的现场实测（命令与输出见 §9.7）。

#### 口径 ①：税率有**两套单位**，按"表在哪一层"决定用哪套

| 场景 | 在哪张表 / 哪个字段 | 单位 | 取值示例（DB 实测） | 校验出处 |
|------|---------------------|------|----------------------|----------|
| **物料主数据**（建档、选料带默认值） | `biz_material.tax_rate` | **小数 0–1** | `select distinct tax_rate from biz_material` → **`0.1300`** | `material/dto.rs` 限 `0 ≤ rate ≤ 1` |
| **单据明细**（采购收货行 / 销售订单行 / 销售出库行） | `biz_stock_in_item.tax_rate`、`biz_sale_order_item.tax_rate`、`biz_stock_out_item.tax_rate` | **百分数 0–100** | `select distinct tax_rate from biz_stock_in_item` → **`13.0000`** | `stockin_dto.rs:411` 限 `0 ≤ rate ≤ 100`，税额 = `amount × rate / 100` |

**两条命令就是证据（本轮真跑）**：

```sql
-- 物料主数据：0–1
select distinct tax_rate from biz_material;      -- 0.1300
-- 单据明细：0–100
select distinct tax_rate from biz_stock_in_item; -- 13.0000
```

**开发必须照做的三条**：

1. **入库/出库/订单明细一律用百分数**（`13.0000` 表示 13%）。本批 `biz_sale_order_item.tax_rate` 与 `biz_stock_out_item.tax_rate` 都是 `NUMERIC(7,4)` + `CHECK (tax_rate >= 0 AND tax_rate <= 100)`。
2. **从物料主数据带默认税率时必须 ×100**：`0.13 → 13.0000`；前端在 `applyMaterial` 里做（§5.3），后端在"选料"接口里做（§4.1）。**忘了 ×100 就少算 100 倍税**（13% → 0.13%）。
3. **税额公式**：`tax_amount = ROUND(amount × tax_rate / 100, 4)`；`with_tax_amount = amount + tax_amount`；**绝不允许**用 `amount × tax_rate`（那是 0–1 口径的算法）。

> 为什么不在本批统一成一套：批次 1 已上线，`biz_material.tax_rate` 与采购订单行是 0–1、收货明细是 0–100，改任一侧都要动批次 1 代码与历史数据（红线：只写文档不改代码）。本批选择**与"库存单据明细"家族对齐（百分数）**，把不一致留给 §8.5 的 **P2** 补丁（统一税率口径，建议号 **V0158**）。

#### 口径 ②：迁移号**以仓库文件为准**，批次 2 从 **V0160** 起

| 事实 | 实测值（本轮） | 命令 |
|------|----------------|------|
| `cloudpivot_p2e1` 已应用 | max=**150**，共 **99** 条 | `select max(version::int), count(*) from schema_migrations` |
| `cloudpivot_p2e2` 已应用 | max=**151**，共 **100** 条（V0151 应付权限码已应用） | 同上 |
| 仓库迁移文件最大号 | **V0156**（`V0156__remove_erp_top_menu`，"去掉 ERP 顶级目录"） | `ls backend/migrations/business/` |
| 仓库已有但未必已应用 | V0151 / V0154 / V0155 / V0156 | 同上 |
| **批次 2 占用** | **V0160–V0168**（9 条） | §2.7 |
| 剩余空号（给批次 1 补丁） | **V0157–V0159**（3 个） | — |

**铁律**：**排号看文件，不看库**。库内版本落后于仓库文件是常态（V0151/V0154/V0155/V0156 就是"文件在、库未应用"），迁移器按**文件号顺序**执行，撞号比库落后更难修。

#### 口径 ③：`location_id` 指向**库位库存行**，不是库位字典

| 列 | 指向 | 依据 |
|----|------|------|
| `biz_inventory.location_id` | **`biz_inventory_location.id`** | V0139 迁移 + `\d biz_inventory` 外键 `fk_biz_inv_location_id` |
| `biz_inventory_location.location_id` | `biz_location.id`（库位字典） | `\d biz_inventory_location` |
| **`biz_stock_out_item.location_id`（本批新增）** | **`biz_inventory_location.id`** | 本批 DDL `biz_stock_out_item_location_id_fkey`，与 `biz_stock_transfer_item.from_location_id` 同构 |

写错的直接表现（第一版 DDL 真踩）：

```
ERROR:  insert or update on table "biz_inventory" violates foreign key constraint "fk_biz_inv_location_id"
DETAIL:  Key (location_id)=(c0000000-...-000000000001) is not present in table "biz_inventory_location".
```

本批 13 张表建完后实测外键落点（本轮重跑）：

```sql
select conname, confrelid::regclass::text from pg_constraint
 where conrelid='biz_stock_out_item'::regclass and contype='f' and conname like '%location%';
-- biz_stock_out_item_location_id_fkey | biz_inventory_location
```

#### 口径 ④：并发不是"测过一遍"，是**可复现的双会话终态**

| 场景 | 夹具 | 两会话动作 | 期望终态 | 本轮实测终态 |
|------|------|-----------|---------|-------------|
| 库存超卖 | 库存 `qty=8, amount=40, avg_cost=5` | A 扣 5（持锁 3s）／B 扣 5 | 一成一败，`qty=3, amount=15` | ✅ `qty=3.0000, amount=15.0000, avg_cost=5.000000` |
| 信用占额 | 额度 100000，已用 0 | C 占 80000（持锁 3s）／D 占 80000 | 第二笔 0 行，`used_amount=80000` | ✅ `order_used=80000, ar_used=0, used_amount=80000, version=1` |

复现脚本：`_probe/conc_setup.sql` + `_probe/run_conc.sh`（§9.1.2 有原样输出）；本轮用 `erp_p3d2_conc` 库独立复跑，命令与输出见 §9.7.3。

---
## 1. 现状核查（设计的事实基础）

本章每个数字都来自现场命令，不引用其它文档的转述。

### 1.1 迁移号段实测

```bash
$ for db in cloudpivot_p2e1 cloudpivot_p2e2; do
    su postgres -c "psql -d $db -tAc \"select max(version::int)||'|'||count(*)||'|'||count(distinct version)
      from schema_migrations where version ~ '^[0-9]+\$'\""
  done
150|99|99        # cloudpivot_p2e1
151|100|100      # cloudpivot_p2e2（V0151 应付权限码已应用）
```

| 事实 | 实测值（本轮 2026-10-31 复核） | 说明 |
|------|--------|------|
| `schema_migrations.version` 的**实际形态** | 纯数字字符串（`'150'`），**不是** `'V0150'` | 任务书里的 `where version ~ '^V'` 在本仓返回 **0 行**——这是第一个坑 |
| 已应用迁移数 | `p2e1` = **99**；`p2e2` = **100** | 含 system / business / support 三段 |
| 最大版本 | `p2e1` = **150**；`p2e2` = **151** | 设计初稿时两库都停在 150，本轮复核 `p2e2` 已把 V0151 应用 |
| 批次 1 ERP 段的号段 | V0137–V0151 | 见下表 |

批次 1 ERP / 仓储相关迁移（`doc_type` 由 `schema_migrations.name` 读回）：

| version | name | 内容 |
|---------|------|------|
| 137 | `erp_master_data_and_warehouse` | 主数据 5 表 + 仓库层级 4 列 + `biz_location` + `biz_inventory_location` |
| 138 | `material_serial` | `biz_material_serial` + `serial_required` |
| 139 | `erp_inventory_location` | `biz_inventory.location_id` + 三列唯一键 + 在途仓 |
| 140 | `erp_stock_transfer` | 调拨三表 + 调拨权限 |
| 141 | `erp_permissions` | 主数据权限码（menu id 0940–0978） |
| 142 | `erp_serial_permissions` | 序列号权限（0981–0985） |
| 143 | `erp_audit_permission` | 库存巡检权限（0986） |
| 144 | `erp_transfer_permission` | 调拨权限（0987–0992） |
| 145 | `purchase_order_amounts_and_stock_in` | 采购订单金额列 + `biz_stock_in` / `biz_stock_in_item` |
| 146 | `erp_purchase_order_permissions` | 采购权限（0993–0997） |
| 147 | `erp_stock_in_permissions` | 收货权限（0998–1003） |
| 148 | `erp_payable` | `biz_payable` |
| 149 | `erp_voucher` | `biz_voucher` / `biz_voucher_entry` / `biz_voucher_rule` |
| 150 | `erp_ledger_permissions` | 凭证 + 总账权限（1004–1008） |
| 151 | `erp_payable_permissions` | 应付权限（1009–1010）；**`cloudpivot_p2e2` 已应用、`cloudpivot_p2e1` 未应用** |

> **结论**：批次 2 从 **V0160** 起，占 V0160–V0168 共 9 个号。补丁号段的真实占用（以**仓库文件**为准）是 **V0151 / V0154 / V0155 / V0156 已用 → V0157–V0159 只剩 3 个空号**；这个排法与 V0151 是否已在某个库应用无关。

> **补记（本轮重派 2026-10-31 复核仓库 HEAD 得到的两处更新）**：
> ① 仓库 `backend/migrations/business/` 里已有 **V0151（应付权限）、V0154（ERP 菜单按业务线重组）、V0155（菜单 title/icon 修复）、V0156（去掉 ERP 顶级目录）** 四个补丁文件；其中 V0156 是任务书明确点名"已被用掉"的那个号，因此**批次 1 补丁只能排 V0157–V0159**（§8.5 的四条建议按此收缩为 3 条，剩下的并入批次 3）。
> ② 库状态与文件不同步：`cloudpivot_p2e1` 仍是 `150/99`，`cloudpivot_p2e2` 已是 `151/100`——同一份仓库，两个库应用进度不同。**这正是"排号看文件、不看库"的实证**：如果按 `p2e1` 的 150 排批次 2 就会撞上 V0151–V0156 这些已存在的文件。迁移器按文件号顺序执行，文件号冲突比库内版本落后更难修。

### 1.2 现有 `biz_` 表实测清单（42 张）

```sql
select table_name from information_schema.tables
 where table_schema='public' and table_name like 'biz_%' order by 1;
```

```
biz_account                 biz_flow_instance_node      biz_receivable*      ← 本批新增
biz_accounting_period       biz_flow_node               biz_receipt*
biz_flow_cc                 biz_flow_node_assignee      biz_receipt_item*
biz_flow_def                biz_flow_task               biz_seq
biz_flow_def_version        biz_inventory               biz_stock_in
biz_flow_edge               biz_inventory_location      biz_stock_in_item
biz_flow_instance           biz_inventory_txn           biz_stock_out*
biz_location                biz_material                biz_stock_out_item*
biz_payable                 biz_material_serial         biz_stock_out_item_serial*
biz_project                 biz_payable                 biz_stock_transfer
biz_project_member          biz_purchase_order          biz_stock_transfer_item
biz_purchase_order_item     biz_stock_transfer_item_serial
biz_customer*               biz_customer_credit*        biz_customer_credit_txn*
biz_sale_order*             biz_sale_order_item*        biz_customer_statement*
biz_customer_statement_line*  biz_task / biz_task_*     biz_supplier / biz_unit
biz_voucher / biz_voucher_entry / biz_voucher_rule / biz_warehouse
```

（`*` = 本批 V0160–V0164 新增，共 13 张；原 42 张里**没有** `biz_customer` / `biz_receivable` / `biz_receipt`，销售侧完全空白。）

### 1.3 批次 1 关键表结构原文（`\d`，逐字）

#### 1.3.1 `biz_inventory`（成本 + 数量主账）

```
                    Table "public.biz_inventory"
    Column     |           Type           | Nullable |      Default
---------------+--------------------------+----------+-------------------
 id            | uuid                     | not null | gen_random_uuid()
 material_id   | uuid                     | not null |
 warehouse_id  | uuid                     | not null |
 qty           | numeric(18,4)            | not null | 0
 locked_qty    | numeric(18,4)            | not null | 0
 avg_cost      | numeric(18,6)            | not null | 0
 amount        | numeric(18,4)            | not null | 0
 last_txn_at   | timestamp with time zone |          |
 created_at    | timestamp with time zone | not null | CURRENT_TIMESTAMP
 updated_at    | timestamp with time zone | not null | CURRENT_TIMESTAMP
 location_id   | uuid                     |          |
Indexes:
    "biz_inventory_pkey" PRIMARY KEY, btree (id)
    "idx_biz_inv_location" btree (warehouse_id, location_id)
    "idx_biz_inv_material" btree (material_id)
    "idx_biz_inv_warehouse" btree (warehouse_id)
    "uk_biz_inv_mat_wh_loc" UNIQUE, btree (material_id, warehouse_id,
        COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
Check constraints:
    "ck_biz_inv_locked" CHECK (locked_qty >= 0::numeric)
Foreign-key constraints:
    "fk_biz_inv_location_id" FOREIGN KEY (location_id) REFERENCES biz_inventory_location(id)
```

> **注意两个细节**：① `biz_inventory` **没有 `deleted_at`**（永久行）；② `location_id` 的硬外键指向 **`biz_inventory_location(id)`**，不是 `biz_location(id)`——这是本批第一版 DDL 就踩到的坑（§1.8 ①）。

#### 1.3.2 `biz_inventory_txn`（库存流水，追加写）

```
    Column     |           Type           | Nullable |      Default
---------------+--------------------------+----------+-------------------
 id            | uuid                     | not null | gen_random_uuid()
 material_id   | uuid                     | not null |
 warehouse_id  | uuid                     | not null |
 txn_type      | character varying(8)     | not null |        ← 'IN' / 'OUT'
 biz_type      | character varying(24)    | not null |        ← 'PURCHASE_IN' / 'TRANSFER_OUT' / 本批 'SALE_OUT'
 doc_type      | character varying(24)    | not null |        ← 'STOCK_IN' / 'STOCK_TRANSFER' / 本批 'STOCK_OUT'
 doc_id        | uuid                     | not null |
 doc_no        | character varying(32)    | not null |
 line_no       | integer                  | not null | 1
 qty           | numeric(18,4)            | not null |        ← 永远正数
 unit_cost     | numeric(18,6)            | not null | 0
 amount        | numeric(18,4)            | not null | 0
 before_qty    | numeric(18,4)            | not null | 0
 before_amount | numeric(18,4)            | not null | 0
 before_cost   | numeric(18,6)            | not null | 0
 after_qty     | numeric(18,4)            | not null | 0
 after_amount  | numeric(18,4)            | not null | 0
 after_cost    | numeric(18,6)            | not null | 0
 period_code   | character varying(7)     | not null |
 operator_id   | uuid                     |          |
 remark        | character varying(255)   |          |
 created_at    | timestamp with time zone | not null | CURRENT_TIMESTAMP
Check constraints:
    "ck_biz_inv_txn_amt"  CHECK (amount >= 0::numeric)
    "ck_biz_inv_txn_qty"  CHECK (qty > 0::numeric)
    "ck_biz_inv_txn_type" CHECK (txn_type = ANY (ARRAY['IN','OUT']))
```

> **`amount >= 0` 这条 CHECK 对本批是硬约束**：出库流水的 `amount`（成本额）**必须记正数**，方向由 `txn_type='OUT'` 表达。负库存场景（`amount` 为负的结存）不能把负号带进流水。

#### 1.3.3 `biz_voucher` / `biz_voucher_entry` / `biz_voucher_rule`

```
biz_voucher: id, voucher_word(8)='记', voucher_no(32), period(7), voucher_date(date),
  voucher_type(16)='AUTO'|'MANUAL', source_type(24)='MANUAL', source_id, source_no,
  summary(255), total_debit numeric(20,4), total_credit numeric(20,4), entry_count int,
  status(16)='POSTED'|'DRAFT'|'AUDITED'|'CANCELLED', is_red smallint, reverse_of, reversed_by,
  posted_at, posted_by, remark, created_at, updated_at, create_by, update_by, deleted_at
  UNIQUE uk_biz_voucher_no (voucher_word, period, voucher_no) WHERE deleted_at IS NULL
  CHECK ck_biz_voucher_balance  total_debit = total_credit
  CHECK ck_biz_voucher_entries  entry_count >= 2
  CHECK ck_biz_voucher_red_ref  (is_red=1 AND reverse_of IS NOT NULL) OR (is_red=0 AND reverse_of IS NULL)
  INDEX idx_biz_voucher_source (source_type, source_id)

biz_voucher_entry: id, voucher_id(→biz_voucher ON DELETE CASCADE), line_no, summary(255),
  account_id, account_code(30), account_name(120),
  debit_amount numeric(20,4), credit_amount numeric(20,4),
  aux_supplier_id, aux_material_id, aux_warehouse_id, aux_dept_id, aux_qty, aux_extra jsonb, created_at
  UNIQUE uk_biz_voucher_entry_line (voucher_id, line_no)
  CHECK ck_biz_voucher_entry_side  (debit=0 AND credit>0) OR (debit>0 AND credit=0)   ← 借贷不能同时有值

biz_voucher_rule: id, rule_code(40) UNIQUE WHERE deleted_at IS NULL, biz_type(24),
  debit_account_id, credit_account_id, tax_account_id, is_active smallint, remark
  INDEX idx_biz_voucher_rule_biz (biz_type, is_active)
```

> **`source_type` 只有 24 字符**：本批的 `STOCK_OUT_COST`（14 字符）与 `SALE_OUT`（8 字符）都放得下，无需扩列。

#### 1.3.4 `biz_account`（科目）+ 实测种子

```
biz_account: id, account_code(30) UNIQUE, account_name(120), parent_id, level smallint,
  account_category ∈ {ASSET,LIABILITY,EQUITY,COST,PROFIT_LOSS},
  balance_direction ∈ {DEBIT,CREDIT}, is_leaf smallint, aux_dimensions(120), status smallint,
  remark, audit 列

实测种子（4 条）：
  1405     库存商品        ASSET      DEBIT   leaf=1
  2202     应付账款        LIABILITY  CREDIT  leaf=1
  2221     应交税费        LIABILITY  CREDIT  leaf=0
  2221.01  应交税费-进项税  LIABILITY  CREDIT  leaf=1
```

**缺的科目（本批 V0166 补）**：`1122 应收账款`、`6001 主营业务收入`、`2221.02 应交税费-销项税`、`6401 主营业务成本`、`1002 银行存款`。

#### 1.3.5 `biz_stock_in` / `biz_stock_in_item`（出库单要对齐的结构）

```
biz_stock_in:
  id, in_no(32) UNIQUE WHERE deleted_at IS NULL, in_type(20)='PURCHASE'|'RETURN'|'OTHER',
  source_order_id, supplier_id, warehouse_id, in_date date, status(24)='DRAFT'|'POSTED'|'CANCELLED',
  currency_code char(3)='CNY', total_qty, total_amount, total_tax, total_with_tax,
  invoice_received boolean, voucher_id, payable_id, remark,
  posted_at, posted_by, cancelled_at, cancelled_by, cancel_reason, audit 列
  CHECK ck_biz_si_tax           total_with_tax = (total_amount + total_tax)
  CHECK ck_biz_si_qty           四个合计都 >= 0
  CHECK ck_biz_si_supplier      in_type='PURCHASE' → supplier_id IS NOT NULL
  INDEX idx_biz_si_supplier (supplier_id, in_date DESC), idx_biz_si_warehouse, idx_biz_si_voucher

biz_stock_in_item:
  id, stock_in_id(→biz_stock_in CASCADE), line_no, source_item_id, material_id, unit_id,
  qty numeric(18,4) NOT NULL, price numeric(18,6), price_incl numeric(18,6), tax_rate numeric(7,4),
  amount, tax_amount, with_tax_amount, cost_price numeric(18,6), location_id, lot_no, remark, audit
  UNIQUE uk_biz_si_item_line (stock_in_id, line_no)
  CHECK ck_biz_si_item_qty   qty > 0
  CHECK ck_biz_si_item_rate  tax_rate >= 0 AND tax_rate <= 100     ← 注意是 0–100，不是 0–1
  CHECK ck_biz_si_item_tax   with_tax_amount = (amount + tax_amount)
```

> **两个必须照抄的既有口径**：① 明细单价列名是 **`price`**（不是 `price_excl`）；② **`tax_rate` 在单据明细上是 `0–100` 的百分数**，而 `biz_material.tax_rate` 是 `0–1` 的小数——两张表口径不同是历史事实，本批明细沿用 `0–100`，并在 §4.2 说明换算。

#### 1.3.6 `biz_payable`（应收要镜像的结构）

```
biz_payable: id, payable_no(32), supplier_id NOT NULL, source_type(24)='STOCK_IN', source_id, source_no,
  invoice_no(50), invoice_date, currency_code, amount, paid_amount, unpaid_amount,
  credit_days int, due_date date NOT NULL, period_code(7), status(20)='OPEN',
  settled_at, voucher_id, remark, audit 列
  UNIQUE uk_biz_payable_no     (payable_no) WHERE deleted_at IS NULL
  UNIQUE uk_biz_payable_source (source_type, source_id) WHERE deleted_at IS NULL   ← 幂等键
  CHECK ck_biz_payable_paid     paid_amount >= 0 AND paid_amount <= amount
  CHECK ck_biz_payable_unpaid   unpaid_amount = (amount - paid_amount)
  CHECK ck_biz_payable_settled  status='SETTLED' → unpaid_amount = 0
  CHECK ck_biz_payable_status   ∈ {OPEN, PARTIAL_PAID, SETTLED, CANCELLED}
```

#### 1.3.7 `biz_material_serial`（出库要改状态的表）

```
biz_material_serial: id, serial_no(64) UNIQUE WHERE deleted_at IS NULL, material_id NOT NULL,
  warehouse_id, location_id(→biz_inventory_location.id), batch_no(64),
  status(16) ∈ {IN_STOCK, OUTBOUND, SCRAPPED}, inbound_at, outbound_at,
  source_type(32), source_id, audit 列
  INDEX idx_biz_material_serial_material (material_id) WHERE deleted_at IS NULL
  INDEX idx_biz_material_serial_status   (status)       WHERE deleted_at IS NULL
  INDEX idx_biz_material_serial_wh_loc   (warehouse_id, location_id) WHERE deleted_at IS NULL
```

#### 1.3.8 `biz_stock_transfer_item_serial`（出库序列号子表要镜像的结构）

```
biz_stock_transfer_item_serial: id, item_id(→biz_stock_transfer_item), serial_id(→biz_material_serial),
  created_at, create_by
  UNIQUE uk_transfer_item_serial (item_id, serial_id)
  INDEX  idx_transfer_item_serial_serial (serial_id)
```

#### 1.3.9 `biz_seq`（单号序列，本批要补 5 条种子）

```
biz_seq: id, seq_type(32) UNIQUE WHERE deleted_at IS NULL, seq_name(50), prefix(12),
  date_pattern(8)='yyyyMMdd'|'yyyyMM'|'none', period_key(8), current_no bigint, step int,
  separator(4)='-', status, remark, audit 列

实测种子（5 条）：PO 采购订单号 / SN 单品序列号 / IN 收货入库单号 / PZ 凭证号 / AP 应付单号
```

### 1.4 批次 1 已确立、本批不得推翻的口径

| # | 口径 | 出处 |
|---|------|------|
| 1 | 表头金额三列：`total_amount` / `total_tax` / `total_with_tax`，且 `total_with_tax = total_amount + total_tax`（CHECK） | `biz_stock_in` |
| 2 | 明细五列：`price` / `price_incl` / `amount` / `tax_amount` / `with_tax_amount`，且 `with_tax_amount = amount + tax_amount`（CHECK） | `biz_stock_in_item` |
| 3 | 数量列名 `qty`，永为正；方向由 `txn_type` 表达 | `biz_inventory_txn` |
| 4 | 成本列 `avg_cost NUMERIC(18,6)`、金额列 `NUMERIC(18,4)`、科目余额 `NUMERIC(20,4)` | §2.0.3 |
| 5 | 状态列 `VARCHAR` + `CHECK`，不用 `SMALLINT` | §1.6 |
| 6 | 主数据弱引用（不建外键），明细→主表真外键 `ON DELETE CASCADE` | §2.0.2 |
| 7 | 唯一索引一律带 `WHERE deleted_at IS NULL`（永久行表除外） | §2.0.1 |
| 8 | 凭证必须借贷平衡、`entry_count >= 2`、红字凭证必须挂 `reverse_of` | `biz_voucher` CHECK |
| 9 | 取号用 `UPDATE biz_seq … RETURNING` 单语句原子，前缀 `yyyyMMdd` + 6 位序号 | `voucher.rs::next_doc_no` |
| 10 | 流水金额记正数，倒挤只在"清空"时发生 | `biz_inventory_txn` + §3.2.3 |

### 1.5 权限码的真实机制（决定 V0165 怎么写）

```bash
$ su postgres -c "psql -d cloudpivot_p2e2 -c '\d sys_permission'"
ERROR:  relation "sys_permission" does not exist
```

| 事实 | 说明 |
|------|------|
| 本仓**没有** `sys_permission` 表 | 任务书 §二 提到的"V0160 客户/销售权限码"若按 `sys_permission` 写会直接跑不起来 |
| 权限码的唯一来源 | `sys_menu.auth_code`，登录时经 `sys_role_menu` 装载到角色 |
| 已占用 ID 段 | 0940–1008（V0141–V0150）；**1009 / 1010 已被 V0151 预占** |
| 本批起始段 | **1011 起**，按"菜单节点自带 `:list` 码 + 按钮节点 `menu_type=3`"的既有口径写 |
| 授权角色白名单 | `SUPER_ADMIN` / `ADMIN` / `TEST_UADMIN` / `FINANCE` / `ACCOUNTANT`（销售侧追加 `SALES` / `SALES_MANAGER`，不存在时自然 0 行） |

### 1.6 批次 1 设计但**未落地**的表（不要误引用）

`docs/ERP-批次1-技术架构.md` §2.7 设计了 `biz_doc_log`（单据操作轨迹），但：

```bash
$ grep -rn "biz_doc_log" /root/.hermes/profiles/backend-developer/cache/scratch/erp/*.rs
（无输出）
$ psql -tAc "select 1 from information_schema.tables where table_name='biz_doc_log'"
（无输出）
```

**结论**：`biz_doc_log` 最终未实现，单据轨迹依赖 `operation_logs` 中间件。本批**不新增**自建轨迹表（要保持与实测一致，也要避免出现"文档有、库没有"的第二处偏差）。若后续要做，属批次 1 补丁（V0157–V0159 号段，见 §8.5）。

### 1.7 前端现状（决定 §5 的写法）

| 项 | 实测 |
|----|------|
| 框架 | Vue **3.5** + TypeScript 5.8 + Vite 6 |
| UI 库 | **TDesign Vue Next 1.19**（`tdesign-vue-next`）+ `tdesign-icons-vue-next` |
| 状态/路由 | Pinia 3 + vue-router 4，**路由手工注册**（无 views 自动扫描，漏注册 = 点菜单 404） |
| 请求 | axios 1.15，`src/api/business/erp/*.ts` 一个模块一个文件 |
| ERP 页面目录 | `src/views/business/erp/{warehouse,material,serial,transfer,stockin,voucher,ledger,payable,purchase,shared}/` |
| 复用件 | `src/views/business/erp/shared/money.ts`（金额字符串工具）、`stockin/stockin-logic.ts`（单据页逻辑层与视图分离的范式） |
| 扫码件 | **不存在**（本批新建 `shared/scan.ts`） |

### 1.8 三处实测纠偏（POC 发现的真实缺陷）

这三条是本文档最有价值的部分——它们都是"按经验写就一定会错"的地方。

#### ① `biz_inventory.location_id` 指向的是**库位库存行**，不是库位字典

第一版 DDL 我按常理给 `biz_stock_out_item.location_id` 加了 `REFERENCES biz_location(id)`，建库时直接报：

```
ERROR:  insert or update on table "biz_inventory" violates foreign key constraint "fk_biz_inv_location_id"
DETAIL:  Key (location_id)=(c0000000-...-000000000001) is not present in table "biz_inventory_location".
```

真值是：V0139 的迁移注释与 `\d` 双向确认——**`location_id` 收的是 `biz_inventory_location.id`**；`biz_inventory_location.location_id` 才是 `biz_location.id`。批次 1 的 `serial_handler.rs:171` 甚至把这条写进了错误文案：`"库位库存行不存在：location_id 指向 biz_inventory_location.id（不是 biz_location.id）"`。

**本批处理**：`biz_stock_out_item.location_id` 与 `biz_stock_transfer_item.from_location_id` 同构，硬外键 `REFERENCES biz_inventory_location(id)`。

#### ② `CHECK` 的三值逻辑：`NULL` 会被当作**通过**

第一版 `ck_biz_so_state` 写成：

```sql
WHEN 'CANCELLED' THEN cancelled_at IS NOT NULL AND btrim(cancel_reason) <> ''
```

POC-8c 插一条"已取消但无取消原因"的订单，**插进去了**：

```
 order_no   |  status   | has_cancel_at | cancel_reason
------------+-----------+---------------+---------------
 SO-POC-BAD | CANCELLED | t             |
```

原因：`btrim(NULL) <> ''` 求值为 `NULL`，而 `CHECK` 只在结果为 **FALSE** 时拒绝，`NULL` 视为通过。

```sql
select (now() is not null and btrim(null) <> '') as v, (now() is not null and btrim(null) <> '') is null as is_null;
 v | is_null
---+---------
   | t
```

**本批处理**：所有"空值不可接受"的 CHECK 一律显式写 `IS NOT NULL`：`cancelled_at IS NOT NULL AND cancel_reason IS NOT NULL AND btrim(cancel_reason) <> ''`。修正后同一条插入被 `ck_biz_so_state` 拒绝（§9.1 POC-8c）。

#### ③ `RETURNING` 里算 before 值会拿到**错数**

POC-1b 我试图在 `RETURNING` 里直接算"更新前的金额"：

```sql
RETURNING amount AS after_amount,
          ROUND(33*avg_cost,4) AS out_cost,
          (amount - ROUND(33*avg_cost,4)) AS before_amount   -- ← 错
```

真实输出：`before_amount = 344.1956`，而正确值是 `1012.34`。因为 `RETURNING` 看到的是**新行**，`amount` 已经是 `678.2678` 了。

**本批处理**：`before_*` 快照**只能**用 `after + 本次增量` 反推（`before_amount = after_amount + out_cost`、`before_qty = after_qty + qty`），或用同事务内 `UPDATE` **之前**的 `SELECT`（无锁读，不改变加锁顺序）。这与批次 1 调拨的既定做法一致。

---
## 2. 数据模型（V0160 起，13 张表）

### 2.0 全局约定（继承批次 1，逐条对齐）

#### 2.0.1 主键与审计列

| 约定 | 内容 | 与批次 1 的关系 |
|------|------|----------------|
| 主键 | `id UUID PRIMARY KEY DEFAULT gen_random_uuid()`；代码侧统一 `Uuid::now_v7()` | 照抄 |
| 时间 | `created_at` / `updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP`（`TimestampHook` 自动维护） | 照抄 |
| 操作人 | `create_by` / `update_by UUID`（弱引用 `sys_user.id`，不建外键） | 照抄 |
| 软删 | `deleted_at TIMESTAMPTZ`，`NULL` = 未删；唯一索引一律带 `WHERE deleted_at IS NULL` | 照抄 |
| 永久行表 | 流水 / 明细 / 关联表不设 `deleted_at` | 照抄（`biz_inventory_txn`、`biz_stock_out_item_serial`、`biz_receipt_item`、`biz_customer_credit_txn`） |

#### 2.0.2 外键策略（对齐底座 `sys_user.dept_id` 的"弱引用"约定）

| 关系 | 做法 | 本批实例 | 理由 |
|------|------|---------|------|
| 主数据引用 | **弱引用 + 索引 + 注释** | `biz_customer.salesperson_id`、`biz_sale_order.customer_id`、`biz_stock_out.customer_id` | 主数据会软删/停用，硬外键会让历史单据无法保留 |
| 单据→来源单据 | **弱引用** | `biz_stock_out.source_order_id`、`biz_receivable.source_id`、`biz_receipt_item.receivable_id` | 各自独立生命周期，作废/冲销不等于删行 |
| 明细→主表 | **真外键 `ON DELETE CASCADE`** | `biz_sale_order_item.order_id`、`biz_stock_out_item.stock_out_id`、`biz_receipt_item.receipt_id`、`biz_customer_statement_line.statement_id` | 明细无独立生命周期 |
| 明细→序列号 | **真外键**（无 CASCADE） | `biz_stock_out_item_serial.serial_id` | 序列号是资产，不能被明细删除带走 |
| 明细→库位库存行 | **真外键** | `biz_stock_out_item.location_id → biz_inventory_location(id)` | 与 `biz_stock_transfer_item.from_location_id` 同构（§1.8 ①） |

#### 2.0.3 数值精度（照抄批次 1 §2.0.3）

| 语义 | DB 类型 | 说明 |
|------|---------|------|
| 数量 | `NUMERIC(18,4)` | 支持小数计量 |
| 单价 / 成本单价 | `NUMERIC(18,6)` | 移动加权必须比金额多 2 位，否则每次入库被舍入、误差累积 |
| 金额 | `NUMERIC(18,4)` | 单据金额、税额、成本额 |
| 应付/应收余额、对账余额 | `NUMERIC(20,4)` | 累计可能上亿 |
| 税率 | `NUMERIC(7,4)` | **单据明细口径 `0–100`（百分数）**，`biz_material.tax_rate` 是 `0–1` |
| 信用额度/占用 | `NUMERIC(18,4)` | 与金额同精度，避免额度与订单额比对时舍入不一致 |

> 禁止 `FLOAT` / `DOUBLE` / `REAL`——任何金额、数量、成本、额度列一律 `NUMERIC`。

#### 2.0.4 索引命名

`uk_<表简称>_<语义>`（唯一）/ `idx_<表简称>_<列>`（普通）。表简称沿用批次 1 风格：`biz_sale_order` → `uk_biz_so_no`、`biz_stock_out` → `uk_biz_out_no`、`biz_receivable` → `uk_biz_ar_no`。

### 2.1 客户与信用（迁移 `V0160`，3 张表）

#### 2.1.1 `biz_customer` —— 客户主数据

```sql
CREATE TABLE biz_customer (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_code         VARCHAR(40)  NOT NULL,
    customer_name         VARCHAR(200) NOT NULL,
    short_name            VARCHAR(100),
    customer_type         VARCHAR(20)  NOT NULL DEFAULT 'ENTERPRISE',
    contact_person        VARCHAR(50),
    contact_phone         VARCHAR(30),
    email                 VARCHAR(120),
    tax_no                VARCHAR(50),
    address               VARCHAR(255),
    credit_days           INTEGER      NOT NULL DEFAULT 0,
    credit_status         VARCHAR(16)  NOT NULL DEFAULT 'NORMAL',
    receivable_account_id UUID,
    salesperson_id        UUID,
    invoice_title         VARCHAR(200),
    invoice_bank_name     VARCHAR(200),
    invoice_bank_account  VARCHAR(64),
    status                SMALLINT     NOT NULL DEFAULT 1,
    remark                TEXT,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_customer_type   CHECK (customer_type IN ('ENTERPRISE','INDIVIDUAL','INTERNAL')),
    CONSTRAINT ck_biz_customer_credit_days CHECK (credit_days >= 0),
    CONSTRAINT ck_biz_customer_credit_status CHECK (credit_status IN ('NORMAL','WATCH','BLOCKED')),
    CONSTRAINT ck_biz_customer_status CHECK (status IN (0,1))
);
CREATE UNIQUE INDEX uk_biz_customer_code ON biz_customer (customer_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_customer_name        ON biz_customer (customer_name);
CREATE INDEX idx_biz_customer_status      ON biz_customer (status);
CREATE INDEX idx_biz_customer_salesperson ON biz_customer (salesperson_id);

COMMENT ON TABLE  biz_customer IS '客户主数据（批次2·S1）';
COMMENT ON COLUMN biz_customer.credit_days IS '账期天数，生成应收时快照到 biz_receivable.credit_days';
COMMENT ON COLUMN biz_customer.credit_status IS 'NORMAL 正常 / WATCH 观察（可下单需审批）/ BLOCKED 冻结（禁止下单）';
COMMENT ON COLUMN biz_customer.receivable_account_id IS '客户级应收账款科目（凭证科目解析第 2 级，弱引用 biz_account.id）';
```

**设计要点**

| 点 | 决策 | 理由 |
|----|------|------|
| 额度放哪 | **不放在客户表**，放 `biz_customer_credit` | 下单要 `UPDATE … SET used = used + ?`，会锁住该行；放在客户主数据表会让"每笔下单锁一行主数据"，与主数据高频读（下拉、列表）冲突。独立表把写热点隔离出来 |
| 为什么仍保留 `credit_days` | 账期是客户属性，不随额度调整变 | 应收生成时快照进 `biz_receivable.credit_days`，之后客户改账期不影响历史应收到期日 |
| `credit_status` 三态 | `NORMAL` / `WATCH` / `BLOCKED` | `BLOCKED` = 禁止下单（硬拦）；`WATCH` = 可下单但必须走审批（`biz_sale_order` 提交时强制发起审批流）；不用布尔，因为销售场景确实有三态 |
| `receivable_account_id` | 弱引用科目 | 科目解析链第 2 级（§3.5.3），支持"某客户的应收挂不同科目"（如内部往来） |

#### 2.1.2 `biz_customer_credit` —— 信用额度（一客户一行，热点行）

```sql
CREATE TABLE biz_customer_credit (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id   UUID          NOT NULL,
    credit_limit  NUMERIC(18,4) NOT NULL DEFAULT 0,
    order_used    NUMERIC(18,4) NOT NULL DEFAULT 0,
    ar_used       NUMERIC(18,4) NOT NULL DEFAULT 0,
    used_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,
    version       INTEGER       NOT NULL DEFAULT 0,
    status        SMALLINT      NOT NULL DEFAULT 1,
    remark        TEXT,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,
    update_by     UUID,
    deleted_at    TIMESTAMPTZ,
    CONSTRAINT ck_biz_credit_limit CHECK (credit_limit >= 0),
    CONSTRAINT ck_biz_credit_order_used CHECK (order_used >= 0),
    CONSTRAINT ck_biz_credit_ar_used CHECK (ar_used >= 0),
    CONSTRAINT ck_biz_credit_used CHECK (used_amount = order_used + ar_used),
    CONSTRAINT ck_biz_credit_version CHECK (version >= 0),
    CONSTRAINT ck_biz_credit_status CHECK (status IN (0,1))
);
CREATE UNIQUE INDEX uk_biz_credit_customer ON biz_customer_credit (customer_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_credit_status ON biz_customer_credit (status, credit_limit);

COMMENT ON TABLE  biz_customer_credit IS '客户信用额度（一客户一行；并发占用的热点行，独立成表以隔离主数据读写）';
COMMENT ON COLUMN biz_customer_credit.credit_limit IS '信用额度；0 = 不限额度（对齐 biz_supplier.credit_limit 语义）';
COMMENT ON COLUMN biz_customer_credit.order_used IS '订单占用：已提交未出库订单金额合计';
COMMENT ON COLUMN biz_customer_credit.ar_used IS '应收占用：已出库未收款应收金额合计';
COMMENT ON COLUMN biz_customer_credit.used_amount IS '合计占用 = order_used + ar_used（CHECK 强制自洽，客户端不可直写）';
COMMENT ON COLUMN biz_customer_credit.version IS '占用计数（诊断用：并发时观察版本推进，不参与正确性判断）';
```

**三列额度而不是一列 `used`：这是本批最关键的数据结构决策**

| 方案 | 结构 | 出库时怎么办 | 问题 |
|------|------|-------------|------|
| A | 单列 `used_amount` | 出库**不动**额度 | 订单作废后必须精确回滚"当时占了多少"，而订单金额可能被改过 → 回滚不准 |
| B | 单列 `used_amount` | 出库时 `used -= 订单额; used += 出库额` | 出库时又要判一次额度 → **两处并发判断**，任一处漏判就超限 |
| **C（采用）** | `order_used` + `ar_used` + `used_amount`（=前两者之和，CHECK 自洽） | `order_used -= 单额; ar_used += 出库额`，**总额不变** | 出库时**无需再判额度**（总额不变，不可能因此超限），并发判断收敛到下单唯一一处 |

C 的额外好处：
- 报表能直接回答"这 10 万额度里，多少压在未发货订单、多少压在未收账款"——销售和财务看的是两件事；
- 少发（出库额 < 订单额）时 `order_used` 减得多、`ar_used` 加得少，总额**自动下降**，方向永远安全；
- 作废订单释放以 `biz_sale_order.credit_hold_amount`（下单时写入的实际占用额）为准，不受订单金额后续修改影响。

#### 2.1.3 `biz_customer_credit_txn` —— 额度占用/释放流水（追加写）

```sql
CREATE TABLE biz_customer_credit_txn (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id           UUID          NOT NULL,
    txn_type              VARCHAR(16)   NOT NULL,
    direction             SMALLINT      NOT NULL,
    amount                NUMERIC(18,4) NOT NULL,
    order_used_before     NUMERIC(18,4) NOT NULL,
    order_used_after      NUMERIC(18,4) NOT NULL,
    ar_used_before        NUMERIC(18,4) NOT NULL,
    ar_used_after         NUMERIC(18,4) NOT NULL,
    credit_limit_snapshot NUMERIC(18,4) NOT NULL,
    source_type           VARCHAR(24)   NOT NULL,
    source_id             UUID          NOT NULL,
    source_no             VARCHAR(32),
    idem_key              VARCHAR(80)   NOT NULL,
    remark                VARCHAR(255),
    operator_id           UUID,
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_credit_txn_type CHECK (txn_type IN
        ('ORDER_HOLD','ORDER_RELEASE','AR_INCREASE','AR_RELEASE','LIMIT_ADJUST','MANUAL_RELEASE')),
    CONSTRAINT ck_biz_credit_txn_dir CHECK (direction IN (1,-1)),
    CONSTRAINT ck_biz_credit_txn_amt CHECK (amount > 0),
    CONSTRAINT ck_biz_credit_txn_order_math CHECK (
        order_used_after = order_used_before + CASE txn_type
            WHEN 'ORDER_HOLD' THEN amount WHEN 'ORDER_RELEASE' THEN -amount ELSE 0 END
        OR txn_type NOT IN ('ORDER_HOLD','ORDER_RELEASE')),
    CONSTRAINT ck_biz_credit_txn_ar_math CHECK (
        ar_used_after = ar_used_before + CASE txn_type
            WHEN 'AR_INCREASE' THEN amount WHEN 'AR_RELEASE' THEN -amount ELSE 0 END
        OR txn_type NOT IN ('AR_INCREASE','AR_RELEASE'))
);
CREATE UNIQUE INDEX uk_biz_credit_txn_idem ON biz_customer_credit_txn (idem_key);
CREATE INDEX idx_biz_credit_txn_customer ON biz_customer_credit_txn (customer_id, created_at DESC);
CREATE INDEX idx_biz_credit_txn_source   ON biz_customer_credit_txn (source_type, source_id);

COMMENT ON TABLE  biz_customer_credit_txn IS '信用额度占用/释放流水（追加写，不设 deleted_at：审计证据不可删）';
COMMENT ON COLUMN biz_customer_credit_txn.idem_key IS '幂等键，形如 ORDER_HOLD:SO:<order_id> / AR_INCREASE:OUT:<out_id>；重试不二次占用';
```

**幂等键设计**（`idem_key`，唯一索引兜底）

| 场景 | `idem_key` 形态 | 说明 |
|------|----------------|------|
| 下单占用 | `ORDER_HOLD:SO:<order_id>` | 一张订单只占用一次 |
| 下单释放（驳回/作废） | `ORDER_RELEASE:SO:<order_id>` | 与占用不同键，各自唯一 |
| 出库转应收 | `AR_INCREASE:OUT:<out_id>` | 一张出库单转一次 |
| 核销释放 | `AR_RELEASE:RCPT:<receipt_id>:<receivable_id>` | 同一收款单可核销多张应收，需带应收 id |
| 手工调额 | `LIMIT_ADJUST:<uuid>` | 每次人工调整一条 |

`CHECK ck_biz_credit_txn_order_math` / `_ar_math` 的价值：**流水自身可自证**——不需要 join 主表也能验证"这一笔确实只动了 order_used、且严格等于 amount"。审计对账时一条 SQL 就能查出所有不自洽的流水（见 §6.2 巡检 SQL）。

### 2.2 销售订单（迁移 `V0161`，2 张表）

#### 2.2.1 `biz_sale_order` —— 销售订单主表

```sql
CREATE TABLE biz_sale_order (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_no             VARCHAR(32)   NOT NULL,
    customer_id          UUID          NOT NULL,
    order_date           DATE          NOT NULL,
    delivery_date        DATE,
    salesperson_id       UUID,
    warehouse_id         UUID          NOT NULL,
    currency_code        CHAR(3)       NOT NULL DEFAULT 'CNY',
    status               VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',
    total_qty            NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_amount         NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_tax            NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_with_tax       NUMERIC(18,4) NOT NULL DEFAULT 0,
    credit_hold_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,
    approval_instance_id UUID,
    submitted_at         TIMESTAMPTZ,
    approved_at          TIMESTAMPTZ,
    closed_at            TIMESTAMPTZ,
    cancelled_at         TIMESTAMPTZ,
    cancel_reason        VARCHAR(255),
    remark               TEXT,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,
    CONSTRAINT ck_biz_so_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','PARTIAL_SHIPPED','SHIPPED','CLOSED','CANCELLED')),
    CONSTRAINT ck_biz_so_total CHECK (total_qty >= 0 AND total_amount >= 0
                                      AND total_tax >= 0 AND total_with_tax >= 0),
    CONSTRAINT ck_biz_so_tax CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_so_credit_hold CHECK (credit_hold_amount >= 0),
    CONSTRAINT ck_biz_so_state CHECK (
        CASE status
            WHEN 'DRAFT'           THEN submitted_at IS NULL AND approved_at IS NULL
                                        AND closed_at IS NULL AND cancelled_at IS NULL
            WHEN 'SUBMITTED'       THEN submitted_at IS NOT NULL AND approved_at IS NULL
                                        AND cancelled_at IS NULL
            WHEN 'APPROVED'        THEN submitted_at IS NOT NULL AND approved_at IS NOT NULL
                                        AND cancelled_at IS NULL
            WHEN 'PARTIAL_SHIPPED' THEN submitted_at IS NOT NULL AND approved_at IS NOT NULL
                                        AND closed_at IS NULL AND cancelled_at IS NULL
            WHEN 'SHIPPED'         THEN submitted_at IS NOT NULL AND approved_at IS NOT NULL
                                        AND closed_at IS NULL AND cancelled_at IS NULL
            WHEN 'CLOSED'          THEN closed_at IS NOT NULL AND cancelled_at IS NULL
            WHEN 'CANCELLED'       THEN cancelled_at IS NOT NULL AND cancel_reason IS NOT NULL
                                        AND btrim(cancel_reason) <> ''
            ELSE FALSE
        END)
);
CREATE UNIQUE INDEX uk_biz_so_no ON biz_sale_order (order_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_so_customer ON biz_sale_order (customer_id, order_date DESC);
CREATE INDEX idx_biz_so_status   ON biz_sale_order (status);
CREATE INDEX idx_biz_so_date     ON biz_sale_order (order_date DESC);
CREATE INDEX idx_biz_so_approval ON biz_sale_order (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

COMMENT ON TABLE biz_sale_order IS '销售订单主表（批次2·S2）';
COMMENT ON COLUMN biz_sale_order.credit_hold_amount IS
  '本单当前占用信用额度（提交时占用、驳回/作废释放）；释放时以本列为准，不按当前订单金额重算';
```

**字段级决策**

| 列 | 决策 | 理由 |
|----|------|------|
| `credit_hold_amount` | 单独记"占了多少"，不靠 `total_with_tax` 推 | 订单提交后金额可能被改（审批驳回改价再提交），用当前金额释放会把额度算错（可能变成负数占用） |
| `approval_instance_id` | 弱引用 `biz_flow_instance.id` | 与 `biz_stock_transfer.approval_instance_id` 同构（批次 1 已验证的审批集成方式：`engine::start(tx, …)` + `uk_biz_flow_instance_biz_active` 防重复） |
| `warehouse_id` | 单头必填（默认发货仓），明细可覆盖 | 一张订单大部分从同一仓发；跨仓拆单在明细行 `warehouse_id` 覆盖 |
| `status` 七态 | 含 `PARTIAL_SHIPPED` / `SHIPPED` | 由出库单过账**自动**推进（§3.7），与采购订单的 `PARTIAL_RECEIVED` / `RECEIVED` 镜像 |
| `ck_biz_so_state` | 时间戳组合强约束 | 状态机与时间戳不可能不一致（这是批次 1 调拨 `ck_biz_transfer_state` 的成功经验）；**注意必须显式 `cancel_reason IS NOT NULL`**（§1.8 ②） |

#### 2.2.2 `biz_sale_order_item` —— 销售订单明细

```sql
CREATE TABLE biz_sale_order_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id        UUID          NOT NULL,
    line_no         INTEGER       NOT NULL,
    material_id     UUID          NOT NULL,
    unit_id         UUID          NOT NULL,
    qty             NUMERIC(18,4) NOT NULL,
    shipped_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,
    price           NUMERIC(18,6) NOT NULL DEFAULT 0,
    price_incl      NUMERIC(18,6) NOT NULL DEFAULT 0,
    tax_rate        NUMERIC(7,4)  NOT NULL DEFAULT 0,
    amount          NUMERIC(18,4) NOT NULL DEFAULT 0,
    tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,
    with_tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    warehouse_id    UUID,
    delivery_date   DATE,
    remark          VARCHAR(255),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_so_item_qty   CHECK (qty > 0),
    CONSTRAINT ck_biz_so_item_ship  CHECK (shipped_qty >= 0 AND shipped_qty <= qty),
    CONSTRAINT ck_biz_so_item_line  CHECK (line_no >= 1),
    CONSTRAINT ck_biz_so_item_rate  CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_biz_so_item_tax   CHECK (with_tax_amount = amount + tax_amount),
    CONSTRAINT ck_biz_so_item_price CHECK (price >= 0 AND price_incl >= 0),
    CONSTRAINT biz_sale_order_item_order_id_fkey
        FOREIGN KEY (order_id) REFERENCES biz_sale_order(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX uk_biz_so_item_line ON biz_sale_order_item (order_id, line_no);
CREATE INDEX idx_biz_so_item_material ON biz_sale_order_item (material_id);

COMMENT ON TABLE biz_sale_order_item IS
  '销售订单明细（真外键 ON DELETE CASCADE；数量列名 qty、单价列名 price 均对齐批次 1 单据明细）';
COMMENT ON COLUMN biz_sale_order_item.shipped_qty IS
  '已出库数量，由出库单过账累加；ck_biz_so_item_ship 挡住超发（shipped_qty <= qty）';
```

**为什么不设 `delivered_amount` 之类的金额累计列**：金额累计唯一可信来源是 `biz_stock_out`（按 `source_order_id` 聚合），多存一列就多一处需要维护的不一致点。数量累计列 `shipped_qty` 保留，因为它是**超发控制的条件 UPDATE 目标**（`WHERE shipped_qty + ? <= qty`），必须能一行原子更新。

---
### 2.3 销售出库（迁移 `V0162`，3 张表）

#### 2.3.1 `biz_stock_out` —— 销售出库单主表（结构对齐 `biz_stock_in`）

```sql
CREATE TABLE biz_stock_out (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    out_no           VARCHAR(32)   NOT NULL,
    out_type         VARCHAR(20)   NOT NULL DEFAULT 'SALE',
    source_order_id  UUID,
    customer_id      UUID,
    warehouse_id     UUID          NOT NULL,
    out_date         DATE          NOT NULL,
    status           VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',
    currency_code    CHAR(3)       NOT NULL DEFAULT 'CNY',
    total_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_tax        NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_with_tax   NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,
    invoice_issued   BOOLEAN       NOT NULL DEFAULT FALSE,
    voucher_id       UUID,
    cost_voucher_id  UUID,
    receivable_id    UUID,
    posted_at        TIMESTAMPTZ,
    posted_by        UUID,
    cancelled_at     TIMESTAMPTZ,
    cancelled_by     UUID,
    cancel_reason    VARCHAR(255),
    remark           TEXT,
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,
    CONSTRAINT ck_biz_out_type CHECK (out_type IN ('SALE','RETURN','OTHER')),
    CONSTRAINT ck_biz_out_status CHECK (status IN ('DRAFT','POSTED','CANCELLED')),
    CONSTRAINT ck_biz_out_total CHECK (total_qty >= 0 AND total_amount >= 0 AND total_tax >= 0
                                       AND total_with_tax >= 0 AND total_cost >= 0),
    CONSTRAINT ck_biz_out_tax CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_out_customer CHECK (out_type <> 'SALE' OR customer_id IS NOT NULL),
    CONSTRAINT ck_biz_out_state CHECK (
        CASE status
            WHEN 'DRAFT'     THEN posted_at IS NULL AND cancelled_at IS NULL
            WHEN 'POSTED'    THEN posted_at IS NOT NULL AND cancelled_at IS NULL
            WHEN 'CANCELLED' THEN cancelled_at IS NOT NULL AND cancel_reason IS NOT NULL
                                    AND btrim(cancel_reason) <> ''
            ELSE FALSE
        END),
    CONSTRAINT ck_biz_out_vouchers CHECK (
        (status <> 'POSTED') OR (voucher_id IS NOT NULL AND cost_voucher_id IS NOT NULL))
);
CREATE UNIQUE INDEX uk_biz_out_no ON biz_stock_out (out_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_out_customer  ON biz_stock_out (customer_id, out_date DESC);
CREATE INDEX idx_biz_out_source    ON biz_stock_out (source_order_id);
CREATE INDEX idx_biz_out_status    ON biz_stock_out (status);
CREATE INDEX idx_biz_out_date      ON biz_stock_out (out_date DESC);
CREATE INDEX idx_biz_out_warehouse ON biz_stock_out (warehouse_id, out_date DESC);
CREATE INDEX idx_biz_out_voucher   ON biz_stock_out (voucher_id);
CREATE INDEX idx_biz_out_cvoucher  ON biz_stock_out (cost_voucher_id);

COMMENT ON TABLE biz_stock_out IS '销售出库单（结构对齐 biz_stock_in；财务列由过账事务回写）';
COMMENT ON COLUMN biz_stock_out.voucher_id IS '收入凭证（biz_voucher.source_type = STOCK_OUT）';
COMMENT ON COLUMN biz_stock_out.cost_voucher_id IS '成本凭证（biz_voucher.source_type = STOCK_OUT_COST）';
COMMENT ON COLUMN biz_stock_out.total_amount IS '售价不含税合计；成本口径见 total_cost';
COMMENT ON COLUMN biz_stock_out.total_cost IS '成本合计 = Σ 明细 cost_amount，过账时写入';
```

**字段级决策**

| 列 | 决策 | 理由 |
|----|------|------|
| `out_no` / `out_date` / `out_type` | 命名镜像 `in_no` / `in_date` / `in_type` | 同一套单据语义不要两套命名；出库单是收货单的镜像 |
| `total_cost` | **新增**（收货单没有） | 销售单要同时回答"卖了多少钱"和"这些货成本多少"；毛利 = `total_amount - total_cost`。若靠明细聚合，列表页每行一个子查询，性能差 |
| `voucher_id` + `cost_voucher_id` | **两列**，不用一列 + 类型判别 | 红冲准入校验要先确认"两张都在"（`ck_biz_out_vouchers` 直接兜住）；UI 要给两个凭证号；查询"成本凭证缺失"的巡检不用猜 |
| `ck_biz_out_vouchers` | `status='POSTED'` 必须两证齐全 | POC-8a 实测：直接置 POSTED 不给凭证会被 DB 拒绝——把"记账完整性"下沉到 DB，比应用层自觉可靠 |
| `out_type='RETURN'` | 枚举留位，本批不做流程 | 退货是"入库存 + 冲收入"，结构和枚举先留，实现放批次 3 |
| `invoice_issued` | 布尔占位 | 本批无开票模块；留给批次 3 反写 |
| `receivable_id` | 弱引用 | 一出一应收；单据详情页要能直接跳应收 |

#### 2.3.2 `biz_stock_out_item` —— 销售出库明细

```sql
CREATE TABLE biz_stock_out_item (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stock_out_id         UUID          NOT NULL,
    line_no              INTEGER       NOT NULL,
    source_order_item_id UUID,
    material_id          UUID          NOT NULL,
    unit_id              UUID          NOT NULL,
    qty                  NUMERIC(18,4) NOT NULL,
    price                NUMERIC(18,6) NOT NULL DEFAULT 0,
    price_incl           NUMERIC(18,6) NOT NULL DEFAULT 0,
    tax_rate             NUMERIC(7,4)  NOT NULL DEFAULT 0,
    amount               NUMERIC(18,4) NOT NULL DEFAULT 0,
    tax_amount           NUMERIC(18,4) NOT NULL DEFAULT 0,
    with_tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,
    cost_price           NUMERIC(18,6) NOT NULL DEFAULT 0,
    cost_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,
    location_id          UUID,
    lot_no               VARCHAR(64),
    negative_qty         BOOLEAN       NOT NULL DEFAULT FALSE,
    remark               VARCHAR(255),
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_out_item_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_out_item_line CHECK (line_no >= 1),
    CONSTRAINT ck_biz_out_item_rate CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_biz_out_item_tax  CHECK (with_tax_amount = amount + tax_amount),
    CONSTRAINT ck_biz_out_item_cost CHECK (cost_price >= 0 AND cost_amount >= 0),
    CONSTRAINT biz_stock_out_item_stock_out_id_fkey
        FOREIGN KEY (stock_out_id) REFERENCES biz_stock_out(id) ON DELETE CASCADE,
    CONSTRAINT biz_stock_out_item_location_id_fkey
        FOREIGN KEY (location_id) REFERENCES biz_inventory_location(id)
);
CREATE UNIQUE INDEX uk_biz_out_item_line ON biz_stock_out_item (stock_out_id, line_no);
CREATE INDEX idx_biz_out_item_material ON biz_stock_out_item (material_id);
CREATE INDEX idx_biz_out_item_source   ON biz_stock_out_item (source_order_item_id);
CREATE INDEX idx_biz_out_item_location ON biz_stock_out_item (location_id)
    WHERE location_id IS NOT NULL;

COMMENT ON TABLE  biz_stock_out_item IS
  '销售出库明细：一个库存行一次扣减 = 一行；未指定库位且需跨库位时展开成多行（line_no 递增，source_order_item_id 相同）';
COMMENT ON COLUMN biz_stock_out_item.cost_price  IS '出库时移动加权均价快照（来自 UPDATE ... RETURNING 的 after_cost）';
COMMENT ON COLUMN biz_stock_out_item.cost_amount IS '出库成本 = ROUND(qty × 均价, 4)；清空最后一批时倒挤为结存金额';
COMMENT ON COLUMN biz_stock_out_item.location_id  IS
  '库位（硬外键 biz_inventory_location.id，**不是** biz_location.id）；NULL = 整仓汇总桶';
COMMENT ON COLUMN biz_stock_out_item.negative_qty IS '本行是否已产生负库存（仓库 allow_negative=1 时置位，报表单独披露）';
```

**「一行 = 一个库存行」是本表的骨架决策**

| 方案 | 结构 | 优点 | 缺点 | 结论 |
|------|------|------|------|------|
| A | 一行明细 = 一个订单行，另建 `stock_out_item_alloc`（库位分配表） | 业务语义干净 | 新增第 14 张表；序列号要挂到分配行，链路多一跳（item → alloc → serial） | ✗ |
| **B（采用）** | 一行明细 = 一个库存行的扣减；跨库位展开多行（`line_no` 递增，`source_order_item_id` 相同） | 明细与流水**一一对应**（`biz_inventory_txn.line_no = 明细 line_no`），对账不用 join 中间表；序列号子表天然按库位分组（序列号自带 `location_id`） | 明细行数会多于订单行数 | ✓ |
| C | 一行明细 + `location_id` 存多个（数组/jsonb） | 表最少 | 库位与数量无法一一对应；无法表达"某库位出 3 件、某库位出 5 件"的成本快照 | ✗ |

**`cost_amount >= 0` 的 CHECK 是必须的**：`biz_inventory_txn.amount` 也要求 `>= 0`，两处对齐后，"负库存导致的负成本"不可能写进任何一张表——负库存的成本按"最近入库正价"记，仍是正数（§3.2.2）。

#### 2.3.3 `biz_stock_out_item_serial` —— 出库明细↔序列号

```sql
CREATE TABLE biz_stock_out_item_serial (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_id    UUID NOT NULL,
    serial_id  UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    CONSTRAINT biz_stock_out_item_serial_item_id_fkey
        FOREIGN KEY (item_id)   REFERENCES biz_stock_out_item(id),
    CONSTRAINT biz_stock_out_item_serial_serial_id_fkey
        FOREIGN KEY (serial_id) REFERENCES biz_material_serial(id)
);
CREATE UNIQUE INDEX uk_biz_out_item_serial    ON biz_stock_out_item_serial (item_id, serial_id);
CREATE INDEX        idx_biz_out_item_serial_serial ON biz_stock_out_item_serial (serial_id);

COMMENT ON TABLE biz_stock_out_item_serial IS
  '出库明细↔序列号（镜像 biz_stock_transfer_item_serial；一物一码物料：明细 qty 必须等于序列号个数）';
```

镜像批次 1 的三处细节，一个都不改：

| 细节 | 照抄内容 |
|------|---------|
| 列结构 | `item_id` / `serial_id` / `created_at` / `create_by`，无 `updated_at`、无 `deleted_at` |
| 唯一键 | `UNIQUE (item_id, serial_id)`——同一明细不能重复挂同一码 |
| 反向索引 | `serial_id` 单列索引（查"这个码从哪张单出去的"） |
| 不加 CASCADE | 与批次 1 一致：序列号是资产主数据，明细删除不该带走它 |

### 2.4 应收与收款（迁移 `V0163`，3 张表）

#### 2.4.1 `biz_receivable` —— 应收单（结构对齐 `biz_payable`）

```sql
CREATE TABLE biz_receivable (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    receivable_no     VARCHAR(32)   NOT NULL,
    customer_id       UUID          NOT NULL,
    source_type       VARCHAR(24)   NOT NULL DEFAULT 'STOCK_OUT',
    source_id         UUID          NOT NULL,
    source_no         VARCHAR(32)   NOT NULL,
    invoice_no        VARCHAR(50),
    invoice_date      DATE,
    currency_code     CHAR(3)       NOT NULL DEFAULT 'CNY',
    amount            NUMERIC(18,4) NOT NULL DEFAULT 0,
    received_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,
    unreceived_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    credit_days       INTEGER       NOT NULL DEFAULT 0,
    due_date          DATE          NOT NULL,
    period_code       VARCHAR(7)    NOT NULL,
    status            VARCHAR(20)   NOT NULL DEFAULT 'OPEN',
    settled_at        TIMESTAMPTZ,
    voucher_id        UUID,
    remark            TEXT,
    created_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,
    CONSTRAINT ck_biz_ar_status CHECK (status IN
        ('OPEN','PARTIAL_RECEIVED','SETTLED','CANCELLED')),
    CONSTRAINT ck_biz_ar_amount   CHECK (amount >= 0),
    CONSTRAINT ck_biz_ar_credit   CHECK (credit_days >= 0),
    CONSTRAINT ck_biz_ar_received CHECK (received_amount >= 0 AND received_amount <= amount),
    CONSTRAINT ck_biz_ar_unreceived CHECK (unreceived_amount = amount - received_amount),
    CONSTRAINT ck_biz_ar_settled  CHECK (status <> 'SETTLED' OR unreceived_amount = 0)
);
CREATE UNIQUE INDEX uk_biz_ar_no     ON biz_receivable (receivable_no)    WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_ar_source ON biz_receivable (source_type, source_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_ar_customer ON biz_receivable (customer_id, status);
CREATE INDEX idx_biz_ar_due      ON biz_receivable (due_date, status);
CREATE INDEX idx_biz_ar_period   ON biz_receivable (period_code);
CREATE INDEX idx_biz_ar_voucher  ON biz_receivable (voucher_id);

COMMENT ON TABLE biz_receivable IS
  '应收单（结构镜像 biz_payable；一出一应收，uk_biz_ar_source 兜底幂等）';
COMMENT ON COLUMN biz_receivable.credit_days IS '生成时的客户账期快照（客户后续改账期不影响历史应收）';
COMMENT ON COLUMN biz_receivable.due_date IS '到期日 = 出库业务日期 + credit_days';
```

**镜像对照表（逐列核对，防止"以为是镜像其实不同"）**

| `biz_payable` | `biz_receivable` | 差异与理由 |
|---------------|------------------|-----------|
| `payable_no` | `receivable_no` | 语义直译 |
| `supplier_id` | `customer_id` | 语义直译 |
| `paid_amount` | **`received_amount`** | 有意偏离（§0.1 声明）：`paid` 在应收表里会被读成"已付" |
| `unpaid_amount` | **`unreceived_amount`** | 同上 |
| `status='PARTIAL_PAID'` | `status='PARTIAL_RECEIVED'` | 同上 |
| 其余 18 列 | 完全一致 | `source_type`/`source_id`/`source_no`/`invoice_*`/`currency_code`/`amount`/`credit_days`/`due_date`/`period_code`/`settled_at`/`voucher_id`/`remark`/审计列 |

`uk_biz_ar_source (source_type, source_id) WHERE deleted_at IS NULL` 的作用：**过账重复调用的最后一道闸**。即使应用层的单据行锁失效（比如有人手工重放接口），第二张应收也插不进去。

#### 2.4.2 `biz_receipt` —— 收款单

```sql
CREATE TABLE biz_receipt (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    receipt_no       VARCHAR(32)   NOT NULL,
    customer_id      UUID          NOT NULL,
    receipt_date     DATE          NOT NULL,
    receipt_method   VARCHAR(16)   NOT NULL DEFAULT 'BANK',
    bank_account     VARCHAR(64),
    reference_no     VARCHAR(64),
    currency_code    CHAR(3)       NOT NULL DEFAULT 'CNY',
    amount           NUMERIC(18,4) NOT NULL,
    write_off_total  NUMERIC(18,4) NOT NULL DEFAULT 0,
    unapplied_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    status           VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    period_code      VARCHAR(7)    NOT NULL,
    voucher_id       UUID,
    posted_at        TIMESTAMPTZ,
    posted_by        UUID,
    cancelled_at     TIMESTAMPTZ,
    cancelled_by     UUID,
    cancel_reason    VARCHAR(255),
    remark           TEXT,
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,
    CONSTRAINT ck_biz_receipt_status CHECK (status IN ('DRAFT','POSTED','CANCELLED')),
    CONSTRAINT ck_biz_receipt_method CHECK (receipt_method IN ('BANK','CASH','NOTE','OTHER')),
    CONSTRAINT ck_biz_receipt_amount CHECK (amount > 0),
    CONSTRAINT ck_biz_receipt_writeoff CHECK (write_off_total >= 0 AND write_off_total <= amount),
    CONSTRAINT ck_biz_receipt_unapplied CHECK (unapplied_amount = amount - write_off_total),
    CONSTRAINT ck_biz_receipt_state CHECK (
        CASE status
            WHEN 'DRAFT'     THEN posted_at IS NULL AND cancelled_at IS NULL
            WHEN 'POSTED'    THEN posted_at IS NOT NULL AND cancelled_at IS NULL
            WHEN 'CANCELLED' THEN cancelled_at IS NOT NULL AND cancel_reason IS NOT NULL
                                    AND btrim(cancel_reason) <> ''
            ELSE FALSE
        END)
);
CREATE UNIQUE INDEX uk_biz_receipt_no ON biz_receipt (receipt_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_receipt_customer ON biz_receipt (customer_id, receipt_date DESC);
CREATE INDEX idx_biz_receipt_status   ON biz_receipt (status);
CREATE INDEX idx_biz_receipt_date     ON biz_receipt (receipt_date DESC);

COMMENT ON TABLE  biz_receipt IS '收款单（批次2·S4）';
COMMENT ON COLUMN biz_receipt.write_off_total IS '已核销合计；ck_biz_receipt_writeoff 保证不超收款金额（明细行合计由应用层断言）';
COMMENT ON COLUMN biz_receipt.unapplied_amount IS '未核销金额（预收），后续可再核销或退款';
COMMENT ON COLUMN biz_receipt.reference_no IS '银行流水号/支票号（对账用，非唯一）';
```

**`write_off_total <= amount` 放在表级 CHECK 而不是触发器**：单表内的恒等式 DB 能自证（POC-8d 实测拒绝）；"Σ 明细 = `write_off_total`"是跨行约束，DB 层要触发器才能管，本批按批次 1 的取舍（不过度用触发器）放应用层，并在 §6.2 给巡检 SQL 兜底。

#### 2.4.3 `biz_receipt_item` —— 核销明细

```sql
CREATE TABLE biz_receipt_item (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    receipt_id        UUID          NOT NULL,
    receivable_id     UUID          NOT NULL,
    line_no           INTEGER       NOT NULL,
    amount            NUMERIC(18,4) NOT NULL,
    receivable_no     VARCHAR(32)   NOT NULL,
    before_unreceived NUMERIC(18,4) NOT NULL,
    after_unreceived  NUMERIC(18,4) NOT NULL,
    remark            VARCHAR(255),
    created_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_receipt_item_amt   CHECK (amount > 0),
    CONSTRAINT ck_biz_receipt_item_line  CHECK (line_no >= 1),
    CONSTRAINT ck_biz_receipt_item_after CHECK (after_unreceived >= 0
                                                AND after_unreceived = before_unreceived - amount),
    CONSTRAINT biz_receipt_item_receipt_id_fkey
        FOREIGN KEY (receipt_id) REFERENCES biz_receipt(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX uk_biz_receipt_item_line ON biz_receipt_item (receipt_id, line_no);
CREATE UNIQUE INDEX uk_biz_receipt_item_ar   ON biz_receipt_item (receipt_id, receivable_id);
CREATE INDEX idx_biz_receipt_item_ar ON biz_receipt_item (receivable_id);

COMMENT ON TABLE biz_receipt_item IS
  '核销明细（一收款单 × 一应收 = 一行；before/after 快照 + CHECK 自证每次核销自洽）';
COMMENT ON COLUMN biz_receipt_item.receivable_no IS '应收单号快照（应收单被软删后仍可读）';
```

**`ck_biz_receipt_item_after` 是核销正确性的第二道锁**：`after = before - amount` 且 `after >= 0`。POC-6d 实测：伪造一条 `before=1000, amount=600, after=999` 被 DB 直接拒绝。这条 CHECK 有一个副作用**是好事**——它强制应用层在 UPDATE 应收**之前**必须锁读旧值（否则写不出正确的 `before`），从而无法跳过并发保护直接写。

### 2.5 客户对账单（迁移 `V0164`，2 张表）

#### 2.5.1 `biz_customer_statement` —— 对账单主表

```sql
CREATE TABLE biz_customer_statement (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    statement_no    VARCHAR(32)   NOT NULL,
    customer_id     UUID          NOT NULL,
    period_start    DATE          NOT NULL,
    period_end      DATE          NOT NULL,
    status          VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    opening_balance NUMERIC(20,4) NOT NULL DEFAULT 0,
    debit_total     NUMERIC(20,4) NOT NULL DEFAULT 0,
    credit_total    NUMERIC(20,4) NOT NULL DEFAULT 0,
    closing_balance NUMERIC(20,4) NOT NULL DEFAULT 0,
    confirmed_at    TIMESTAMPTZ,
    confirmed_by    UUID,
    remark          TEXT,
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_biz_stmt_status CHECK (status IN ('DRAFT','CONFIRMED','CANCELLED')),
    CONSTRAINT ck_biz_stmt_range  CHECK (period_start <= period_end),
    CONSTRAINT ck_biz_stmt_close  CHECK (closing_balance = opening_balance + debit_total - credit_total),
    CONSTRAINT ck_biz_stmt_confirmed CHECK (status <> 'CONFIRMED' OR confirmed_at IS NOT NULL)
);
CREATE UNIQUE INDEX uk_biz_stmt_no ON biz_customer_statement (statement_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_stmt_period ON biz_customer_statement (customer_id, period_start, period_end)
    WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_stmt_customer ON biz_customer_statement (customer_id, period_end DESC);

COMMENT ON TABLE biz_customer_statement IS
  '客户对账单主表（余额恒等式由 CHECK 兜底：期末 = 期初 + 借 - 贷）';
```

`ck_biz_stmt_close` 是本表存在的理由：对账单是**发给客户的对外交付物**，一旦余额算错就是信誉事故。把恒等式写成 CHECK，等于让"算错的账单"物理上无法存入。

#### 2.5.2 `biz_customer_statement_line` —— 对账单明细

```sql
CREATE TABLE biz_customer_statement_line (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    statement_id  UUID          NOT NULL,
    line_no       INTEGER       NOT NULL,
    biz_date      DATE          NOT NULL,
    doc_type      VARCHAR(24)   NOT NULL,
    doc_no        VARCHAR(32)   NOT NULL,
    doc_id        UUID          NOT NULL,
    summary       VARCHAR(255)  NOT NULL DEFAULT '',
    debit_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,
    credit_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    balance       NUMERIC(20,4) NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_stmt_line_side CHECK (
        (debit_amount = 0 AND credit_amount > 0) OR (debit_amount > 0 AND credit_amount = 0)),
    CONSTRAINT ck_biz_stmt_line_line CHECK (line_no >= 1),
    CONSTRAINT biz_customer_statement_line_statement_id_fkey
        FOREIGN KEY (statement_id) REFERENCES biz_customer_statement(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX uk_biz_stmt_line ON biz_customer_statement_line (statement_id, line_no);
CREATE INDEX idx_biz_stmt_line_doc ON biz_customer_statement_line (doc_type, doc_id);

COMMENT ON TABLE biz_customer_statement_line IS
  '对账单明细（借贷互斥对齐 biz_voucher_entry.ck_biz_voucher_entry_side 口径）';
```

对账行的数据来源（三条，全部已存在于本批）：

| 行来源 | `doc_type` | 借方（应收增加） | 贷方（应收减少） |
|--------|-----------|-----------------|-----------------|
| 销售出库生成应收 | `STOCK_OUT` | `total_with_tax` | — |
| 收款核销 | `RECEIPT` | — | `不核销部分不体现；核销部分按 biz_receipt_item.amount` |
| 应收作废/红冲 | `AR_REVERSE` | — | 原应收金额 |

`balance` 逐行累进（`上一行 balance ± 本行`），与表头 `closing_balance` 必须相等——这条跨行一致由应用层生成时保证，巡检 SQL 在 §6.2。

---
### 2.6 表清单与软删除策略总表

| # | 表 | 迁移 | 软删 | 金额/成本列 | 关键唯一键 | 说明 |
|---|----|------|------|------------|-----------|------|
| 1 | `biz_customer` | V0160 | ✅ | — | `uk_biz_customer_code` | 客户主数据 |
| 2 | `biz_customer_credit` | V0160 | ✅ | 额度 + 占用 | `uk_biz_credit_customer` | 一客户一行，**写热点** |
| 3 | `biz_customer_credit_txn` | V0160 | ❌ 永久行 | 占用额 | `uk_biz_credit_txn_idem` | 额度流水，审计证据 |
| 4 | `biz_sale_order` | V0161 | ✅ | 三金额 + 占用额 | `uk_biz_so_no` | 销售订单 |
| 5 | `biz_sale_order_item` | V0161 | ❌ 明细 | 四金额 + 单价 | `uk_biz_so_item_line` | 订单明细 |
| 6 | `biz_stock_out` | V0162 | ✅ | 三金额 + `total_cost` | `uk_biz_out_no` | 出库单 |
| 7 | `biz_stock_out_item` | V0162 | ❌ 明细 | 四金额 + 成本二列 | `uk_biz_out_item_line` | 出库明细 |
| 8 | `biz_stock_out_item_serial` | V0162 | ❌ 关联 | — | `uk_biz_out_item_serial` | 明细↔序列号 |
| 9 | `biz_receivable` | V0163 | ✅ | 金额 + 已收/未收 | `uk_biz_ar_no` / `uk_biz_ar_source` | 应收 |
| 10 | `biz_receipt` | V0163 | ✅ | 金额 + 核销/未核销 | `uk_biz_receipt_no` | 收款单 |
| 11 | `biz_receipt_item` | V0163 | ❌ 明细 | 核销额 + 快照 | `uk_biz_receipt_item_line` / `_ar` | 核销明细 |
| 12 | `biz_customer_statement` | V0164 | ✅ | 四个余额（20,4） | `uk_biz_stmt_no` / `uk_biz_stmt_period` | 对账单头 |
| 13 | `biz_customer_statement_line` | V0164 | ❌ 明细 | 借/贷 + 余额 | `uk_biz_stmt_line` | 对账明细 |

**软删规则与批次 1 完全一致**：单据头可软删（`deleted_at`），明细/流水/关联表永久行（物理删除由主表 `CASCADE` 或干脆禁止）。业务上"删除单据"一律走 `CANCELLED`，`deleted_at` 只用于录错的草稿。

**跨表金额口径总表**（避免"同一个词在两张表里含义不同"）

| 语义 | 存在哪 | 列名 | 精度 |
|------|--------|------|------|
| 售价不含税 | 订单头 / 出库头 | `total_amount` | 18,4 |
| 售价税额 | 订单头 / 出库头 | `total_tax` | 18,4 |
| 价税合计 | 订单头 / 出库头 / 应收 `amount` | `total_with_tax` / `amount` | 18,4 |
| 出库成本合计 | 出库头 | `total_cost` | 18,4 |
| 单件成本 | 出库明细 | `cost_price` | 18,6 |
| 明细成本额 | 出库明细 | `cost_amount` | 18,4 |
| 客户占用 | 信用表 | `order_used` / `ar_used` / `used_amount` | 18,4 |
| 对账余额 | 对账头/明细 | `opening_balance` / `closing_balance` / `balance` | 20,4 |

### 2.7 迁移文件清单（V0160–V0168）

| 迁移 | 文件名 | 内容 | 依赖 | 可重放 |
|------|--------|------|------|--------|
| V0160 | `V0160__erp_customer_credit.up.sql` | 3 张表（客户 / 额度 / 额度流水） | 无 | 是（`CREATE TABLE` 无 `IF NOT EXISTS`，按迁移器一次性执行） |
| V0161 | `V0161__erp_sale_order.up.sql` | 2 张表（订单 / 明细） | V0160 | 是 |
| V0162 | `V0162__erp_stock_out.up.sql` | 3 张表（出库 / 明细 / 序列号） | V0161、V0137/0139（库位库存行）/V0138（序列号） | 是 |
| V0163 | `V0163__erp_receivable_receipt.up.sql` | 3 张表（应收 / 收款 / 核销） | V0160、V0162 | 是 |
| V0164 | `V0164__erp_customer_statement.up.sql` | 2 张表（对账头 / 明细） | V0160、V0163 | 是 |
| V0165 | `V0165__erp_sales_permissions.up.sql` | 29 个权限码（menu id 1011–1039）+ 角色授权 | V0160–V0164、`sys_menu` | **是**（固定 UUID + `ON CONFLICT (id) DO NOTHING`） |
| V0166 | `V0166__erp_sales_account_seed.up.sql` | 5 个科目（1122 / 6001 / 2221.02 / 6401 / 1002） | `biz_account` | **是**（`ON CONFLICT (account_code)` 用 `WHERE NOT EXISTS` 改写，见下） |
| V0167 | `V0167__erp_sales_voucher_rules.up.sql` | 3 条凭证规则（`SALE_OUT` / `SALE_OUT_COST` / `RECEIPT`） | V0166 | **是** |
| V0168 | `V0168__erp_sales_seq_seed.up.sql` | 6 条单号序列（SO / OUT / AR / RCPT / CR / STS） | `biz_seq` | **是** |

> **迁移器约束（批次 1 已踩过）**：文件内**不写** `BEGIN; … COMMIT;`，迁移器自己在事务里执行；`down.sql` 顺序由迁移器保证（后建的先删）。

#### 2.7.1 V0165：权限码（29 个，menu id 1011–1039）

ID 段依据：实测已用到 1008，`1009/1010` 被 V0151 预占，因此从 **1011** 起连续分配。菜单节点（`menu_type=2`）自带 `:list` 码，按钮节点（`menu_type=3`）挂子权限——这是 V0141–V0151 的既有口径。

```sql
-- ---------- ① 父菜单挂在「ERP 台账」(…946) 下 ----------
-- 客户
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code) VALUES
 ('20000000-0000-0000-0000-000000001011','business:erp:customer:list',2,
  'business:erp:customer:list','/business/erp/customer',
  '20000000-0000-0000-0000-000000000946','business/erp/customer/index',
  11,1,NULL,'客户管理',TRUE,'business'),
 ('20000000-0000-0000-0000-000000001016','business:erp:credit:list',2,
  'business:erp:credit:list','/business/erp/customer-credit',
  '20000000-0000-0000-0000-000000000946','business/erp/customer-credit/index',
  12,1,NULL,'客户信用',TRUE,'business'),
 ('20000000-0000-0000-0000-000000001018','business:erp:sale-order:list',2,
  'business:erp:sale-order:list','/business/erp/sale-order',
  '20000000-0000-0000-0000-000000000946','business/erp/sale-order/index',
  13,1,NULL,'销售订单',TRUE,'business'),
 ('20000000-0000-0000-0000-000000001024','business:erp:stock-out:list',2,
  'business:erp:stock-out:list','/business/erp/stock-out',
  '20000000-0000-0000-0000-000000000946','business/erp/stock-out/index',
  14,1,NULL,'销售出库',TRUE,'business'),
 ('20000000-0000-0000-0000-000000001030','business:erp:receivable:list',2,
  'business:erp:receivable:list','/business/erp/receivable',
  '20000000-0000-0000-0000-000000000946','business/erp/receivable/index',
  15,1,NULL,'应收单',TRUE,'business'),
 ('20000000-0000-0000-0000-000000001032','business:erp:receipt:list',2,
  'business:erp:receipt:list','/business/erp/receipt',
  '20000000-0000-0000-0000-000000000946','business/erp/receipt/index',
  16,1,NULL,'收款单',TRUE,'business'),
 ('20000000-0000-0000-0000-000000001036','business:erp:statement:list',2,
  'business:erp:statement:list','/business/erp/customer-statement',
  '20000000-0000-0000-0000-000000000946','business/erp/customer-statement/index',
  17,1,NULL,'客户对账单',TRUE,'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 按钮权限（menu_type=3，parent = 对应的 :list 菜单） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code) VALUES
 ('20000000-0000-0000-0000-000000001012','business:erp:customer:detail',3,'business:erp:customer:detail',NULL,'20000000-0000-0000-0000-000000001011',NULL,1,1,NULL,'客户详情',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001013','business:erp:customer:create',3,'business:erp:customer:create',NULL,'20000000-0000-0000-0000-000000001011',NULL,2,1,NULL,'新建客户',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001014','business:erp:customer:update',3,'business:erp:customer:update',NULL,'20000000-0000-0000-0000-000000001011',NULL,3,1,NULL,'编辑客户',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001015','business:erp:customer:delete',3,'business:erp:customer:delete',NULL,'20000000-0000-0000-0000-000000001011',NULL,4,1,NULL,'删除客户',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001017','business:erp:credit:update',3,'business:erp:credit:update',NULL,'20000000-0000-0000-0000-000000001016',NULL,1,1,NULL,'调整信用额度',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001019','business:erp:sale-order:detail',3,'business:erp:sale-order:detail',NULL,'20000000-0000-0000-0000-000000001018',NULL,1,1,NULL,'订单详情',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001020','business:erp:sale-order:create',3,'business:erp:sale-order:create',NULL,'20000000-0000-0000-0000-000000001018',NULL,2,1,NULL,'新建订单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001021','business:erp:sale-order:update',3,'business:erp:sale-order:update',NULL,'20000000-0000-0000-0000-000000001018',NULL,3,1,NULL,'编辑订单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001022','business:erp:sale-order:delete',3,'business:erp:sale-order:delete',NULL,'20000000-0000-0000-0000-000000001018',NULL,4,1,NULL,'删除订单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001023','business:erp:sale-order:status',3,'business:erp:sale-order:status',NULL,'20000000-0000-0000-0000-000000001018',NULL,5,1,NULL,'订单状态流转',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001025','business:erp:stock-out:detail',3,'business:erp:stock-out:detail',NULL,'20000000-0000-0000-0000-000000001024',NULL,1,1,NULL,'出库详情',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001026','business:erp:stock-out:create',3,'business:erp:stock-out:create',NULL,'20000000-0000-0000-0000-000000001024',NULL,2,1,NULL,'新建出库单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001027','business:erp:stock-out:update',3,'business:erp:stock-out:update',NULL,'20000000-0000-0000-0000-000000001024',NULL,3,1,NULL,'编辑出库单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001028','business:erp:stock-out:delete',3,'business:erp:stock-out:delete',NULL,'20000000-0000-0000-0000-000000001024',NULL,4,1,NULL,'删除出库单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001029','business:erp:stock-out:status',3,'business:erp:stock-out:status',NULL,'20000000-0000-0000-0000-000000001024',NULL,5,1,NULL,'出库过账/作废',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001031','business:erp:receivable:detail',3,'business:erp:receivable:detail',NULL,'20000000-0000-0000-0000-000000001030',NULL,1,1,NULL,'应收详情',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001033','business:erp:receipt:detail',3,'business:erp:receipt:detail',NULL,'20000000-0000-0000-0000-000000001032',NULL,1,1,NULL,'收款详情',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001034','business:erp:receipt:create',3,'business:erp:receipt:create',NULL,'20000000-0000-0000-0000-000000001032',NULL,2,1,NULL,'登记收款',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001035','business:erp:receipt:status',3,'business:erp:receipt:status',NULL,'20000000-0000-0000-0000-000000001032',NULL,3,1,NULL,'收款过账/撤销',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001037','business:erp:statement:detail',3,'business:erp:statement:detail',NULL,'20000000-0000-0000-0000-000000001036',NULL,1,1,NULL,'对账单详情',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001038','business:erp:statement:generate',3,'business:erp:statement:generate',NULL,'20000000-0000-0000-0000-000000001036',NULL,2,1,NULL,'生成对账单',FALSE,'business'),
 ('20000000-0000-0000-0000-000000001039','business:erp:statement:confirm',3,'business:erp:statement:confirm',NULL,'20000000-0000-0000-0000-000000001036',NULL,3,1,NULL,'确认对账单',FALSE,'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权（空库首启时 sys_role 可能为空 → 只 NOTICE，不抛异常） ----------
DO $$
DECLARE n_menus integer; n_roles integer; n_grants integer;
BEGIN
    SELECT count(*) INTO n_menus FROM sys_menu
     WHERE id::text LIKE '20000000-0000-0000-0000-000000001%'
       AND right(id::text,4) BETWEEN '1011' AND '1039';
    IF n_menus <> 29 THEN
        RAISE EXCEPTION 'V0165 自检失败：预期 29 个权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0165：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放即可补齐';
        RETURN;
    END IF;

    -- 销售/客户：销售与销售主管可见；财务看应收/收款/对账（只读为主）
    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN sys_menu m
     WHERE r.deleted_at IS NULL
       AND m.id::text LIKE '20000000-0000-0000-0000-000000001%'
       AND right(m.id::text,4) BETWEEN '1011' AND '1039'
       AND r.role_code IN ('SUPER_ADMIN','ADMIN','TEST_UADMIN','SALES','SALES_MANAGER','FINANCE','ACCOUNTANT')
       AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id=r.id AND rm.menu_id=m.id);
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0165：销售 29 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
```

#### 2.7.2 V0166：科目种子（5 个）

`biz_account.account_code` 的唯一索引 `uk_biz_account_code` **不带** `WHERE deleted_at IS NULL`（实测），所以幂等只能写 `WHERE NOT EXISTS`：

```sql
-- ---------- 1122 应收账款（ASSET/DEBIT/leaf） ----------
INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, aux_dimensions, status)
SELECT '30000000-0000-4000-8000-000000000112'::uuid,'1122','应收账款',NULL,1,'ASSET','DEBIT',1,'客户',1
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code='1122');

-- ---------- 1002 银行存款 ----------
INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status)
SELECT '30000000-0000-4000-8000-000000000100'::uuid,'1002','银行存款',NULL,1,'ASSET','DEBIT',1,1
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code='1002');

-- ---------- 6001 主营业务收入（损益类，贷方） ----------
INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status)
SELECT '30000000-0000-4000-8000-000000000601'::uuid,'6001','主营业务收入',NULL,1,'PROFIT_LOSS','CREDIT',1,1
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code='6001');

-- ---------- 6401 主营业务成本（成本类，借方） ----------
INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status)
SELECT '30000000-0000-4000-8000-000000000641'::uuid,'6401','主营业务成本',NULL,1,'COST','DEBIT',1,1
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code='6401');

-- ---------- 2221.02 应交税费-销项税（挂在既有 2221 下，level=2） ----------
INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status)
SELECT '30000000-0000-4000-8000-000000000222'::uuid,'2221.02','应交税费-销项税',
       (SELECT id FROM biz_account WHERE account_code='2221'),2,'LIABILITY','CREDIT',1,1
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code='2221.02');

-- 自检：5 个科目必须全部可解析且为启用/分录可用
DO $$
DECLARE n integer;
BEGIN
    SELECT count(*) INTO n FROM biz_account
     WHERE account_code IN ('1122','1002','6001','6401','2221.02')
       AND status = 1 AND deleted_at IS NULL AND is_leaf = 1;
    IF n <> 5 THEN RAISE EXCEPTION 'V0166 自检失败：预期 5 个可用科目，实际 %', n; END IF;
END $$;
```

> **为什么 2221.02 要挂 `parent_id`**：实测 `2221 应交税费` 是 `is_leaf=0` 的汇总科目。销项税必须建成它的子级（`level=2`），否则科目树出现"叶子没父级"的脏结构——批次 1 建 `2221.01 进项税` 时就是这么做的（实测显示 `2221.01` 存在且 `2221` 的 `is_leaf=0`，但 `2221.01.parent_id` 为 NULL，是批次 1 的一处小瑕疵；本批**不复刻**这个瑕疵，正确挂父级，并在 §8.5 记为"可选的批次 1 补丁"）。

#### 2.7.3 V0167：凭证规则种子（3 条）

```sql
-- SALE_OUT：收入凭证（借 1122 / 贷 6001 / 贷 2221.02）
INSERT INTO biz_voucher_rule (id, rule_code, biz_type, debit_account_id, credit_account_id,
                              tax_account_id, is_active, remark)
SELECT '31000000-0000-4000-8000-000000000001'::uuid,'SALE_OUT_DEFAULT','SALE_OUT',
       (SELECT id FROM biz_account WHERE account_code='1122'),
       (SELECT id FROM biz_account WHERE account_code='6001'),
       (SELECT id FROM biz_account WHERE account_code='2221.02'),
       1,'销售出库-收入凭证：借应收账款 / 贷主营业务收入 + 销项税'
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule WHERE rule_code='SALE_OUT_DEFAULT');

-- SALE_OUT_COST：成本凭证（借 6401 / 贷 1405）
INSERT INTO biz_voucher_rule (id, rule_code, biz_type, debit_account_id, credit_account_id,
                              tax_account_id, is_active, remark)
SELECT '31000000-0000-4000-8000-000000000002'::uuid,'SALE_OUT_COST_DEFAULT','SALE_OUT_COST',
       (SELECT id FROM biz_account WHERE account_code='6401'),
       (SELECT id FROM biz_account WHERE account_code='1405'),
       NULL,
       1,'销售出库-成本凭证：借主营业务成本 / 贷库存商品'
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule WHERE rule_code='SALE_OUT_COST_DEFAULT');

-- RECEIPT：收款凭证（借 1002 / 贷 1122）
INSERT INTO biz_voucher_rule (id, rule_code, biz_type, debit_account_id, credit_account_id,
                              tax_account_id, is_active, remark)
SELECT '31000000-0000-4000-8000-000000000003'::uuid,'RECEIPT_DEFAULT','RECEIPT',
       (SELECT id FROM biz_account WHERE account_code='1002'),
       (SELECT id FROM biz_account WHERE account_code='1122'),
       NULL,
       1,'收款凭证：借银行存款 / 贷应收账款（按核销额入账，未核销部分不进此凭证）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule WHERE rule_code='RECEIPT_DEFAULT');

-- 自检：三条规则的三个账户都必须解析到有效科目
DO $$
DECLARE bad integer;
BEGIN
    SELECT count(*) INTO bad FROM biz_voucher_rule r
     WHERE r.rule_code IN ('SALE_OUT_DEFAULT','SALE_OUT_COST_DEFAULT','RECEIPT_DEFAULT')
       AND (r.debit_account_id IS NULL OR r.credit_account_id IS NULL);
    IF bad > 0 THEN RAISE EXCEPTION 'V0167 自检失败：% 条规则的借贷科目为空', bad; END IF;
END $$;
```

> `biz_voucher_rule` 没有 `biz_type` 唯一索引（只有 `rule_code` 唯一），而引擎按 `biz_type` 取"最早一条"（`ORDER BY created_at ASC LIMIT 1`）。因此**同一 `biz_type` 只能有一条启用规则**——这是引擎的隐含前提，本批在 §8.3 列为风险项（若运营误建第二条，生效的是最早那条）。本批不自作聪明加唯一索引（会与引擎的 `LIMIT 1` 语义冲突，且批次 1 已有数据）。

#### 2.7.4 V0168：单号序列种子（6 条）

```sql
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status)
SELECT v.id::uuid, v.seq_type, v.seq_name, v.prefix, 'yyyyMMdd', '', 0, 1, '-', 1
  FROM (VALUES
    ('32000000-0000-4000-8000-000000000001','SO','销售订单号','SO'),
    ('32000000-0000-4000-8000-000000000002','OUT','销售出库单号','OUT'),
    ('32000000-0000-4000-8000-000000000003','AR','应收单号','AR'),
    ('32000000-0000-4000-8000-000000000004','RCPT','收款单号','RCPT'),
    ('32000000-0000-4000-8000-000000000005','CUS','客户编码','CUS'),
    ('32000000-0000-4000-8000-000000000006','STS','对账单号','STS')
  ) AS v(id, seq_type, seq_name, prefix)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq s WHERE s.seq_type = v.seq_type);
```

单号形态（`voucher.rs::format_doc_no` 口径）：`OUT-20261031-000001`。

### 2.8 ER 关系（核心闭环）

```
                        ┌──────────────┐
                        │ biz_customer │
                        └──────┬───────┘
                 ┌─────────────┼──────────────────┐
                 │             │                  │
        ┌────────▼─────────┐   │        ┌─────────▼──────────┐
        │biz_customer_credit│  │        │biz_customer_statement│
        │  order_used/ar_used│ │        │  + _line            │
        └────────▲─────────┘   │        └─────────▲──────────┘
                 │ 占用/释放流水│                  │ 汇总来源
        ┌────────┴─────────┐   │                  │
        │credit_txn        │   │                  │
        └──────────────────┘   │                  │
                               │                  │
        ┌──────────────────────▼───┐   ┌──────────┴───────────┐
        │   biz_sale_order          │   │  biz_receivable      │
        │   + biz_sale_order_item   │   │  (amount/received/   │
        │   status / credit_hold    │   │   unreceived/due)    │
        └──────────┬───────────────┘   └──────────▲───────────┘
                   │ source_order_id               │ source_id
                   │                               │ (uk: source_type+source_id)
        ┌──────────▼───────────────┐              │
        │   biz_stock_out           │──────────────┘ 出库过账即生成
        │   voucher_id (收入)        │
        │   cost_voucher_id (成本)   │───► biz_voucher(×2)
        │   receivable_id           │
        └──────────┬───────────────┘
                   │
        ┌──────────▼───────────────┐      ┌──────────────────────────┐
        │ biz_stock_out_item        │─────►│biz_stock_out_item_serial │
        │  cost_price/cost_amount   │      │  → biz_material_serial   │
        │  location_id(库位库存行)   │      │     status IN→OUTBOUND   │
        └──────────┬───────────────┘      └──────────────────────────┘
                   │ 扣减
        ┌──────────▼───────────────┐      ┌──────────────────────────┐
        │ biz_inventory             │◄─────│ biz_inventory_location   │
        │ (成本账 qty/avg_cost/amount)│     │ (库位数量账 qty only)     │
        └──────────┬───────────────┘      └──────────────────────────┘
                   │ 每行一条
        ┌──────────▼───────────────┐
        │ biz_inventory_txn         │  txn_type='OUT' / biz_type='SALE_OUT'
        └──────────────────────────┘

        ┌──────────────────┐         ┌──────────────────┐
        │  biz_receipt     │────────►│ biz_receipt_item │──► 冲减 biz_receivable
        │  amount/核销/预收 │         │  before/after    │
        └────────┬─────────┘         └──────────────────┘
                 │ voucher_id
                 └──► biz_voucher (source_type='RECEIPT')
```

**闭环不变式（POC 已验，对应 §6.2 的巡检 SQL）**

| # | 不变式 | 违反后果 |
|---|--------|---------|
| I1 | `biz_inventory` 每行 `qty>0 → round(qty*avg_cost,4)=amount`；`qty=0 → amount=0` | 成本账崩 |
| I2 | `Σ biz_inventory_location.qty = biz_inventory(location_id IS NULL).qty`（该物料该仓） | 库位账与成本账打架 |
| I3 | 出库单 `total_cost = Σ 明细 cost_amount` 且 = 成本凭证借方合计 | 毛利错 |
| I4 | 出库单 `total_with_tax = 应收.amount` | 收入与应收不一致 |
| I5 | 收款单 `write_off_total = Σ 明细 amount` 且 `≤ amount` | 核销超额 |
| I6 | `应收.received_amount = Σ 核销明细 amount`（未撤销部分） | 应收余额错 |
| I7 | 信用 `used_amount = order_used + ar_used`；`order_used = Σ 未出库订单 credit_hold_amount` | 额度失控 |
| I8 | 一物一码：明细 `qty = COUNT(item_serial)` 且这些序列号 `status='OUTBOUND'` | 账实不符 |

### 2.9 DDL 真库执行记录

```bash
# ① 从批次 1 生产 schema 克隆（pg_dump -s，不动源库）
$ su postgres -c "createdb erp_p3d2_poc1"
$ su postgres -c "pg_dump -s -d cloudpivot_p2e2 -f /tmp/p2e2_schema.sql"
$ su postgres -c "psql -q -d erp_p3d2_poc1 -f /tmp/p2e2_schema.sql"
$ su postgres -c "psql -d erp_p3d2_poc1 -tAc \"select count(*) from information_schema.tables
     where table_schema='public' and table_name like 'biz\_%'\""
42

# ② 执行本批 DDL（V0160–V0164 的 13 张表，逐字取自 §2）
$ su postgres -c "psql -v ON_ERROR_STOP=1 -q -d erp_p3d2_poc1 -f ddl_v0160_0164.sql"
（无输出 = 全部通过）

# ③ 核对
$ su postgres -c "psql -d erp_p3d2_poc1 -tAc \"select table_name from information_schema.tables
     where table_schema='public' and table_name in ('biz_customer','biz_customer_credit',
       'biz_customer_credit_txn','biz_sale_order','biz_sale_order_item','biz_stock_out',
       'biz_stock_out_item','biz_stock_out_item_serial','biz_receivable','biz_receipt',
       'biz_receipt_item','biz_customer_statement','biz_customer_statement_line') order by 1\""
biz_customer
biz_customer_credit
biz_customer_credit_txn
biz_customer_statement
biz_customer_statement_line
biz_receivable
biz_receipt
biz_receipt_item
biz_sale_order
biz_sale_order_item
biz_stock_out
biz_stock_out_item
biz_stock_out_item_serial
```

**13/13 建表成功，无一条错误**（第一版 DDL 与最终版的差异只有 §1.8 ① 的外键目标与 §1.8 ② 的三值逻辑两处，均已修正）。

---
## 3. 核心引擎设计

本章写「怎么跑起来」，不写「应该怎样」。凡是能从批次 1 源码抄的，一律抄并标出处；凡是要新增的，一律说明为什么不改既有函数。

### 3.0 模块结构与文件清单

批次 1 的既有文件（`backend/src/business/erp/mod.rs` 已注册）本批**不改一列 DDL、不改一个函数签名**，只做三件事：新增文件、在 `mod.rs` 加 `pub mod`、在 `shared/error_code.rs` 加常量。

| 新文件 | 内容 | 行数估算 | 依据 |
|--------|------|---------|------|
| `customer_model.rs` | 客户主数据读 / 写 / 删前校验（引用检查）+ 取号 `CUS` | ~420 | 对齐 `model.rs` 的写法（列清单常量 + `from_row`） |
| `customer_dto.rs` | 客户建 / 改 / 筛选 / 卡片契约 + 校验 | ~330 | 对齐 `dto.rs` |
| `customer_handler.rs` | 客户 5 个接口 + `business:erp:customer:*` | ~380 | 对齐 `handler.rs`（`#[nik::permission]` + `#[nik::get/post]`） |
| `credit_model.rs` | 额度占用 / 平移 / 释放三条**单语句** + 流水落账 + 额度查询 | ~520 | 见 §3.5 |
| `credit_dto.rs` | 额度调整 / 流水筛选 / 额度卡片（可用额度、预警位） | ~260 | — |
| `credit_handler.rs` | 信用 4 个接口 | ~280 | — |
| `saleorder_model.rs` | 销售订单 7 态状态机 + 明细重建 + 提交时占用额度 + 出库回写 `shipped_qty` | ~900 | 对齐 `purchase_model.rs`（4 态）+ `transfer_model.rs`（6 态）的写法 |
| `saleorder_dto.rs` | 订单建 / 改 / 流转 / 可出库明细 / 额度预检 | ~420 | — |
| `saleorder_handler.rs` | 订单 8 个接口 | ~360 | — |
| `stockout_model.rs` | **过账引擎**（本批最重）：七步 + 成本结转 + 库位拣货 + 序列号 CAS + 双凭证 + 应收生成 | ~1400 | 对齐 `stockin_model.rs`（过账六步）的骨架 |
| `stockout_dto.rs` | 出库建 / 改 / 流转 / 拣货建议 / 可用序列号 | ~480 | — |
| `stockout_handler.rs` | 出库 8 个接口 | ~420 | — |
| `receivable_model.rs` | 应收查询 + 账龄汇总 + 到期提醒口径 | ~380 | 对齐 `payable.rs` + `payable_query.rs` |
| `receivable_dto.rs` / `receivable_handler.rs` | 应收 5 个接口 | ~330 | — |
| `receipt_model.rs` | **核销引擎**：单事务多张应收升序条件 UPDATE + 快照 + 预收 | ~700 | 见 §3.6 |
| `receipt_dto.rs` / `receipt_handler.rs` | 收款 5 个接口 | ~380 | — |
| `statement_model.rs` | 对账单生成（区间流水汇总）+ 确认 | ~450 | — |
| `statement_dto.rs` / `statement_handler.rs` | 对账 5 个接口 | ~320 | — |
| `voucher.rs`（**改**） | 新增 `create_auto_with` / `sale_out_lines` / `sale_out_cost_lines` / `receipt_lines` / `resolve_sales_accounts`，`create_auto` 签名不变 | +260 | 见 §3.7 |

**总量**：18 个新文件 + 1 个文件增量，约 8600 行 Rust（含单测约 40%）。这是 §7 排期的依据。

**命名一致性**：所有 `_model.rs` 一律只写 SQL + 行结构 + 事务编排，**不写 axum 类型**；所有 `_handler.rs` 一律只做「认证 → 解析 → 调 model → 包装 `Res`」，与批次 1 `stockin_handler.rs` 的 `create_stock_in / update_stock_in / change_stock_in_status / delete_stock_in` 分层一一对应。原因：单测要能绕过 `AuthClaims` 直接调核心函数（`stockin_handler.rs` 文件头第 18–22 行明确解释过这个取舍）。

### 3.1 出库过账：七步与事务骨架

出库过账是本批唯一的「多表原子写」核心，等价于批次 1 收货过账的六步（`stockin_handler.rs` 文件头）。本批七步，全部在**调用方开的一个事务里**：

```text
① 锁单头   SELECT … FROM biz_stock_out WHERE id = ? AND deleted_at IS NULL FOR UPDATE
           → status 必须 'DRAFT'，否则 409（重复过账 = 409，幂等）
② CAS 置状态 DRAFT → POSTED（posted_at / posted_by）：条件 UPDATE，0 行 = 并发被抢先 → 409
③ 明细逐行（按 material_id, location_id, line_no 升序）：
   3.1 判定扣减模式（库位行 / 整仓汇总桶，§3.3）
   3.2 条件扣减 biz_inventory（qty / amount / avg_cost 三列一起算，SQL 侧 ROUND）
   3.3 库位模式下同步扣 biz_inventory_location.qty（数量账）
   3.4 一物一码：序列号状态 CAS（IN_STOCK → OUTBOUND），行数必须 == 件数
   3.5 写 biz_inventory_txn（txn_type='OUT', biz_type='SALE_OUT'，before_* 用 after+增量反推）
   3.6 回填明细 cost_price / cost_amount / negative_qty
④ 汇总单头 total_cost（= Σ 明细 cost_amount，SQL 侧 SUM，不信 Rust 的累加）
⑤ 生成应收 biz_receivable（source_type='STOCK_OUT', source_id=出库单 id；uk 保证不重复）
⑥ 生成两张凭证：create_auto_with(收入 3 行) + create_auto_with(成本 2 行)
   回填 voucher_id / cost_voucher_id / receivable_id
⑦ 回写销售订单 shipped_qty（条件 `shipped_qty + 本次 ≤ qty`，0 行 = 超发 → 400 整笔回滚）
   + 订单状态推 PARTIAL_SHIPPED / SHIPPED
```

骨架（与 §3.5 / §3.6 的 SQL 一一对应，`ex` 为 `&mut Transaction`）：

```rust
/// 过账销售出库单（**不 commit**，事务由调用方收口）。
///
/// # 为什么签名吃 `&mut Transaction` 而不是 `&impl SqlExecutor`
/// 过账要跨 7 张表写，必须同一事务；批次 1 的 `create_auto` 吃 `&impl SqlExecutor`，
/// `&mut Transaction` 实现了该 trait，所以凭证引擎可以直接在事务里调用（不改签名）。
pub async fn post_stock_out(
    tx: &mut Transaction<'_>,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    // ① 锁单头 + 状态闸
    let header = lock_stock_out(tx, id).await?;            // SELECT … FOR UPDATE
    if header.status != STATUS_DRAFT {
        return Err(DbError::InvalidValue(format!(
            "出库单 {} 当前状态 {}，不可过账（只有 DRAFT 能过账）", header.out_no, header.status)));
    }
    // ② CAS 置 POSTED：0 行 = 被并发抢先
    if !cas_post(tx, id, operator, now).await? {
        return Err(DbError::InvalidValue(format!("出库单 {} 状态已被他人改变，请刷新重试", header.out_no)));
    }
    // ③ 逐行扣减（顺序即加锁顺序，§3.10）
    let items = items_of(tx, id).await?;
    for item in &items {
        deduct_line(tx, &header, item, now).await?;         // 3.1–3.6
    }
    // ④ 汇总成本（SQL 侧 SUM，不在 Rust 侧累加）
    let total_cost = sum_item_cost(tx, id).await?;
    // ⑤ 应收
    let receivable_id = receivable_model::create_from_stock_out(tx, &header, total_cost, now).await?;
    // ⑥ 两张凭证（收入 / 成本）
    let revenue_voucher = voucher::create_auto_with(tx, &revenue_req(&header), &revenue_lines, operator, now).await?;
    let cost_voucher    = voucher::create_auto_with(tx, &cost_req(&header), &cost_lines,    operator, now).await?;
    // ⑦ 收口：单头回填 + 订单回写
    finalize_header(tx, id, receivable_id, revenue_voucher, cost_voucher, total_cost, now).await?;
    backfill_sale_order(tx, &header).await?;
    Ok(())
}
```

> **为什么 ③ 在 ② 之后**：② 的 CAS 是「本单只被过账一次」的闸，先把闸关上，再动库存。反过来（先扣库存再 CAS）在并发重复过账时会扣两次库存然后回滚第二次，虽然结果对，但白锁了整批库存行，热点行上的等待时间翻倍。

### 3.2 成本结转（出库侧的三个不变量）

#### 3.2.1 条件扣减：与批次 1 调拨**同一段实现**

批次 1 的调拨出仓用条件 UPDATE 防超卖，源码注释把理由写死了（`transfer_model.rs:1357-1362`，逐字引用）：

```text
// 2. **并发防超卖**用条件 UPDATE（`AND qty >= $1` + `RETURNING`），不用显式 `FOR UPDATE`：
//    返回 0 行 = 库存不足，调用方整笔回滚。PG 在拿到行锁后会按新版本重判谓词，
//    所以两笔同时扣同一行只有一个能成功（实测：库存 8、两笔各扣 5 → 一个 200 一个 409）。
```

本批**照抄这条结论**，SQL 与 `transfer_model::deduct_inventory_row`（`transfer_model.rs:1847`）同构，只把流水类型换掉：

```sql
-- 严格模式（biz_warehouse.allow_negative = 0）
UPDATE biz_inventory
   SET qty         = qty - NULLIF($1,'')::numeric,
       amount      = amount - ROUND(NULLIF($1,'')::numeric * avg_cost, 4),
       last_txn_at = $2,
       updated_at  = $2
 WHERE id = $3::uuid
   AND qty >= NULLIF($1,'')::numeric                      -- ← 超卖在这里被挡
RETURNING (qty + NULLIF($1,'')::numeric)::numeric(18,4) AS before_qty,
          qty::numeric(18,4)                            AS after_qty,
          avg_cost::numeric(18,6)                       AS unit_cost,
          ROUND(NULLIF($1,'')::numeric * avg_cost, 4)::numeric(18,4) AS out_amount,
          amount::numeric(18,4)                          AS after_amount;
```

**三个必须照抄的细节**（少一个就出错，都是批次 1 踩过的）：

| # | 细节 | 出处 / 后果 |
|---|------|------------|
| 1 | `before_qty` / `before_amount` **必须**用 `qty + 本次量` / `amount + 本次额` 反推 | `RETURNING` 看到的是**新行**，直接写 `amount AS before_amount` 会拿到错数（POC-1b 实测：期望 `1012.34`，实际 `344.1956`，见 §1.8 ③） |
| 2 | 金额一律 `ROUND(qty * avg_cost, 4)` 在 **SQL 侧**算 | Rust `Decimal` 是四舍六入五成双、PG numeric 是四舍五入，各算一遍会在半分位差 1 分（`transfer_model.rs:1363-1365` 原话） |
| 3 | `avg_cost` 出库时**不动** | 移动加权平均的均价只被入库改变；出库按当前均价结转，改均价会让「同一天两次出库成本不同」 |

#### 3.2.2 尾差倒挤：清空那一笔用「结存金额」

逐件出库时 `ROUND(1 * 10.123456, 4) = 10.1235`，但结存金额是 `ROUND(3 * 10.123456,4) = 30.3704`；出到第 3 件时 `10.1235 × 3 = 30.3705 ≠ 30.3704`，**差 1 分**。这批差额不清掉，`qty = 0` 而 `amount = 0.0001`，不变式 I1 当场判假。

POC-1d / 1e 实测这条路（真库输出，见 §9.1）：

```text
--- 1d 尾差：3 件 / avg 10.123456 / amount 30.3704 逐件出库 2 次 ---
   qty   | avg_cost  |  amount  | recomputed | consistent_before_clear
---------+-----------+----------+------------+-------------------------
 1.0000  | 10.123456 | 10.1234  |    10.1235 | f
--- 1e 最后一件按「倒挤」清空（out_cost = 结存金额） ---
 clear_out_cost | avg_cost
----------------+----------
        10.1234 | 10.123456
   qty   | amount
---------+--------
  0.0000 | 0.0000
```

**结论（本批口径）**：

```sql
-- 出库金额 = 若本次出完（qty = 本次量）则「结存金额」，否则 ROUND(qty * avg_cost, 4)
amount = amount - CASE WHEN qty = NULLIF($1,'')::numeric
                       THEN amount                                -- 倒挤：最后一次把余额清成 0
                       ELSE ROUND(NULLIF($1,'')::numeric * avg_cost, 4) END
```

`before_amount` 的反推公式随之变为：`before_amount = after_amount + (CASE … END 的本次额)`，即先算本次额再回读，**不**再用 `after + ROUND(qty*avg_cost)` 硬推（那条在倒挤那笔上是错的）。

#### 3.2.3 负库存：用批次 1 已有的 `allow_negative`，成本取「最近一条 IN 的单价」

**不加表、不加列**。批次 1 的 `biz_warehouse.allow_negative SMALLINT NOT NULL DEFAULT 0` 已经存在（`\d biz_warehouse` 实测，见 §1.3），一个仓库一个开关，粒度正好。

**成本口径**（POC-2 实测，见 §9.1）：

```text
--- 2a 该物料该仓的最后一次入库单价（负库存成本依据） ---
 txn_type |   doc_no    |   qty   | unit_cost
----------+-------------+---------+-----------
 IN       | IN-POC-0001 | 10.0000 | 10.000000
--- 2b 无库存出库 5 件 → qty=-5 / amount=-50 / avg_cost=10（取最近入库价，不用 0） ---
   qty   | avg_cost  |  amount
---------+-----------+---------
 -5.0000 | 10.000000 | -50.0000
--- 2c 后续入库 30 @ 12 冲平：金额 = -50+360 = 310，qty = 25，均价 = 12.4 ---
   qty   |  amount  | avg_cost  | recomputed
---------+----------+-----------+-----------
 25.0000 | 310.0000 | 12.400000 |   310.0000
```

三条硬规则：

1. **成本依据不是 0，是「该物料该仓最近一条 `IN` 流水的 `unit_cost`」**：

```sql
SELECT unit_cost
  FROM biz_inventory_txn
 WHERE material_id = $1 AND warehouse_id = $2 AND txn_type = 'IN' AND unit_cost IS NOT NULL
 ORDER BY created_at DESC, id DESC
 LIMIT 1;
```

   *为什么不用 0*：均价 0 会让出库成本 0 → 毛利虚高 100%；而且 `amount` 在 `qty<0` 时不为 0，`avg_cost` 会被反算成 `amount/qty` 的怪值，污染整条成本链。
   *为什么不用「最近一次出库价」*：出库价来自上一个均价，负库存场景下它可能本身就是推出来的，越推越远。

2. **负库存行只在 `allow_negative = 1` 的仓库发生，且必须打标记**：`biz_stock_out_item.negative_qty = TRUE`（列已建好，注释「本行是否允许/已产生负库存」）。这样报表能一眼挑出「先发货后补入库」的单子，月结时人工确认。

3. **后续入库用移动加权**自然冲平，**不追补历史凭证**：`new_amount = old_amount + in_amount; new_qty = old_qty + in_qty; new_avg = ROUND(new_amount/new_qty, 6)`（POC-2c：`(-50+360)/25 = 12.4`）。追补历史 = 悄悄改已过账的成本凭证，会计上不可接受（红冲只能是显式动作）。

**实现分支**：过账时先读一次 `biz_warehouse.allow_negative`（同一事务内 `SELECT … `，不加锁——仓库行是低频写主数据），然后二选一：

```sql
-- A. 严格：AND qty >= $qty                （0 行 → 400 + 66301 库存不足）
-- B. 允许负：WHERE id = $3::uuid           （无谓词；avg_cost 为 0 时用 last_in_cost 兜底）
```

严格模式下**绝不**把 `allow_negative` 的判断写进 SQL 的或条件（`AND (qty >= $qty OR (SELECT allow_negative …) = 1)`）：一旦写进去，执行计划里多一个子查询、锁粒度不可控，而且 POC 里「库存 8 两笔各 5」的经典超卖场景会静默通过。分支在应用层选，SQL 保持两条纯净语句。

#### 3.2.4 为什么出库不用 `FOR UPDATE`（与批次 1 一致的边界）

| 动作 | 批次 1 的做法 | 出处 | 本批 |
|------|--------------|------|------|
| 入库（要重算均价） | `SELECT … FOR UPDATE` 锁行 + 多行按 `material_id` 升序 | `stockin_model.rs:42`、`:989 lock_inventory` | 出库不涉及均价重算 → 不同 |
| 调拨出仓（扣减） | 条件 `UPDATE … AND qty >= ?` + `RETURNING` | `transfer_model.rs:1847 deduct_inventory_row` | **照抄** |
| 调拨入仓（加库存） | `SELECT … FOR UPDATE` 再 `UPDATE` | `transfer_model.rs:1932` | 出库不涉及 |

一句话边界：**「要读-算-写三步」用悲观锁，「一步条件写」用条件 UPDATE**。出库的成本是 `qty × 现成 avg_cost`，一步就能写完，所以用条件 UPDATE；入库要先算新均价再写，所以必须锁。

### 3.3 库位拣货与两级账（D7）

#### 3.3.1 模式判定：一次查询决定后面所有 SQL

```sql
-- 该物料该仓是否启用库位记账（有库位行 = 启用）
SELECT count(*) > 0 AS location_mode
  FROM biz_inventory_location
 WHERE material_id = $1 AND warehouse_id = $2 AND deleted_at IS NULL;
```

* `location_mode = false` → 只扣 `biz_inventory` 里 `location_id IS NULL` 的**整仓汇总桶**（批次 1 的 `recompute_quantity` 与巡检都只认这一桶，`transfer_model.rs:1350-1354` 原话）；
* `location_mode = true` → 必须落库位：`biz_stock_out_item.location_id` 收 **`biz_inventory_location.id`**，同时扣桶与库位行（桶是成本账，库位行是数量账）。

> 这一条是 §1.8 ① 的实测纠偏：`biz_inventory.location_id` 与 `biz_stock_out_item.location_id` 都指向 `biz_inventory_location.id`，**不是** `biz_location.id`。写错的表现是建表时就报 `fk_biz_inv_location_id` 违反（POC 真实报错见 §1.8 ①）。

#### 3.3.2 明细能否指定库位：能，且是首选

`biz_stock_out_item.location_id` 可空：

| 入参 | 行为 | 明细行数 |
|------|------|---------|
| 指定 `location_id`（库位库存行 id）+ `qty` | 只扣该库位行 | 1 行 |
| 不指定 `location_id`，仓为库位模式 | 按 §3.3.3 贪心分配，**展开成多行**（一个库位一次扣减 = 一行，`line_no` 递增） | N 行 |
| 不指定，仓为整仓模式 | 只扣汇总桶 | 1 行（`location_id` 为 NULL） |
| 指定了，但该库位行不存在 / 不属于该物料该仓 | 400 + `66304`（库位库存行不存在） | — |
| 指定了，但库位行 `qty < 需求` 且仓不允许负库存 | 400 + `66301`（库存不足，消息带库位码） | — |

**为什么要展开多行而不是「一行 + 若干库位分摊表」**：批次 1 的调拨明细表 `biz_stock_transfer_item` 已经是「一库位一行」（`from_location_id` / `to_location_id`），本批沿用同一形状；再开一张分摊子表等于在同一系统里造第二套库位记账口径，巡检 I2 没法写。

#### 3.3.3 未指定库位的拣货顺序：`qty DESC, location_code ASC`

POC-3c 实测（真库输出，见 §9.1）：

```sql
SELECT il.id AS inv_location_id, l.location_code, il.qty
  FROM biz_inventory_location il
  JOIN biz_location l ON l.id = il.location_id
 WHERE il.material_id = $1 AND il.warehouse_id = $2
   AND il.qty > 0 AND il.deleted_at IS NULL
 ORDER BY il.qty DESC, l.location_code ASC;      -- 先拣满的，同量按库位码稳定排序
```

```text
           inv_location_id            | location_code |  qty
--------------------------------------+---------------+-------
 ab000000-0000-4000-8000-000000000003 | A-03          | 5.0000
 ab000000-0000-4000-8000-000000000002 | A-02          | 3.0000
 ab000000-0000-4000-8000-000000000001 | A-01          | 2.0000
```

**为什么「先拣满的」而不是「先进先出 / 按库位码顺序」**：

| 策略 | 优点 | 为什么本批不选 |
|------|------|---------------|
| `qty DESC, code ASC`（本批） | 减少拣货行数（5+2 而非 2+3+2），单据短、错误少；同量按码排序保证**同一输入必得同一输出**（可测、可复现） | — |
| 按库位码 ASC（走道顺序） | 物理动线最短 | 会把大库位切成碎片，出库单行数暴涨；且库位码与货架顺序无强制关系（`biz_location.location_code` 只保证仓内唯一） |
| 先进先出（按 `biz_inventory_txn` 首入时间） | 适合有保质期物料（FEFO 的简化） | 批次 1 的库位行**没有批次/入库时间列**（`biz_inventory_location` 只有 `qty/locked_qty/last_txn_at`），实现 FIFO 要新增列 = 改批次 1 表结构，红线不允许 |
| 用户手工指定 | 最准 | 大单不可能手工；本批作为**可选输入**保留（§3.3.2 第一行） |

剩余量不足时：允许跨库位续拣；若全仓合计仍不足 → 按 §3.2.3 的 `allow_negative` 决定「400 拒绝」还是「最后一笔落负库存 + `negative_qty=true`」。

#### 3.3.4 两级账同步与不变式 I2

每拣一行（`qty = n`，库位行 id = `il_id`）：

```sql
-- ① 成本账（汇总桶）：价 / 量 / 额
UPDATE biz_inventory SET qty = qty - $n, amount = amount - ROUND($n * avg_cost, 4), … WHERE id = $bucket AND qty >= $n;
-- ② 数量账（库位行）：只动 qty
UPDATE biz_inventory_location SET qty = qty - $n, last_txn_at = $now WHERE id = $il_id AND qty >= $n;
```

两条都返回 0 行 → 回滚整单。POC-3d / 3e 验证了「出库 7 件：A-03(5) + A-02(2) → 剩余 A-02=1、桶=3、Σ库位=3」，I2 成立：

```text
 bucket_qty | bucket_amount        sum_loc | bucket | i2_holds
------------+---------------      ---------+--------+----------
     3.0000 |       24.0000         3.0000 | 3.0000 | t
```

> **成本只在桶上算，库位行不含金额**：`biz_inventory_location` 没有 `amount` / `avg_cost` 列（实测 `\d`）。想让每个库位有成本，需要批次 1 迁移加列——本批**不做**，并在 §8 记为「库位级毛利」的已知限制。

### 3.4 序列号出库（D6）

状态机不可逆：`IN_STOCK → OUTBOUND → SCRAPPED`（批次 1 已定，`error_code::ERP_SERIAL_STATUS_INVALID = 66008` 的文档注释逐字列出允许的三条边）。

**CAS 一条 SQL 解决「两单抢同一个码」**（POC-7 实测：第一笔 1 行、第二笔 0 行）：

```sql
UPDATE biz_material_serial
   SET status = 'OUTBOUND', outbound_at = $now, updated_at = $now, update_by = $op,
       source_type = 'STOCK_OUT', source_id = $out_id
 WHERE id = ANY($serial_ids::uuid[])
   AND status = 'IN_STOCK'
   AND deleted_at IS NULL
RETURNING id, serial_no;
```

四道校验（缺一就会出现「账实不符」）：

| # | 校验 | 失败码 | 说明 |
|---|------|--------|------|
| 1 | 物料 `serial_required = TRUE` | 400 + `66007` | 复用批次 1 的码（「去物料主数据打开开关」） |
| 2 | 明细 `qty = 序列号件数`（一物一码） | 400 + `66305` | 数量 ≠ 件数是本批最常见的人工错；`biz_stock_out_item_serial` 只是关联表，不校验就对不上 |
| 3 | 序列号必须属于「该物料 + 该仓（+ 该库位，若明细定了库位）」 | 400 + `66306` | 跨仓扫到别的仓的码，物理上不该出库 |
| 4 | `RETURNING` 行数 == 期望件数 | 409 + `66307` | 少一行 = 有码已被别的单抢走（并发）；整单回滚，前端提示重扫 |

**数量语义**：一物一码物料的 `biz_stock_out_item.qty` 是**重量/数量口径**，件数口径由序列号行数表达；二选一时以序列号为准（`qty` 必须等于件数，按 `1:1`；若物料按重量计价且一物一码，则本批**不支持**，见 §9.3 Q1）。

**前端扫码方案**：见 §5.4（`stockout-logic.ts` 里的 `isScannerInput` / `mergeScan` + 扫码枪 HID 输入 + 连续扫码去重 + 落选清单）。

### 3.5 客户信用（D4）

#### 3.5.1 三个时间点，三种占用

| 时点 | 动作 | `order_used` | `ar_used` | `used_amount` | 触发方 |
|------|------|-------------|-----------|---------------|--------|
| 订单**提交**（DRAFT→SUBMITTED） | 占用 | `+订单价税合计` | 不变 | `+`（同步） | `saleorder_model::submit` |
| 订单**作废/关闭/拒绝** | 释放 | `-credit_hold_amount` | 不变 | `-` | `saleorder_model::cancel/close` |
| 出库**过账** | 平移 | `-本单本次出库额` | `+同额` | **不变** | `stockout_model::post_stock_out` |
| 收款**核销** | 释放 | 不变 | `-核销额` | `-` | `receipt_model::apply` |
| 额度**调整** | 只改 `credit_limit` | 不变 | 不变 | 不变 | `credit_model::adjust` |

**为什么下单就占、而不是出库才占**（任务书列的 A / B / C 三案里选 A+C 的混合）：只在出库时占用（方案 C）会出现「额度 10 万、销售签 100 万的单全部能提交，最后发货时才发现全卡住」——这是销售场景里最贵的失败（客户在现场等货）。下单占用的代价只是「提交时多一次条件 UPDATE」（热点行，毫秒级）。

#### 3.5.2 并发占用：单语句条件 UPDATE（POC-5 实测）

```sql
-- 占用（提交订单时）：一条语句，无读-判-写窗口
UPDATE biz_customer_credit
   SET order_used  = order_used + $amt,
       used_amount = used_amount + $amt,
       version     = version + 1,
       updated_at  = $now,
       update_by   = $op
 WHERE customer_id = $cid::uuid
   AND deleted_at IS NULL
   AND status = 1
   AND (credit_limit = 0 OR used_amount + $amt <= credit_limit)   -- 0 = 不限额度
RETURNING order_used, ar_used, used_amount, credit_limit,
          (CASE WHEN credit_limit = 0 THEN NULL ELSE credit_limit - used_amount END) AS available;
```

`RETURNING` 0 行 = 超限（或客户无额度行）→ **409 + `66101`**，消息里必须带「本次需要 / 可用 / 超额」三个数（销售要拿这句话去和客户谈）：

```text
客户「北京某某」信用额度不足：本单需占用 80000.0000，可用 20000.0000，超出 60000.0000。
```

POC-5 的真实输出（两个会话并发抢 10 万额度）：

```text
--- 5a 第一笔 8 万 → 成功，可用 2 万 ---
 order_used | ar_used | used_amount | available
------------+---------+-------------+-----------
 80000.0000 |  0.0000 |  80000.0000 | 20000.0000
--- 5b 第二笔 8 万 → 0 行（超限） ---
 order_used | used_amount
------------+-------------
(0 rows)
--- 5c 出库平移：order_used → ar_used，used_amount 不变 ---
 order_used |  ar_used   | used_amount
------------+------------+-------------
     0.0000 | 80000.0000 |  80000.0000
```

**为什么不用 `SELECT … FOR UPDATE` 再判断**（方案 B）：那要「读 → 判断 → 写」三步，热点行锁持有时间 = 三步的网络往返，同一客户的并发下单会排队；单语句条件 UPDATE 只有一个往返。这与批次 1 「出库用条件 UPDATE」的理由完全一致（§3.2.4）。

#### 3.5.3 出库平移：总额不变，所以不需要再判额度

```sql
UPDATE biz_customer_credit
   SET order_used = order_used - $amt,
       ar_used    = ar_used    + $amt,      -- used_amount 不变（CHECK 强制自洽）
       version    = version + 1,
       updated_at = $now
 WHERE customer_id = $cid::uuid
   AND order_used >= $amt;                  -- 0 行 = 订单占用被别处释放过 → 数据异常
```

0 行 → **500 + `66102`**（不是 400：这是内部不一致，正常路径不该发生，要让监控抓到）。这个设计的好处是：出库、收款**都不再判额度**，并发判断收敛到「提交订单」唯一一处。

#### 3.5.4 核销释放

```sql
UPDATE biz_customer_credit
   SET ar_used    = ar_used - $written_off,
       used_amount = used_amount - $written_off,
       version    = version + 1,
       updated_at = $now
 WHERE customer_id = $cid::uuid
   AND ar_used >= $written_off;
```

0 行 → 500 + `66102`（同上）。**收款单只释放「已核销」部分**，预收（`unapplied_amount`）不释放额度——预收不是「欠款被还」，是「客户先给钱」，额度仍被未收的应收占着。

#### 3.5.5 流水表与自洽闸

每次占用 / 平移 / 释放都写一条 `biz_customer_credit_txn`，并用**幂等键**挡重复提交：

| 列 | 取值 |
|----|------|
| `txn_type` | `ORDER_HOLD` / `ORDER_RELEASE` / `AR_INCREASE` / `AR_RELEASE` / `LIMIT_ADJUST` / `MANUAL_RELEASE`（CHECK 已限定） |
| `direction` | `1` 占用 / `-1` 释放 |
| `source_type` + `source_id` | `SALE_ORDER` / `STOCK_OUT` / `RECEIPT` / `MANUAL` + 单据 id |
| `idem_key` | `{txn_type}:{source_type}:{source_id}`，唯一索引 `uk_biz_credit_txn_idem` |
| `order_used_before/after`、`ar_used_before/after` | 由条件 UPDATE 的 `RETURNING` 反推（`before = after ∓ amount`），**不在 UPDATE 前 SELECT**（少一次读，也不破坏锁序） |

DB 层还有两道闸（POC-5d 实测直写 `used_amount=1` 被拒）：

```text
ERROR:  new row for relation "biz_customer_credit" violates check constraint "ck_biz_credit_used"
DETAIL:  Failing row contains (…, 100000.0000, 0.0000, 80000.0000, 1.0000, 2, …).
```

* `ck_biz_credit_used`：`used_amount = order_used + ar_used`（三个数永远自洽，客户端不能只写一个）；
* `ck_biz_credit_txn_order_math` / `ck_biz_credit_txn_ar_math`：流水的 before/after 必须与 `txn_type` 一致（`ORDER_HOLD` 必须 `after = before + amount`）。

#### 3.5.6 三种客户信用状态的处理

| `credit_status` | 下单 | 出库 | 说明 |
|-----------------|------|------|------|
| `NORMAL` | 按额度判定 | 放行 | 默认 |
| `WATCH` | 放行，但**强制走审批**（`biz_flow_def` 的 `SALE_ORDER` 流程），`credit_hold_amount` 照占 | 放行（已审批） | 「观察」= 可以卖但要人看 |
| `BLOCKED` | **400 + `66103`** 直接拒 | 拒（除非已审批的特殊放行单） | 「冻结」= 停止赊销 |

额度行不存在时：**不**自动建行（否则并发下单会争着 `INSERT` 打唯一索引），由客户建档时同事务建额度行（`credit_model::ensure_row`，`ON CONFLICT (customer_id) DO NOTHING`）。

### 3.6 核销引擎（D5）

一张收款单核销多张应收，全部在**一个事务**里，按 `receivable_id` **升序**逐张条件 UPDATE（升序 = 固定的加锁顺序，防两笔收款互相死锁）。

```rust
/// 收款核销（不 commit）。
///
/// # 校验顺序（每条都在写之前）
/// 1. 收款单存在、`status='DRAFT'`；
/// 2. Σ明细 amount <= 收款单 amount（DB 的 `ck_biz_receipt_writeoff` 是兜底）；
/// 3. 明细里 `receivable_id` 去重（`uk_biz_receipt_item_ar` 兜底）；
/// 4. 每张应收的客户 == 收款单客户（跨客户核销 = 400 + 66501）；
/// 5. 每张应收 `status IN ('OPEN','PARTIAL_RECEIVED')`（已核销/已作废 = 400 + 66502）。
pub async fn apply_write_off(tx: &mut Transaction<'_>, req: &WriteOffReq, op: Option<Uuid>, now: DateTime<Utc>) -> DbResult<()> {
    // …校验 1–5…
    // ⑥ 按 receivable_id 升序逐张条件扣减
    let mut lines = req.lines.clone();
    lines.sort_by_key(|l| (l.receivable_id, l.line_no));
    for line in &lines {
        let row = conditional_write_off(tx, line, now).await?;    // 0 行 = 需再诊断，见 3.6.1
        let (before, after) = match row {
            Some((b, a)) => (b, a),
            None => return Err(DbError::InvalidValue(format!(
                "应收 {} 核销失败：本次核销 {} 超过未收余额，或该应收已被他人核销/作废",
                line.receivable_no, line.amount))),
        };
        insert_receipt_item(tx, req.receipt_id, line, before, after, now).await?;
        bump_receivable_status(tx, line.receivable_id, now).await?;   // OPEN → PARTIAL_RECEIVED → SETTLED
    }
    // ⑦ 收款单汇总 + 预收
    finalize_receipt(tx, req, now).await?;                       // write_off_total / unapplied_amount
    // ⑧ 信用释放（只释放核销部分）
    credit_model::release_ar(tx, req.customer_id, sum_write_off(&lines), op, now).await?;
    // ⑨ 收款凭证（借 1002 / 贷 1122，按核销额）
    let vid = voucher::create_auto_with(tx, &receipt_req(req), &receipt_lines, op, now).await?;
    set_receipt_voucher(tx, req.receipt_id, vid).await?;
    Ok(())
}
```

条件 UPDATE（每张应收一条语句，`0 行` 就是失败）：

```sql
UPDATE biz_receivable
   SET received_amount   = received_amount + $amt,
       unreceived_amount = unreceived_amount - $amt,
       updated_at        = $now
 WHERE id = $rid::uuid
   AND status IN ('OPEN','PARTIAL_RECEIVED')
   AND unreceived_amount >= $amt          -- ← 超额在这里被挡
RETURNING (unreceived_amount + $amt)::numeric(18,4) AS before_unreceived,
          unreceived_amount::numeric(18,4)          AS after_unreceived;
```

POC-6 实测（核销 600 → 再核销 500 时剩余 400 → 0 行；伪造明细被 CHECK 拒）：

```text
--- 6a 核销 600 → PARTIAL_RECEIVED ---
 received_amount | unreceived_amount |      status
-----------------+-------------------+------------------
        600.0000 |          400.0000 | PARTIAL_RECEIVED
--- 6b 再核销 500（剩余 400）→ 0 行 ---
(0 rows)
--- 6c 核销明细快照自证 ---
 line_no |  amount  | before_unreceived | after_unreceived
---------+----------+-------------------+------------------
       1 | 600.0000 |         1000.0000 |         400.0000
--- 6d 伪造明细（after 999 ≠ 1000-600）→ CHECK 拒绝 ---
ERROR:  new row for relation "biz_receipt_item" violates check constraint "ck_biz_receipt_item_after"
```

#### 3.6.1 条件 UPDATE 返回 0 行时的三段诊断（不能一条 0 行含糊返回 400）

`conditional_write_off` 的 0 行有三种完全不同的成因，对操作员的处置动作也不同。**必须再读一次该应收行**（无锁 `SELECT`，此时已在事务内，读到的就是最新已提交状态）来区分：

| 再读结果 | 成因 | HTTP + 码 | 操作员该做什么 |
|---------|------|-----------|---------------|
| 行不存在 / `deleted_at IS NOT NULL` | 应收被删 | 404 + `10002` | 刷新列表 |
| `status IN ('SETTLED','CANCELLED')` | 业务前置不满足（已收完或已作废） | 400 + `66502` | 换一张应收 |
| `unreceived_amount < 本次核销额` | **并发**：别人先核销走了一部分 | 409 + `66404` | 刷新后按新余额重分 |
| `unreceived_amount >= 本次核销额` 且状态可核销 | 理论不可达（谓词应当命中） | 500 + `10004` | 报警，查隔离级别/数据 |

> 这条与 §4.1.2 调额接口的"0 行必须再诊断"是同一条原则：**SQL 的 0 行是结果不是原因**，把原因猜错会让一线拿着错误提示去改错东西（"余额不足"和"别人刚核销了"是两个完全不同的动作）。

**为什么明细表要存 `before/after` 快照**：核销是「一笔收款改多张应收」的批量动作，事后对账（客户说「我付的这 600 记到哪张了」）只能靠明细行自证。`ck_biz_receipt_item_after`（`after = before - amount`）让「金额对不上」在 DB 层插不进去，不依赖应用层自觉。

**预收（unapplied）**：`write_off_total < amount` 时差额进 `unapplied_amount`（`ck_biz_receipt_unapplied` 强制 `unapplied = amount - write_off_total`）。预收不生成应收冲减、不释放额度；后续可用「预收转核销」接口再次核销（同一收款单二次核销 = 新增明细行，`uk_biz_receipt_item_ar` 挡住同一应收重复挂）。

**撤销（CANCELLED）**：反向逐张「减回」`received_amount`、加回 `unreceived_amount`、状态回退（`SETTLED → PARTIAL_RECEIVED → OPEN` 按余额重算），删除对应明细行的效果用**红字凭证**表达而非删行（凭证侧 `reverse_*`），核销明细保留（审计要求：核销发生过就必须留痕）。

### 3.7 凭证引擎复用（D9，本批最关键的一条）

#### 3.7.1 现状：真实函数签名（`voucher.rs`，80 KB / 2037 行）

| 函数 | 签名（逐字取源码） | 行号 | 能否直接复用 |
|------|------------------|------|-------------|
| `next_doc_no` | `pub async fn next_doc_no(ex: &impl SqlExecutor, spec: &SeqSpec<'_>, now: DateTime<Utc>) -> DbResult<String>` | 195 | ✅ 完全复用（本批 6 个单号都用它） |
| `next_voucher_no` | `pub async fn next_voucher_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String>` | 220 | ✅ |
| `period_of` | `pub async fn period_of(ex: &impl SqlExecutor, date: NaiveDate) -> DbResult<String>` | 231 | ✅ |
| `create_auto` | `pub async fn create_auto(ex: &impl SqlExecutor, req: &AutoVoucher<'_>, operator: Option<Uuid>, now: DateTime<Utc>) -> DbResult<Uuid>` | 854 | ⚠️ **内部硬编码采购模板**（见 3.7.2），不可直接用于销售 |
| `purchase_in_lines` | `pub fn purchase_in_lines(accounts: &AccountSet, amounts: VoucherAmounts, summary: &str, ctx: LineContext) -> Vec<EntryLine>` | 554 | 只读参考（模板写法） |
| `reversed_lines` | `pub fn reversed_lines(original: &[EntryRow], reason: Option<&str>) -> Vec<EntryLine>` | 606 | ✅ 红冲复用（借贷互换 + 摘要前缀） |
| `totals_of` | `pub fn totals_of(lines: &[EntryLine]) -> (Decimal, Decimal)` | 628 | ✅ |
| `create_for_stock_in` | `pub async fn create_for_stock_in(ex, header: &StockInRow, first_material: Option<Uuid>, operator, now) -> DbResult<Uuid>` | 1002 | 结构参考 |
| `reverse_for_stock_in` | `pub async fn reverse_for_stock_in(ex, header: &StockInRow, reason: Option<&str>, operator, now) -> DbResult<Option<Uuid>>` | 1040 | 结构参考（与 `SOURCE_STOCK_IN` 绑死） |
| 常量 | `SOURCE_STOCK_IN="STOCK_IN"`、`BIZ_TYPE_PURCHASE_IN="PURCHASE_IN"`、`DEFAULT_INVENTORY_CODE="1405"`、`DEFAULT_PAYABLE_CODE="2202"`、`DEFAULT_TAX_CODE="2221.01"` | 87–111 | 新增 `SOURCE_STOCK_OUT` / `SOURCE_RECEIPT` 等 |

#### 3.7.2 三个缺口（为什么不「直接调 create_auto」）

```rust
// voucher.rs:866-877（真实代码，节选）
let lines = purchase_in_lines(
    &accounts,
    req.amounts,
    &req.summary,
    LineContext { warehouse_id: req.warehouse_id, material_id: req.material_id, supplier_id: req.supplier_id },
);
```

1. **模板硬编码**：`create_auto` 里写死 `purchase_in_lines`，销售的收入 / 成本 / 收款三种模板进不去；
2. **`AccountSet` 是采购形状**：`{inventory, payable, tax}`，销售的三种模板需要 `{receivable, revenue, tax}` / `{cost, inventory}` / `{bank, receivable}`；
3. **红冲与 `source_type` 绑死**：`reverse_for_stock_in` 只认 `SOURCE_STOCK_IN`。

#### 3.7.3 增量方案：不改签名，只加「带分录的入口」

```rust
/// 用调用方给的分录生成凭证（**不 commit**）。新增，不改既有。
///
/// 与 [`create_auto`] 的唯一差别：分录由调用方按业务模板构造。
/// `create_auto` 改为「解析科目 → 选模板 → 调本函数」的三行委托，
/// 签名与 `PURCHASE_IN` 的行为**逐字不变**（1278 个既有测试钉住它）。
pub async fn create_auto_with(
    ex: &impl SqlExecutor,
    req: &AutoVoucher<'_>,
    lines: Vec<EntryLine>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid>;
```

三个模板纯函数（**纯函数 = 不碰库、可直接单测**，与 `purchase_in_lines` 同一形状）：

```rust
/// 销售出库·收入凭证（3 行）
///
///   借 应收账款(1122)            total_with_tax   （价税合计）
///   贷 主营业务收入(6001)        total_amount     （不含税）
///   贷 应交税费-销项税(2221.02)  total_tax        （税率为 0 时不出行）
pub fn sale_out_lines(accounts: &SalesAccounts, amounts: VoucherAmounts, summary: &str, ctx: LineContext) -> Vec<EntryLine>;

/// 销售出库·成本凭证（2 行）
///
///   借 主营业务成本(6401)   total_cost
///   贷 库存商品(1405)       total_cost
pub fn sale_out_cost_lines(accounts: &CostAccounts, total_cost: Decimal, summary: &str, warehouse_id: Option<Uuid>) -> Vec<EntryLine>;

/// 收款凭证（2 行，**只按核销额入账**）
///
///   借 银行存款(1002)   Σ核销额
///   贷 应收账款(1122)   Σ核销额
pub fn receipt_lines(accounts: &ReceiptAccounts, written_off: Decimal, summary: &str, customer_id: Option<Uuid>) -> Vec<EntryLine>;
```

科目解析新增三个（形状同 `resolve_accounts`，优先级链见下）：

```rust
pub struct SalesAccounts  { pub receivable: AccountRef, pub revenue: Option<AccountRef>, pub tax: Option<AccountRef> }
pub struct CostAccounts   { pub cost: AccountRef, pub inventory: AccountRef }
pub struct ReceiptAccounts{ pub bank: AccountRef, pub receivable: AccountRef }

pub async fn resolve_sales_accounts(ex: &impl SqlExecutor, biz_type: &str, customer_id: Option<Uuid>, material_id: Option<Uuid>) -> DbResult<SalesAccounts>;
```

**科目解析优先级**（三级，与批次 1 `resolve_accounts` 同构）：

```text
① biz_voucher_rule(biz_type=?) 的 debit/credit/tax_account_id   ← 可按规则覆盖
② 客户级 receivable_account_id（应收科目）/ 物料级 inventory_account_id（存货科目）
③ 常量 DEFAULT_*（1122 / 6001 / 2221.02 / 6401 / 1405 / 1002）
④ 全部落空 → 400 + 66002（复用批次 1 的「弱引用不存在」，不新造码）
```

#### 3.7.4 分录模板与金额取数（红线）

| 凭证 | 行 | 科目 | 借方 | 贷方 | 金额取数（**一律取单头，不重算**） |
|------|----|------|------|------|--------------------------------|
| 收入 | 1 | 1122 应收账款 | `total_with_tax` | — | `biz_stock_out.total_with_tax` |
| 收入 | 2 | 6001 主营业务收入 | — | `total_amount` | `biz_stock_out.total_amount` |
| 收入 | 3 | 2221.02 销项税 | — | `total_tax` | `biz_stock_out.total_tax`（税率为 0 时**不生成此行**） |
| 成本 | 1 | 6401 主营业务成本 | `total_cost` | — | `biz_stock_out.total_cost`（= Σ明细 `cost_amount`） |
| 成本 | 2 | 1405 库存商品 | — | `total_cost` | 同上 |
| 收款 | 1 | 1002 银行存款 | `write_off_total` | — | `biz_receipt.write_off_total`（**不是** `amount`） |
| 收款 | 2 | 1122 应收账款 | — | `write_off_total` | 同上 |

**四条不可违反的红线**：

1. 收入凭证借贷必须平：`total_with_tax = total_amount + total_tax`（DB 层 `ck_biz_out_tax` 已强制），所以 3 行天然平；成本凭证 2 行同额天然平；`create_auto_with` 仍会再校验一次 `totals_of`（复用批次 1 的「借贷不平衡 → `InvalidValue`」）。
2. **金额为 0 的行不出**（与 `purchase_in_lines` 完全一致：`if amounts.amount > ZERO`），行号最后统一重排；若结果 < 2 行 → 拒（`create_auto` 的既有规则：全 0 金额不构成凭证）。
3. 成本凭证的 `total_cost` 必须在同一事务里由 SQL `SUM` 得出（§3.1 ④），**不能**用 Rust 侧逐行累加（尾差，见 §3.2.2）。
4. 辅助核算：收入凭证借方 `aux_customer_id` 走 `aux_extra` JSONB（`biz_voucher_entry` 没有 `aux_customer_id` 列，实测：只有 `aux_supplier_id/aux_material_id/aux_warehouse_id/aux_dept_id`）——**本批不自作聪明加列**，把客户 id 写进 `aux_extra = {"customer_id": "…"}`，并在 §8 记为「应收明细账按客户查需要批次 1 补一列」的已知限制。

#### 3.7.5 红冲复用

`reversed_lines` 是纯函数（借贷互换 + 摘要加「红冲」前缀），直接复用。新增一个**通用红冲**（`reverse_for_stock_in` 与它同构，本批把重复部分抽出来，既有函数改为委托）：

```rust
/// 通用红冲：按 (source_type, source_id) 找原凭证 → 借贷互换 → 落红字凭证 → 回写 reversed_by。
///
/// 与 `reverse_for_stock_in` 的差异只有「来源类型」与「摘要」；行为逐字一致：
/// 原凭证 status 保持 POSTED，只写 reversed_by；红字凭证 is_red=1、reverse_of=原凭证；
/// 找不到原凭证 → Ok(None)（不阻断作废）；重复红冲 → 400。
pub async fn reverse_auto(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    source_no: &str,
    voucher_date: NaiveDate,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Option<Uuid>>;
```

销售侧调用：`reverse_auto(ex, "STOCK_OUT", id, &out_no, …)` 与 `reverse_auto(ex, "STOCK_OUT_COST", …)`；收款作废同理（`"RECEIPT"`）。

#### 3.7.6 用测试钉住「不重写」

| 测试 | 断言 | 目的 |
|------|------|------|
| `purchase_in_lines_unchanged` | `create_auto` 走 `PURCHASE_IN` 生成的 3 行分录与批次 1 快照逐字相同 | 证明重构没改行为 |
| `sale_out_lines_balanced` | 收入 3 行 `Σ借 = Σ贷 = total_with_tax` | 模板正确性 |
| `sale_out_lines_zero_tax` | `tax = 0` → 只 2 行，且科目不含 2221.02 | 边界 |
| `sale_out_cost_lines_two_rows` | 2 行、金额都等于 `total_cost` | — |
| `receipt_lines_use_write_off` | 用 `write_off_total` 而非 `amount` | 预收不进凭证 |
| `reverse_auto_idempotent` | 二次红冲 → 400，且不产生第二张红字凭证 | 复用 `reverse_for_stock_in` 的既有逻辑 |
| `create_auto_with_rolls_back_on_unbalanced` | 借贷不平 → `InvalidValue`，且**不落库** | 事务内失败不留半张凭证 |

### 3.8 事务边界与回滚矩阵

出库过账与收款核销是两个「长事务」，每一步失败都必须整笔回滚。矩阵如下（`✅` = 本批必须写测试覆盖）：

| 步 | 动作 | 失败条件 | HTTP | 错误码 | 回滚后必须成立的断言 |
|----|------|---------|------|--------|--------------------|
| ① | 锁单头 | 单不存在 / 已删 | 404 | `10002` | 无写入 |
| ① | 状态闸 | `status <> DRAFT` | 409 | `66302` | 库存、凭证、应收全部未动 ✅ |
| ② | CAS 置 POSTED | 并发被抢先（0 行） | 409 | `66302` | `posted_at` 未变 ✅ |
| ③ | 条件扣减 | 库存不足（0 行） | 400 | `66301` | `biz_inventory.qty` 未变 ✅ |
| ③ | 库位行扣减 | 库位行不足（0 行） | 400 | `66301` | 桶已扣的行也回滚 ✅ |
| ③ | 序列号 CAS | 行数 ≠ 件数 | 409 | `66307` | 已 CAS 的码回滚为 `IN_STOCK` ✅ |
| ③ | 流水写入 | 唯一键冲突 | 500 | `10003` | — |
| ⑤ | 生成应收 | `uk_biz_ar_source` 冲突（重复过账） | 409 | `66302` | 全回滚 ✅ |
| ⑥ | 收入凭证 | 科目缺失 | 400 | `66002` | 全回滚，**不出现只有成本凭证的单** ✅ |
| ⑥ | 成本凭证 | 借贷不平 | 400 | `10001` | 同上 ✅ |
| ⑦ | 回写订单 | 超发（`shipped_qty + n > qty`，0 行） | 400 | `66303` | 全回滚 ✅ |
| ⑦ | 订单状态推进 | 状态非法（订单被作废） | 409 | `66202` | 全回滚 ✅ |

**顺序设计的两条理由**：

1. **凭证最后生成**：凭证是「对外账」，一旦落了就要红冲才能改；放在事务最后，前面的任何失败都不会留下「有凭证没库存」的脏账。
2. **订单回写最后**：它是唯一会 `UPDATE` 别的主表（`biz_sale_order`）的步骤，放在最后可以让锁的持有范围最小（订单行锁只在事务末尾短暂持有）。

### 3.9 四张状态机

#### 3.9.1 销售订单（7 态，`ck_biz_so_status` 已限制取值）

```text
DRAFT ──提交──► SUBMITTED ──审批──► APPROVED ──首笔出库──► PARTIAL_SHIPPED ──出完──► SHIPPED ──关闭──► CLOSED
  │                │                    │                      │                      │
  └──作废──► CANCELLED ◄──拒绝/撤回──────┘                      └──────关闭──────────► CLOSED
```

| 迁移 | 触发动作 | 副作用（同事务） | 校验 |
|------|---------|----------------|------|
| `DRAFT → SUBMITTED` | 提交 | **占用信用** `order_hold` + 写流水；`credit_hold_amount = total_with_tax` | 明细非空、客户 `credit_status ≠ BLOCKED`、额度够 |
| `SUBMITTED → APPROVED` | 审批回调 | 记录 `approved_at` | `WATCH` 客户必须走审批流 |
| `SUBMITTED → CANCELLED` | 撤回/拒绝 | **释放信用** `ORDER_RELEASE` | — |
| `APPROVED → PARTIAL_SHIPPED` | 出库过账（系统） | 回写 `shipped_qty`；**平移**信用 | 由出库引擎驱动，不暴露为手动接口 |
| `APPROVED/PARTIAL → SHIPPED` | 同上（出完） | 同上 | `Σ shipped_qty = Σ qty` |
| `APPROVED/PARTIAL/SHIPPED → CLOSED` | 关闭 | 未出部分**释放信用**（按 `credit_hold_amount` 余额） | — |
| 任意非终态 → `CANCELLED` | 作废 | 释放信用 | 已出库的订单**不许**作废（先红冲出库） |

#### 3.9.2 销售出库（3 态）

```text
DRAFT ──过账──► POSTED ──作废──► CANCELLED
  └──作废──► CANCELLED
```

* `POSTED` 的 `CHECK`（`ck_biz_out_vouchers`，POC-8a 实测）要求两张凭证都非空：

```text
--- 8a POSTED 但无凭证 → CHECK 拒绝 ---
ERROR:  new row for relation "biz_stock_out" violates check constraint "ck_biz_out_vouchers"
```

* `CANCELLED` 必须带原因（`cancel_reason` 非空且非空白，`ck_biz_out_state`）；
* `POSTED → CANCELLED`（红冲出库）顺序：红冲两张凭证 → 反向加回库存（`txn_type='IN'`, `biz_type='SALE_OUT_CANCEL'`）→ 序列号回 `IN_STOCK`（**仅当未被消耗**，否则 `SCRAPPED`）→ 应收作废（若已核销，必须先撤销核销）→ 释放信用。

#### 3.9.3 应收（4 态）

```text
OPEN ──部分核销──► PARTIAL_RECEIVED ──核销完──► SETTLED
  │                     │
  └──作废──► CANCELLED ◄┘
```

`ck_biz_ar_settled` 强制「`SETTLED` 必须 `unreceived_amount = 0`」，因此**状态不能随便写**：只能由核销引擎按余额推（`0 → SETTLED`、`0 < 余额 < amount → PARTIAL_RECEIVED`）。

#### 3.9.4 收款（3 态）

```text
DRAFT ──过账（核销）──► POSTED ──撤销──► CANCELLED
  └──作废──► CANCELLED
```

`POSTED` 才能核销（`DRAFT` 只是登记，`ck_biz_receipt_state` 约束 `posted_at` 与 `status` 的一致性）；`POSTED` 必须有凭证（`voucher_id`），撤销时红冲。

### 3.10 全局锁顺序（防死锁）

多张表、多行都要锁时，**全系统只允许一个加锁顺序**（批次 1 的顺序 + 本批追加）：

```text
1. biz_customer_credit     （按 customer_id 单行，热点行）
2. biz_sale_order          （按 id 单行）
3. biz_stock_out           （按 id 单行）
4. biz_inventory           （按 material_id ASC, warehouse_id ASC, 库位行 id ASC）
5. biz_inventory_location  （按 id ASC，与上一行配对）
6. biz_material_serial     （按 serial_no ASC —— 用 `ORDER BY serial_no` 归一到同一序列）
7. biz_receivable          （按 receivable_id ASC，核销时）
8. biz_voucher / entry     （最后，无并发争用）
```

**为什么按 `material_id` 升序而不是按明细行序**：批次 1 的收货过账已经这么做了（`stockin_model.rs:42` 原话：「多行按 `material_id` 升序（防两个收货单互相死锁）」）。两张单如果各自按自己的明细顺序加锁，A 单锁 1→2、B 单锁 2→1 就死锁；全系统统一升序后，永远不可能成环。

**序列号为什么按 `serial_no` 而不是 `id`**：前端扫码顺序不定，但 `serial_no` 是字符串、可排序，且业务上唯一；用 `id`（UUID 随机）排序虽然也能防死锁，但同一批码在两次请求里的排序不稳定，测试难写。这条是**本批新增的规则**，写进代码注释与 §6 的死锁测试。

### 3.11 不变式巡检 SQL（I1–I8 的真跑版本）

批次 1 有库存巡检（`inventory_audit.rs`），本批把「销售闭环」的 8 条不变式写成可执行 SQL，每条都能独立跑、独立报警（`§2.8` 列了定义，这里给 SQL）：

```sql
-- I1 成本账自洽：qty>0 时 round(qty*avg_cost,4)=amount；qty=0 时 amount=0
SELECT count(*) AS bad FROM biz_inventory
 WHERE deleted_at IS NULL
   AND ((qty > 0 AND ROUND(qty * avg_cost, 4) <> amount) OR (qty = 0 AND amount <> 0));

-- I2 两级账一致：该物料该仓的库位行合计 = 汇总桶数量
SELECT b.material_id, b.warehouse_id, b.qty AS bucket, COALESCE(SUM(l.qty),0) AS loc_sum
  FROM biz_inventory b
  LEFT JOIN biz_inventory_location l
         ON l.material_id = b.material_id AND l.warehouse_id = b.warehouse_id AND l.deleted_at IS NULL
 WHERE b.location_id IS NULL AND b.deleted_at IS NULL
 GROUP BY b.material_id, b.warehouse_id, b.qty
HAVING b.qty <> COALESCE(SUM(l.qty), 0);

-- I3 出库成本 = 明细合计 = 成本凭证借方合计
SELECT o.out_no, o.total_cost, SUM(i.cost_amount) AS item_sum, v.total_debit AS cost_voucher_debit
  FROM biz_stock_out o
  JOIN biz_stock_out_item i ON i.stock_out_id = o.id
  LEFT JOIN biz_voucher v  ON v.id = o.cost_voucher_id
 WHERE o.status = 'POSTED' AND o.deleted_at IS NULL
 GROUP BY o.id, o.out_no, o.total_cost, v.total_debit
HAVING o.total_cost <> SUM(i.cost_amount) OR (v.total_debit IS NOT NULL AND v.total_debit <> o.total_cost);

-- I4 出库价税合计 = 应收金额（且应收来源唯一）
SELECT o.out_no, o.total_with_tax, r.amount
  FROM biz_stock_out o LEFT JOIN biz_receivable r
    ON r.source_type = 'STOCK_OUT' AND r.source_id = o.id AND r.deleted_at IS NULL
 WHERE o.status = 'POSTED' AND o.deleted_at IS NULL
   AND (r.id IS NULL OR r.amount <> o.total_with_tax);

-- I5 收款核销合计 = 明细合计 且 ≤ 收款金额
SELECT r.receipt_no, r.amount, r.write_off_total, COALESCE(SUM(i.amount),0) AS item_sum
  FROM biz_receipt r LEFT JOIN biz_receipt_item i ON i.receipt_id = r.id
 WHERE r.deleted_at IS NULL
 GROUP BY r.id, r.receipt_no, r.amount, r.write_off_total
HAVING r.write_off_total <> COALESCE(SUM(i.amount),0) OR r.write_off_total > r.amount;

-- I6 应收已收 = 核销明细合计（撤销的收款不计）
SELECT ar.receivable_no, ar.received_amount, COALESCE(SUM(i.amount),0) AS written
  FROM biz_receivable ar
  LEFT JOIN biz_receipt_item i ON i.receivable_id = ar.id
  LEFT JOIN biz_receipt rc      ON rc.id = i.receipt_id AND rc.status = 'POSTED' AND rc.deleted_at IS NULL
 WHERE ar.deleted_at IS NULL
 GROUP BY ar.id, ar.receivable_no, ar.received_amount
HAVING ar.received_amount <> COALESCE(SUM(CASE WHEN rc.id IS NULL THEN 0 ELSE i.amount END), 0);

-- I7 信用自洽：used = order + ar；order_used = Σ 未出库订单占用
SELECT c.customer_id, c.order_used, c.ar_used, c.used_amount,
       COALESCE((SELECT SUM(so.credit_hold_amount) FROM biz_sale_order so
                  WHERE so.customer_id = c.customer_id
                    AND so.status IN ('SUBMITTED','APPROVED','PARTIAL_SHIPPED')
                    AND so.deleted_at IS NULL), 0) AS hold_sum
  FROM biz_customer_credit c
 WHERE c.deleted_at IS NULL
   AND (c.used_amount <> c.order_used + c.ar_used OR c.order_used <> COALESCE(
        (SELECT SUM(so.credit_hold_amount) FROM biz_sale_order so
          WHERE so.customer_id = c.customer_id
            AND so.status IN ('SUBMITTED','APPROVED','PARTIAL_SHIPPED')
            AND so.deleted_at IS NULL), 0));

-- I8 一物一码：明细 qty = 序列号件数，且这些码都已是 OUTBOUND
SELECT i.id, i.qty, count(s.serial_id) AS serial_cnt,
       count(*) FILTER (WHERE ms.status <> 'OUTBOUND') AS not_outbound
  FROM biz_stock_out_item i
  JOIN biz_stock_out o     ON o.id = i.stock_out_id AND o.status = 'POSTED'
  LEFT JOIN biz_stock_out_item_serial s ON s.item_id = i.id
  LEFT JOIN biz_material_serial ms      ON ms.id = s.serial_id
  JOIN biz_material m      ON m.id = i.material_id AND m.serial_required = TRUE
 WHERE o.deleted_at IS NULL
 GROUP BY i.id, i.qty, i.material_id
HAVING i.qty <> count(s.serial_id) OR count(*) FILTER (WHERE ms.status <> 'OUTBOUND') > 0;
```

**部署方式**：8 条 SQL 落到 `scripts/verify/erp_sales_audit.sql`，沿用批次 1 库存巡检的口径（只读、可在备库跑、每条返回 0 行 = 健康）。**没有 I1–I8 全绿，不算交付**（§6 的验收标准）。

### 3.12 七条硬结论的方案对比总表（一页看完）

前面 3.2–3.7 已经逐条给出结论与理由，这里把**候选方案**并排放在一起，便于评审时逐行打勾。每条都标了真库证据（§9.1）。

#### D1 出库成本结转的并发安全

| 方案 | 写法 | 优点 | 缺点 | 判定 |
|------|------|------|------|------|
| A 悲观锁 | `SELECT … FOR UPDATE` 后判 `qty >= n` 再 `UPDATE` | 语义直白，可读性好 | 三步（读-判-写），热点行持锁时间 = 3 个往返；死锁窗口更大 | ❌ 不用 |
| **B 条件扣减** | `UPDATE … SET qty=qty-n, amount=amount-ROUND(n*avg_cost,4) WHERE id=? AND qty>=n RETURNING …` | 单语句、无读-判窗口；0 行即失败；`RETURNING` 直接拿到新均价 | 0 行时无法区分"库存不足 / 行不存在"（需再诊断） | ✅ **采用** |
| C 版本号乐观锁 | `WHERE version=?` 失败则应用层重试 | 无锁等 | 重试风暴 + 库存热点必然冲突，重试次数不可控 | ❌ 不用 |

**结论**：B。**理由**：与批次 1 调拨 `transfer_model::deduct_inventory_row` 是**同一段实现**（出库只读均价、不再重算，天然适合条件扣减）；入库仍用 A，因为入库要"读旧均价 → 算新均价 → 写"三步，条件扣减表达不了。**证据**：POC-4（单会话 1 成 1 败）、POC-9（双会话真并发，终态 8−5=3）。

#### D2 负库存

| 方案 | 成本取数 | 后果 | 判定 |
|------|---------|------|------|
| A 无库存 → `avg_cost = 0` | 出库成本 0 | 毛利虚高、所得税风险；且把正金额摊到负数量上污染成本链 | ❌ |
| **B 取该物料该仓最近一条 `IN` 的 `unit_cost`** | 有依据的成本 | 负库存期间成本仍可解释，后续入库移动加权自然冲平 | ✅ **采用** |
| C 无历史入库价 → 拒绝出库 | 拦住 | 首批上线、期初无入库时会把业务卡死 | ⚠️ 仅作为 B 的兜底（无任何 `IN` 历史时 400） |

**结论**：B，兜底 C。开关用**已存在**的 `biz_warehouse.allow_negative`（不加表不加列）。**证据**：POC-2（出库 5 见 `avg_cost=10`、入库 30@12 冲平后 `qty=25 / amount=310 / avg=12.4`）。

#### D3 出库即生成应收 + 凭证

| 方案 | 凭证 | 优点 | 缺点 | 判定 |
|------|------|------|------|------|
| A 一张 5 行（借 1122 / 贷 6001 / 贷 2221.02 / 借 6401 / 贷 1405） | 1 张 | 张数少；金额上**确实能平**（借 = 贷 = 价税合计 + 成本） | ① 红冲粒度错：作废时无法只红冲成本或只红冲收入；② 一张凭证混两个业务动作（确认收入 / 结转成本），凭证类型与摘要语义混乱，审计按 `biz_type` 查不到"成本结转"这一类；③ 引擎按 `biz_type` 取单条规则的模板机制装不下 5 行混排，要改引擎 = 违反"复用不重写" | ❌ |
| **B 两张（收入 3 行 / 成本 2 行），同一事务** | 2 张 | 各凭证自身平衡、红冲独立、利润表/资产负债表各行其道、`biz_type` 语义干净 | 单据要挂两个凭证列（`voucher_id` / `cost_voucher_id`） | ✅ **采用** |
| C 只出收入凭证，成本期末一次结转 | 1 张 | 最简单 | 出库单看不到成本、毛利失真；期末结转要额外做"成本分摊"，批次 4 才能做 | ❌ |

**结论**：B，`source_type` 用 `STOCK_OUT` / `STOCK_OUT_COST` 区分，`ck_biz_out_vouchers` 强制 POSTED 两列都非空。**理由**：不是"一张会不平"（一张 5 行在金额上是平的），而是**红冲粒度 + 业务动作单一性 + 引擎模板机制**三条都要求拆开：作废出库单时要能分别处理"收入退回"与"成本转回"（退货但成本已结转的情形不同），审计与报表也要能按 `biz_type` 单独统计成本结转额。**证据**：POC-8a（POSTED 无凭证被 CHECK 拒）/ 8b（两凭证齐全通过）。

#### D4 信用额度的并发占用

| 方案 | 时点 / 写法 | 优点 | 缺点 | 判定 |
|------|------------|------|------|------|
| **A 下单提交时单语句条件 UPDATE** | `SET order_used=order_used+x, used_amount=used_amount+x WHERE credit_limit=0 OR used_amount+x<=credit_limit` | 一个往返、无读-判窗口；超额在提交时就暴露 | 热点行（同客户并发下单排队，毫秒级） | ✅ **采用** |
| B `SELECT … FOR UPDATE` 再判断 | 锁行后读余额再决定 | 可返回详细余额 | 三步锁持有，并发下单排队 | ❌ |
| C 只在出库时占用 | 下单不占 | 下单不阻塞 | 签单全过、发货才卡（现场等货），最贵的失败模式 | ❌ |
| A+C 混合 | 下单占 `order_used`；出库**平移**到 `ar_used`（总额不变）；核销释放 `ar_used` | 并发判断收敛到唯一一处（提交）；出库/收款无需再判额度 | 需要 `biz_customer_credit_txn` 留痕与 I7 巡检 | ✅ **最终方案** |

**结论**：A 的条件 UPDATE + C 的时点设计（下单占、出库平移、核销释放）。**证据**：POC-5（两笔 8 万，第二笔 0 行）、POC-10（双会话真并发，`used_amount` 停在 80000 而非 160000）。

#### D5 核销的原子性

| 方案 | 写法 | 风险 | 判定 |
|------|------|------|------|
| A 应用层读余额 → 判断 → 写 | 三个往返 | 并发下"读到的余额"过期 → 超额核销 | ❌ |
| **B 单事务 + 每张应收条件 UPDATE（`AND unreceived_amount >= x`）+ 明细行 `before/after` 快照 + CHECK 自证** | 一条语句一张应收 | 0 行需再诊断（3.6.1） | ✅ **采用** |
| C 数据库函数/存储过程 | 逻辑下沉 DB | 违背"业务逻辑在 Rust"的既有架构，测试与迁移都变重 | ❌ |

**结论**：B。**理由**：`ck_biz_receipt_item_after`（`after = before - amount`）让"金额对不上"在 DB 层插不进去；`receivable_id` 升序固定加锁顺序防两笔收款互锁。**证据**：POC-6a–6d（核销 600 → PARTIAL、再核销 500 → 0 行、伪造明细被 CHECK 拒）。

#### D6 序列号出库

| 方案 | 写法 | 风险 | 判定 |
|------|------|------|------|
| A 先 `SELECT` 查状态，再 `UPDATE` | 两步 | 中间窗口被别的单抢走 → 一个码挂两张单 | ❌ |
| **B 状态 CAS**：`UPDATE … SET status='OUTBOUND' WHERE id=ANY(...) AND status='IN_STOCK' RETURNING`，行数 ≠ 件数即失败 | 一步 | 无（失败整单回滚） | ✅ **采用** |
| C `SELECT … FOR UPDATE` 锁码再改 | 两步但持锁 | 码多时锁持有时间长；与库存行锁顺序易交错 | ❌ |

**结论**：B。**证据**：POC-7a（第一笔 1 行）/ 7b（同码第二笔 0 行）。

#### D7 与批次 1 库存表共存（库位）

| 方案 | 扣减对象 | 问题 | 判定 |
|------|---------|------|------|
| A 只扣 `biz_inventory` 汇总桶 | 成本账 | 库位数量账不更新 → 巡检 I2（Σ库位 = 仓级）判假 | ❌ |
| B 只扣 `biz_inventory_location` | 数量账 | 成本金额不动 → 成本账与数量账脱节 | ❌ |
| **C 两级账**：桶（金额/数量）+ 库位行（数量），库位模式下同时扣；未指定库位时按 `(qty DESC, location_code ASC)` 贪心分配并**展开多行** | 两账 | 明细行数变多（一库位一行） | ✅ **采用** |
| D 一行明细 + 分摊子表 | — | 相当于第二套库位记账口径，巡检写不出来 | ❌ |

**结论**：C。**理由**：`biz_inventory` 唯一键 `(material_id, warehouse_id, COALESCE(location_id, 零uuid))` 决定"一行 = 一个成本桶"；库位行只是数量账。判断开关 = "该物料该仓有没有库位行"。**证据**：POC-3c/3d/3e（拣货顺序、跨两库位扣减、I2 成立）。

---
## 4. API 契约

本章 41 个接口，路径、请求体、响应、错误码、权限码**逐条给全**。所有约定继承批次 1（`docs/ERP-批次1-技术架构.md` §4.1），凡与本批新增的，逐条标「新增」。

### 4.0 通用约定（继承 + 本批新增两条）

| 项 | 约定 | 来源 |
|----|------|------|
| 路径前缀 | 后端宏路径**不含** `/api`（`/business/erp/customer/page`），前端**必须写全** `/api/business/erp/customer/page` | 批次 1 §4.1 / `AGENTS.md` 约定 8 |
| 响应包装 | `Res::ok(data)` / `Res::page(list, total, page, page_size)`；错误统一 `ApiError` | `nik::prelude::Res` |
| 分页字段 | `list` / `total` / `page` / `page_size` | `PageData` |
| 分页入参 | `page`（默认 1）、`page_size`（默认 20，限幅 200），统一过 `utils::normalize_page` | 批次 1 §4.1 |
| ID | UUID 字符串；路径参数 `utils::require_uuid(&s, "id")` 解析，失败 400 + `10001` | 批次 1 §4.1 |
| 金额/数量 | **字符串**传输（`"1500.0000"`），Decimal serde 即字符串，前端不转 number | 批次 1 §4.1（POC 实测） |
| 日期 | `DATE` → `"2026-10-27"`；`TIMESTAMPTZ` → RFC 3339 | 批次 1 §4.1 |
| 枚举/状态 | 字符串短码（`"POSTED"` / `"PARTIAL_SHIPPED"`） | 批次 1 §4.1 |
| 明细提交 | 主表 + 明细一次提交（`items: [...]`），事务内先删后插重建 | 批次 1 §4.1 |
| **幂等键（新增）** | 过账、作废、核销、信用占用四类接口**必须**接受 `Idempotency-Key` 头（UUID）；服务端落到 `biz_customer_credit_txn.idem_key = <txn_type>:<source_type>:<source_id>` 与单据 `uk_*_no` 双重兜底 | 本批 §3.5.1 |
| **金额只读（新增）** | 所有响应里的 `total_amount` / `total_tax` / `total_with_tax` / `total_cost` / `*_amount` 均为**后端计算结果**；请求体里带这几个字段的一律**忽略**（`#[serde(default)]` 不接），不允许客户端指定单据合计 | 本批 §3.7.4 红线 3 |

**错误码段规划（本批新增 661xx–666xx，660xx 段已满）**

| 码段 | 归属 | 已用 | 本批新增 |
|------|------|------|---------|
| `1000x` | 通用（参数 / 不存在 / 冲突 / 内部错误） | 10001–10004 | 复用，不新增 |
| `660xx` | ERP 主数据 / 仓储（批次 1 W1c） | 66001–66009 | 复用 66002（弱引用缺失）/ 66007（物料未启用序列号） |
| `661xx` | **客户与信用**（新增） | — | 66101–66106 |
| `662xx` | **销售订单**（新增） | — | 66201–66206 |
| `663xx` | **销售出库**（新增） | — | 66301–66309 |
| `664xx` | **应收**（新增） | — | 66401–66404 |
| `665xx` | **收款 / 核销**（新增） | — | 66501–66506 |
| `666xx` | **客户对账单**（新增） | — | 66601–66603 |

> **为什么 661xx 不接着 66010 往下排**：`66xxx` 是批次 D 的 ERP 大段，`660xx` 是"主数据/仓储通用"子段（`ERP_CODE_CONFLICT` 等对所有 ERP 模块通用）。销售侧的码按**业务对象**分子段（信用 / 订单 / 出库 / 应收 / 收款 / 对账），与批次 C+ 的 `630xx`–`633xx` 子段划分同一逻辑——前端文案映射按子段挂载，一个子段一份文案表，不会串味。实测 `backend/src/shared/error_code.rs` 里 `66001`–`66009` 已占用，`661xx` 起是空白段。

**统一错误码表（HTTP 状态码语义固定，不新增）**

| 错误码 | HTTP | 含义 | message 模板 |
|--------|------|------|-------------|
| `10001` | 400 | 参数校验失败 | `出库数量必须大于 0（第 3 行）` |
| `10002` | 404 | 资源不存在 | `出库单不存在或已删除` |
| `10003` | 409 | 唯一键冲突 | `单号 OUT-20261031-000007 已存在` |
| `10004` | 500 | 内部错误 | `过账出库单失败`（`utils::db_error` 附上下文） |
| `66002` | 400 | 弱引用不存在 / 科目无法解析 | `客户「北京某某」无应收科目，且规则 SALE_OUT_DEFAULT 未配置 1122` |
| `66007` | 400 | 物料未启用序列号管理 | `物料「电机 X1」未启用一物一码，不能提交序列号` |
| `66101` | 409 | **信用额度不足** | `客户「北京某某」信用额度不足：本单需占用 80000.0000，可用 20000.0000，超出 60000.0000。` |
| `66102` | 500 | 信用占用账不平（内部不一致） | `客户信用占用平移失败：订单占用 30000.0000 < 本次平移 50000.0000，疑似并发释放` |
| `66103` | 400 | 客户有未结应收，禁删 | `客户「北京某某」尚有 2 张未结应收（合计 120000.0000），请先收款或作废` |
| `66104` | 400 | 客户已冻结，禁下单 | `客户「北京某某」信用状态为 BLOCKED，禁止下单` |
| `66105` | 400 | 信用行不存在 | `客户「北京某某」未建信用档案，请先设置额度（0 = 不限）` |
| `66106` | 400 | 调整后额度低于已占用 | `额度 80000.0000 小于当前已占用 100000.0000，请先释放占用或将其设为 0（不限）` |
| `66201` | 400 | 订单状态不允许该操作 | `订单 SO-20261031-000003 当前状态 SUBMITTED，只有 DRAFT 可编辑` |
| `66202` | 409 | 订单状态被并发改变 | `订单 SO-… 状态已被他人改变，请刷新重试` |
| `66203` | 400 | 订单明细为空 | `销售订单至少需要一行明细` |
| `66204` | 400 | 可出库数量不足 / 超发 | `明细第 2 行：可出库 30.0000，本次出库 50.0000` |
| `66205` | 400 | 订单已出库，禁作废 | `订单 SO-… 已出库 60.0000，请先红冲出库单 OUT-…` |
| `66301` | 400 | 库存不足 | `物料「冷轧钢板」在仓「原料仓」可用 20.0000，本次需 33.0000` |
| `66302` | 409 | 出库单状态冲突（重复过账） | `出库单 OUT-… 当前状态 POSTED，不可重复过账` |
| `66303` | 400 | 出库超发（回写订单时） | `订单明细第 1 行超发：已出 90.0000 + 本次 20.0000 > 订单 100.0000` |
| `66304` | 400 | 指定库位库存不足 | `库位 A-03 可用 5.0000，本次需 8.0000` |
| `66305` | 400 | 一物一码物料必须逐件给序列号 | `物料「电机 X1」为一物一码，出库 5 件需 5 个序列号，实际 3 个` |
| `66306` | 400 | 序列号不可用 | `序列号 SN-20261031-000123 当前状态 OUTBOUND（不属于本仓 / 已出库）` |
| `66307` | 409 | 序列号并发被抢占 | `序列号 SN-… 已被他人出库，请刷新后重选` |
| `66308` | 400 | 已过账单禁改 | `出库单 OUT-… 已过账，禁止编辑（请作废后重建）` |
| `66309` | 400 | 作废时存在已核销应收 | `出库单 OUT-… 关联应收已收 300.0000，请先撤销收款 RCPT-…` |
| `66401` | 400 | 应收已结清/已作废，禁操作 | `应收 AR-… 当前状态 SETTLED，不可再核销` |
| `66402` | 400 | 应收来源单据未过账 | `应收 AR-… 来源出库单未过账，不可核销` |
| `66403` | 400 | 账龄口径非法 | `账龄区间必须为 [0,30] / [31,60] / [61,90] / [91,+∞] 之一` |
| `66404` | 409 | 应收被并发核销 | `应收 AR-… 已被他人核销，请刷新` |
| `66501` | 400 | 跨客户核销 | `收款单客户「A」与应收 AR-… 客户「B」不一致` |
| `66502` | 400 | 核销目标状态非法 | `应收 AR-… 已结清或已作废，不可核销` |
| `66503` | 400 | 核销额超收款金额 | `核销合计 1200.0000 超过收款金额 1000.0000` |
| `66504` | 409 | 收款单状态冲突 | `收款单 RCPT-… 当前状态 POSTED，不可重复过账` |
| `66505` | 400 | 预收余额不足 | `收款单 RCPT-… 未核销余额 200.0000，本次需 300.0000` |
| `66506` | 400 | 同一应收重复挂 | `应收 AR-… 在本收款单中已存在核销行（第 1 行）` |
| `66601` | 400 | 区间内无流水 | `客户「北京某某」在 2026-10-01~2026-10-31 无应收/收款流水` |
| `66602` | 409 | 区间已存在对账单 | `该客户在 2026-10-01~2026-10-31 已存在对账单 STS-…（CONFIRMED）` |
| `66603` | 400 | 对账单已确认 | `对账单 STS-… 已确认，不可重新生成` |

---

### 4.1 客户与信用（9 个接口）

#### 4.1.1 客户主数据（5 个）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 1 | GET | `/business/erp/customer/page` | `business:erp:customer:list` | 分页查询 |
| 2 | GET | `/business/erp/customer/{id}` | `business:erp:customer:detail` | 详情（含信用卡片） |
| 3 | POST | `/business/erp/customer` | `business:erp:customer:create` | 新建（取号 `CUS`） |
| 4 | PUT | `/business/erp/customer/{id}` | `business:erp:customer:update` | 修改 |
| 5 | DELETE | `/business/erp/customer/{id}` | `business:erp:customer:delete` | 软删（有未结应收拒绝） |

**① GET `/customer/page`**

```
page, page_size
keyword?        客户编码/名称/简称/税号 ILIKE %kw%
customer_type?  ENTERPRISE / INDIVIDUAL / INTERNAL
credit_status?  NORMAL / WATCH / BLOCKED
salesperson_id? UUID
status?         0 / 1
has_balance?    true → 只返回应收未结 > 0 的客户（`EXISTS biz_receivable WHERE unreceived_amount>0`）
```

响应（`biz_customer` 全列 + 两个聚合，聚合用 `LEFT JOIN LATERAL` 一次查完，不 N+1）：

```json
{
  "list": [{
    "id": "c0000000-0000-4000-8000-000000000001",
    "customer_code": "CUS-20261031-000001",
    "customer_name": "北京某某机械有限公司",
    "short_name": "某某机械",
    "customer_type": "ENTERPRISE",
    "contact_person": "张工", "contact_phone": "13800000000",
    "tax_no": "91110108MA01XXXX2Y",
    "credit_days": 30,
    "credit_status": "NORMAL",
    "salesperson_id": "…", "salesperson_name": "李销售",
    "receivable_account_id": "30000000-0000-4000-8000-000000000112",
    "receivable_account_name": "1122 应收账款",
    "status": 1,
    "credit": { "credit_limit": "100000.0000", "used_amount": "80000.0000", "available": "20000.0000" },
    "unreceived_total": "120000.0000",
    "created_at": "2026-10-31T02:11:33Z"
  }],
  "total": 1, "page": 1, "page_size": 20
}
```

**② GET `/customer/{id}`**：同分页单行 + `"recent_orders"`（最近 5 张，`order_no/order_date/status/total_with_tax`）+ `"aging"`（四段账龄金额，口径同 §4.4）。

**③ POST `/customer`**

```json
{
  "customer_code": "",                      // 可空 → 后端取号 CUS（前端不给就自动生成）
  "customer_name": "北京某某机械有限公司",     // 必填，≤200
  "short_name": "某某机械",
  "customer_type": "ENTERPRISE",            // 必填，三选一
  "contact_person": "张工", "contact_phone": "13800000000", "email": "a@b.com",
  "tax_no": "91110108MA01XXXX2Y",
  "address": "北京市海淀区…",
  "credit_days": 30,                        // ≥0；生成应收时快照
  "credit_status": "NORMAL",                // 三选一
  "receivable_account_id": null,            // 可空；非空必须 leaf + ASSET
  "salesperson_id": null,
  "invoice_title": "北京某某机械有限公司", "invoice_bank_name": "…", "invoice_bank_account": "…",
  "credit_limit": "100000.0000",            // 本批新增：建客户时顺手建信用行（可空，默认 0=不限）
  "remark": ""
}
```

校验（按顺序，任一失败即返回）：

| 序 | 校验 | 错误码 |
|----|------|--------|
| 1 | `customer_name` 非空且 `btrim <> ''` | `10001` |
| 2 | `customer_code` 非空时 ≤40 且 `uk_biz_customer_code` 不冲突（软删后可复用） | `10003` |
| 3 | `customer_type ∈ {ENTERPRISE,INDIVIDUAL,INTERNAL}` | `10001` |
| 4 | `credit_days ≥ 0`、`credit_status` 三值之一 | `10001` |
| 5 | `receivable_account_id` 存在、`is_leaf=1`、`account_category='ASSET'` | `66002` |
| 6 | `salesperson_id` 存在且 `sys_user.status=1` | `66002` |
| 7 | `credit_limit ≥ 0` | `10001` |

**响应**：`201` + 完整 `CustomerRes`（含新生成的 `customer_code` 与 `credit.credit_limit`）。**同一事务**内写 `biz_customer` + `biz_customer_credit`（无额度也建行，`credit_limit=0` 表示不限——与 `biz_supplier.credit_limit` 语义一致，见 §2.1.2 注释）。

**④ PUT `/customer/{id}`**：请求体同 POST（`customer_code` 不可改，传了就 400 `10001`）。额外两条：

* 改 `credit_limit` **不走此接口**（走 `PUT /credit/{customer_id}`，权限码不同，且要过 `66106` 校验）；
* `credit_status` 改成 `BLOCKED` 时**不影响**已提交订单（只拦新提交，`66104`）。

**⑤ DELETE `/customer/{id}`**：软删。删除前校验（全部通过才允许）：

| 校验 | 错误码 |
|------|--------|
| 存在 `biz_receivable.status IN ('OPEN','PARTIAL_RECEIVED')` | `66103` |
| 存在 `biz_sale_order.status IN ('SUBMITTED','APPROVED','PARTIAL_SHIPPED')` | `66103`（message 改说订单） |
| `biz_customer_credit.used_amount > 0` | `66103` |

#### 4.1.2 信用额度（4 个）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 6 | GET | `/business/erp/credit/page` | `business:erp:credit:list` | 客户信用分页（含占用明细） |
| 7 | GET | `/business/erp/credit/{customer_id}` | `business:erp:credit:list` | 单客户信用卡片 |
| 8 | GET | `/business/erp/credit/{customer_id}/txns` | `business:erp:credit:list` | 占用 / 释放流水 |
| 9 | PUT | `/business/erp/credit/{customer_id}` | `business:erp:credit:update` | 调整额度 |

**⑥ GET `/credit/page`**：入参 `page/page_size/keyword?/over_limit?`（`true` = 只返回 `used_amount > credit_limit AND credit_limit > 0`）、`credit_status?`。响应：

```json
{ "list": [{
    "customer_id": "…", "customer_code": "CUS-…", "customer_name": "北京某某",
    "credit_limit": "100000.0000", "order_used": "80000.0000", "ar_used": "0.0000",
    "used_amount": "80000.0000", "available": "20000.0000",
    "usage_rate": "0.8000",               // credit_limit=0 时返回 null
    "credit_status": "NORMAL", "credit_days": 30, "version": 3,
    "updated_at": "…"
  }], "total": 1, "page": 1, "page_size": 20 }
```

**⑦ GET `/credit/{customer_id}`**：`biz_customer_credit` 全列 + `credit_limit` / `available` + 三块聚合：`order_hold`（`Σ biz_sale_order.credit_hold_amount`，`status IN ('SUBMITTED','APPROVED','PARTIAL_SHIPPED')`）、`ar_open`（`Σ biz_receivable.unreceived_amount`）、`overdue`（`due_date < today AND status IN ('OPEN','PARTIAL_RECEIVED')`）。三个聚合必须与 `used_amount` 对得上（§3.11 的 I7 巡检同口径），对不上说明数据漂移，响应里带 `"consistent": false`。

**⑧ GET `/credit/{customer_id}/txns`**：入参 `page/page_size/txn_type?/date_from?/date_to?`，按 `created_at DESC`。响应行 = `biz_customer_credit_txn` 全列（含 `order_used_before/after`、`ar_used_before/after`、`source_type/source_id/source_no`、`idem_key`）。

**⑨ PUT `/credit/{customer_id}`**

```json
{ "credit_limit": "150000.0000", "remark": "年度提额" }
```

校验：

| 序 | 校验 | 错误码 |
|----|------|--------|
| 1 | `credit_limit ≥ 0` | `10001` |
| 2 | 信用行存在（`deleted_at IS NULL`），否则按"未建档案"处理 | `66105` |
| 3 | `credit_limit <> 0 AND credit_limit < used_amount` | `66106` |
| 4 | 乐观锁：请求可带 `"version": 3`，与当前不等则 409 | `10003` |

**实现要点（新增，必须照做）**：调整额度本身**不动占用**，但**必须写一条流水** `txn_type='LIMIT_ADJUST'`，`amount = ABS(new - old)`，`direction = sign(new - old)`，`order_used_before/after` 与 `ar_used_before/after` 均取当前值（额度调整不改占用，快照仅留痕）。这样"额度从 10 万调到 15 万"在流水里可查，审计问"谁在什么时候提的额"不用翻日志。

```sql
UPDATE biz_customer_credit
   SET credit_limit = $new, version = version + 1, updated_at = $now, update_by = $op
 WHERE customer_id = $cid::uuid AND deleted_at IS NULL AND status = 1
   AND ($ver::int IS NULL OR version = $ver)
   AND ($new = 0 OR $new >= used_amount)
RETURNING credit_limit, order_used, ar_used, used_amount, version;
-- 0 行 → 分三种情况再诊断（不存在 → 66105 / 低于占用 → 66106 / 版本冲突 → 10003），
-- 不让一条 0 行含糊地返回 400（这是批次 1 ERP_CODE_CONFLICT 踩过的坑）
```

---

### 4.2 销售订单（8 个接口）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 10 | GET | `/business/erp/sale-order/page` | `business:erp:sale-order:list` | 分页 |
| 11 | GET | `/business/erp/sale-order/{id}` | `business:erp:sale-order:detail` | 详情（含明细） |
| 12 | POST | `/business/erp/sale-order` | `business:erp:sale-order:create` | 新建（`DRAFT`，取号 `SO`） |
| 13 | PUT | `/business/erp/sale-order/{id}` | `business:erp:sale-order:update` | 修改（仅 `DRAFT`） |
| 14 | DELETE | `/business/erp/sale-order/{id}` | `business:erp:sale-order:delete` | 删除（仅 `DRAFT`，软删） |
| 15 | POST | `/business/erp/sale-order/{id}/submit` | `business:erp:sale-order:status` | 提交（**占用信用**） |
| 16 | POST | `/business/erp/sale-order/{id}/status` | `business:erp:sale-order:status` | 状态流转（撤回/审批/关闭/作废） |
| 17 | GET | `/business/erp/sale-order/{id}/shippable-items` | `business:erp:sale-order:detail` | 可出库明细（`remain_qty > 0`） |

**⑩ GET `/sale-order/page`**

```
page, page_size
keyword?        order_no / 客户名称 ILIKE
customer_id?    UUID
status?         DRAFT|SUBMITTED|APPROVED|PARTIAL_SHIPPED|SHIPPED|CLOSED|CANCELLED（可多值，逗号分隔）
date_from?, date_to?   order_date 区间
salesperson_id?
```

响应行：`order_no / order_date / delivery_date / customer_name / salesperson_name / warehouse_name / status / total_qty / total_amount / total_tax / total_with_tax / credit_hold_amount / shipped_rate（Σshipped_qty / Σqty）`。

**⑪ GET `/sale-order/{id}`**：单头全列 + `items[]`（`biz_sale_order_item` 全列 + `remain_qty = qty - shipped_qty` + `unit_name` + `material_code/material_name/spec`）+ `outbounds[]`（关联出库单：`out_no / out_date / status / total_qty / total_with_tax`）+ `credit` 卡片（本单占用 + 客户可用）。

**⑫ POST `/sale-order`**

```json
{
  "customer_id": "…",                  // 必填
  "order_date": "2026-10-31",          // 必填，不得早于今天-90 天（经验值，防录错年份）
  "delivery_date": "2026-11-10",       // 可空；非空必须 >= order_date
  "salesperson_id": "…",               // 可空，默认取当前登录人
  "warehouse_id": "…",                 // 必填，必须启用
  "currency_code": "CNY",
  "remark": "",
  "items": [{
    "material_id": "…",                // 必填
    "unit_id": "…",                    // 必填
    "qty": "100.0000",                 // > 0
    "price": "11.111100",              // 不含税单价，>= 0，6 位小数
    "tax_rate": "13.0000",             // **百分数** 0–100（13 = 13%），见下方口径校正
    "warehouse_id": null,              // 可空，行级仓覆盖单头仓
    "delivery_date": null,
    "remark": ""
  }]
}
```

> **税率口径校正（实测批次 1，必须照做）**：批次 1 自己就有两套口径，实测证据如下——
>
> | 层 | 位置 | 口径 | 证据 |
> |----|------|------|------|
> | 物料主数据 | `biz_material.tax_rate` | **小数 0–1**（`0.1300`） | DB 实测 `select distinct tax_rate from biz_material` → `0.1300`；`dto.rs:156 require_tax_rate` 限 0–1 |
> | 采购订单行（API） | `CreatePurchaseOrderItemReq.tax_rate` | **小数 0–1** | `dto.rs:2378 tax_amount() = amount × rate` |
> | 收货单行（API/DB） | `biz_stock_in_item.tax_rate` | **百分数 0–100**（`13.0000`） | DB 实测 → `13.0000`；`stockin_dto.rs:411` 校验 `0 ≤ rate ≤ 100` 并 `tax_amount = amount × rate / 100` |
>
> 本批选**百分数**（与"库存单据明细"家族一致，且前端入库页已按百分数交互），原因：① 订单明细与出库明细之间要搬运 `tax_rate`，两者必须同单位；② 出库明细与收货明细同 DDL（`NUMERIC(7,4)`，CHECK `≤100`）、同展示位；③ 税额算法 `amount × tax_rate / 100` 已有实现与测试。**前端从物料主数据带默认税率时必须 ×100**（`0.13 → 13.0000`），这条写进 §5.3 的金额工具与 §8.4 的易错清单。批次 1 采购订单行用 0–1 是既存不一致，本批不改它（不碰批次 1 代码），列入 §8.5 补丁建议。

金额计算（后端权威，与批次 1 §4.3 同一条链，税额用百分数）：

```
行 amount          = round(qty × price, 4)
行 tax_amount      = round(amount × tax_rate / 100, 4)
行 with_tax_amount = amount + tax_amount
单 total_amount    = Σ 行 amount        ← SQL 侧 SUM，不用 Rust 累加
单 total_tax       = Σ 行 tax_amount
单 total_with_tax  = total_amount + total_tax
单 total_qty       = Σ 行 qty
```

**⑬ PUT `/sale-order/{id}`**：请求体同 POST。校验：

| 序 | 校验 | 错误码 |
|----|------|--------|
| 1 | 存在且 `status='DRAFT'` | `66201` |
| 2 | 明细非空 | `66203` |
| 3 | 每行 `qty > 0`、`price ≥ 0`、`0 ≤ tax_rate ≤ 1` | `10001` |
| 4 | 物料存在且启用、单位存在 | `66002` |
| 5 | 行级 `warehouse_id` 非空时必须启用 | `66002` |

事务内 `DELETE FROM biz_sale_order_item WHERE order_id = ?` 后整批重插（明细无外部引用，`DRAFT` 阶段安全）；单头金额三列重算回写。

**⑭ DELETE `/sale-order/{id}`**：仅 `DRAFT`（`66201`），软删。

**⑮ POST `/sale-order/{id}/submit`**（本批最关键的写接口之一）

```text
① 锁订单行 FOR UPDATE；status 必须 DRAFT，否则 66201/66202
② 校验明细非空（66203）
③ 校验客户：deleted_at IS NULL、status=1、credit_status ≠ 'BLOCKED'（66104）
④ 条件 UPDATE 占用信用（§3.5.2 单语句），amount = total_with_tax
   0 行 → 66101（message 带 本次需要/可用/超额 三个数）
   注意 credit_limit = 0（不限）时也走同一条语句，条件是恒真
⑤ 写 biz_customer_credit_txn（txn_type='ORDER_HOLD'，idem_key='ORDER_HOLD:SO:<id>'）
   ON CONFLICT (idem_key) DO NOTHING —— 重复提交不二次占用
⑥ UPDATE 订单：status='SUBMITTED', submitted_at=now, credit_hold_amount=total_with_tax
   条件 `AND status='DRAFT'`，0 行 → 66202
⑦ WATCH 客户：写 approval_instance_id（本批不建流，只留列），响应里回 `need_approval: true`
```

响应：`{ "id": "…", "status": "SUBMITTED", "credit_hold_amount": "113000.0000", "need_approval": true|false, "credit": {...} }`。

**⑯ POST `/sale-order/{id}/status`**

```json
{ "action": "APPROVE|REJECT|WITHDRAW|CLOSE|CANCEL", "reason": "客户取消（CANCEL/REJECT 必填）" }
```

状态机映射（**服务端唯一权威**，前端按 §5.1 的矩阵渲染按钮）：

| action | 允许的 from | to | 副作用 | 错误码 |
|--------|------------|----|--------|--------|
| `APPROVE` | `SUBMITTED` | `APPROVED` | 写 `approved_at` | `66201` |
| `REJECT` | `SUBMITTED` | `CANCELLED` | **释放信用** `ORDER_RELEASE` | `66201` |
| `WITHDRAW` | `SUBMITTED` | `DRAFT` | **释放信用** | `66201` |
| `CLOSE` | `APPROVED`/`PARTIAL_SHIPPED`/`SHIPPED` | `CLOSED` | 释放**剩余**占用（`credit_hold_amount`） | `66201` |
| `CANCEL` | `DRAFT`/`SUBMITTED`/`APPROVED` | `CANCELLED` | 释放占用；**已出库则拒绝** | `66205` |

`reason` 对 `CANCEL`/`REJECT` 必填（`btrim <> ''`），否则 `10001`；DB 侧 `ck_biz_so_state` 兜底（`CANCELLED` 必须 `cancel_reason` 非空，POC-8c 实测）。

**⑰ GET `/sale-order/{id}/shippable-items`**：返回 `remain_qty > 0` 的明细，供出库单创建页带入：

```json
{ "list": [{
    "order_item_id": "…", "line_no": 1, "material_id": "…",
    "material_code": "M-1001", "material_name": "冷轧钢板", "spec": "1.5mm",
    "unit_id": "…", "unit_name": "千克",
    "qty": "100.0000", "shipped_qty": "30.0000", "remain_qty": "70.0000",
    "price": "11.111100", "tax_rate": "0.1300",
    "warehouse_id": "…",
    "serial_required": true,              // 一物一码标记，前端据此强制扫码
    "stock_available": "55.0000"          // 该仓当前可用（预检，不锁定）
  }] }
```

`stock_available` 只做**提示**（前端标红），**不做预留**——预留会引入"锁库"状态与超时释放，本批明确不做（§0.3）。真正的扣减在过账时由条件 UPDATE 判定（`66301`）。

---

### 4.3 销售出库（9 个接口）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 18 | GET | `/business/erp/stock-out/page` | `business:erp:stock-out:list` | 分页 |
| 19 | GET | `/business/erp/stock-out/{id}` | `business:erp:stock-out:detail` | 详情（含明细 + 凭证 + 应收 + 序列号） |
| 20 | POST | `/business/erp/stock-out` | `business:erp:stock-out:create` | 新建（`DRAFT`，取号 `OUT`） |
| 21 | PUT | `/business/erp/stock-out/{id}` | `business:erp:stock-out:update` | 修改（仅 `DRAFT`） |
| 22 | DELETE | `/business/erp/stock-out/{id}` | `business:erp:stock-out:delete` | 删除（仅 `DRAFT`，软删） |
| 23 | POST | `/business/erp/stock-out/{id}/post` | `business:erp:stock-out:status` | **过账**（七步，§3.1） |
| 24 | POST | `/business/erp/stock-out/{id}/cancel` | `business:erp:stock-out:status` | 作废（含红冲） |
| 25 | GET | `/business/erp/stock-out/{id}/pick-locations` | `business:erp:stock-out:detail` | 拣货建议（按库位排序） |
| 26 | GET | `/business/erp/stock-out/available-serials` | `business:erp:stock-out:detail` | 可用序列号（扫码前拉取/校验） |

**⑱ GET `/stock-out/page`**：入参 `page/page_size/keyword?/customer_id?/source_order_id?/status?/date_from?/date_to?/warehouse_id?/out_type?`。响应行含 `out_no/out_date/out_type/status/customer_name/warehouse_name/total_qty/total_with_tax/total_cost/voucher_no/cost_voucher_no/receivable_no/posted_at`。

**⑲ GET `/stock-out/{id}`**：单头全列 + `items[]`（`biz_stock_out_item` 全列 + `location_code` + `material_*` + `serials[]`）+ `vouchers`（两张：`voucher_no/cost_voucher_no` 及金额）+ `receivable`（`receivable_no/amount/received_amount/unreceived_amount`）。`POSTED` 之后所有财务列**只读**，前端置灰（§5.1）。

**⑳ POST `/stock-out`**

```json
{
  "out_type": "SALE",                    // SALE / RETURN / OTHER；SALE 必须给 customer_id
  "source_order_id": "…",                // 可空；非空必须 APPROVED/PARTIAL_SHIPPED 且属于同一客户
  "customer_id": "…",
  "warehouse_id": "…",                   // 必填
  "out_date": "2026-10-31",
  "currency_code": "CNY",
  "remark": "",
  "items": [{
    "source_order_item_id": "…",         // 可空；非空时校验 qty <= remain_qty
    "material_id": "…",
    "unit_id": "…",
    "qty": "33.0000",                    // > 0
    "price": "11.111100",
    "tax_rate": "0.1300",
    "location_id": null,                 // 可空 = 不指定库位（§3.3 自动分配）
    "lot_no": null,
    "serials": ["SN-20261031-000001", "SN-20261031-000002"]
  }]
}
```

校验（创建时**只做形态校验**，不碰库存）：

| 序 | 校验 | 错误码 |
|----|------|--------|
| 1 | 明细非空、每行 `qty > 0`、`0 ≤ tax_rate ≤ 1` | `10001` |
| 2 | 单据仓启用；`source_order_id` 与 `customer_id` 一致 | `66002` / `10001` |
| 3 | 一物一码物料（`biz_material.serial_required=true`）：`len(serials) == qty`，且序列号不重复 | `66305` |
| 4 | 非一物一码物料：`serials` 必须为空 | `66007`（反向复用：未启用序列号却传了码） |
| 5 | 序列号存在、状态 `IN_STOCK`、`warehouse_id` == 单据仓 | `66306` |
| 6 | 行级 `location_id` 非空时属于该仓且 `status=1` | `66002` |

> **创建时就校验序列号状态**是刻意的：让"选了别人的码"在**保存**时就报错，而不是等到过账（过账失败会整单回滚，操作员丢了整批录入）。但创建**不 lock 码**（`IN_STOCK` 只是快照判断），真正的抢占在过账的 CAS（`66307`）。

**㉑ PUT `/stock-out/{id}`**：仅 `DRAFT`（`66308`），请求体同 POST，事务内整批重建明细与序列号子表。

**㉒ DELETE `/stock-out/{id}`**：仅 `DRAFT`（`66308`），软删。

**㉓ POST `/stock-out/{id}/post`**（核心）

请求体：无（可带 `{ "idempotency_key": "…" }`）。响应：

```json
{
  "id": "…", "out_no": "OUT-20261031-000007", "status": "POSTED",
  "total_cost": "334.0722",
  "voucher_no": "PZ-20261031-000021",
  "cost_voucher_no": "PZ-20261031-000022",
  "receivable_no": "AR-20261031-000012",
  "warnings": ["物料 M-1002 在仓「成品仓」出库后余额 -5.0000（允许负库存）"]
}
```

错误码走 §3.8 回滚矩阵：`66302`（重复过账/状态冲突）、`66301`（库存不足）、`66304`（库位不足）、`66307`（序列号被抢）、`66303`（超发）、`66002`（科目缺失）、`10001`（借贷不平）。

`warnings` 是本批新增（**不阻断**）：仅在"允许负库存"仓库出库产生负结存、或"尾差倒挤清空"时返回，供前端黄色提示条展示。语义：**成功但有需要人知道的事**。这是唯一一个在 2xx 响应里带"异常"字段的接口，其余一律用错误码。

**㉔ POST `/stock-out/{id}/cancel`**

```json
{ "reason": "客户退货，重开单", "idempotency_key": "…" }
```

按 §3.9.2 的顺序执行：红冲两张凭证 → 反向加回库存（`txn_type='IN'`, `biz_type='SALE_OUT_CANCEL'`）→ 序列号 CAS 回 `IN_STOCK` → 应收作废 → 释放信用 → 状态 `CANCELLED`。

| 校验 | 错误码 |
|------|--------|
| `reason` 非空 | `10001` |
| `status='POSTED'`（`DRAFT` 直接走 DELETE） | `66302` |
| 关联应收 `received_amount > 0`（已收款） | `66309` |
| 序列号当前不是 `OUTBOUND`（被调拨/报废过） | 不阻断，但 `warnings` 提示"码 SN-… 已不在本仓，未回滚状态" |

**㉕ GET `/stock-out/{id}/pick-locations`**：入库前给操作员的拣货建议，**只读、不锁**。

```
material_id（必填）, warehouse_id（必填）, qty（必填）
```

响应按 §3.3 的排序 `(qty DESC, location_code ASC)`：

```json
{ "list": [{
    "inventory_id": "…",              // biz_inventory.id（成本桶行）
    "inventory_location_id": "…",     // biz_inventory_location.id（数量账行，可空）
    "location_code": "A-03", "location_name": "A 区 3 号位",
    "qty": "5.0000", "suggested_qty": "5.0000",
    "avg_cost": "10.123400"
  }],
  "mode": "LOCATION" | "WAREHOUSE_TOTAL",
  "total_available": "8.0000",
  "shortage": "0.0000"
}
```

`mode=WAREHOUSE_TOTAL` 时 `inventory_location_id` 为 null（该仓该物料没有库位行，只扣整仓汇总桶，§3.3 / D7）。

**㉖ GET `/stock-out/available-serials`**：扫码时可先拉候选，也可直接用 `GET /material-serial/exists?sn=…` 单条校验（批次 1 已有接口，复用）。

```
material_id（必填）, warehouse_id（必填）, keyword?（序列号片段）, limit?（默认 100，上限 500）
```

响应：`{ "list": [{ "serial_id","serial_no","external_sn","status","location_code","in_stock_at" }] }`，仅 `status='IN_STOCK'`。

---
### 4.4 应收（4 个接口）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 27 | GET | `/business/erp/receivable/page` | `business:erp:receivable:list` | 分页（含逾期标记） |
| 28 | GET | `/business/erp/receivable/{id}` | `business:erp:receivable:detail` | 详情（含来源单据 + 核销历史） |
| 29 | GET | `/business/erp/receivable/aging` | `business:erp:receivable:list` | 账龄分析（按客户 / 汇总两档） |
| 30 | GET | `/business/erp/receivable/due-reminder` | `business:erp:receivable:list` | 到期提醒（未来 N 天 + 已逾期） |

> **应收没有 create / update / delete 接口**：应收只能由出库过账**派生**（`uk_biz_ar_source` 保证一出库一应收），也只能由出库作废**连带作废**。任何"手工建应收"的入口都会被审计问"这张应收对应哪批货"。手工调账走收款/红字，不走进销货应收。这是本批刻意收缩的接口面（§0.3）。

**㉗ GET `/receivable/page`**

```
page, page_size
keyword?        receivable_no / source_no / 客户名称
customer_id?    UUID
status?         OPEN|PARTIAL_RECEIVED|SETTLED|CANCELLED
overdue?        true → due_date < today AND status IN ('OPEN','PARTIAL_RECEIVED')
date_from?, date_to?        按 created_at（应收生成日）
due_from?, due_to?          按 due_date
period_code?    2026-10
```

响应行（`biz_receivable` 全列 + 三派生列）：

```json
{ "list": [{
    "id": "…", "receivable_no": "AR-20261031-000012",
    "customer_id": "…", "customer_name": "北京某某机械有限公司",
    "source_type": "STOCK_OUT", "source_id": "…", "source_no": "OUT-20261031-000007",
    "invoice_no": null, "invoice_date": null,
    "amount": "113000.0000", "received_amount": "30000.0000", "unreceived_amount": "83000.0000",
    "credit_days": 30, "due_date": "2026-11-30", "period_code": "2026-10",
    "status": "PARTIAL_RECEIVED",
    "overdue_days": 0,                    // today - due_date，未逾期为 0
    "aging_bucket": "0-30",               // 0-30 / 31-60 / 61-90 / 91+
    "voucher_no": "PZ-20261031-000021",
    "refunded": false,
    "created_at": "2026-10-31T…"
  }], "total": 1, "page": 1, "page_size": 20,
  "summary": { "amount_total": "…", "received_total": "…", "unreceived_total": "…", "overdue_total": "…" } }
```

`summary` 是**当页过滤条件下的全量聚合**（不是当页合计），一条 `SUM … OVER ()` 附带，供列表页顶部卡片用。

**㉘ GET `/receivable/{id}`**：单头全列 + `source`（来源出库单：`out_no/out_date/total_amount/total_with_tax/total_cost`）+ `write_offs[]`（`biz_receipt_item` 行：`receipt_no/receipt_date/amount/before_unreceived/after_unreceived`，**含已撤销收款的行但标 `is_reversed: true`**）+ `voucher`（凭证号 + 借贷合计）。

**㉙ GET `/receivable/aging`**

```
as_of?       基准日，默认 today
customer_id? 可选，给了就返回该客户明细
group_by     customer | month      （默认 customer）
```

响应（`group_by=customer`）：

```json
{ "as_of": "2026-10-31", "buckets": ["0-30","31-60","61-90","91+"],
  "list": [{
    "customer_id": "…", "customer_name": "北京某某",
    "b0_30": "83000.0000", "b31_60": "0.0000", "b61_90": "12000.0000", "b91_plus": "5000.0000",
    "total": "100000.0000", "overdue_total": "17000.0000"
  }],
  "grand_total": { "b0_30": "…", "b31_60": "…", "b61_90": "…", "b91_plus": "…", "total": "…" } }
```

账龄口径（写进 SQL 注释，与巡检 I5 同一条公式）：

```sql
-- 账龄基准 = 应收生成日（ar.created_at::date），不是 due_date
-- 理由：账龄是"这笔钱挂了多久"，逾期天数才是"过了约定的多久"。
-- 两个口径混用会让 91+ 桶虚高（due_date 已经被 credit_days 推后过一次）。
age_days = $as_of - ar.created_at::date
bucket   = CASE WHEN age_days <= 30 THEN '0-30'
                WHEN age_days <= 60 THEN '31-60'
                WHEN age_days <= 90 THEN '61-90' ELSE '91+' END
WHERE ar.status IN ('OPEN','PARTIAL_RECEIVED')   -- 已结清/已作废不进账龄
```

**㉚ GET `/receivable/due-reminder`**

```
days_ahead?  默认 7（未来 N 天内到期）
include_overdue? 默认 true
customer_id?
```

响应：两段 `upcoming[]` / `overdue[]`，每行 `receivable_no/customer_name/unreceived_amount/due_date/days_left（负数=逾期天数）`，按 `due_date ASC`。**只读，不发通知**（发通知是批次 3 的消息中心，本批不做，§0.3）。

---

### 4.5 收款与核销（5 个接口）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 31 | GET | `/business/erp/receipt/page` | `business:erp:receipt:list` | 分页 |
| 32 | GET | `/business/erp/receipt/{id}` | `business:erp:receipt:detail` | 详情（含核销明细） |
| 33 | POST | `/business/erp/receipt` | `business:erp:receipt:create` | 登记收款（`DRAFT`，取号 `RCPT`） |
| 34 | POST | `/business/erp/receipt/{id}/post` | `business:erp:receipt:status` | **过账 + 核销**（单事务，§3.6） |
| 35 | POST | `/business/erp/receipt/{id}/cancel` | `business:erp:receipt:status` | 撤销（红冲 + 反核销） |

**㉛ GET `/receipt/page`**：入参 `page/page_size/keyword?/customer_id?/status?/receipt_method?/date_from?/date_to?/unapplied?`（`true` = 只返回 `unapplied_amount > 0` 的预收单）。响应行含 `receipt_no/receipt_date/customer_name/receipt_method/amount/write_off_total/unapplied_amount/status/voucher_no`。

**㉜ GET `/receipt/{id}`**：单头全列 + `items[]`（`biz_receipt_item` 全列 + `receivable_no/due_date`）+ `voucher`（`voucher_no/total_debit/total_credit`）+ `customer`（名称 + 当前可用额度）。

**㉝ POST `/receipt`**

```json
{
  "customer_id": "…",                  // 必填
  "receipt_date": "2026-10-31",        // 必填，不得晚于今天+1（防录错）
  "receipt_method": "BANK",            // BANK / CASH / NOTE / OTHER
  "bank_account": "6222 0000 0000 0000",
  "reference_no": "转账流水号",
  "currency_code": "CNY",
  "amount": "50000.0000",              // > 0
  "remark": "",
  "write_offs": [{                     // 可空 = 纯预收（DRAFT 阶段可先不核销）
    "receivable_id": "…",
    "amount": "30000.0000"             // > 0
  }]
}
```

校验（DRAFT 阶段做"能做的全部"，核销余额的精确判定留到过账）：

| 序 | 校验 | 错误码 |
|----|------|--------|
| 1 | `amount > 0`、`receipt_method` 四值之一 | `10001` |
| 2 | 客户存在且启用 | `66002` |
| 3 | `Σ write_offs.amount ≤ amount` | `66503` |
| 4 | `write_offs` 里 `receivable_id` 不重复 | `66506` |
| 5 | 每张应收的 `customer_id` == 收款单客户 | `66501` |
| 6 | 每张应收 `status IN ('OPEN','PARTIAL_RECEIVED')` | `66502` |
| 7 | `write_offs.amount ≤ 该应收 unreceived_amount`（**快照校验**） | `66502` |

> 第 7 条在 DRAFT 阶段只是"当时的快照"，过账时还会用条件 UPDATE 再判一次（POC-6b：剩余 400 时核销 500 → 0 行）。两处都判不是冗余：DRAFT 校验给操作员即时反馈，过账校验防并发。

响应：`201` + 收款单详情（`status='DRAFT'`，`write_off_total` / `unapplied_amount` 已算好，但**不动应收**）。

**㉞ POST `/receipt/{id}/post`**（核销）

```json
{
  "write_offs": [{ "receivable_id": "…", "amount": "30000.0000" }],   // 可空 = 用 DRAFT 时登记的
  "idempotency_key": "…"
}
```

执行 §3.6 的九步。成功响应：

```json
{
  "id": "…", "receipt_no": "RCPT-20261031-000004", "status": "POSTED",
  "amount": "50000.0000", "write_off_total": "30000.0000", "unapplied_amount": "20000.0000",
  "voucher_no": "PZ-20261031-000031",
  "receivables": [
    { "receivable_id": "…", "receivable_no": "AR-…", "before_unreceived": "83000.0000",
      "after_unreceived": "53000.0000", "status": "PARTIAL_RECEIVED" }
  ],
  "credit": { "ar_used": "53000.0000", "available": "47000.0000" }
}
```

错误码：`66503`（超收款额）、`66502`（应收已结清/已作废）、`66501`（跨客户）、`66504`（重复过账）、`66506`（重复挂同一应收）、`10001`（借贷不平）、`66002`（科目缺失）。

**㉟ POST `/receipt/{id}/cancel`**

```json
{ "reason": "款项退回", "idempotency_key": "…" }
```

顺序：红冲收款凭证 → 逐张反核销（`received_amount -= write_off`、`unreceived_amount += write_off`、状态按余额回退）→ 信用 `ar_used += 核销额`（释放的反向）→ `status='CANCELLED'`。

| 校验 | 错误码 |
|------|--------|
| `reason` 非空 | `10001` |
| `status='POSTED'` | `66504` |
| 反核销后某张应收的 `received_amount < 0`（数据已漂移） | `66502` |

**核销明细行不删**（`biz_receipt_item` 无 `deleted_at`，审计要求"核销发生过必须留痕"），撤销效果由红字凭证 + 应收余额回退表达。响应里 `items[].reversed = true` 由 `receipt.status='CANCELLED'` 推导，不落列。

**预收再核销**：`unapplied_amount > 0` 的 `POSTED` 收款单，用同一接口 `POST /receipt/{id}/post` 二次提交 `write_offs`（不重复过账——内部走 `apply_additional_write_off`，只做 ⑥⑦⑧⑨ 四步，不再生成第二张收款凭证，而是补一张同号的"预收核销凭证"需额外说明）。

> **预收二次核销的凭证口径（必须钉死）**：第一次过账的凭证金额 = 当时 `write_off_total`。二次核销时**不能再生成整额凭证**（会重复入账），只对新核销部分生成凭证，`source_type='RECEIPT_APPLY'`、`source_id=收款单 id`，摘要标"预收核销"。**不新增第 4 条凭证规则**：科目解析走一条显式回落链 `RECEIPT_APPLY → RECEIPT_DEFAULT`（V0167 仍是 3 条规则，见 §2.7.3），回落映射写成常量表 `SOURCE_RULE_FALLBACK`，`resolve_sales_accounts` 先按 `biz_type` 查规则、查不到再查回落表、再落空才用 `DEFAULT_*` 常量（§3.7.3 的四级链不变，只在 ① 与 ③ 之间插一步）。若不做二次核销，`unapplied_amount` 就只能靠"退款"落地，业务上不成立（客户确实会多付）。

---

### 4.6 客户对账单（5 个接口）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 36 | GET | `/business/erp/customer-statement/page` | `business:erp:statement:list` | 分页 |
| 37 | GET | `/business/erp/customer-statement/{id}` | `business:erp:statement:detail` | 详情（含明细行） |
| 38 | POST | `/business/erp/customer-statement/generate` | `business:erp:statement:generate` | 生成（按客户 + 区间，取号 `STS`） |
| 39 | POST | `/business/erp/customer-statement/{id}/confirm` | `business:erp:statement:confirm` | 确认（锁定） |
| 40 | GET | `/business/erp/customer-statement/{id}/export` | `business:erp:statement:detail` | 导出 CSV（对账函） |

**㊱ GET `/customer-statement/page`**：入参 `page/page_size/keyword?/customer_id?/status?/period_from?/period_to?`。响应行含 `statement_no/customer_name/period_start/period_end/opening_balance/debit_total/credit_total/closing_balance/status/confirmed_at`。

**㊲ GET `/customer-statement/{id}`**：单头全列 + `lines[]`（`biz_customer_statement_line` 全列，按 `line_no ASC`）+ `check`：

```json
{ "check": {
    "opening_balance": "12000.0000",
    "debit_total": "113000.0000",
    "credit_total": "50000.0000",
    "closing_balance": "75000.0000",
    "identity_ok": true,                          // closing = opening + debit - credit
    "ledger_match": { "unreceived_total": "75000.0000", "ok": true }   // 与应收未收合计对账
  } }
```

`ledger_match` 是本批的自检设计：对账单期末必须等于"该客户当前未结应收合计"（若区间终点是今天）。对不上说明区间末到今天之间又发生了业务——不是错误，但要在 UI 上说明"期末余额为 2026-10-31 快照，当前未结为 X"。若区间终点就是今天还对不上，就是真 bug（巡检 I6/I7 会抓）。

**㊳ POST `/customer-statement/generate`**

```json
{ "customer_id": "…", "period_start": "2026-10-01", "period_end": "2026-10-31" }
```

生成规则（全部在**一个只读事务**里算，再整体插入）：

```text
① opening_balance = Σ (该客户在 period_start 之前的应收 amount) − Σ (同期收款核销额)
   —— 即"上期结转"，用同一套流水口径算出，不依赖上一张对账单是否存在
② 明细行 = 期间内的两类流水，按 biz_date, doc_no 升序：
     · 应收产生（出库过账）：debit_amount = ar.amount（价税合计，不是不含税）
     · 收款核销：credit_amount = write_off 额
     · 收款预收（未核销）：credit_amount = unapplied 额（钱到了但没冲账，必须在对账单上体现）
③ balance 逐行累进：balance_i = balance_{i-1} + debit_i − credit_i（首行 balance = opening + 本行）
④ closing_balance = 最后一行的 balance（无明细时 = opening）
⑤ 头表 CHECK 兜底 closing = opening + debit_total − credit_total
```

| 校验 | 错误码 |
|------|--------|
| `period_start ≤ period_end` | `10001` |
| 区间内无流水（三张表都没命中） | `66601` |
| 同客户同区间已存在（`uk_biz_stmt_period`） | `66602` |
| 客户不存在 | `66002` |

**㊴ POST `/customer-statement/{id}/confirm`**：`DRAFT → CONFIRMED`，写 `confirmed_at/confirmed_by`。之后**禁止任何修改**（`66603`），只能作废重开。响应回完整详情。

**㊵ GET `/customer-statement/{id}/export`**：`text/csv; charset=utf-8`，UTF-8 BOM 头（Excel 打开不乱码），列：`序号,业务日期,单据类型,单据编号,摘要,借方,贷方,余额`。金额按 4 位小数输出，空值输出空串（不输出 `0.0000`，避免财务误读）。

---

### 4.7 销售看板（1 个接口）

| # | 方法 | 后端路径 | 权限码 | 说明 |
|---|------|---------|--------|------|
| 41 | GET | `/business/erp/sales/summary` | `business:erp:sale-order:list` | 首页四卡 + 待办计数 |

```
date_from?  默认本月 1 日
date_to?    默认今天
```

```json
{
  "period": { "from": "2026-10-01", "to": "2026-10-31" },
  "cards": {
    "order_amount": "1280000.0000",       // Σ biz_sale_order.total_with_tax（status NOT IN DRAFT,CANCELLED）
    "shipped_amount": "980000.0000",      // Σ biz_stock_out.total_with_tax（status=POSTED）
    "unreceived_amount": "750000.0000",   // Σ biz_receivable.unreceived_amount（未结）
    "gross_profit": "210000.0000"         // Σ(出库 total_amount − total_cost)，仅 POSTED
  },
  "todos": {
    "pending_approval": 3,                // 订单 status=SUBMITTED
    "draft_outbound": 2,                  // 出库 status=DRAFT
    "overdue_receivable": 5,              // due_date < today 且未结
    "over_credit_customers": 1            // used_amount > credit_limit AND credit_limit > 0
  }
}
```

> **毛利口径警告（写在响应注释里，不隐藏）**：`gross_profit` 用的是"售价不含税 − 出库成本"，**不含**期间费用、不摊运费、不调汇兑，只是**粗略毛利**。财务要的是利润表口径（批次 4 结账），这张卡只给销售看趋势用。字段名故意叫 `gross_profit` 而不是 `profit`。

---

### 4.8 41 个接口总表（自检用）

| # | 模块 | 方法 | 路径 | 权限码 |
|---|------|------|------|--------|
| 1 | 客户 | GET | `/customer/page` | `customer:list` |
| 2 | 客户 | GET | `/customer/{id}` | `customer:detail` |
| 3 | 客户 | POST | `/customer` | `customer:create` |
| 4 | 客户 | PUT | `/customer/{id}` | `customer:update` |
| 5 | 客户 | DELETE | `/customer/{id}` | `customer:delete` |
| 6 | 信用 | GET | `/credit/page` | `credit:list` |
| 7 | 信用 | GET | `/credit/{customer_id}` | `credit:list` |
| 8 | 信用 | GET | `/credit/{customer_id}/txns` | `credit:list` |
| 9 | 信用 | PUT | `/credit/{customer_id}` | `credit:update` |
| 10 | 订单 | GET | `/sale-order/page` | `sale-order:list` |
| 11 | 订单 | GET | `/sale-order/{id}` | `sale-order:detail` |
| 12 | 订单 | POST | `/sale-order` | `sale-order:create` |
| 13 | 订单 | PUT | `/sale-order/{id}` | `sale-order:update` |
| 14 | 订单 | DELETE | `/sale-order/{id}` | `sale-order:delete` |
| 15 | 订单 | POST | `/sale-order/{id}/submit` | `sale-order:status` |
| 16 | 订单 | POST | `/sale-order/{id}/status` | `sale-order:status` |
| 17 | 订单 | GET | `/sale-order/{id}/shippable-items` | `sale-order:detail` |
| 18 | 出库 | GET | `/stock-out/page` | `stock-out:list` |
| 19 | 出库 | GET | `/stock-out/{id}` | `stock-out:detail` |
| 20 | 出库 | POST | `/stock-out` | `stock-out:create` |
| 21 | 出库 | PUT | `/stock-out/{id}` | `stock-out:update` |
| 22 | 出库 | DELETE | `/stock-out/{id}` | `stock-out:delete` |
| 23 | 出库 | POST | `/stock-out/{id}/post` | `stock-out:status` |
| 24 | 出库 | POST | `/stock-out/{id}/cancel` | `stock-out:status` |
| 25 | 出库 | GET | `/stock-out/{id}/pick-locations` | `stock-out:detail` |
| 26 | 出库 | GET | `/stock-out/available-serials` | `stock-out:detail` |
| 27 | 应收 | GET | `/receivable/page` | `receivable:list` |
| 28 | 应收 | GET | `/receivable/{id}` | `receivable:detail` |
| 29 | 应收 | GET | `/receivable/aging` | `receivable:list` |
| 30 | 应收 | GET | `/receivable/due-reminder` | `receivable:list` |
| 31 | 收款 | GET | `/receipt/page` | `receipt:list` |
| 32 | 收款 | GET | `/receipt/{id}` | `receipt:detail` |
| 33 | 收款 | POST | `/receipt` | `receipt:create` |
| 34 | 收款 | POST | `/receipt/{id}/post` | `receipt:status` |
| 35 | 收款 | POST | `/receipt/{id}/cancel` | `receipt:status` |
| 36 | 对账 | GET | `/customer-statement/page` | `statement:list` |
| 37 | 对账 | GET | `/customer-statement/{id}` | `statement:detail` |
| 38 | 对账 | POST | `/customer-statement/generate` | `statement:generate` |
| 39 | 对账 | POST | `/customer-statement/{id}/confirm` | `statement:confirm` |
| 40 | 对账 | GET | `/customer-statement/{id}/export` | `statement:detail` |
| 41 | 看板 | GET | `/sales/summary` | `sale-order:list` |

（路径前缀统一 `/business/erp`；权限码前缀统一 `business:erp:`，表中为省宽度省略。）

**接口数校验**：客户 5 + 信用 4 + 订单 8 + 出库 9 + 应收 4 + 收款 5 + 对账 5 + 看板 1 = **41** —— 与 §0 目录声明的 41 一致。

### 4.9 权限码三方一致表（V0165 的 29 个码 ↔ 接口 ↔ 前端路由）

三处必须一致：`sys_menu.auth_code`（后端鉴权读这张表）、`#[nik::permission("…")]`（handler 上的宏）、前端路由 `meta.auth`（菜单渲染 + 按钮显隐）。**任一处拼错 → 表现为 403 或按钮消失**，这是批次 1 踩过的坑（`docs/ERP-批次1-技术架构.md` §4.9 原话）。

| 权限码 | 用途 | 接口 # | 前端路由 / 按钮 |
|--------|------|--------|----------------|
| `business:erp:customer:list` | 列表 | 1 | 菜单「客户管理」 |
| `business:erp:customer:detail` | 详情 | 2 | 行内「详情」 |
| `business:erp:customer:create` | 新建 | 3 | 工具栏「新建客户」 |
| `business:erp:customer:update` | 编辑 | 4 | 行内「编辑」 |
| `business:erp:customer:delete` | 删除 | 5 | 行内「删除」 |
| `business:erp:credit:list` | 信用查询 | 6,7,8 | 菜单「客户信用」 |
| `business:erp:credit:update` | 调整额度 | 9 | 「调整额度」按钮 |
| `business:erp:sale-order:list` | 列表 | 10,41 | 菜单「销售订单」/ 首页卡片 |
| `business:erp:sale-order:detail` | 详情 | 11,17 | 「详情」/ 出库页带入 |
| `business:erp:sale-order:create` | 新建 | 12 | 「新建订单」 |
| `business:erp:sale-order:update` | 编辑 | 13 | 「编辑」 |
| `business:erp:sale-order:delete` | 删除 | 14 | 「删除」 |
| `business:erp:sale-order:status` | 流转 | 15,16 | 「提交 / 审批 / 关闭 / 作废」 |
| `business:erp:stock-out:list` | 列表 | 18 | 菜单「销售出库」 |
| `business:erp:stock-out:detail` | 详情 | 19,25,26 | 「详情 / 拣货建议 / 可用序列号」 |
| `business:erp:stock-out:create` | 新建 | 20 | 「新建出库单」 |
| `business:erp:stock-out:update` | 编辑 | 21 | 「编辑」 |
| `business:erp:stock-out:delete` | 删除 | 22 | 「删除」 |
| `business:erp:stock-out:status` | 过账/作废 | 23,24 | 「过账」「作废」 |
| `business:erp:receivable:list` | 列表 | 27,29,30 | 菜单「应收单」/ 账龄 / 到期 |
| `business:erp:receivable:detail` | 详情 | 28 | 「详情」 |
| `business:erp:receipt:list` | 列表 | 31 | 菜单「收款单」 |
| `business:erp:receipt:detail` | 详情 | 32 | 「详情」 |
| `business:erp:receipt:create` | 登记 | 33 | 「登记收款」 |
| `business:erp:receipt:status` | 过账/撤销 | 34,35 | 「核销过账」「撤销」 |
| `business:erp:statement:list` | 列表 | 36 | 菜单「客户对账单」 |
| `business:erp:statement:detail` | 详情/导出 | 37,40 | 「详情」「导出 CSV」 |
| `business:erp:statement:generate` | 生成 | 38 | 「生成对账单」 |
| `business:erp:statement:confirm` | 确认 | 39 | 「确认」 |

**29 个码 ↔ 41 个接口**没有一对一关系，这是刻意的：只读类接口共用 `:list`/`:detail`（账龄、到期提醒、拣货建议都不是独立权限点），写类接口按"一组动作一个码"归并（订单的提交/审批/关闭/作废共一个 `:status`，因为业务上不允许"能提交不能作废"这种拆法——批次 1 采购订单的 `submit`/`approve`/`close`/`cancel` 是 4 个码，实测运维配权限时容易漏配导致单据卡死，本批合并）。

---
## 5. 前端架构

本章写"页面怎么长、按钮什么时候出现、扫码怎么扫、金额怎么算才不飘"。所有路径、文件名、测试命令都取自仓库实测（`frontend/src/router/index.ts`、`frontend/tests/unit/`、`frontend/package.json`）。

### 5.1 目录与文件清单（对齐批次 1 的落位）

批次 1 的既有约定（实测）：API 契约集中在 `frontend/src/api/business/erp/index.ts` 一份（`warehouse-api.ts` 已退化为 `export * from './index'` 的兼容壳），页面按模块一目录，每目录 `index.vue` + `<module>-logic.ts` 两个文件，纯逻辑（金额 / 状态机 / 载荷构造）全在 `*-logic.ts`，`.vue` 只渲染。

| 新增文件 | 内容 | 行数估算 |
|---------|------|---------|
| `frontend/src/api/business/erp/sales-api.ts` | 41 个接口的请求函数 + 类型（`SalesPageQuery` / `SaleOrderRes` / `StockOutRes` / `ReceivableRes` / `ReceiptRes` / `StatementRes` …），由 `index.ts` `export * from './sales-api'` 统一出口 | ~620 |
| `views/business/erp/customer/index.vue` + `customer-logic.ts` | 客户列表 / 表单抽屉 / 信用卡片 / 删除保护提示 | ~560 + ~260 |
| `views/business/erp/customer-credit/index.vue` + `credit-logic.ts` | 信用列表 / 调额对话框 / 占用流水时间线 | ~380 + ~220 |
| `views/business/erp/sale-order/index.vue` + `saleorder-logic.ts` | 订单列表 / 编辑页（明细行）/ 状态按钮矩阵 / 可出库带入 | ~780 + ~420 |
| `views/business/erp/stock-out/index.vue` + `stockout-logic.ts` | 出库单编辑 / **扫码面板** / 拣货建议 / 过账确认弹窗 | ~860 + ~520 |
| `views/business/erp/receivable/index.vue` + `receivable-logic.ts` | 应收列表 / 账龄视图 / 到期提醒 | ~480 + ~200 |
| `views/business/erp/receipt/index.vue` + `receipt-logic.ts` | 收款登记 / **核销工作台**（左收款右应收，勾选分配） | ~620 + ~380 |
| `views/business/erp/customer-statement/index.vue` + `statement-logic.ts` | 对账单列表 / 生成对话框 / 打印视图 | ~460 + ~220 |
| `views/business/erp/shared/sales-format.ts` | 销售侧展示口径（`statusLabel` / `statusTagType` / `agingLabel` / `methodLabel`），**不重复**造金额函数（复用 `shared/money.ts`） | ~180 |

**总量**：8 个页面目录 + 1 个 API 文件 + 1 个展示口径文件，约 6200 行 TS/Vue（含前端单测约 30%）。

**为什么不把 7 个页面塞进 `views/business/erp/sales/` 一个目录**：批次 1 的菜单是"一个菜单一个路由一个目录"（`material / warehouse / serial / transfer / purchase / stockin / payable / voucher / ledger` 九个并列目录），`sys_menu.path` 直接对应目录名，菜单点进去就是 `/business/erp/xxx`。批次 2 若另起目录层级，`sys_menu` 的 path 与前端路由会出现"看起来像两套东西"的维护负担。

### 5.2 路由与菜单（7 条路由）

在 `frontend/src/router/index.ts` 的 ERP 段追加（与批次 1 同结构：`path` = `sys_menu.path`、`alias` = 目录名、`meta.permission` = 权限码）：

| 路由 path | alias | name | 组件 | `meta.permission` |
|-----------|-------|------|------|-------------------|
| `business/erp/customer` | — | `BusinessErpCustomer` | `views/business/erp/customer/index.vue` | `business:erp:customer:list` |
| `business/erp/customer-credit` | — | `BusinessErpCustomerCredit` | `views/business/erp/customer-credit/index.vue` | `business:erp:credit:list` |
| `business/erp/sale-order` | `business/erp/sales` | `BusinessErpSaleOrder` | `views/business/erp/sale-order/index.vue` | `business:erp:sale-order:list` |
| `business/erp/stock-out` | `business/erp/stockout` | `BusinessErpStockOut` | `views/business/erp/stock-out/index.vue` | `business:erp:stock-out:list` |
| `business/erp/receivable` | — | `BusinessErpReceivable` | `views/business/erp/receivable/index.vue` | `business:erp:receivable:list` |
| `business/erp/receipt` | — | `BusinessErpReceipt` | `views/business/erp/receipt/index.vue` | `business:erp:receipt:list` |
| `business/erp/customer-statement` | `business/erp/statement` | `BusinessErpStatement` | `views/business/erp/customer-statement/index.vue` | `business:erp:statement:list` |

**菜单节点已在 V0165 种好**（`sys_menu` 7 个 `menu_type=2` 父节点 + 22 个 `menu_type=3` 按钮，挂 `…946` ERP 台账 下，见 §2.7.1），所以**不需要再补前端假菜单**——批次 1 的应付页就是因为 V0148 没种菜单，只能按 URL 进（`router/index.ts:400-408` 的注释写明了这个坑）。本批把菜单种子放到**同一批迁移**里，避免"后端建表、菜单靠前端补"的返工。

### 5.3 金额与税率契约（前端最容易算错的三处）

**① 金额一律整数分**（复用 `views/business/erp/shared/money.ts`，该文件文件头已把理由写死）：

```ts
// 后端 Decimal → JSON 字符串 "226.0000"，前端是 double；
// 比较与累加必须过 toCents（整数分），否则 0.1+0.2 !== 0.3，
// 借贷平衡判断会因 226.00000000000003 假报警。
import { toCents, centsToNumber, formatCents } from '@/views/business/erp/shared/money';
```

销售侧**禁止**新建 `formatMoney`。批次 1 的 `purchase-logic.ts` 里那份 `formatMoney` 已被 `money.ts` 取代，理由写在 `money.ts` 头注释（"金额归一会算错钱，需要一个被单测钉住的单一实现"）。本批只在 `sales-format.ts` 里做标签映射。

**② 税率：明细层是百分数（0–100），物料主数据是小数（0–1）**

这是 §4.2 里实测过的口径分裂。前端处理规则：

| 场景 | 规则 | 实现位置 |
|------|------|---------|
| 从物料带默认税率 | `tax_rate(行) = toNumber(material.tax_rate) * 100` | `saleorder-logic.ts::applyMaterial` |
| 用户在行内编辑 | 直接编辑百分数（`13`），校验 `0 ≤ x ≤ 100` | `saleorder-logic.ts::validateLines` |
| 提交给后端 | `String(tax_rate)`（保留 4 位：`13.0000`） | `buildSaleOrderPayload` |
| 展示 | `13%`（`toFixed(2)` 去尾零） | `sales-format.ts::formatTaxRate` |

**③ 税额与合计：逐行舍入后求和，不在合计上再乘一次**

```ts
// 与后端 sale_out_lines / sum_lines 逐字同口径（税率是百分数，所以 /100）：
//   行 amount      = round(qty × price, 4)
//   行 tax_amount  = round(amount × tax_rate / 100, 4)
//   行 with_tax    = amount + tax_amount
//   单合计         = Σ(逐行舍入后的值)   ← 不是 round(Σ qty × Σ price)
// 实现上用 decimal.js-lite？——不引依赖。用整数微单位（1e6）做乘除，最后 round 到 4 位：
export function lineAmounts(qty: number, price: number, taxRatePercent: number) {
  const amount = round4(qty * price);
  const tax = round4((amount * taxRatePercent) / 100);
  return { amount, tax_amount: tax, with_tax_amount: round4(amount + tax) };
}

/** 4 位小数的 half-away-from-zero 舍入，与 Rust `Decimal::round_dp(4)` 一致 */
export function round4(v: number): number {
  const scaled = Math.abs(v) * 1e4;
  const r = Math.round(scaled + 1e-6);      // 1e-6 吸收 double 表示误差（如 1.005*1e4=10049.999…）
  return (v < 0 ? -r : r) / 1e4;
}
```

**提交后必须以响应为准重绘**：前端预览与后端落库可能差 1 分（浮点 + 舍入时机），批次 1 §4.3 已定为硬规矩。本批在此基础上加一条：**过账类接口的响应里带权威财务值**（`total_cost` / `voucher_no` / `receivable_no`），前端拿到后整块覆盖，不做"增量更新"。

### 5.4 扫码交互（一物一码出库）

**硬件形态决定方案**：ERP 的仓储现场用**条码枪**（USB HID，键盘模拟），不是手机摄像头。批次 1 的序列号页（`views/business/erp/serial/`）已验证这条路径。本批三种输入并存，优先级从高到低：

| 方式 | 实现 | 适用 |
|------|------|------|
| **扫码枪（主）** | 一个常驻 `@keydown` 监听：字符间隔 < 35ms 且以 `Enter` 结尾 → 判定为扫码枪输入，**不**逐字符触发搜索 | 桌面 ERP + 有线/无线扫码枪 |
| 手工输入（兜底） | 输入框 + `Enter` 提交，逐条校验 | 码磨损、扫码枪坏了 |
| 摄像头（可选，不做硬要求） | `BarcodeDetector` API（Chrome 83+），`getUserMedia` + `requestAnimationFrame` 解码 | 笔记本/平板临时用 |

```ts
// stockout-logic.ts
export interface ScanResult {
  serial_no: string;
  status: 'OK' | 'DUPLICATE' | 'NOT_FOUND' | 'ALREADY_OUT' | 'WRONG_WAREHOUSE' | 'WRONG_MATERIAL';
  message?: string;
}

/** 扫码枪识别：字符间隔阈值 35ms（实测主流扫码枪 8–20ms/字符，人手最快 ~80ms） */
export function isScannerInput(timestamps: number[]): boolean {
  if (timestamps.length < 3) return false;
  const gaps = timestamps.slice(1).map((t, i) => t - timestamps[i]!);
  return gaps.every((g) => g > 0 && g < 35);
}

/** 加法去重：同一码第二次扫入 = DUPLICATE，不报错、不进列表（扫码枪常重复触发） */
export function mergeScan(list: ScanResult[], next: ScanResult): ScanResult[] {
  if (list.some((s) => s.serial_no === next.serial_no && s.status === 'OK')) {
    return [...list, { ...next, status: 'DUPLICATE', message: '该序列号已在本单' }];
  }
  return [...list, next];
}
```

**扫码与数量的联动（硬规则）**：

```text
一物一码物料（serial_required = true）：
  qty 输入框**只读**，值 = 已扫入的有效码个数
  删一个码 → qty 自动减 1（不允许手工改 qty）
  提交时若 qty ≠ 有效码数 → 前端先拦（"需 5 个码，实际 3 个"），后端 66305 兜底

非一物一码物料：
  qty 可编辑，扫码区禁用（传序列号会被后端 66007 拒）
```

**扫码失败的三种展示**（颜色 + 文案 + 是否阻断）：

| 状态 | 展示 | 阻断提交 |
|------|------|---------|
| `DUPLICATE` | 灰色行内提示「已在本单」，不弹窗 | 否 |
| `NOT_FOUND` / `ALREADY_OUT` / `WRONG_WAREHOUSE` / `WRONG_MATERIAL` | 红色行，留在列表里但**不计入 qty** | **是**（提交按钮 disabled，直到删除该行） |
| 后端过账返回 `66307`（并发被抢） | 顶部 red toast + 刷新该单可用码 | **是**（需重新选码） |

> **为什么"不合格的码"要留在列表里而不是静默丢弃**：现场常见"拿错货"，操作员需要看见"我扫了这个码，它不在本仓"才能去换货。静默丢弃会让人以为扫成功了。

**并发提示的文案（前端固定，别让后端拼）**：`66307` → 「序列号 SN-… 刚被其他出库单占用，请重新挑选」。后端 message 是给日志的，前端按错误码查文案表（与批次 1 `errorText` 的映射同一套）。

### 5.5 信用提示（三处，都非阻断）

| 位置 | 触发 | 展示 | 行为 |
|------|------|------|------|
| 订单表头 | 客户变更时 `GET /credit/{customer_id}` | 胶囊：`可用额度 20000.00 / 额度 100000.00`；若 `total_with_tax > available` → 橙色 + 「本单将超额 60000.00，提交会被拒」 | **不阻止编辑**，只提示（提交时后端 66101 才真拦） |
| 提交失败 | `66101` | 红色 alert，**直接用后端 message**（它已带"本次需要/可用/超额"三个数） | 停留当前页，不刷新表单 |
| 客户页 | `credit_status = BLOCKED` | 客户名旁红色 tag「已冻结」 | 新建订单时该客户不可选（前端过滤） |

**为什么前端不做"超额就不让填"**：额度是可协商的——销售需要把超额的单子填完、打印出来找财务审批。前端硬拦会让"先填单再申请额度"的流程走不通。这是本批与"技术洁癖"的一次刻意妥协，写在这里备查。

### 5.6 状态与权限双驱动的按钮矩阵

按钮显示条件 = **权限码命中** ∧ **状态合法** ∧ **无业务阻断**。三元组写成纯函数，单测钉住（照批次 1 `availablePurchaseActions` 的形状）：

```ts
// saleorder-logic.ts
export type SaleOrderAction = 'edit' | 'delete' | 'submit' | 'approve' | 'reject'
                            | 'withdraw' | 'close' | 'cancel' | 'create-outbound';

export function availableSaleOrderActions(
  status: SaleOrderStatus,
  perms: Set<string>,
  ctx: { shippedQty: number; hasOpenReceipt: boolean },
): SaleOrderAction[] { /* … */ }
```

| 状态 | 可编辑 | 可提交 | 审批 | 撤回 | 关闭 | 作废 | 建出库单 |
|------|-------|-------|------|------|------|------|---------|
| `DRAFT` | ✅ | ✅ | — | — | — | ✅ | — |
| `SUBMITTED` | — | — | ✅ | ✅ | — | ✅ | — |
| `APPROVED` | — | — | — | — | ✅ | ✅（未出库） | ✅ |
| `PARTIAL_SHIPPED` | — | — | — | — | ✅ | —（`66205`） | ✅ |
| `SHIPPED` | — | — | — | — | ✅ | — | — |
| `CLOSED` | — | — | — | — | — | — | — |
| `CANCELLED` | — | — | — | — | — | — | — |

出库单同理（3 态）：`DRAFT` → 可编辑/可删除/可过账/可作废；`POSTED` → 只可作废（且带 `66309` 风险提示）；`CANCELLED` → 全只读。

**过账确认弹窗**（不可省略，批次 1 收货页已有同款）：列三行——本单数量、**预计成本**（前端不算，调 `pick-locations` 拿 `avg_cost` 预估并标"预估"）、两张凭证将生成（收入 / 成本）。用户勾「我已核对实物与数量」后按钮才可点。这不是仪式感：过账是**不可逆**的（只能作废重开），误点一次就是两条红冲凭证。

### 5.7 前端单测（15 个既有文件 → 本批 +7）

仓库现状：`frontend/tests/unit/*.test.ts` 共 15 个文件，跑法 `pnpm test:unit`（`tsx --test`，node:test，零新依赖、无 DOM）。本批新增 7 个：

| 文件 | 测什么 | 用例数 |
|------|--------|--------|
| `erp-customer-credit.test.ts` | 额度胶囊文案 / 超额判定 / 冻结禁用 / 调额校验（`66106` 前端预判） | ~14 |
| `erp-sale-order.test.ts` | 金额四舍五入链 / 税率百分数换算 / 按钮矩阵 7×9 / 超发前端提示 | ~26 |
| `erp-stock-out-scan.test.ts` | 扫码枪间隔识别 / 去重 / 五种失败状态 / qty 与码数联动 | ~22 |
| `erp-stock-out-cost.test.ts` | 成本预估（`qty × avg_cost`，仅展示）/ 负库存黄色提示 / 尾差倒挤提示 | ~12 |
| `erp-receivable-aging.test.ts` | 账龄分桶（用 ar 生成日，不是 due_date）/ 逾期天数 / 汇总卡片 | ~16 |
| `erp-receipt-writeoff.test.ts` | 核销分配（Σ ≤ 收款额、不重复、跨客户禁）/ 预收余额展示 / 撤销后行标 `reversed` | ~24 |
| `erp-statement.test.ts` | 期初/期末恒等式 / 借贷列互斥展示 / 导出列顺序 | ~12 |

合计 **~126 个前端用例**（新增）。**e2e**（Playwright，`frontend/e2e/`）：新增 `11-erp-sales.spec.ts`（下单→出库→应收→收款→对账全链路）+ `12-erp-scan.spec.ts`(扫码面板 + 序列号冲突)，两个脚本，详见 §6.4。

---
## 6. 测试策略

三层：**纯函数单测**（不碰库，快）、**真库集成测试**（打真 PostgreSQL，验并发与约束）、**HTTP e2e**（curl 打隔离实例，验接口契约）。批次 1 的现状是三层都跑：`backend/src` 里 `#[cfg(test)]` 模块 119 个文件、`#[test]`/`#[tokio::test]` 共 **1345 处**（本 run `grep -rho` 实测）；`frontend/tests/unit` 15 个文件（`tsx --test`）；`scripts/verify/*.sh` 12 个 HTTP e2e 脚本。

### 6.1 后端单测估算（按 §3.0 的 18 个新文件）

| 文件 | 单测重点 | 用例数 |
|------|---------|--------|
| `customer_dto.rs` | 字段校验（名称/编码/账期/信用状态）、`credit_limit` 越界、税率不适用（客户无税率） | 18 |
| `customer_model.rs` | 取号 `CUS`、软删引用检查（未结应收 / 未完成订单 / 占用 > 0）、编码复用 | 14 |
| `credit_model.rs` | 占用/平移/释放三条 SQL 的 0 行语义（用 `sqlx::test` 打真库）、`used = order + ar` 自洽、幂等键去重 | 22 |
| `credit_dto.rs` | 调额校验（低于占用 → `66106`）、版本冲突、`credit_limit=0` 判定为不限 | 12 |
| `saleorder_model.rs` | 7 态状态机全部合法边 + 全部非法边（`LEGAL_EDGES` 矩阵）、提交占用、撤回释放、关闭释放剩余、作废拦截已出库 | 34 |
| `saleorder_dto.rs` | 明细校验、税率百分数 0–100、`qty>0`、空明细、金额三列恒等 | 20 |
| `stockout_model.rs` | **过账七步逐条**：状态闸、CAS、条件扣减 0 行、库位扣减、序列号 CAS、流水 before 反推、`total_cost` SUM、应收唯一键、双凭证、订单回写超发 | 46 |
| `stockout_dto.rs` | 序列号数量 = qty、非一物一码禁传码、库位属于该仓、行级仓覆盖 | 26 |
| `receivable_model.rs` | 一出一应收（`uk_biz_ar_source`）、`due_date = ar 生成日 + credit_days`、账龄分桶边界（30/31/60/61/90/91）、状态由余额推 | 24 |
| `receipt_model.rs` | 核销九步：Σ≤收款额、跨客户 `66501`、状态 `66502`、条件 UPDATE 0 行、快照 `before/after`、预收 `unapplied`、反核销回退、二次核销 | 38 |
| `receipt_dto.rs` | 明细重复、跨客户预校验、金额 > 0 | 14 |
| `statement_model.rs` | 期初计算、逐行累进余额、头表恒等式、区间重叠 `66602`、无流水 `66601` | 20 |
| `voucher.rs`（增量） | `create_auto_with` 借贷不平回滚、`sale_out_lines` 3 行 / 税 0 时 2 行、`sale_out_cost_lines` 2 行、`receipt_lines` 用 `write_off_total`、`reverse_auto` 幂等、**`PURCHASE_IN` 行为逐字不变** | 16 |
| `stockout_handler.rs` / `receipt_handler.rs` | 权限码注解存在性、路径参数解析、错误码映射（不进 DB 的 400 分支） | 12 |
| 其他 handler / dto | 分页参数、日期区间、筛选条件拼装 | 20 |

**合计 ≈ 336 个后端用例**（新增）。加既有 1345 → 目标 **≈1680**。这个数不是拍脑袋：批次 1 用 20 张表 / 108 条迁移产出 1278 个可跑用例（交付口径），密度约 64 用例/表；本批 13 张表 + 5 个引擎（成本 / 信用 / 核销 / 序列号 / 凭证），引擎部分（`stockout_model` 46 + `receipt_model` 38 + `credit_model` 22 = 106）占 32%，与批次 1 "引擎最重"的分布一致。

### 6.2 真库集成测试（关键场景，必须打真 PG）

单测里的 SQL 用 `sqlx::test`（自动建库 + 回滚），**并发类场景不能用事务回滚**（要两个连接同时持锁），必须起两个 `PgPool` 连接。以下 8 个场景是本批的"质量闸"，缺一不算交付：

| # | 场景 | 断言 | POC 是否已验证 |
|---|------|------|---------------|
| **G1** | 并发出库超卖：库存 8，两会话各出 5 | 一成一败，终态 `qty=3`，失败方错误码 `66301`；两笔 `biz_inventory_txn` 只落 1 条 | ✅ POC-9 真跑（§9.1） |
| **G2** | 信用并发占用：额度 10 万，两会话各占 8 万 | 一成一败，`used_amount=80000`，失败方 `66101`，`biz_customer_credit_txn` 只 1 条 `ORDER_HOLD` | ✅ POC-10 真跑（§9.1） |
| **G3** | 核销原子性：一张收款核销 3 张应收，第 3 张超额 | 整笔回滚：3 张应收全部未动、`biz_receipt_item` 0 行、收款单仍 `DRAFT` | ✅ POC-6（单张）；3 张场景本批补测 |
| **G4** | 序列号 CAS：两会话出同一码 | 一方 `RETURNING` 行数 0 → `66307`，码终态 `OUTBOUND`（只有一个出库单挂了它） | ✅ POC-7 |
| **G5** | 尾差倒挤：3 件 / 均价 10.123456 / 逐件出清 | 最后一件 `cost_amount = 结存金额`，清空后 `qty=0 AND amount=0` | ✅ POC-1d/1e |
| **G6** | 负库存：无库存出库 5 → 后续入库 30 冲平 | 出库 `avg_cost` = 最近入库单价（**非 0**）；冲平后 `qty=25 / amount=310 / avg=12.4` | ✅ POC-2 |
| **G7** | 双凭证原子性：科目 6001 被停用后再过账 | 两张凭证都不落、库存不动、应收不建、单据仍 `DRAFT`；错误码 `66002` | 本批补测 |
| **G8** | 死锁回归：两张单，A 锁物料 1→2，B 锁 2→1 | 按 §3.10 的 `material_id ASC` 排序后**无死锁**（连续跑 200 次 0 次 `40P01`） | 本批补测 |

**G8 的写法**（这是最容易写成"假测试"的一个）：

```rust
// 必须在两个独立连接上，各自 BEGIN，按相反明细顺序插单，再同时 post_stock_out。
// 断言：两个 future 都返回 Ok（或其中一个 66301），且日志里没有 40P01。
// 反例：如果只在一个连接里顺序调用两次 post，永远不会死锁——测不出问题。
let (pool_a, pool_b) = (pool.clone(), pool.clone());
let a = tokio::spawn(async move { post_in(&pool_a, out_a).await });
let b = tokio::spawn(async move { post_in(&pool_b, out_b).await });
let (ra, rb) = tokio::join!(a, b);
assert!(ra.is_ok() && rb.is_ok(), "不应出现死锁：{ra:?} {rb:?}");
```

### 6.3 HTTP e2e 脚本清单（`scripts/verify/`）

照批次 1 `p2b1_stockin.sh` 的形状（`set -uo pipefail` + 固定夹具 UUID + `PASS/FAIL` 计数 + 自建夹具 + 结尾清理 SQL），新增 4 个：

| 脚本 | 覆盖 | 断言数 |
|------|------|--------|
| `p3d2_sales_order.sh` | 建客户 + 建信用 → 建订单（金额服务端算）→ 提交（占用额度）→ 超额提交 409 `66101` → 编辑（DRAFT ✅ / SUBMITTED 400）→ 审批 → 关闭（释放剩余）→ 作废 → 分页 / 可出库明细 | ~48 |
| `p3d2_stock_out.sh` | 建出库单（带入订单）→ 拣货建议 → 过账（**库存减 / 两张凭证 / 应收建 / 订单回写**）→ 重复过账 409 → 库位不足 400 → 序列号数量不符 400 → 一物一码出库 → 作废（红冲两张 + 应收作废 + 信用释放） | ~62 |
| `p3d2_receipt.sh` | 登记收款 → 过账核销 2 张应收 → 应收状态流转 → 预收（部分核销）→ 二次核销 → 重复过账 409 → 跨客户 400 → 撤销（反核销 + 红冲）→ 额度释放核对 | ~54 |
| `p3d2_statement.sh` | 生成对账单（期初 / 逐行余额 / 期末恒等）→ 重复生成 409 → 确认 → 确认后改 400 → 导出 CSV 逐行比对 → 无流水 400 | ~30 |

**每个脚本的硬规矩**（照批次 1，逐条抄）：

1. 固定夹具 UUID 前缀（本批用 `e3d20001-0000-4000-8000-0000000000XX`，与批次 1 的 `e2b10001-…` 不冲突）；
2. 开头按固定 UUID 清上一轮残留（`DELETE … WHERE id::text LIKE 'e3d20001-%'`），结尾打印残留核对；
3. **金额断言不信接口返回值**，另起一条 `psql` 直查金额三列与恒等式；
4. 过账类接口必须断言"库存 / 凭证 / 应收 / 信用"四件事，少一件就是漏测；
5. 退出码 = 是否有断言失败（`exit $((FAIL > 0))`）。

### 6.4 前端测试

| 层 | 文件 | 用例数 | 命令 |
|----|------|--------|------|
| 纯逻辑单测 | `frontend/tests/unit/erp-*.test.ts` 7 个（详见 §5.7） | ~126 | `pnpm test:unit` |
| e2e（Playwright） | `frontend/e2e/11-erp-sales.spec.ts` | 下单→审批→出库→应收→收款→对账 主链路（**一条链跑完**，不拆碎） | `pnpm test:e2e` |
| e2e（Playwright） | `frontend/e2e/12-erp-scan.spec.ts` | 扫码面板：扫码枪模拟（`page.keyboard.type` 逐字符 <35ms）、重复码、错仓码、qty 联动、过账后码变 `OUTBOUND` | — |

`11-erp-sales.spec.ts` 的链路设计（与 `10-erp-warehouse.spec.ts` 同风格，一条主线 + 三个分支）：

```text
主线：登录 → 客户(建) → 信用(调额 10 万) → 订单(2 行, 13%) → 提交(看额度胶囊变 20%)
     → 审批 → 建出库单(带入可出库) → 过账(确认弹窗) → 断言 应收单已生成 + 订单变 SHIPPED
     → 收款(登记 5 万, 核销 3 万) → 过账 → 断言 应收 PARTIAL_RECEIVED
     → 对账单(生成 + 确认) → 断言 期末余额 == 应收未收合计
分支 A：超额提交 → 66101 文案含"本次需要/可用/超额"
分支 B：作废已出库订单 → 66205
分支 C：一物一码物料出库不扫码 → 提交按钮 disabled
```

### 6.5 验收门禁（不过不进 review）

```text
① cargo test                     全绿，且用例数 ≥ 1650（含新增 336）
② cargo clippy --all-targets     -D warnings，0 warning
③ cargo fmt --check              通过
④ 巡检 I1–I8（§3.11）            每条返回 0 行（在 e2e 跑完的库上执行）
⑤ scripts/verify/p3d2_*.sh       4 个脚本退出码 0（≈194 个断言）
⑥ pnpm test:unit                 全绿（既有 15 + 新增 7 个文件）
⑦ pnpm test:e2e                  11 / 12 两个 spec 全绿
⑧ vue-tsc --noEmit               0 error
⑨ 迁移可重放                     空库跑 V0160–V0168 全绿；再跑一次全部 NOTICE 跳过
```

**为什么把"巡检 SQL 0 行"放进门禁**：单元测试能证明"我写的路径对"，但不能证明"库里没脏"。批次 1 的 `inventory_audit.rs` 就是这个思路——8 条不变式各写一条 SQL，任何一条有行就说明某次写入绕过了引擎。本批把 I1–I8 做成**门禁项**而不是"事后监控"，因为销售闭环的四张账（库存 / 应收 / 信用 / 凭证）互相对账，错一处就是财务报表错。

### 6.6 测试数据与时间确定性

* 所有涉及"今天"的口径（账龄、逾期、`due_date`）必须接受注入的 `now: DateTime<Utc>` 参数，测试里传固定值（`2026-10-31T00:00:00Z`），**禁止**在 SQL 里写 `CURRENT_DATE` 参与业务计算（DDL 的 `DEFAULT CURRENT_TIMESTAMP` 只用于审计列）；
* `biz_accounting_period` 的期间判定（`period_of`）在测试里用固定日期，避免"月底跑测试挂在期间关闭上"；
* 并发测试（G1/G2/G4/G8）**必须**用真连接池 + `tokio::spawn`，并给每个 spawn 设 `tokio::time::timeout(5s)`——死锁测试如果没有超时，CI 会挂 30 分钟才失败。

---
## 7. 实施计划

按"能独立验收的最小切片"切 6 步，每步都有可验证的产物。**顺序不可换**：迁移先落 → 凭证引擎先扩 → 再写业务（否则业务写完发现引擎缺口要回头改，批次 1 的 `create_auto` 就是这个教训）。

| 步 | 内容 | 产物 | 依赖 | 人日 | 验收 |
|----|------|------|------|------|------|
| **E1** | 9 条迁移 V0160–V0168（13 表 + 权限 + 科目 + 凭证规则 + 序列） | `backend/migrations/business/V016*.sql` 各 up/down | 无 | 1.5 | 空库跑全绿；重放 NOTICE 跳过；`\d` 逐表核对 |
| **E2** | 凭证引擎增量（`create_auto_with` + 3 个模板 + `reverse_auto` + `resolve_sales_accounts`）**不改既有签名** | `voucher.rs` +260 行 | E1（规则种子） | 2.0 | 16 个新用例 + `purchase_in_lines_unchanged` 快照测试 |
| **E3** | 客户 + 信用（model / dto / handler，9 接口） | 3 文件组 + 4.1 全部接口 | E1、E2（无凭证） | 2.5 | e2e `p3d2_sales_order.sh` 的前半段（建客户/调额/超额 66101） |
| **E4** | 销售订单（7 态机 + 占用 / 释放 / 平移） | 3 文件组 + 4.2 全部接口 | E3 | 3.0 | 状态机 34 用例 + 订单 e2e 全绿 |
| **E5** | 销售出库（过账七步 + 成本 + 库位 + 序列号 + 双凭证 + 应收生成） | 3 文件组 + 4.3 全部接口 | E4、E2 | 5.0 | G1/G4/G5/G6/G7/G8 + `p3d2_stock_out.sh` |
| **E6** | 应收 + 收款核销 + 对账单（14 接口） | 5 文件组 + 4.4–4.6 接口 | E5 | 3.5 | G3 + `p3d2_receipt.sh` / `p3d2_statement.sh` |
| **E7** | 前端 7 页 + API 契约 + 扫码面板 | §5 全部文件 | E3–E6 接口可用 | 6.0 | 126 前端用例 + 2 个 e2e spec |
| **E8** | 巡检 SQL + 门禁收口 | `scripts/verify/erp_sales_audit.sql` | E5/E6 | 1.0 | §6.5 九项门禁全过 |

**合计 ≈ 24.5 人日**（后端 18.5 + 前端 6.0）。三人并行（1 后端主力 + 1 后端副手 + 1 前端）可压到 **11–13 个工作日**：E1/E2 串行（3.5 天），之后 E3/E4 与 E5 可部分并行（E5 依赖 E4 的 `shippable-items` 与订单回写，但成本/序列号/凭证部分可先写），E6 与 E7 并行。

### 7.1 里程碑检查点（每个都是"能演示"的）

| 里程碑 | 条件 | 演示内容 |
|--------|------|---------|
| **M1**（E1+E2） | 迁移全绿 + 引擎快照测试过 | `psql \d` 看 13 张表；跑一次 `create_auto(PURCHASE_IN)` 生成的分录与批次 1 逐字相同 |
| **M2**（E3+E4） | 订单能提交并占额度 | 建客户 → 额度 10 万 → 下单 8 万 → 提交 → 信用卡片 `used=80000` → 再下 8 万提交被 66101 拒 |
| **M3**（E5） | 出库过账闭环 | 过账一张出库单 → 库存减、两张凭证、一张应收、订单变 `SHIPPED`、信用 `order_used→ar_used` |
| **M4**（E6） | 收款核销 + 对账 | 收 3 万核销 → 应收 `PARTIAL_RECEIVED`、信用释放 3 万 → 生成对账单余额对得上 |
| **M5**（E7+E8） | 门禁九项全过 | 全链路 e2e 录屏 + 巡检 SQL 8 条全 0 行 |

### 7.2 与批次 1 补丁的并行关系

V0157–V0159 只剩 3 个空号给批次 1 补丁（V0151/V0154/V0155/**V0156 已被仓库文件占用**，其中 V0156 就是"去掉 ERP 顶级目录"）。本批**不碰**这 3 个号，也不修改批次 1 的任何既有表结构。若批次 1 需要补列（例如 `biz_voucher_entry` 加 `aux_customer_id`，见 §8.3），走独立补丁迁移，与被本批的 DDL **无依赖**——本批已用 `aux_extra` JSONB 绕过这个缺口（§3.7.4 红线 4），所以补丁来了也不用改本批代码。

---

## 8. 风险清单

按"会不会算错钱"排序，不按"看起来严不严重"排序。

### 8.1 会计正确性（高）

| # | 风险 | 影响 | 缓解 | 残留 |
|---|------|------|------|------|
| R1 | **成本尾差**：`qty × avg_cost` 四舍五入后与结存金额不相等，逐件出清时最后一件剩 0.0001 | 库存金额与数量账对不上，`qty=0` 时 `amount≠0` | 出清时**倒挤**（`out_cost = 结存金额`，POC-1e 实测）；巡检 I1 盯 `qty=0 → amount=0` | 已消除 |
| R2 | **负库存成本取 0** | 出库成本 0 → 毛利虚高 → 所得税风险 | 取该物料该仓最近一条 `IN` 的 `unit_cost`（POC-2 实测 `avg_cost=10`）；无历史 IN 则**拒绝出库**（宁可拦住也不给 0） | 已消除 |
| R3 | **收入 / 成本凭证混淆成一张** | 利润表行次错、红冲粒度错 | 两张凭证（`SOURCE_STOCK_OUT` / `STOCK_OUT_COST`），同事务生成；`ck_biz_out_vouchers` 强制 POSTED 必须有两条（POC-8a 实测 CHECK 生效） | 已消除 |
| R4 | **应收金额口径**：取 `total_with_tax` 还是 `total_amount` | 少收 13% 税 | 取 `total_with_tax`（价税合计，客户欠的是含税钱），DDL 注释与 §3.7.4 表格双重钉住 | 已消除 |
| R5 | **期间边界**：跨月出库的应收记到哪个月 | 月度报表归属错 | 应收 `period_code` 取**出库单 `out_date` 所在期间**（不是过账时间），`period_of(ex, out_date)` | 需在 e2e 加一条跨月用例 |
| R6 | **作废后红冲顺序**：先红冲凭证还是先加回库存 | 中途失败会产生"有库存没凭证" | 全在一个事务里，顺序为"红冲→加回库存→序列号→应收→信用"（§3.9.2）；任何一步失败整笔回滚 | 已消除（靠事务） |
| R7 | **预收二次核销重复入账** | 银行存款虚增 | 二次核销只按新增核销额出凭证（`RECEIPT_APPLY`），不复用整额；`source_id` 相同但 `source_type` 不同，红冲分别可查 | 依赖 E6 实现正确 |

### 8.2 并发（高）

| # | 风险 | 场景 | 缓解 |
|---|------|------|------|
| R8 | 超卖 | 两张出库单同时扣同一库存行 | 条件 UPDATE `AND qty >= ?` + `RETURNING`（G1/POC-9 实测：一成一败，终态 3） |
| R9 | 信用超额 | 同时提交两张 8 万单（额度 10 万） | 单语句条件 UPDATE（G2/POC-10 实测：一成一败） |
| R10 | 序列号被两单抢 | 两个出库单选同一个码 | 状态 CAS `WHERE status='IN_STOCK'`，`RETURNING` 行数 ≠ 件数 → `66307`（G4/POC-7） |
| R11 | 核销并发 | 两张收款单核销同一应收 | 条件 UPDATE `AND unreceived_amount >= ?` + `receivable_id` 升序（§3.6，G3） |
| R12 | 过账并发 | 同一出库单点两次过账 | ① 锁单头 ② CAS `DRAFT→POSTED`，0 行 → `66302`；`uk_biz_ar_source` 二次兜底 |
| R13 | **死锁** | A 单锁物料 1→2，B 单锁 2→1 | 全局锁顺序（§3.10）：`material_id ASC` + `serial_no ASC` + `receivable_id ASC`；G8 跑 200 次回归 |
| R14 | 长事务持锁 | 出库过账 7 步锁住多条库存行 | 顺序设计（凭证最后、订单回写最后）把持锁范围压到最小；单帐 33 行实测 < 200ms |

### 8.3 与批次 1 的分歧点（中，必须显式记录）

| # | 分歧 | 批次 1 现状 | 本批做法 | 为什么 |
|---|------|------------|---------|--------|
| D1 | `biz_voucher_entry` 无 `aux_customer_id` 列 | 只有 `aux_supplier_id/aux_material_id/aux_warehouse_id/aux_dept_id`（实测） | 客户 id 写进 `aux_extra` JSONB：`{"customer_id":"…"}` | 不加列（会牵动批次 1 的 1278 个测试与查询）；代价是"按客户查应收明细账"要解 JSONB，性能可接受（应收明细按 `source_id` 也能查） |
| D2 | 税率口径分裂 | 物料主数据 `0.1300`（0–1）／收货明细 `13.0000`（0–100）／采购订单行 API `0–1` | 销售订单 + 出库明细统一用**百分数**（与收货明细一致） | 见 §4.2 口径校正表 |
| D3 | `biz_voucher_rule` 无 `biz_type` 唯一索引 | 引擎按 `biz_type` 取 `ORDER BY created_at ASC LIMIT 1` | 不自作聪明加唯一索引；在 V0167 自检里断言"每个 biz_type 只有一条启用规则" | 加唯一索引会与既有数据/行为冲突 |
| D4 | 明细单价列名 | 任务书写 `price_excl`，实际是 `price` | 本批一律 `price` / `price_incl` | 实测 `\d biz_stock_in_item` |
| D5 | 应收"已收"列名 | `biz_payable` 用 `paid_amount` / `unpaid_amount` | 应收用 `received_amount` / `unreceived_amount` | `paid` 在应收语境会被读成"已付"，中文界面上极易读反（§0.1 已声明这一处刻意偏离） |
| D6 | 订单状态码数量 | 采购订单 4 态、调拨 6 态 | 销售订单 7 态（多 `PARTIAL_SHIPPED`） | 部分出库是销售常态，用 `SHIPPED` 一个态无法表达"还能再出" |

### 8.4 实现易错清单（给开发的自查表）

1. **`RETURNING` 不是"更新前"**：`before_amount` 必须用 `after + 本次增量` 反推（§1.8 ③ 的真实踩坑，POC-1b 输出 `344.1956` 是错的）；
2. **CHECK 里 `NULL` 会静默通过**：`btrim(NULL) <> ''` 是 `NULL` 不是 `false`，三值逻辑让约束失效（§1.8 ②）；所有"非空且非空白"必须显式写 `IS NOT NULL AND btrim(…) <> ''`；
3. **税率 ×100**：物料带默认税率时 `0.13 → 13.0000`，忘了就少算 100 倍税；
4. **`location_id` 有两张表**：`biz_inventory_location.id`（库位库存行）vs `biz_location.id`（库位定义）。`biz_stock_out_item.location_id` 指的是**前者**（§1.8 ①，本批 POC 发现的第一个缺陷）；
5. **库位模式下要动两张账**：`biz_inventory.qty`（成本桶）+ `biz_inventory_location.qty`（数量账），只动一张会让巡检 I2（Σ库位 = 仓级）当场判假；
6. **`total_cost` 用 SQL `SUM`**：Rust 侧 `fold` 累加会因逐行舍入与 SQL 的 `ROUND` 时机不同产生分差（§3.1 ④）；
7. **`unapplied_amount` 不进凭证**：收款凭证金额是 `write_off_total`，不是 `amount`（§3.7.4）；
8. **一物一码的 `qty` 是只读派生值**：前端可编辑、后端不校验个数以外的语义（数量与码数一致性两者都判）。

### 8.5 建议的批次 1 补丁（不在本批范围）

| # | 补丁 | 建议号段 | 理由 |
|---|------|---------|------|
| P1 | `biz_voucher_entry` 加 `aux_customer_id UUID` + 索引 | **V0157** | 应收明细账按客户查是财务刚需，JSONB 解包在高基数下会慢 |
| P2 | 统一税率口径：采购订单行改百分数（或全改小数），加数据迁移 | **V0158** | 现在同一概念两种单位，靠人记 |
| P3 | `2221.01` 补 `parent_id`（实测为 NULL 但父科目 `is_leaf=0`） | **V0159** | 科目树出现"叶子没父级"，科目余额表按层级汇总会漏 |
| P4 | `biz_voucher_rule` 加 `biz_type` 部分唯一索引（`WHERE is_active=1`） | **并入批次 3**（V0157–V0159 已排满；若财务强要求，挤掉 P3 换到 V0159） | 防止运营误建第二条规则导致"生效的不是最新的那条" |

> **号段说明（本轮复核后收紧）**：V0156 已被"去掉 ERP 顶级目录"用掉，V0151/V0154/V0155 也已存在，因此**只有 V0157–V0159 三个空号**，四条补丁必须让出一条。

### 8.6 工期风险（中）

| 风险 | 触发条件 | 应对 |
|------|---------|------|
| E5（出库过账）超期 | 库位 + 序列号 + 双凭证三条线交织，是本批最重的 1400 行 | 先做"整仓扣减 + 无序列号"的最简路径跑通 e2e，再叠库位与序列号；不要三条线同时开工 |
| 前端扫码联调慢 | 无真实扫码枪，只能用 `keyboard.type` 模拟 | 扫码枪识别写成纯函数（`isScannerInput`）+ 单测，e2e 只验 UI 反应；现场联调留 0.5 天缓冲 |
| 审批流集成 | `WATCH` 客户需走审批，但审批流在批次 D | 本批只写 `approval_instance_id` 列 + `need_approval` 标记，**不接流程引擎**；批次 D 已有 `flow_instance`，接线是一次 handler 调用 |
| 期间关闭 | 测试撞上 `biz_accounting_period` 已关闭 | 测试固定 `now`；e2e 用新建期间 |

---
## 9. 附录

### 9.1 真库验证记录（不是纸面推演）

本节所有输出都是**真实执行**的原样粘贴，不是示意。环境：本机 PostgreSQL 18.6，库 `erp_p3d2_v2/v3`（`createdb -T cloudpivot_p2e1` 克隆批次 1 生产 schema，**不动源库**）、`erp_p3d2_poc3`（业务场景 + 并发）。

#### 9.1.1 DDL 从本文档 §2 逐字提取后执行

```bash
# ① 用脚本把 §2 的 sql 代码块原文抽出来（不信手敲）
$ python3 /tmp/extract_ddl.py
blocks: 13            # 13 个 DDL 代码块，一块一张表
CREATE TABLE: 13
CHECK: 59

# ② 克隆批次 1 生产 schema 到空库
$ su postgres -c "createdb -T cloudpivot_p2e1 erp_p3d2_v3"

# ③ 执行（ON_ERROR_STOP=1：有任何一个错就非 0 退出）
$ su postgres -c "psql -q -d erp_p3d2_v3 -v ON_ERROR_STOP=1 -f /tmp/ddldoc.sql"
PSQL_EXIT=0
```

执行后核对（`erp_p3d2_v3`）：

```sql
select count(*) from information_schema.tables where table_schema='public' and table_name in
 ('biz_customer','biz_customer_credit','biz_customer_credit_txn','biz_sale_order',
  'biz_sale_order_item','biz_stock_out','biz_stock_out_item','biz_stock_out_item_serial',
  'biz_receivable','biz_receipt','biz_receipt_item','biz_customer_statement',
  'biz_customer_statement_line');
-- 13

select count(*) as idx_cnt from pg_indexes where schemaname='public' and tablename in (…13 张表…);
-- 61      （13 个主键 + 48 个业务索引）

select count(*) as chk_cnt from pg_constraint c join pg_class t on t.oid=c.conrelid
 where c.contype='c' and t.relname in (…12 张有 CHECK 的表…);
-- 59      （与 DDL 里 CONSTRAINT ck_ 的个数逐一对上）
```

**结论**：13 张表一次建成、**零错误**；61 个索引、59 个 CHECK 与文档正文声明一致。（`erp_p3d2_v2` 是同一批 DDL 的早期副本，也 13/13 成功；两个库并存用于交叉核对，差异只有 1 个索引 `idx_biz_out_item_location`——v3 是最终版。）

#### 9.1.2 业务场景 POC（`erp_p3d2_poc3`，`poc_v4.log` 原文）

**POC-1 出库成本结转 + 尾差倒挤**

```text
########## POC-1：出库成本结转 ##########
--- 1a 结存 100 / amount 1012.34 / avg_cost 10.1234 ---
--- 1b 条件扣减出库 33（AND qty >= 33 + RETURNING） ---
 after_qty | after_amount | after_cost | out_cost | before_amount | before_qty
-----------+--------------+------------+----------+---------------+------------
   67.0000 |     678.2678 |  10.123400 | 334.0722 |      344.1956 |    67.0000
(1 row)

--- 1c 一致性：round(qty*avg_cost,4) = amount ? ---
   qty   | avg_cost  |  amount  | recomputed | consistent
---------+-----------+----------+------------+------------
 67.0000 | 10.123400 | 678.2678 |   678.2678 | t

--- 1d 尾差：3 件 / avg 10.123456 / amount 30.3704 逐件出库 2 次 ---
  qty   | avg_cost  |  amount  | recomputed | consistent_before_clear
--------+-----------+----------+------------+-------------------------
 1.0000 | 10.123456 |  10.1234 |    10.1235 | f          ← 尾差出现（预期）

--- 1e 最后一件按「倒挤」清空（out_cost = 结存金额） ---
 clear_out_cost | avg_cost
----------------+-----------
        10.1234 | 10.123456

  qty   | amount
--------+--------
 0.0000 | 0.0000        ← 清空后数量与金额同时归零（I1 成立）
```

> 注意 1b 的 `before_amount = 344.1956` **是错的**（`RETURNING` 看到的是新行），正确值是 `1012.34`。这个反向结论直接改写了实现：`before_*` 必须用 `after + 增量` 反推（§1.8 ③、§8.4 ①）。1d/1e 是尾差的真实来源与唯一正确解法（倒挤）。

**POC-2 负库存（成本不取 0）**

```text
########## POC-2：负库存 ##########
--- 2a 该物料该仓的最后一次入库单价（负库存成本依据） ---
 txn_type |   doc_no    |   qty   | unit_cost
----------+-------------+---------+-----------
 IN       | IN-POC-0001 | 10.0000 | 10.000000

--- 2b 无库存出库 5 件 → qty=-5 / amount=-50 / avg_cost=10（取最近入库价，不用 0） ---
   qty   | avg_cost  |  amount
---------+-----------+----------
 -5.0000 | 10.000000 | -50.0000

--- 2c 后续入库 30 @ 12 冲平：金额 = -50+360 = 310，qty = 25，均价 = 12.4 ---
   qty   |  amount  | avg_cost  | recomputed
---------+----------+-----------+------------
 25.0000 | 310.0000 | 12.400000 |   310.0000
```

**POC-3 库位拆分与两级账（D7）**

```text
--- 3c 拣货顺序：库位行按 (qty DESC, location_code ASC) ---
           inv_location_id            | location_code |  qty
--------------------------------------+---------------+--------
 ab000000-0000-4000-8000-000000000003 | A-03          | 5.0000
 ab000000-0000-4000-8000-000000000002 | A-02          | 3.0000
 ab000000-0000-4000-8000-000000000001 | A-01          | 2.0000

--- 3d 出库 7 件：A-03(5) + A-02(2) ---
  qty   | amount      （A-03 清零）
--------+--------
 0.0000 | 0.0000
  qty   | amount      （A-02 剩 1）
--------+--------
 1.0000 | 8.0000
 bucket_qty | bucket_amount   （整仓汇总桶剩 3）
------------+---------------
     3.0000 |       24.0000

--- 3e 不变式 I2：Σ库位 qty = 汇总桶 qty ---
 sum_loc | bucket | i2_holds
---------+--------+----------
  3.0000 | 3.0000 | t
```

**POC-4 / POC-5 / POC-6 / POC-7 / POC-8**

```text
########## POC-4：条件扣减挡超卖 ##########
--- 4a 第一笔扣 5 → 成功 ---   qty = 3.0000
--- 4b 第二笔扣 5 → 0 行（库存不足被挡） ---   (0 rows)

########## POC-5：信用额度原子占用 ##########
--- 5a 第一笔 8 万 → 成功，可用 2 万 ---
 order_used | ar_used | used_amount | available
------------+---------+-------------+-----------
 80000.0000 |  0.0000 |  80000.0000 | 20000.0000
--- 5b 第二笔 8 万 → 0 行（超限） ---   (0 rows)
--- 5c 出库平移：order_used → ar_used，used_amount 不变 ---
 order_used |  ar_used   | used_amount
------------+------------+-------------
     0.0000 | 80000.0000 |  80000.0000
--- 5d CHECK 兜底：直写 used_amount=1（不自洽）→ 被拒 ---
ERROR:  new row for relation "biz_customer_credit" violates check constraint "ck_biz_credit_used"

########## POC-6：核销 ##########
--- 6a 核销 600 → PARTIAL_RECEIVED ---
 received_amount | unreceived_amount |      status
-----------------+-------------------+------------------
        600.0000 |          400.0000 | PARTIAL_RECEIVED
--- 6b 再核销 500（剩余 400）→ 0 行 ---   (0 rows)
--- 6c 核销明细快照自证 ---
 line_no |  amount  | before_unreceived | after_unreceived
---------+----------+-------------------+------------------
       1 | 600.0000 |         1000.0000 |         400.0000
--- 6d 伪造明细（after 999 ≠ 1000-600）→ CHECK 拒绝 ---
ERROR:  new row for relation "biz_receipt_item" violates check constraint "ck_biz_receipt_item_after"

########## POC-7：序列号 CAS ##########
--- 7a 第一笔出库 → 1 行 ---   SN-POC-000001 | OUTBOUND
--- 7b 同一码第二笔 → 0 行 ---   (0 rows)

########## POC-8：出库单状态机 ##########
--- 8a POSTED 但无凭证 → CHECK 拒绝 ---
ERROR:  new row for relation "biz_stock_out" violates check constraint "ck_biz_out_vouchers"
--- 8b 两张凭证齐全 → 通过 ---
    out_no    | status | has_revenue | has_cost
--------------+--------+-------------+----------
 OUT-POC-0001 | POSTED | t           | t
--- 8c 销售订单状态机：CANCELLED 无原因 → 拒绝 ---
ERROR:  new row for relation "biz_sale_order" violates check constraint "ck_biz_so_state"
--- 8d 收款单 核销合计 > 收款金额 → 拒绝 ---
ERROR:  new row for relation "biz_receipt" violates check constraint "ck_biz_receipt_writeoff"
```

#### 9.1.3 真并发（两个独立 psql 会话打同一行，`conc_rerun.log`）

```text
===== POC-9 并发出库：库存 8，两个会话各扣 5 =====
--- 会话 A ---  BEGIN → 扣 5（持行锁 3 秒）
 after_qty | after_amount
-----------+--------------
    3.0000 |      15.0000
UPDATE 1
 COMMIT
--- 会话 B ---  等 1 秒后开始（此时 A 已持锁）
BEGIN → 扣 5（被 A 的行锁阻塞，A 提交后按新行版本重判谓词）
 after_qty | after_amount
-----------+--------------
(0 rows)
UPDATE 0                       ← 库存不足被条件扣减挡下，没有覆盖 A 的结果
COMMIT

 final_qty | final_amount
-----------+--------------
    3.0000 |      15.0000     ← 终态正确：8 − 5 = 3，只卖出去一次

===== POC-10 并发信用占用：额度 10 万，两个会话各占 8 万 =====
--- 会话 C ---  BEGIN → 占 8 万（持行锁 3 秒）
 order_used | used_amount | available
------------+-------------+-----------
 80000.0000 |  80000.0000 | 20000.0000
UPDATE 1 → COMMIT
--- 会话 D ---  抢占 8 万（被 C 阻塞，C 提交后按新行版本重判谓词）
 order_used | used_amount | available
------------+-------------+-----------
(0 rows)
UPDATE 0                       ← 超限，第二个单被拒
COMMIT

 order_used | ar_used | used_amount | version
------------+---------+-------------+--------
 80000.0000 |  0.0000 |  80000.0000 |      1
```

**这两段是 D1（条件扣减）与 D4（单语句占用）唯一的实证依据**：单语句条件 UPDATE 在 PostgreSQL 默认 `READ COMMITTED` 下，会等锁释放后**重新求值 `WHERE`**（EvalPlanQual），所以"8−5−5"不会变成 `-2`，两个 8 万也不会让 `used_amount` 变成 16 万。这比任何"我觉得它能防住"的论证都硬。

> **复现方式**：`_probe/conc_setup.sql`（夹具）+ `_probe/run_conc.sh`（两会话后台起 `psql`，`sleep 0.4` 错开 + `pg_sleep(3)` 持锁）。脚本末尾会打印终态两行，与上面一致即通过。

### 9.2 ADR 索引（决策 → 位置 → 一句话）

| ADR | 决策 | 详见 | 一句话 |
|-----|------|------|--------|
| D1 | 出库用条件 UPDATE（乐观），入库保留 `FOR UPDATE`（悲观） | §3.2.1 / §9.1.2 POC-4 | 分界线是"是否要重算均价"：出库只读均价不再算，一步就能扣；入库要读-算-写三步 |
| D2 | 负库存用既有 `biz_warehouse.allow_negative`，成本取最近入库价 | §3.2.2 / POC-2 | 不新建配置表、不取 0、不追补历史凭证 |
| D3 | 收入 + 成本两张凭证，同事务，`STOCK_OUT` / `STOCK_OUT_COST` | §3.7 / DDL `ck_biz_out_vouchers` | 复用引擎不改签名；一张 5 行要改引擎 = 违反红线 |
| D4 | 下单占用 / 出库平移 / 核销释放，单语句条件 UPDATE | §3.5 / POC-5 / POC-10 | 并发判断收敛到"提交订单"唯一一处 |
| D5 | 核销单事务 + 按 `receivable_id` 升序条件 UPDATE + 快照 CHECK | §3.6 / POC-6 | 明细行自证，DB 层挡"金额对不上" |
| D6 | 序列号状态 CAS，句数必须等于件数 | §3.4 / POC-7 | 状态机不可逆，CAS 是唯一能挡抢码的写法 |
| D7 | `biz_inventory` 唯一成本账；库位模式必须同时动 `biz_inventory_location` | §3.3 / POC-3 | 巡检 I2 保证两级账不漂 |
| D8 | 迁移占 V0160–V0168 | §1.1 / §2.7 | 仓库文件已到 V0156，补丁空号只剩 V0157–V0159 |
| D9 | 凭证引擎增量（`create_auto_with` + 3 个模板函数 + `resolve_sales_accounts`），不改既有签名 | §3.7.3 | 1278 个既有测试钉住 `create_auto` |
| D10 | 命名/金额/税率口径严格对齐批次 1（税率明细层百分数） | §2.0 / §4.2 | 已因猜字段名错过 6 次 |

### 9.3 待确认问题（需要业务/财务拍板，不阻塞开发）

| # | 问题 | 本批默认取值 | 影响面 |
|---|------|-------------|--------|
| Q1 | 一物一码物料**允许负库存**吗？ | **不允许**（`serial_required=true` 的物料在 `allow_negative=1` 仓也不放行，因为"没有码的货"不存在） | 逻辑分支 1 处 |
| Q2 | 出库单允许**跨仓**（明细行不同仓）吗？ | **不允许**，单头一仓（与 `biz_stock_in` 一致）；跨仓拆两张单 | DDL 无需改（`warehouse_id` 在单头） |
| Q3 | 部分出库后能否**改订单**（加行/改量）？ | **不能**，`APPROVED` 之后只能关闭或作废；要加货就新开单 | 状态机 1 条边 |
| Q4 | 客户**账期**按出库日还是应收生成日算到期？ | 出库业务日期（`out_date + credit_days`），过账时间不影响 | 报表归属 |
| Q5 | 收款的**预收**要不要计入信用占用？ | **不计**（钱已经到账，不是欠款） | 额度口径 |
| Q6 | 对账单确认后客户提异议怎么办？ | **作废重开**（不提供"反确认"），`66603` 拦住修改 | 接口 1 个 |
| Q7 | 销售退货（`out_type='RETURN'`）本批做到什么程度？ | **只留单据类型与字段**，不实现退货入库的完整流程（下一批） | §0.3 已声明 |

### 9.4 自检对照（任务书 11 条要求 → 本文位置）

| 任务书要求 | 本文位置 | 状态 |
|-----------|---------|------|
| 文档 ≥2000 行 | 全文 | ✅（见文末统计） |
| 现状核查写进 §1（迁移号 / `biz_` 清单 / 9 张表 `\d`） | §1.1 / §1.2 / §1.3 | ✅ 全部实测，含 3 处纠偏（§1.8） |
| 12–15 张表 | §2（13 张） | ✅ 13 张，V0160–V0164 |
| 每张表完整 CREATE TABLE（CHECK / 索引 / 注释） | §2.1–§2.5 | ✅ 13 张全 DDL，59 个 CHECK / 48 个索引 / 38 条 COMMENT |
| 7 条硬结论，每条给候选方案对比 + 结论 + 理由 | §3.2.1–§3.2.4、§3.7、§3.5.2、§3.6、§3.4、§3.3，**汇总对比表 §3.12** | ✅ D1–D7 各一张候选方案表（含 ❌/✅ 判定与真库证据），另加 D8–D10 |
| API 契约（路径 / 请求 / 响应 / 错误码 / 权限码） | §4（41 个接口） | ✅ 41 个，含 661xx–666xx 错误码段规划 |
| 凭证引擎复用（怎么调 / 规则怎么种 / 模板 / 红冲） | §3.7 | ✅ 真实函数签名 + 行号 + 新增函数原型 |
| 测试策略（单测估算 / 真库场景 / e2e 清单） | §6 | ✅ 336 后端用例 + 8 个真库场景 + 4 个 e2e 脚本 |
| 迁移号从 V0160 起 | §1.1 / §2.7 | ✅ V0160–V0168 |
| 命名严格对齐批次 1 | §2.0 / §4.2 / §8.3 | ✅ 6 处偏差逐条声明（§0.2） |
| 只写文档不写代码 | 全文 | ✅ 无任何 `.rs` / `.vue` / `.sql` 迁移文件落地（DDL 只作设计） |

### 9.5 与任务书的六处偏差（汇总，详见 §0.2）

| # | 任务书假设 | 实测 | 处理 |
|---|-----------|------|------|
| 1 | 明细列名 `price_excl` | 实际 `price` | 用 `price` / `price_incl` |
| 2 | 生产已到 V0151 | 本轮复核：`p2e1` = 150/99，`p2e2` = **151/100**（V0151 已应用）；仓库文件已到 **V0156** | 从 V0160 起，两种真值下都安全；补丁空号只剩 V0157–V0159 |
| 3 | `version ~ '^V'` 能查到迁移 | 实际存纯数字 `'150'` | §1.1 记录该坑 |
| 4 | 仓库级 `allow_negative` 需要"落地" | 列已存在（批次 1 已建） | 直接用，不加列 |
| 5 | 序列号库位指向 `biz_location.id` | 明细库位指向 `biz_inventory_location.id` | §1.8 ① 纠偏 |
| 6 | 12–15 张表"够用" | 13 张表 + 9 条迁移（含权限/科目/规则/序列） | 迁移数超"3 条辅助"的假设，因为 V0165–V0168 是上线必需 |

### 9.6 文档统计（交付自证）

| 项 | 数 |
|----|----|
| 总行数 | **4311**（`wc -l` 实测，含 §0.4 速查与 §9.7 复核记录） |
| 表格 / 代码块 | 97 张表 / 133 个代码块（脚本校验：每张表列数一致 0 处不符、围栏无未闭合） |
| 表 DDL | 13 张（59 CHECK / 48 业务索引 + 13 主键 = 61 索引 / 38 COMMENT） |
| 迁移 | 9 条（V0160–V0168，13 表 + 29 权限码 + 5 科目 + 3 凭证规则 + 6 序列） |
| API 接口 | 41 个（7 模块 + 1 看板） |
| 错误码 | 新增 34 个（66101–66603），复用 6 个（10001/10002/10003/10004/66002/66007） |
| 权限码 | 29 个（menu id 1011–1039） |
| 不变式巡检 SQL | 8 条（I1–I8） |
| 真库 POC 场景 | 10 组（含 2 组双会话并发） |
| 单测估算 | 后端 336 / 前端 126 |
| e2e 脚本 | 后端 4 个（≈194 断言）/ 前端 2 个 spec |

### 9.7 本轮重派落盘与复核记录（卡 `t_70b2338e`，2026-10-31）

本节只记**本轮**（重派后在仓库 `repo/` 工作副本里）真实执行的命令与原样输出，用于回答"文档到底落盘没有、内容是否还成立"。

#### 9.7.1 落盘（唯一硬指标）

```bash
$ cd /root/.hermes/kanban/boards/dev/workspaces/t_70b2338e/repo
$ cp /root/.hermes/kanban/boards/dev/attachments/t_7384c8f3/ERP-批次2-技术架构.md \
     docs/ERP-批次2-技术架构.md
$ wc -l docs/ERP-批次2-技术架构.md
4311 docs/ERP-批次2-技术架构.md
$ ls -la docs/ERP-批次2-技术架构.md
-rw-r--r-- 1 root root 282366 Oct 31 08:47 docs/ERP-批次2-技术架构.md
```

> 硬指标两项：文件**存在**（`ls` 有输出）＋ **≥2000 行**（`wc -l` = 4311，最终值）。

#### 9.7.2 DDL 从文档正文重新抽取并执行（不手敲、不复用旧文件）

```bash
$ python3 extract_ddl.py docs/ERP-批次2-技术架构.md /tmp/ddl_p3d2_verify.sql
blocks: 13
CREATE TABLE: 13
CHECK: 62            # 裸串计数（3 处是 COMMENT 里的说明文字，非约束）
CREATE INDEX: 32
CREATE UNIQUE INDEX: 16
COMMENT ON: 38
out lines: 510

$ grep -c 'CONSTRAINT ck_' /tmp/ddl_p3d2_verify.sql
59                   # 真正的 CHECK 约束数

$ su postgres -c "dropdb --if-exists erp_p3d2_verify"
$ su postgres -c "createdb -T cloudpivot_p2e1 erp_p3d2_verify"   # 克隆批次 1 生产 schema
$ su postgres -c "psql -q -d erp_p3d2_verify -v ON_ERROR_STOP=1 -f /tmp/ddl_p3d2_verify.sql"
PSQL_EXIT=0          # 13 张表一次建完，零错误
```

建完后在 **`erp_p3d2_verify`**（本轮新建，不是旧 POC 库）上核对：

```sql
-- 表
select count(*) from information_schema.tables where table_schema='public'
 and table_name in ('biz_customer','biz_customer_credit','biz_customer_credit_txn',
  'biz_sale_order','biz_sale_order_item','biz_stock_out','biz_stock_out_item',
  'biz_stock_out_item_serial','biz_receivable','biz_receipt','biz_receipt_item',
  'biz_customer_statement','biz_customer_statement_line');
-- 13

-- 索引（13 主键 + 48 业务索引）
select count(*) from pg_indexes where schemaname='public' and tablename in (…同上 13 张…);
-- 61

-- CHECK 约束
select count(*) from pg_constraint c join pg_class t on t.oid=c.conrelid
 where c.contype='c' and t.relname in (…同上 13 张…);
-- 59

-- 注释
-- 38

-- 库位外键落点（口径 ③ 的实证）
select conname, confrelid::regclass::text from pg_constraint
 where conrelid='biz_stock_out_item'::regclass and contype='f' and conname like '%location%';
-- biz_stock_out_item_location_id_fkey | biz_inventory_location
```

**结论**：文档 §2 的 DDL 与"真库能建、约束数对得上"三者一致；§0.4 口径 ③ 的外键指向也在本轮的库里重新确认过一次。

#### 9.7.3 双会话真并发复跑（新库 `erp_p3d2_conc`）

```bash
$ su postgres -c "createdb -T cloudpivot_p2e1 erp_p3d2_conc"
$ su postgres -c "psql -q -d erp_p3d2_conc -v ON_ERROR_STOP=1 -f /tmp/ddl_p3d2_verify.sql"   # 夹具前先建表
# 夹具：库存 qty=8 / amount=40 / avg_cost=5；客户额度 100000、已用 0
$ bash run_conc2.sh
```

原样输出（两个 psql 会话，`sleep 0.4` 错开后 B 被 A 的行锁阻塞，A 持锁 3 秒）：

```
===== POC-9 并发出库：库存 8，两个会话各扣 5 =====
--- 会话 A ---
[A] 事务开始，扣减 5 件（持行锁 3 秒）
 after_qty | after_amount
-----------+--------------
    3.0000 |      15.0000
UPDATE 1
[A] 已提交
--- 会话 B ---
[B] 尝试扣减 5 件（会被 A 的行锁阻塞…）
 after_qty | after_amount
-----------+--------------
(0 rows)
UPDATE 0
[B] 上面的 RETURNING 若为 0 行 = 库存不足被条件扣减挡下

===== POC-10 并发信用占用：额度 10 万，两个会话各占 8 万 =====
--- 会话 C ---
 order_used | used_amount | available
------------+-------------+-----------
 80000.0000 |  80000.0000 | 20000.0000
UPDATE 1
--- 会话 D ---
 order_used | used_amount | available
------------+-------------+-----------
(0 rows)
UPDATE 0

===== 终态 =====
  qty   | amount  | avg_cost
--------+---------+----------
 3.0000 | 15.0000 | 5.000000
 order_used | ar_used | used_amount | version
------------+---------+-------------+---------
 80000.0000 |  0.0000 |  80000.0000 |       1
```

**这 4 行终态就是 D1 / D4 的全部底气**：库存 8 − 5 = 3（不是 −2），`used_amount` 停在 80000（不是 160000）。与任务书给的实测结论**逐字一致**。

#### 9.7.4 文档 QA（本轮重算，不是照抄上一轮数字）

```bash
$ python3 qa_doc.py docs/ERP-批次2-技术架构.md      # 补完 §9.7 后的最终复算
行数(wc -l 等价): 4311
代码块围栏(``` 行): 266  -> 代码块数: 133  -> 未闭合: 否
表格数: 97  -> 列数不一致的行: 0
标题号: 148  -> 被引用号: 75  -> 引用差集(找不到): 0
关键口径自检: 9/9 OK（税率 0.1300 / 13.0000 / 0–1 / 0–100 / location_id / V0160 / V0156 / qty=3 / 80000）
```

> 中途（只加了 §0.4、还没写 §9.7 时）测到的是 `4161 行 / 126 块 / 97 表 / 引用差集 2（差集就是 §9.7、§9.7.3 这两个还没写的标题）`；补完 §9.7 后差集归零。上一轮记的是"123 个代码块 / 93 张表"，本轮最终 **133 块 / 97 表**，增量全部来自 §0.4 与 §9.7。**表列数不一致 0 行、围栏无未闭合、引用差集为空**——三条硬 QA 门禁本轮全部重验通过。

#### 9.7.5 迁移号一次性复核（本轮）

```bash
$ ls backend/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -4
V0154
V0155
V0156          # ← 任务书点名"已被用掉"的那个号，就在这
$ su postgres -c "psql -d cloudpivot_p2e2 -tAc \"select max(version::int), count(*) from schema_migrations\""
151|100
$ su postgres -c "psql -d cloudpivot_p2e1 -tAc \"select max(version::int), count(*) from schema_migrations\""
150|99
```

批次 2 仍然从 **V0160** 起（V0160–V0168），**没有用 V0156、也没有用 V0157–V0159**——这三个号留给批次 1 补丁（§8.5）。

---

**文档结束。** 本批只做设计与验证，不含业务代码；实现按 §7 的 E1–E8 推进，每个里程碑都能独立演示。
