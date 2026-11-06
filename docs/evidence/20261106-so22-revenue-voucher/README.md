# SO-22 收入确认凭证（证据）

日期：2026-11-06　｜　卡：`t_83fe84c8`　｜　前置：SO-20 发票 API（`b396a09`）/ SO-21 发票前端（`07dfb67`）
口径依据：`docs/architecture/ARCH-7-销售模块设计.md` §5.5（收入确认）、§7.2（分录表）、§3.1.6（`biz_receivable.voucher_id`）

---

## 1. 交付内容（文件 / 函数）

| 文件 | 性质 | 说明 |
|---|---|---|
| `backend/src/business/erp/voucher.rs` | 改（+738 行） | 新增三个科目常量 `ACCOUNT_AR / ACCOUNT_REVENUE / ACCOUNT_OUTPUT_TAX`、`SOURCE_INVOICE`、`BALANCE_TOLERANCE`；新增 `SalesRevenueVoucher` 入参结构 + **`create_for_sales_revenue`**（卡面要求的那支）+ 两个纯函数 `sales_revenue_amounts` / `sales_revenue_split`；7 条单测 |
| `backend/src/business/erp/invoice.rs` | 改 | `post_invoice_voucher` 改为**只做翻译**（把发票行 → `SalesRevenueVoucher`）后调 `voucher::create_for_sales_revenue`，删掉原来那份内联分录构造（105 行）；`writeback_receivable` 增加**回填 `biz_receivable.voucher_id`**；`clear_receivable_writeback` 一并撤回 `voucher_id`；`ACCOUNT_*` 常量改为指向 `voucher::`（唯一真相） |
| `backend/src/business/erp/receivable.rs` | 改（+70 行） | 新增 `reverse_revenue_voucher`（按凭证 id 红冲收入凭证）；`cancel_for_stock_out`（出库作废 → 应收 `CANCELLED`）在同一事务里对「`voucher_id` 非空」的应收红冲收入凭证 |
| `backend/src/business/flow/linkage/spec.rs` | 改（+12 行） | **顺带修红**：`TRIGGER_EXEMPT` 补 `RECEIPT`（SO-17 收款）/ `INVOICE`（SO-20 发票）—— 这两个状态机实体漏登记导致 `triggers_cover_state_machine_entities` 常红（全量 round 1 的失败 ③） |

**没有新增迁移**：本卡不建表、不加列 —— `biz_receivable.voucher_id`（V0286 已有）、`biz_voucher` / `biz_voucher_entry` / `biz_accounting_period` 都是已存在结构，科目 `1122 / 5001 / 2221.02` 由 `V0183` 种下。

### 1.1 卡面要求 vs 实现（逐条）

| 卡面要求 | 实现 |
|---|---|
| 新增 `voucher::create_for_sales_revenue`（参照 `create_for_stock_out`） | ✅ `voucher.rs`，同签名形状 `(ex, &SalesRevenueVoucher, operator, now) -> DbResult<Option<Uuid>>`、同事务约定（吃 `&impl SqlExecutor`，不 begin/commit）、同样的「零金额跳过 + WARN 点名」 |
| 分录 借 1122（含税）/ 贷 6001（不含税）/ 贷 2221.02（税额） | ✅ 结构一致，但**贷方科目用 `5001` 不用稿面的 `6001`**：本仓 `biz_account` 的损益类是 `5xxx`（`V0183` 种 `5001 主营业务收入`，`V0184` 利润表按 `5001,5051` 归集），`6001` 在科目表里**不存在**，写它会直接吃 `account_required` 的「科目不存在」（SO-20 已踩，见其证据 §2.2）。销项税用叶子科目 `2221.02`（`2221` 是汇总科目，会被 `post_voucher` 的非末级闸拒） |
| 触发点二选一 → 选「开票时确认收入」 | ✅ 唯一触发点是发票 `DRAFT → ISSUED`（`invoice::change_status`）。`source_type = INVOICE`、`source_id = 发票 id`、`source_no = 发票号`。挂账（出库转应收）**不**生成收入凭证（挂账只是债权成立） |
| 合并/排序/尾差吸收复用 `VoucherMaterialLine::normalize` | ✅ `sales_revenue_split` = `normalize`（合并同物料 + 按物料 id 升序）+ `absorb_tail_diff`（尾差落到最后一行），与出库成本凭证**同一套实现**；发票是「一张票一行金额」粒度，`material_lines` 传空即回退成单行汇总（退货/多行票卡可直接填这个字段，无需改 builder） |
| 借贷平衡（差额 < 0.01 视为平衡） | ✅ 三层：① `sales_revenue_amounts` 校验「含税 = 不含税 + 税额」，差额 ≥ 0.01 **拒绝**（金额本身算错）；② 差额 < 0.01 的尾差吸收进销项税行（贷方税额 = 含税 − 不含税），落库分毫不差；③ 建完行再算一次 `Σ借 − Σ贷`，≥ 0.01 拒绝、< 0.01 吸收到最后一笔贷方行。`post_voucher` 的严格相等校验因此恒过 |
| 测试：正常凭证 / 多行科目 / 尾差处理 / 期间已关闭拒绝 | ✅ 7 条（见 §3.1） |

### 1.2 顺带落地的两条移交（卡内 comment）

1. **出库作废冲销的红字凭证**（SO-14 遗留卡移交）：`receivable::cancel_for_stock_out` 在 `UPDATE … WHERE status='OPEN'` 成功之后、返回之前，对 `row.voucher_id` 非空的应收调 `reverse_revenue_voucher`。红字 = 收入凭证反向（借 1122 红字 / 贷 5001 + 2221.02 红字），由 `voucher::reverse_posted` 生成（**不再写一套负数分录**）。
   - 触发条件靠 `biz_receivable.voucher_id`：本卡让**开票时把它回填**（`writeback_receivable`），所以「已开票 → 出库作废」这条链真正会冲；挂着未开票的应收 `voucher_id` 仍为 NULL，分支空转（预期）。
   - 幂等：原凭证 `reversed_by` 已置位则跳过（发票作废先冲过的场景不会二次红冲）。
2. **坏账核销凭证（6702/1122）没有落** —— 明确不做，原因见 §5。

---

## 2. 关键设计决定（含理由）

1. **收入科目 5001 而非 6001**：见 §1.1。`invoice.rs` 的同名常量已改为 `= voucher::ACCOUNT_REVENUE`，科目口径现在只有一处（`voucher.rs`），不会再出现「发票侧一套、别的单据一套」。
2. **`biz_receivable.voucher_id` 由开票回填**：这一列从 SO-13 起就恒 NULL（SO-13 的迁移注释写的是「等收入确认凭证回填」）。本卡把它接上，同时也给「出库作废 → 应收冲销」提供了**判断依据**（没有它就只能靠猜）。
3. **红冲复用 `voucher::reverse_posted`**：原凭证保持 `POSTED` + 写 `reversed_by`，新凭证 `is_red = 1`、`PZR-` 号段。期间口径「红字记作废当天」（`today_cst`），与出库/发票作废一致。
4. **期间闸只在一个地方**：`post_voucher` 查 `biz_accounting_period.status`（`CLOSED` ⇒ 报错「已结账」）。`create_for_sales_revenue` **不重复查**，避免两处判定漂移。
5. **不做「挂账时也确认收入」**：卡面二选一，且两种触发都做会导致同一笔销售两张收入凭证（重复确认收入 = 虚增利润）。

---

## 3. 门禁实测

### 3.1 编译 / 单测（真库 `ERP_DB_URL`，未设置则 DB 用例 SKIP）

```
cargo check --lib                               → Finished，0 warning
cargo test --lib business::erp::voucher::tests::  → 27 passed / 0 failed（含本卡 7 条）
```

本卡 7 条（前两条纯函数，**无 ERP_DB_URL 也会真跑**；其余 5 条真库）：

| # | 用例 | 断言 |
|---|---|---|
| ① | `sales_revenue_amounts_absorbs_under_a_cent_and_rejects_a_cent` | 1130=1000+130 通过；税额 129.9960（尾差 0.004）→ 吸收成 130 且 Σ=1130；税额 129.99（差额正好 0.01）→ 拒绝（文案含「不自洽」）；负数不含税/负数税额 → 拒绝 |
| ② | `sales_revenue_split_merges_same_material_and_absorbs_tail` | 明细 A400 + A100 + B499.99 → 合并成 2 行（A=500，按物料升序）+ 尾差 0.01 吸收 → Σ=1000 |
| ③ | `sales_revenue_voucher_posts_balanced_entries`（真库） | 3 行：1 借 1122 1130 / 2 贷 5001 1000 / 3 贷 2221.02 130；Σ借=Σ贷=1130；凭证头 `source_type=INVOICE`、`status=POSTED`、`entry_count=3` |
| ④ | `sales_revenue_voucher_splits_revenue_by_material`（真库） | 多行科目：借 1 行 + 收入 2 行（Σ=1000）+ 税 1 行 = 4 行，Σ借=Σ贷=1130 |
| ⑤ | `sales_revenue_voucher_rejects_closed_period`（真库） | 期间 2031-03 置 `CLOSED` ⇒ 报错「会计期间 2031-03 已结账，不能再生成凭证」；库里**无残留凭证**（半张凭证 = 账实不符） |
| ⑥ | `sales_revenue_voucher_skips_zero_amount`（真库） | 价税合计 0 ⇒ `Ok(None)`，不落凭证、不报错 |
| ⑦ | `receivable_cancel_reds_the_revenue_voucher`（真库） | 出库作废（`receivable::cancel_for_stock_out`）→ 应收 `CANCELLED` + 红字凭证（`is_red=1`、`reverse_of` 指回蓝字），红字每行与蓝字逐行反向核对 |

实测输出（`round-1-unit-tests.out.txt`）：

```
[SO22-①] 价税自洽 / 尾差吸收 / 0.01 拒绝 / 负数拒绝 全部通过 ✓
[SO22-②] 合并 400+100=500 / 升序 / 尾差 0.01 吸收 → Σ=1000 ✓
[SO22-③] 真库 3 行分录 借1122 1130 / 贷5001 1000 / 贷2221.02 130，借贷各 1130 ✓
[SO22-④] 真库多行科目：收入拆 2 行 Σ=1000 + 税 130，借贷各 1130 ✓
[SO22-⑤] 真库已结账期间拒绝：invalid value: 会计期间 2031-03 已结账，不能再生成凭证（凭证日期 2031-03-15）… ✓（无残留凭证）
[SO22-⑥] 真库零金额 → Ok(None)，不留凭证 ✓
[SO22-⑦] 真库出库作废 → 应收 CANCELLED + 收入凭证红冲（反向分录逐行核对） ✓
test result: ok. 7 passed; 0 failed
```

### 3.2 全量 `cargo test --lib`（`ERP_DB_URL` 指向**全新隔离库**，`--test-threads=1`）

| 轮 | 库 | 结果 | 失败项 |
|---|---|---|---|
| round 1 | `cloudpivot_so22fresh`（空库 → 启动应用 225 个迁移：215 versioned + 10 seed，全 `executed`） | **2846 passed / 3 failed**（1324.59s） | ① `three_tiers_start_flow_and_approve_to_approved`（顺序污染，既有）② `p16_graph_snapshot_selfcheck_is_clean`（so_approve graph_hash 漂移，既有）③ `flow::linkage::triggers_cover_state_machine_entities`（`RECEIPT`/`INVOICE` 漏登记联动豁免 —— **本卡顺带修掉**） |
| round 2 | `cloudpivot_so22fresh2`（同上，空库重迁） | **2847 passed / 2 failed**（1471.95s） | 仅 ① + ②（既有的两条，与本卡无因果关系） |

原始输出：`cargo-test-full-round1.txt` / `cargo-test-full-round2.txt`；逐条归因与单跑反证：`full-suite-attribution.md`。

**归因要点**（详见 `full-suite-attribution.md`）：
- ① `three_tiers` 在**干净库单跑 → ok**，只有跑完前面上千条用例后它才红 ⇒ 顺序污染，与本卡无关；
- ② `p16` 的漂移 hash（`da3c2eed… / 97fe9377…`）与 `docs/evidence/20261106-so08-shipment/full-suite-attribution.md` 记录的**逐字相同** ⇒ SO-06 的既有缺陷，任何库都红；
- ③ 已修：`flow/linkage/spec.rs` 的 `TRIGGER_EXEMPT` 补 `RECEIPT`（SO-17）/ `INVOICE`（SO-20）两条并附理由；修后 `business::flow::linkage::tests` 53 passed / 0 failed，round 2 不再出现。

本卡 7 条用例在两轮全量里都 ok（`[SO22-①]`…`[SO22-⑦]`）。

### 3.3 真实 token / 真实库 / 真凭证的 API 实测

实例：`docs/evidence/20261106-so22-revenue-voucher/start-instance.sh`（库 `cloudpivot_so22e2e`，端口 `18124`，启动 660ms、`/health` 两次 200）。

**A. 卡面定点验收（`verify-e2e.sh`，两轮各 21 断言）**

```
round-1: PASS=21 FAIL=0   → round-1-verify.out.txt
round-2: PASS=21 FAIL=0   → round-2-verify.out.txt
```

每轮覆盖：登录 → 从应收开票 → `ISSUED`（收入凭证 `PZ-20261106-000018`）→ 分录逐行（下）→ 应收回填 → 作废（红字凭证 + 撤回）→ 库层 0 张不平凭证。

```
1 | 1122 | 借 1130.0000 | 贷 0.0000
2 | 5001 | 借 0.0000 | 贷 1000.0000
3 | 2221.02 | 借 0.0000 | 贷 130.0000
PASS  借方 1122 = 价税合计 1130（1130.0000）
PASS  贷方 5001 = 不含税 1000（1000.0000）
PASS  贷方 2221.02 = 税额 130（130.0000）
PASS  Σ借 = Σ贷（0.0000）
PASS  凭证头 借贷平衡（total_debit = total_credit = 1130）
PASS  biz_receivable.voucher_id = 收入凭证 id
PASS  biz_receivable.invoice_no = 发票号（IV-20261106-000030）
PASS  红字凭证已生成（reverse_of 指回蓝字）/ is_red=1
PASS  红字分录反向：贷 1122 = 1130 / 借 5001 = 1000
PASS  应收 voucher_id 已撤回（NULL）/ invoice_no 已撤回（NULL）
PASS  借贷不平凭证数（0）
PASS  分录与凭证头金额不一致的凭证数（0）
```

**B. SO-20 的 70 条发票接口回归（本卡改了它们的凭证生成路径，必须回归）**

```
BASE=http://127.0.0.1:18124/api DB=cloudpivot_so22e2e bash docs/evidence/20261106-so20-invoice/curl-4-endpoints.sh
→ PASS=70 FAIL=0（round-1-so20-regression.out.txt）
```

覆盖开票/额度/分页/详情/开票生成凭证/作废红冲/归档/手工开票/手填号重复 409 等；库层汇总仍是「蓝字 2 / 红字 1、借贷平衡 0 张不平」，**行为未被本卡改动破坏**。

### 3.4 迁移

本卡**无新迁移**（不建表不加列）。隔离库启动日志：`数据库迁移完成: 应用 0 个迁移`（克隆自 `cloudpivot_so20`，`schema_migrations` 225 行，kind=versioned 全部 `applied_via=executed`）。

---

## 4. 复跑方式

```bash
# 1. 一次性隔离库（从已迁移的库克隆，省 225 个迁移的时间）
su - postgres -c "dropdb --if-exists cloudpivot_so22t; createdb -O alun -T cloudpivot_so20 cloudpivot_so22t"

# 2. 单测（真库）
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so22t' \
  cargo test --lib business::erp::voucher::tests:: -- --test-threads=1 --nocapture

# 3. API 实测（另一套库，避开单测夹具）
su - postgres -c "dropdb --if-exists cloudpivot_so22e2e; createdb -O alun -T cloudpivot_so20 cloudpivot_so22e2e"
DB=cloudpivot_so22e2e PORT=18124 bash docs/evidence/20261106-so22-revenue-voucher/start-instance.sh
BASE=http://127.0.0.1:18124/api DB=cloudpivot_so22e2e bash docs/evidence/20261106-so22-revenue-voucher/verify-e2e.sh
```

⚠ 克隆库的 `admin` 口令已被前序卡改成 `Admin@123456_so20`；`verify-e2e.sh` 会按 `Admin@123456 → _so20 → _so21` 顺序试探（登录有频控 10005，试错之间 sleep 6）。

---

## 5. 已知边界（交给后续卡）

1. **坏账核销凭证（借 6702 资产减值损失 / 贷 1122）没做**：`biz_account` 里**没有 6702**（`V0183` 只种了 `5xxx` 损益类 + 资产/负债/权益），要先有「6702 科目 + 损益表映射」的迁移，然后才能在 `receivable::write_off` 里按本卡的 `create_for_*` 形状加一支。硬塞进本卡会变成「顺手改科目表 + 改利润表域」，超出一张卡的验收面。
2. **红字发票（`OUT_RED` 行）不生成收入凭证**：SO-20 的既定边界（红字靠作废走红冲），退货流程（ARCH-7 §15.2.3）落地时再复用 `create_for_sales_revenue`。
3. **期间已结账时的跨期作废**：出库作废的红冲用「作废当天」的期间；若原收入凭证所属期间已结账，`reverse_posted` 会拒绝并让整笔作废回滚（口径正确但用户体验是「作废失败」）。跨期调整要做「在未结账期间做调整凭证」的专门卡。
4. **`biz_invoice` 没有明细行**：本卡的收入行只能汇总（`material_lines` 空）。要按商品拆收入，得先给发票加明细表（ARCH-7 §3.1.8 目前只是一行金额）。
