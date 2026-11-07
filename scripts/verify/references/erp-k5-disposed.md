# K5 / K5b 固定资产口径：处置（DISPOSED）场景下 1602 该怎么对

> 归属：`scripts/verify/erp_invariants.py` 的 K 组（固资）。
> 起因：**t_c3c2793a**（REVERIFY t_2f92ee87 发现 ②）—— K5 卡片侧没排除 `DISPOSED`，
> 任何一次资产处置后必报 BLOCKER，假红挡住 `scripts/check.sh` 的发版闸③。

## 1. 结论（一句话）

**1602 的贷方余额只反映「在役资产」的累计折旧**；已处置卡片的累计折旧已经在处置凭证里被
「借 1602」冲掉，而卡片行**保留历史值**（不是清零）。所以 K5 的卡片侧必须
`status <> 'DISPOSED'`，与 K6（1601 vs 未处置原值）同口径。

## 2. 依据一：处置凭证的固定形状（代码契约，非约定俗成）

`backend/src/business/erp/asset_bridge.rs::post_disposal_vouchers`
→ `backend/src/business/erp/asset_compute.rs::disposal_vouchers`

三种处置（`SALE` / `SCRAP` / `LOSS`）的**第一张**凭证形状相同，只有中间科目不同：

| 处置方式 | 凭证 1（清账） | 之后的凭证 |
|---|---|---|
| 出售 SALE | 借 **1602 累计折旧** + 借 1606 净值 / 贷 1601 原值 | ② 收款（借 1002 / 贷 1606 + 贷 2221.02）③ 结转清理（5711 ↔ 1606） |
| 报废 SCRAP | 借 **1602 累计折旧** + 借 1606 净值 / 贷 1601 原值 | ② 结转（借 5711 / 贷 1606） |
| 盘亏 LOSS | 借 **1602 累计折旧** + 借 1901 净值 / 贷 1601 原值 | ② 结转（借 5711 / 贷 1901） |

要点：

* `借 1602 = amounts.accumulated`，即**卡片的累计折旧全额**（`accumulated > 0` 时才出这条分录）；
* 凭证来源链是确定的：`biz_voucher.source_type = 'FIXED_ASSET_DISPOSAL'` 且
  `source_id = biz_fixed_asset.id`（一个资产可能 1~3 张凭证，全部带同一个 `source_id`）；
* 作废处置（`asset_handler::asset_dispose_cancel`）走 `asset_bridge::reverse_disposal_vouchers`：
  给每张**蓝字**生成一张红字（`is_red = 1`），并把原蓝字置 `reversed_by`，卡片回 `IN_USE`。

## 3. 依据二：卡片行保留累计折旧（不能清零）

* `biz_fixed_asset.accumulated_depreciation` 是「折旧明细的缓存列」，K2 要求它
  `= Σ biz_depreciation(status='POSTED')`；
* 处置只**置状态**（`IN_USE|IDLE → DISPOSED`）+ 回填 `disposal_type/disposal_date/disposal_voucher_id`，
  **不删卡片、不清缓存列**（没有「过账删除」）；
* 所以「处置后把累计折旧清零」会让 K2 立刻变红 —— 那是错的口径。
  若产品将来改成「处置即结清卡片」，必须同时改 K2 判据与处置事务，不是本检查能单方面决定的。

## 4. K5 / K5b / K6 / K7 四路互证

| 检查 | 级别 | 判据 | 处置发生后为什么仍成立 |
|---|---|---|---|
| **K5** | BLOCKER | `1602` 贷方余额 = Σ 在役卡（`status <> 'DISPOSED'`）累计折旧 | 处置凭证借 1602 冲掉该卡累计折旧；卡片侧同时被排除 |
| **K5b** | BLOCKER | 每张 `DISPOSED` 卡：卡片累计折旧 = 它**未红冲的蓝字**处置凭证对 1602 的借方合计 | 直接盯着「处置有没有按卡片金额记账」，逐卡对账 |
| **K6** | BLOCKER | `1601` 借方余额 = Σ 未处置卡原值 | 凭证 1 贷 1601 冲掉原值，卡片侧排除 |
| **K7** | WARN | `1606`（清理）余额 = 0 | 收款 + 结转把 1606 结平（出售/报废） |

为什么「排除 DISPOSED」不是放水：

1. **处置没记账**（凭证没生成 / 生成失败）：1602 仍挂着这笔累计折旧，而卡片侧已被排除
   ⇒ K5 立即变红（K5b 同时红）。
2. **处置金额记错**：两张卡「一多一少」可能把总量对平，K5 看不出来 —— K5b 逐卡比，红。
3. **卡片被手工置 DISPOSED**（SQL 改状态、没有处置凭证）：K5b 红（`无 disposal_voucher_id`），
   K10（DISPOSAL 留痕）也会红。
4. **处置红冲后重处置**：K5b 只算 `is_red = 0 and reversed_by is null`，不会把两次借方重复累加。

## 5. 可重复的判定步骤（真库直查）

以 145 / `cloudpivot_rev_demo`（REVERIFY 留下的库，1 张 `DISPOSED` + 1 张 `IN_USE`）为例：

```bash
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD=$NIK_DATABASE_PASSWORD
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_rev_demo
```

① 逐笔看 16xx 凭证（账是对的）：

```sql
select v.voucher_no, e.account_code, e.debit_amount, e.credit_amount, e.summary
  from biz_voucher_entry e
  join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
 where e.account_code like '16%'
 order by 1, 2;
```

实测（2026-11-08）：

| 凭证 | 科目 | 借 | 贷 | 摘要 |
|---|---|---|---|---|
| PZ…000008 | 1601 | 120000 | 0 | FA-…0001 固定资产购入 |
| PZ…000009 | 1602 | 0 | 3166.6667 | 2026-11 计提折旧汇总 |
| PZ…000010 | 1601 / 1602 / 1606 | 0 / **3166.6667** / 116833.3333 | 120000 / 0 / 0 | 出售处置（1/3） |
| PZ…000011 | 1606 | 0 | 1000 | 出售处置（2/3） |
| PZ…000012 | 1606 | 0 | 115833.3333 | 出售处置（3/3） |
| PZ…000019 | 1601 | 120000 | 0 | FA-…0002 固定资产购入 |

② 三侧余额（口径必须分开算）：

```sql
-- 科目侧
select account_code, sum(debit_amount) d, sum(credit_amount) c
  from biz_voucher_entry e
  join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
 where account_code in ('1601','1602','1606') group by 1;
-- 1601: 借 240000 / 贷 120000 ⇒ 余额 120000      → K6：= 在役卡原值 120000 ✅
-- 1602: 借 3166.6667 / 贷 3166.6667 ⇒ 余额 0      → K5：= 在役卡累计 0 ✅
-- 1606: 借 116833.3333 / 贷 116833.3333 ⇒ 余额 0   → K7：归零 ✅

-- 卡片侧
select asset_no, status, original_value, accumulated_depreciation
  from biz_fixed_asset where deleted_at is null;
-- FA-…0001 DISPOSED 120000 3166.6667  → K5 排除、K6 排除；K5b 用它自己的处置凭证对 3166.6667
-- FA-…0002 IN_USE   120000 0         → 两条都算
```

③ 跑检查（K 组只有固资，跑全量也很快）：

```bash
python3 scripts/verify/erp_invariants.py --group 固资   # 或全量
```

期望：`K5` / `K5b` / `K6` 全绿，`BLOCKER 0`。

## 6. 负向验证（判据不许放水，做过且可复做）

都在**库的副本**上做（别拿证据库当靶子），副本用
`pg_dump … | psql … -d <copy>` 造，跑完 drop：

| # | 构造 | 期望 |
|---|---|---|
| N1 | 副本原样（含 1 张 DISPOSED） | K5 / K5b 绿，BLOCKER 0 |
| N2 | 把 PZ…000009 的 1602 贷方 +1000，同时把该凭证的对方科目借方 +1000（凭证仍平） | K5 **红**（1602 ≠ 在役卡累计），BLOCKER ≥ 1 |
| N3 | 删掉 PZ…000010～000012（= 处置没记账，卡片仍是 DISPOSED） | K5 **红** + K5b **红** |

N2/N3 的实测输出见 `docs/evidence/20261108-k5-disposed/README.md`。

## 7. 变更记录

* **t_c3c2793a**：K5 卡片侧补 `status <> 'DISPOSED'`；检查名改为
  「K5 1602 贷方余额 = Σ在役卡片累计折旧」；新增 K5b 逐卡对账（BLOCKER）；
  覆盖行打印在役/已处置卡片的张数与 Σ（排除多少一目了然）。
