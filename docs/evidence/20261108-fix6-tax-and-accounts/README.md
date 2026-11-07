# FIX-6 验收报告（税务与科目 · 145 测试机）

> 卡：`t_728f716e`【FIX-6】tax_rate 标度统一 + 期初科目可配 + 含税口径 + 留抵重分类
> 环境：145 测试机 · 隔离实例 `127.0.0.1:8047` · 库 `cloudpivot_dev` 的**拷贝库**（8033 服役实例全程未动）
> 提交：见卡内 `commit`（本报告随代码入库）
> 迁移：`V0327__tax_rate_percent_scale` / `V0328__opening_equity_account_config` / `V0329__report_item_negative_reclass`
> 口径文档：`docs/ERP-税额与含税口径.md`

## 0. 一句话结论

四个问题按卡面推荐方案全部修掉，145 真机 **79/79 断言全绿**（`21_e2e.sh`）：
① `POST /invoice {"tax_rate":13}` → 200 且税额 = 金额 × 13%；② `{"tax_rate":0.13}` → 200 + ×100 + WARN；
③ 订单/发票/物料三处税率同口径（都是百分数，存量 0.13 已迁成 13 且**金额一分未动**）；
④ 期初对方科目改成 3101 后新期初凭证就贷 3101；⑤ 留抵税额 208 重分类到「其他流动资产」并写进附注，
报表仍然 资产=负债+权益（diff=0）。

## 1. 交付物

| 层 | 文件 | 说明 |
|---|---|---|
| 后端 | `business/erp/tax_rate.rs`（新增 396 行） | 税率唯一入口：`TaxRateMode`（PERCENT/DECIMAL/LEGACY）+ `normalize_with_mode` + `tax_of`/`split`/`percent_of`，含 8 条单测 |
| 后端 | `business/erp/invoice.rs`（+99/−?） | 百分数口径 + `tax_rate_mode` 落库/回显；默认 13；`tax_of`/`split_tax` 走 `/100` |
| 后端 | `business/erp/dto.rs`、`handler.rs` | 物料税率校验放到 0–100；建/改物料走过渡期归一（0.13 → 13 + WARN）；采购行 DTO 税额 `/100` |
| 后端 | `business/erp/sales_return_posting.rs` | 红字票税率反算 → 百分数（`percent_of`），夹 [0,100] |
| 后端 | `business/erp/opening.rs`（+155/−?） | 期初对方科目读 `sys_config`（默认 3103），凭证/互斥/回显三处统一；新增真库用例 `r5_10b_*` |
| 后端 | `business/erp/report.rs`（+258/−?） | 「第 1.5 遍」负数重分类 + `reclass[]`/`notes[]` 出参；2 条纯逻辑用例（触发/不触发） |
| 迁移 | `V0327`/`V0328`/`V0329`（up+down） | 见 §3 |
| 前端 | `invoice-logic.ts`、`invoice/index.vue`、`erp-invoice.test.ts` | 税率百分数（13）、`taxRateText` 不再 ×100、输入 0–100 + `%`、默认 13 |
| 文档 | `docs/ERP-税额与含税口径.md` | 标度口径 / 过渡期语义 / 含税勾稽三条公式 / 留抵附注 / 期初可配 / 遗留项 |

## 2. 各问题的修法与证据

### 问题 1：tax_rate 两种标度（100 倍税差）

**修法**：统一百分数（13 = 13%），`tax_rate_mode` 做过渡期（缺省 LEGACY：`(0,1]` 自动 ×100 + WARN；
显式 `DECIMAL` 才按小数解析；显式 `PERCENT` 时 `1` = 1% —— 这是「0 与 1 要能区分」的实现）。

**145 实测**（`raw/e2e.log`）：

| 断言 | 结果 |
|---|---|
| `POST /invoice {tax_rate:13}` | 200，`tax_amount=52`（400×13%）、`total=452`、`tax_rate=13`、`tax_rate_mode=PERCENT` |
| `POST /invoice {tax_rate:0.13}` | 200，`tax_rate=13`、`tax_amount=52`、`tax_rate_mode=DECIMAL`，实例日志新增 `税率按小数（0–1）传入，已自动 ×100`（`raw/tax_rate_warn.log`） |
| `POST /invoice {tax_rate:100.01}` / `{tax_rate:-1}` | 400 |
| `POST /invoice {tax_rate:1, tax_rate_mode:PERCENT}` | 200，`tax_amount=4`（1% 不是 100%） |
| `POST /invoice {tax_rate:0.5, tax_rate_mode:DECIMAL}` | 200，`tax_rate=50` |
| 物料 `tax_rate:0.13` / `13` / `100.5` | 200（库内 13/DECIMAL）/ 200（13/PERCENT）/ 400 |
| 存量迁移 | `biz_invoice`、`biz_material` 已无 `(0,1]` 未标注行；**发票行数 + 三个金额合计与迁移前 dump 逐字一致**（`2|2400.0000|52.0000|2452.0000`），物料 15 行不变 |

### 问题 2：库存期初对方科目写死 3103

**修法**：`sys_config.inventory_opening_equity_account`（V0328 默认 3103），`opening.rs` 三处改用
`inventory_equity_account_code()`；配置成不存在/非末级科目 → 400 点名字段（不静默回落）。

**145 实测**：把参数 `PUT` 成 `3101` → 保存库存期初 1000 元 → 期初凭证 **贷 3101 = 1000、3103 = 0**、
借 1405 = 1000、返回体 `inventory_credit_account = "3101"`；改回 3103 重存 → 贷 3103 = 1000、3101 = 0。

### 问题 3：税率 > 0 时 total_amount ≠ 应付（口径）

**先查（结论）**：`biz_sales_order` **本来就有** `total_with_tax`，且 `ck_sales_order_amount`
是精确恒等（`total_with_tax = total_amount + total_tax`），整单发货时应收金额 = 订单价税合计
（单测 `full_shipment_total_equals_order_line_total` 早已钉死）。所以这不是数据 bug，是**口径混用**：
拿「不含税 `total_amount`」去比「含税应付」。

**修法**：把口径写进 `docs/ERP-税额与含税口径.md` §2（五条勾稽公式 + 自查 SQL），并补两条真机断言：

| 断言 | 结果 |
|---|---|
| P0-01 开票凭证 | 借 1122 = 452（价税合计）/ 贷 5001 = 400（不含税）/ 贷 2221.02 = 52（税额），凭证借贷相等 |
| P0-02 采购单 | `total_amount=200`、`total_tax=26`、`total_with_tax=226`、行 `with_tax_amount=226`（应付 = 226，不是 200） |

### 问题 4：应交税费负数（留抵）未重分类

**修法**：`biz_report_item.reclass_negative_to`（V0329 把 `TAXES_PAYABLE` 指向 `OTHER_CURRENT_ASSETS`）；
`report::compute` 增「第 1.5 遍」：本项为负 → 把 `-金额` 搬到目标项、自己按 0 列示，并输出
`reclass[]`（来源/目标/金额/科目）与 `notes[]`（附注）。只搬列报金额、不动「科目认领」，
所以 `UNMAPPED_*` 仍为 0、勾稽恒等式不变。

**145 实测**（拷贝库把 2221 净额调到借方 208 后出表）：

| 断言 | 结果 |
|---|---|
| 应交税费列示 | `0`（不再是 -208） |
| 重分类明细 | `from=TAXES_PAYABLE` → `to=OTHER_CURRENT_ASSETS`，金额 `208.0000` |
| 附注 | 含「留抵税额」+「可结转下期抵扣」 |
| 取数说明 | 应交税费行 detail 点明「期末为借方余额 208，已重分类…本行按 0 列示」 |
| 勾稽 | `check.balanced=true`、`check.diff=0`（换对方科目、重存库存期初后复核仍为 0） |
| 借贷平衡 | 全库无 `total_debit <> total_credit` 的已记账凭证 |

## 3. 迁移（只追加，未改任何已应用迁移）

| 迁移 | 内容 | 幂等做法 | down |
|---|---|---|---|
| **V0327** `tax_rate_percent_scale` | `biz_invoice.tax_rate` → `NUMERIC(7,4)` + 默认 13 + 上界 100；`biz_material` 上界 100；两表加 `tax_rate_mode`；存量 `(0,1]` ×100 | 数据段以 `tax_rate_mode IS NULL` 为闸（跑完无 NULL → 第二遍更新 0 行）；DDL 全 `IF NOT EXISTS`/`DROP IF EXISTS` | 全部 ÷100（列存在才做，down 双跑跳过）+ 收窄回 `NUMERIC(6,4)`/≤1 |
| **V0328** `opening_equity_account_config` | 种 `sys_config.inventory_opening_equity_account = 3103` | `ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING`（运维手工值不被覆盖） | 删该键（代码回退常量 3103） |
| **V0329** `report_item_negative_reclass` | `biz_report_item.reclass_negative_to` 加列；`TAXES_PAYABLE → OTHER_CURRENT_ASSETS`；目标项 remark 补留抵说明 | `ADD COLUMN IF NOT EXISTS` + `WHERE ... IS DISTINCT FROM` | 删列 + 撤 remark 尾巴 |

**迁移幂等取证**：
- 同库再启实例 → `数据库迁移完成: 应用 0 个迁移`，`schema_migrations` 指纹/条数前后都是 `263|ce7c94c8…`（`raw/idempotency.log`）；
- 全新空库首启 → 应用 **260 个迁移**，V0327/28/29 自检全过（`tax_rate_mode` 列、`reclass_negative_to` 列、
  `equity_cfg=3103`）；
- **数据回填双跑**（99 隔离库，`V0327.up.sql` 手动跑两遍）：`0.13→13`、`0.005→0.5`、`0→0`、`13→13`
  第二遍**无一被二次放大**；`down` 双跑 + `ROLLBACK` 后状态复原；
- 存量库升级路径：145 拷贝库 **schema 326 → 329**，`应用 3 个迁移`，历史单据金额不变。

## 4. 测试

| 层 | 命令 | 结果 |
|---|---|---|
| 后端单测 | `ERP_DB_URL=… cargo test --lib -- --test-threads=1` | 见 §5（145 拷贝库真库全量） |
| 新增/改动用例 | `tax_rate`(8) / `invoice` 税率与金额 / `sales_return_posting` 反算 / `dto` 采购行 / `opening::r5_10b`（真库）/ `report::r1_fix6_*`(2) | 全绿 |
| 前端 | `npx tsx --test tests/unit/erp-invoice.test.ts` | 21/21 通过 |
| 前端 | `pnpm typecheck && pnpm build` | 见卡内汇报 |
| 真机 | `21_e2e.sh`（145 隔离实例 + 拷贝库） | **79 ✅ / 0 ❌ / RESULT: ALL PASS** |

## 5. 全量回归的归因（重要）

在 **99 本地全新库**（只有迁移种子、无演示组织/审批流数据）跑全量时，有 18 条**既有**用例红，
全部是「种子缺失」类环境红，与本卡改动无关，典型报文：

- `asset_linkage_tests.rs:195: 库里没有部门（先跑基础种子迁移）`（4 条 lnk09/st02）
- `expense_linkage_tests` / `contract_approval_tests`：审批流种子/演示用户缺失（10 条）
- `cash_flow::r3_real_config_covers_all_accounts`：全新库某些末级科目没有现金流分类（既有红）
- `attachment` / `aux` / `cancel_cascade`：依赖演示组织与历史单据

同一批用例在 **145 拷贝库**（有完整演示数据）上是绿的 —— 所以本卡的回归口径取 145 拷贝库，
与 FIX-5 的做法一致。

## 6. 复现步骤

```bash
# 145 上（只需一次）
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/10_prepare_145.sh'
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/11_start_instance.sh'

# 断言（E2E + 迁移幂等/空库首启）
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/21_e2e.sh'
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/13_fresh_and_idempotency.sh'

# 收尾（只杀本卡隔离实例，复核 8033 健康）
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/14_kill_instance.sh'
```

## 7. 遗留 & 边界

- `biz_expense_item.tax_rate` 仍是小数口径（报销模块的价税分离 + 审批快照），换标度会动报销金额，
  **单独立卡**；换算用 `tax_rate::to_decimal(percent)`，别自己写 `/100`；
- `biz_supplier.tax_rate` 列在库里但全仓无读写（摆设字段）；
- `tax_rate_mode` 两列是过渡期审计位，过渡期结束后删列 + 删 `TaxRateMode::Legacy` 分支。
