# E2E-FIX-5：凭证摘要 UUID + 退货凭证 source 指向子单据（P3+P4）

- 卡：`t_6e6b43a1`（父卡 `t_4570b9e1` E2E-FIX-4）
- 发现源：E2E-02 销售域演练（145 `cloudpivot_dev`）
- 结论：两条缺陷都已修；**40/40** 条真实 HTTP + 落库断言全绿（隔离库 + 隔离实例，真 token）。
- 提交：`fix(erp-voucher): 凭证摘要取名称 + 退货凭证 source_doc_no（E2E-FIX-5 ①②）`
  （hash 以 `git log -1 --format=%h` 为准，本文件不写死，避免 amend 后失真）

---

## 一、问题（实测复现，不是推断）

### ① 凭证摘要里客户写的是 UUID（P3，违反 AGENTS.md 约定 13）

```sql
-- 修复前：收款单 RC-20261109-000001 过账
select summary from biz_voucher where source_type = 'RECEIPT' and source_id = '<RC1>';
-- → 收款 RC-20261109-000001 客户 01a1bdda-4947-4b3e-9d5c-…      ← 36 位 UUID
```

同一形状还有两条路径（同一份代码写法，扫全仓 `format!` 逐条核过）：

| 路径 | 修复前摘要 | 文件:行（修复前） |
|---|---|---|
| 收款过账 | `收款 RC-… 客户 <uuid>` | `receipt.rs:1599` |
| 收款退款过账 | `收款退款 RR-… 客户 <uuid>` | `receipt_refund.rs:2028` |
| 开票（收入凭证） | `开票 IV-… 客户 <uuid>` | `invoice.rs:1732` |

分录摘要继承单头摘要，所以**分录行里也是 UUID**（`biz_voucher_entry.summary` 同形状），
审计/对账时按摘要搜客户名根本搜不到。

### ② 退货过账的两张凭证 source 指向子单据（P4）

```
退货单 SR-20261109-000001 过账产生：
  · PZ-…-000004  source_type=STOCK_IN  source_no=IN-20261109-000002（退货入库单）
  · PZ-…-000005  source_type=INVOICE   source_no=IV-20261109-000002（红字发票）

按退货单号查凭证 → 0 行（只能 summary LIKE '%SR-…%' 模糊查）
```

根因不是漏写，而是**语义混用**：`source_no` 表达的是「`source_id` 那张单的单号」（记账依据 /
子单据）。一张业务单据派生多张子单据（入库 / 红字发票）时，它必然指向子单据，业务单据号
（用户手里那个号）从来没有落库。

---

## 二、修法

### ① 摘要取名称（不取 UUID）

新增两个小工具，所有涉及往来单位 / 物料的凭证摘要统一走它们：

```rust
// erp::form_snapshot —— 客户显示名（与既有 supplier_name 同口径：不过滤 deleted_at）
pub async fn customer_name(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>>

// erp::voucher —— 摘要里的实体显示名（AGENTS.md 约定 13）
pub fn party_label(name: Option<String>, kind: &str, id: Uuid) -> String
//   有名字 → 名字；查不到 / 空白 → 「客户已删除(01a1bdda)」（只留 8 位短 id，绝不写整段 UUID）
```

改动点（生产代码）：

| 位置 | 改动 |
|---|---|
| `receipt.rs::post_receipt_voucher` | `客户 {customer_id}` → `客户 {party_label(...)}` |
| `receipt_refund.rs::post_refund_voucher` | 同上（复用本模块已有 `customer_name`） |
| `invoice.rs::post_invoice_voucher` | 同上 |
| `sales_return_posting.rs` | `cost_voucher_summary` / `revenue_voucher_summary` 增加客户名参数 → `销售退货 SR-… 客户 <名称> 成本冲减 / 收入冲减`（新增 `customer_label()` 查名） |
| `purchase_return_posting.rs` | `voucher_summary` 增加供应商名 → `采购退货 PR-… 供应商 <名称>（原入库单 IN-…）冲减应付与存货` |
| `payable_payment.rs` | `付款 PY-… 供应商 <名称> 应付单 AP-…` |

> 采购入库 / 出库 / 结转 / 盘点 / 年结等摘要本身**不含实体引用**（只有单号 + 金额），不动；
> 物料类摘要沿用既有 FIX-9 口径（写物料**编码**，`物料 P2C1-M1`），本来就不是 UUID。

### ② 凭证加 `source_doc_no`（卡面方案 A）

```sql
-- V0357
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS source_doc_no VARCHAR(64);
CREATE INDEX ... ON biz_voucher (source_doc_no) WHERE deleted_at IS NULL;
```

两条链各自正确，互不侵占：

```
source_type / source_id / source_no  → 记账依据（子单据），**保持不变**
source_doc_no                        → 业务单据号（用户手里那个号 / 人对外的单号）
```

- **写入口径**（唯一收口 `voucher::post_voucher`）：`COALESCE(显式传入, source_no)`。
  绝大多数单据两者同值，不传即可；只有退货过账显式传退货单号：
  - `sales_return_posting::write_cost_voucher`（入库凭证）→ `source_doc_no = 退货单号`
  - `sales_return_posting::write_revenue_voucher`（红字发票凭证）→ 同上
  - `purchase_return_posting`（出库冲销凭证）→ `PR-…`
- **红冲继承**：`voucher::reverse_posted` 生成的红字凭证继承原凭证的 `source_doc_no`
  （红冲一张退货凭证后，按退货单号仍能查到 2 张）。
- **查询面**：
  - `GET /business/erp/voucher/page?source_doc_no=SR-…`（**精确**，走新索引）
  - 关键词 `voucher_no=SR-…` 现在同时模糊匹配 `voucher_no / source_no / source_doc_no`
    （用户粘一个单号进来就能搜到）
  - `VoucherRes` 回显 `source_doc_no`；前端凭证页加「业务单号」列 + 筛选框 + 详情抽屉标签。

### ③ 存量数据：直接回填（数据量小，不留「只保证新数据」）

V0357 里三条回填（`up.sql` §4–§6），全部幂等：

1. **历史退货凭证的业务单号**：从摘要里正则捞回来
   （`销售退货 SR-YYYYMMDD-NNNNNN` / `采购退货 PR-…` → `source_doc_no`）；
2. **其余凭证**：`source_doc_no = source_no`（业务单据 = 记账依据，绝大多数如此）；
3. **摘要里的 UUID → 名称**：锚定前缀 `客户 <uuid>` / `供应商 <uuid>` / `物料 <uuid>`
   逐条 `replace()`，单头与分录两张表都做；替换后超 `varchar(255)` 的行跳过
   （否则 22001 会把整个迁移打断）。

已知残留（迁移 `NOTICE` 会报数，不阻断）：主数据被**物理删除**的行没有名字可换；
名称超长（替换后 > 255）的行按设计跳过。演练库回填后全库扫描 = 0 行。

---

## 三、验收对照（全部为本机隔离库 `cp_e2efix5` + `:18322` + 真实 HTTP/token 实测）

脚本：`live-99.sh`（**可重放**：自建库、自起实例、全量断言，末尾自动停实例）
原始输出：`live-99.txt` ｜ 报文：`raw/*.json`

| 卡面验收 | 实测 |
|---|---|
| ① 新建收款/退款/退货 → 摘要显示客户/供应商名称 | ✅ `收款 RC-20261109-000001 客户 E2E5 北京云枢示例客户` / `收款退款 RR-… 客户 E2E5 …` / `开票 IV-… 客户 E2E5 …` / `销售退货 SR-… 客户 E2E5 … 成本冲减 / 收入冲减`；4 条路径都断言「不含 36 位 UUID」 |
| ② 退货过账 → 两张凭证 `source_doc_no` = 退货单号 | ✅ `PZ-…-000004 STOCK_IN source_no=IN-…-000002 source_doc_no=SR-…-000001`；`PZ-…-000005 INVOICE source_no=IV-…-000002 source_doc_no=SR-…-000001`；且 `source_no ≠ 退货单号`（两条链分流） |
| ③ 按退货单号能查到凭证 | ✅ API 精确筛 `?source_doc_no=SR-…` → `total=2`；关键词 `?voucher_no=SR-…` → `total=2`；SQL `count(*) WHERE source_doc_no=…` = 2 |
| ④ 存量数据说明 | ✅ **已回填**（不是「只保证新数据」）：造一条「迁移前形状」的历史凭证（摘要含客户 UUID、无 `source_doc_no`）→ 重放 V0357 → 摘要变名称、分录同步、`source_doc_no = source_no`；**再重放一次**结果不变（幂等） |
| ⑤ cargo test + push | ✅ 见下节 |

额外断言：全库 `biz_voucher` / `biz_voucher_entry` 摘要里「实体前缀 + 36 位 UUID」= **0 行**；
有来源单号的 AUTO 凭证 `source_doc_no` 为空 = **0 行**。

---

## 四、测试与门禁

| 项 | 命令 | 结果 |
|---|---|---|
| 编译 | `cargo check --all-targets` | exit 0（零新增告警） |
| 新增纯单测 | `cargo test --lib -- --exact business::erp::voucher::tests::party_label_never_emits_full_uuid` | 1 passed |
| 新增真库单测 | `ERP_DB_URL=… cargo test --lib -- --exact business::erp::voucher::tests::source_doc_no_records_business_doc_and_is_queryable` | 1 passed（2.89s = 真连库，非 SKIP 假绿） |
| 凭证域全量（串行） | `cargo test --lib business::erp::voucher -- --test-threads=1` | **80 passed / 0 failed** |
| 退货 / 采购退货 / 收款 / 发票 / 付款 / 快照 | `cargo test --lib business::erp::{sales_return,purchase_return,receipt,invoice,payable_payment,form_snapshot}` | 33 / 17 / 31 / 17 / 38 / 5 passed，0 failed |
| 前端 typecheck | `pnpm typecheck` | exit 0 |
| 前端单测 | `node --test tests/unit/erp-payable-voucher-ledger.test.ts` | 9 passed |
| 前端构建 | `pnpm build` | exit 0 |
| 真实 HTTP 演练 | `bash live-99.sh` | **PASS=40 FAIL=0** |

> ⚠️ `business::erp::voucher` **必须 `--test-threads=1`**：`voucher_linkage_tests` 的 6 条用例共用
> `uk_flow_linkage_rule`（`(trigger_entity, trigger_status, action_code)` 全表唯一），并行互相踩。
> 该约束模块文档里已写明；并行跑出的 2 条红是**测试并发口径问题**，串行复跑 80/80 全绿（日志见
> `verification.txt` §3 与 §4）。

---

## 五、文件清单

后端生产代码：
```
backend/migrations/business/V0357__voucher_source_doc_no.{up,down}.sql   # 新增
backend/src/business/erp/form_snapshot.rs      # + customer_name()
backend/src/business/erp/voucher.rs            # + party_label() / PostVoucher.source_doc_no / INSERT / 红冲继承 / VOUCHER_COLUMNS
backend/src/business/erp/voucher_query.rs      # + source_doc_no 列/筛选/关键词/出参
backend/src/business/erp/receipt.rs            # 摘要取客户名称
backend/src/business/erp/receipt_refund.rs     # 摘要取客户名称
backend/src/business/erp/invoice.rs            # 摘要取客户名称
backend/src/business/erp/sales_return_posting.rs   # 摘要带客户名 + 两张凭证 source_doc_no=退货单号
backend/src/business/erp/purchase_return_posting.rs# 摘要带供应商名 + source_doc_no=退货单号
backend/src/business/erp/payable_payment.rs    # 摘要带供应商名
+ 20 个 PostVoucher 字面量补 source_doc_no（编译器兜底，全仓 30 处逐一确认）
```

前端：`api/business/erp/index.ts`（`VoucherRow.source_doc_no` / 分页入参）、
`views/business/erp/voucher/{index.vue,voucher-logic.ts}`（业务单号列 + 筛选框 + 详情标签）、
`tests/unit/erp-payable-voucher-ledger.test.ts`（筛选参数单测）。

文档：`docs/编码规范.md` §1.11「凭证摘要 / 业务单据号」（强制约定，防回归）。

---

## 六、未验证 / 风险

- **144 生产未碰**（卡面禁止）；145 主实例未重启（本卡用隔离库 + 隔离实例，见 `live-99.txt` 第 0 节
  的二进制路径与迁移指纹 `290 条 / 最新 357`）。
- 全量 `cargo test --lib`（3882 条）未跑：本卡只跑受影响的 8 个模块套件（见上表），
  合计 184 条真库/纯逻辑用例全绿。
- 前端未做浏览器点击级 E2E：业务单号筛选的 DOM 行为由 `voucher-logic` 单测覆盖（trim / 参数拼装）。
- V0357 的摘要回填**不可回滚**（UUID 一旦换成名称就回不去）——`down.sql` 里已写明，
  真回滚前须先 `\copy` 存档。
