# SO-41 凭证本位币入账 + 汇兑损益月末重估 —— 证据包

卡：`t_6ca67fcb`（ARCH-7 §15.6.4 ②）· 上游 SO-40（V0314 双币列 + 汇率锁定，commit bfd275a）

## 1. 交付清单

| 类别 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/migrations/business/V0315__sales_fx_revaluation.up.sql` / `.down.sql` | 新建重估台账 `biz_fx_revaluation`（14 列 / 4 CHECK / 3 索引）+ 末尾自检（5 组行为探针） |
| 新模块 | `backend/src/business/erp/fx_revaluation.rs` | 期末日 / 账面人民币（比例法 + 累计重估）/ 重估差额 / 凭证 + 台账写入；纯逻辑 6 例 + 真库 7 例 |
| 凭证入账 | `backend/src/business/erp/voucher.rs` | `SalesRevenueVoucher.base: Option<BaseAmounts>`：有本位币三连就按本位币记账；新增 `validate_base_amounts` / `sales_revenue_split_in_base`（纯函数）+ 4 例测试 |
| 发票接入 | `backend/src/business/erp/invoice.rs` | 开票时把 `base_total_*` 交给收入凭证（外币发票 ⇒ 凭证金额 = 原币 × 锁定汇率） |
| 结账接入 | `backend/src/business/erp/period.rs` | `close_period` 在 `OPEN → CLOSED` **之前**调 `revalue_period`（同事务，期间闸只放行 OPEN） |
| 证据 | `scripts/verify/so41_fx_revaluation_evidence.sh`、本目录 | 真实 token curl：USD 应收 → 开票凭证 → 月末结账 → 重估凭证 + 台账 |

## 2. 迁移 APPLY（空库引擎实跑）

```
ERP_DB=cloudpivot_so41_api RECREATE=1 PORT=18140 bash scripts/verify/erp_clean_db.sh
→ schema_migrations(max|count) = 315|247      （246 + 本卡 1）
→ 315 | versioned | executed | success=t       （applied_via 只用 executed）
→ NOTICE 'V0315 自检通过：14 列 / 4 CHECK / 3 索引 / 5 组行为探针'
```

自检覆盖的**行为**（`DO` 块内 savepoint 探针，坏数据必须被拒）：
小写币种码 / `close_rate = 0` / `open_amount = 0` / `diff_base ≠ revalued_base − book_base` /
同期同应收重复行（撞 `uk_biz_fx_period_ar`）。

## 3. 口径与关键决策

### 3.1 会计口径（唯一真相在 `fx_revaluation.rs` 头注释）

```
open   = biz_receivable.unsettled_amount
book   = round(base_total_with_tax × open / amount, 4) + Σ历史 diff_base
reval  = round(open × 期末汇率, 4)              期末 = 期间码那月最后一天
diff   = reval − book
diff > 0：借 1122 应收账款 / 贷 5603 财务费用-汇兑损益
diff < 0：借 5603 财务费用-汇兑损益 / 贷 1122 应收账款
```

### 3.2 为什么是「比例法」而不是「逐笔核销配对法」

收款核销只记原币金额（`biz_receipt_allocation.allocated_amount`），不记本次核销结转了多少人民币。
按逐笔配对算账面人民币必须回溯每笔收款的结算汇率 —— 那是**核销时点**确认汇兑损益的口径（另一张卡）。
本期口径：账面人民币随未核销比例走，差额统一由月末重估吸收；重估后账面 = 未核销原币 × 期末汇率。

### 3.3 为什么必须落一张台账（`biz_fx_revaluation`）

重估会改 1122 的账面人民币。下个月再算「账面人民币」时必须把上个月提过的差额算进去，
否则每月都会把同一块汇兑损益重提一遍。凭证分录是**科目级合计**（一张凭证只有 1122 / 5603 两行），
`biz_voucher_entry` 上没有 receivable 辅助核算列，反查不到「这笔应收提过多少」⇒ 逐笔累计只能落台账。

### 3.4 卡面 6603 → 实际 5603（与既有三次同口径决策一致）

本仓科目表是**小企业准则**：损益类只有 `5xxx`，财务费用 = `5603`
（`V0183` 资产负债表映射、`V0184` 利润表 `FINANCE_EXPENSE` 都按 5603 取数）。
V0226（6602→5602）、V0229（6602→5602.01）、V0311（6601→5601）已有三次同样决策。
新种 66xx 会让财务费用在利润表和预算科目树里多出一份、而 5603 那份恒为 0。
故凭证摘要写「汇兑损益」、科目落 5603，**本卡不种任何科目**（科目编码一旦用过永久占用）。

### 3.5 `create_for_stock_out` 为什么不换算

出库成本结转账的金额来自 `biz_inventory.amount`（账上真实扣减额），**库存账不分币种、本来就是本位币**。
外币订单的出库单若再乘一次汇率就等于把成本翻了几倍。所以本卡对它的「改造」是：
补 `SO41-②` 回归测试钉住「借 6401 / 贷 1405 的金额 = 传入的账上扣减额，一分不加不减」，
并在文档里写清这条口径 —— 不需要也不允许在这里引入币种参数。

### 3.6 幂等与反结账

* 同一期间已提过（`biz_voucher.source_type='FX_REVALUATION'` + `source_no=期间码`）⇒ `skipped = true`，不重复计提；
* 反结账后再结账：账面已被上次重估改过，重算 `diff` 自然趋近 0（不是特判）；要重提必须走凭证红冲；
* 反结账**不回滚**重估凭证与台账（凭证是账务事实，down/回退只回退结构）—— 残留项，见 §7。

## 4. 真实 token curl 实测（`scripts/verify/so41_fx_revaluation_evidence.sh` 全量输出见 `api_evidence.txt`）

```
汇率种子：USD->CNY 7.1 @2026-11-01 / USD->CNY 7.9 @2026-11-30
① USD 手工挂账应收 1130 → 库内 USD | rate=7.10000000 | base_amount=8023 | base_with_tax=8023
② 从应收开票（2026-11-07，1000+130）→ USD | rate=7.1 | base_amount=7100 | base_tax=923 | base_with_tax=8023
③ PATCH status=ISSUED → 收入凭证分录：
   1 | 1122 | 应收账款      | debit=8023.0000  | credit=0
   2 | 5001 | 主营业务收入  | debit=0          | credit=7100.0000
   3 | 2221.02 | 应交税费-销项税 | debit=0     | credit=923.0000
   debit=8023.0000 credit=8023.0000 balanced=true        ← 本位币入账，不是原币 1130
④ 建 CNY 应收 500（对照：本位币不重估）
⑤ close-check：can_close=true, blockers=[]
⑥ POST /period/{id}/close → {"code":0,"status":"CLOSED"}
⑦ 重估凭证 PZ-20261107-000002 | 2026-11 期末汇兑重估（1 笔外币应收，期末汇率取 2026-11-30）| date=2026-11-30
   1 | 1122 | 应收账款   | 调增应收账款 | debit=904.0000 | credit=0
   2 | 5603 | 财务费用   | 汇兑收益     | debit=0        | credit=904.0000
⑧ 台账：2026-11 | AR-20261107-000001 | USD | open=1130 | book=8023 | reval=8927 | diff=904
⑨ CNY 被重估行数 = 0
⑩ 重复结账 → {"code":10003,"msg":"会计期间 2026-11 已经结账（…），不能重复结账"}；重估凭证条数=1
```

## 5. 测试

| 范围 | 结果 |
|---|---|
| `erp::fx_revaluation::tests`（6 纯逻辑 + 8 真库） | 14/14 |
| `so41_*`（4 例：2 纯逻辑 + 2 真库） | 4/4 |
| `erp::currency` / `erp::invoice` / `erp::period` / `erp::receipt` / `erp::receivable` / `erp::voucher::` + 本卡两组 | **143/143**（干净库 `cloudpivot_so41_reg`，247 迁移） |
| 全量 `cargo test --lib -j 1`（ERP_DB_URL 指向 cloudpivot_so41） | 见 §6 |

真库用例清单（都在 `ERP_DB_URL` 下跑过，SKIP 不算绿）：

| 用例 | 钉住的口径 |
|---|---|
| `revalue_period_posts_gain_and_loss_lines` | 汇率涨 ⇒ 借 1122 800 / 贷 5603 800 + 台账留痕 |
| `revalue_period_posts_loss_when_rate_falls` | 汇率跌 ⇒ 借 5603 600 / 贷 1122 600 |
| `revalue_period_ignores_base_currency_receivables` | 本位币应收：0 凭证 / 0 台账行 |
| `revalue_period_is_idempotent` | 同期间重跑 `skipped=true`，凭证数恒 1 |
| `revalue_period_refuses_missing_close_rate` | 缺期末汇率 ⇒ 拒绝并点名币种 + 取汇率日（绝不按 1 重估） |
| `revalue_period_uses_unsettled_share` | 部分核销只重估未核销部分（比例法） |
| `revalue_period_skips_voucher_when_net_is_zero` | 正负相抵 ⇒ 不出凭证（台账仍逐笔留痕，`voucher_id NULL`） |
| `close_period_triggers_fx_revaluation` | **结账链路端到端**：close_period → FX 凭证 + 台账挂凭证 + 期间 CLOSED |
| `so41_revenue_voucher_books_base_currency_amounts` | USD 发票 ⇒ 借 1122 8023 / 贷 5001 7100 / 贷 2221.02 923 |
| `so41_stock_out_voucher_stays_in_base_currency` | 出库成本凭证 = 账上扣减额，不乘汇率 |

真库用例的期次命名空间：**2050-xx 归本模块独占**（2031=period、2033=remind/stocktake、2034–2035=opening、
2036–2038=year_end、2039=reminder、2040=period_items、2026=生产期间绝不碰），
并且与 `voucher_query::tests::DB_LOCK` 共用同一把锁（串行）。

## 6. fmt / clippy / 全量测试

```
rustfmt +1.94.1 --edition 2024 --check（fx_revaluation / voucher / invoice / period / mod）→ 无 diff
cargo clippy --lib → 本卡文件 0 警告
  ⚠ 既有 1 条（非本卡）：sales_order_model::update_order too_many_arguments（SO-40 bfd275a 引入，
    未在本卡修：不属于本卡范围，且 -D warnings 下会拦住整条检查链 ⇒ 已在卡内汇报，交下游）

cargo test --lib -j 1（ERP_DB_URL=cloudpivot_so41，全量）
→ 3067 passed / 6 failed / 1208.98s
```

**6 条失败逐条归因（都不是本卡代码，且都已在干净库上复跑为绿）**：

| 用例 | 归因 | 复跑结果 |
|---|---|---|
| `receivable_aging::db_bucket_boundaries_and_backet_source_agree`<br>`receivable_aging::db_customer_pivot_and_summary_are_consistent`<br>`receivable_aging::db_period_cutoff_excludes_later_documents` | 该模块 `setup()` 里 `DELETE FROM biz_receivable`（全表），被**他人遗留夹具** `biz_receipt_refund_allocation`（RR-20261107-000004，07:58:48 由 receipt_refund 用例留下、未清场）的 FK `RESTRICT` 拦住 ⇒ `db error` | 干净库 `cloudpivot_so41_reg`（247 迁移）：9/9 绿 |
| `ledger::p2d1_ledger_excludes_draft` | 断言「全库 DRAFT 凭证恰好 1 张」，并行线程下别人的 DRAFT 让它数成 2 | 单独跑 1/1 绿 |
| `stocktake_tests::zz_teardown_purges_stocktake_fixtures` | 清场断言受并行模块中间态影响（该模块自带 `settle_orphans` 收敛窗口，5s 内未收敛） | 单独跑 1/1 绿 |
| `sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | 审批链夹具与并行模块共库互扰 | 干净库 4/4 绿 |

即：本卡改动的所有模块（`fx_revaluation` / `voucher` / `invoice` / `period`）在全量与定向运行下都绿；
失败集中在**既有夹具隔离**问题（父卡 SO-40 也记过同类 2 条），本卡未增删这些模块的任何夹具。

## 7. 残留项（明确不做，不是漏了）

1. **核销时点的汇兑损益**：本卡只做「月末重估」。收款按原币核销、不记结算汇率，
   所以「收到钱那一刻的汇率差」不会单独确认，而是并入下个月的月度重估差额
   （账面人民币随未核销比例走）。逐笔配对法属另一张卡。
2. **正负相抵（net = 0）时不出凭证**：单笔差额一正一负、合计为 0 时，1122 合计正确但单笔应收的
   账面人民币仍偏差（凭证分录无应收辅助核算列，无法按笔记账）。台账已逐笔留痕（`voucher_id NULL`），
   可人工按台账调账。
3. **反结账不回滚重估**：`reopen_period` 不删重估凭证 / 台账（凭证是账务事实）。
   反结账后再结账不会重复计提（diff 自然趋 0 + 幂等闸）；要重提必须走凭证红冲。
4. **退货 `biz_sales_return` / 采购链外币**未纳入重估范围（卡面只点名应收侧）。
5. **汇率维护 API / 前端页面**仍未做（SO-39 只给表；本卡证据用 SQL 种 SPOT）。
6. `close_check`（结账预检）**不展示**待重估清单：重估在 `close_period` 内完成，
   前端目前看不到「这次结账会提多少汇兑损益」。要预览需再加一个只读接口（本卡未做，单卡 ≤1.5h）。
