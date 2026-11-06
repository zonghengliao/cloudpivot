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
| `erp::fx_revaluation::tests`（6 纯逻辑 + 7 真库） | 13/13 |
| `so41_*`（4 例：2 纯逻辑 + 2 真库） | 4/4 |
| `erp::currency` / `erp::invoice` / `erp::period` / `erp::receipt` / `erp::receivable` / `erp::voucher::` | 129/129 |
| 全量 `cargo test --lib -j 1`（ERP_DB_URL 指向 cloudpivot_so41） | 见 §6 |

真库用例的期次命名空间：**2050-xx 归本模块独占**（2031=period、2033=remind/stocktake、2034–2035=opening、
2036–2038=year_end、2039=reminder、2040=period_items、2026=生产期间绝不碰），
并且与 `voucher_query::tests::DB_LOCK` 共用同一把锁（串行）。

## 6. fmt / clippy / 全量测试

（见卡内汇报与 `check.log`）
