# 单号取号口径（`biz_seq`）

> 本文件回答三个问题：**取号器是全局单行还是按单据类型分行**、**跳号算不算 bug**、
> **怎么核对有没有重号**。卡面（FIX-8 / t_6ddb0036）问题 1 的结论就写在这里，
> 代码里的执行断言见 `backend/src/business/erp/voucher.rs` 的
> `fix8_concurrent_takes_never_duplicate` / `fix8_seq_types_advance_independently`。

## 1. 结论（先看这段）

| 问题 | 结论 | 依据 |
|------|------|------|
| 取号器是全局单行吗 | **不是**。`biz_seq` 一个 `seq_type`（单据类型）一行 | `V0137 §1.1` 建表 + `uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL`；145 实测 25 行，`TK` / `STOCK_TAKE` / `PO` / `IN` / `STOCK_OUT` 各占一行 |
| 调拨单号跳到 `TK-20261107-000093` 算不算 bug | **不算**。ERP 允许跳号，只禁止重号 | 145 库 `biz_stock_transfer` 只有 `TK-20261107-000001` 与 `TK-20261107-000093` 两张单，而 `biz_seq(TK).current_no = 98` —— 中间 96 个号被并行的单测 / 演练取走（很多随后回滚或删单） |
| 会有重号吗 | 不会，除非有人把「单语句原子取号」改成「先查再加一」 | 取号是一条 `UPDATE … RETURNING`（`voucher::bump_seq`）：PG 行锁天然串行化，第二个事务拿到的是提交后的新值 |
| 并发 10 个取号安全吗 | 安全，**号码两两不同**、计数器正好前进 10 | 单测 `fix8_concurrent_takes_never_duplicate`（10 个并发事务）+ 145 真机 10 个并发 HTTP 建单（`docs/evidence/20261108-fix8-seq-and-stocktake/`） |

## 2. 表结构（`V0137__erp_master_data_and_warehouse.up.sql §1.1`）

```sql
CREATE TABLE biz_seq (
    seq_type     VARCHAR(32) NOT NULL,   -- 取号键 = 单据类型（PO/IN/TK/STOCK_OUT/STOCK_TAKE/…）
    prefix       VARCHAR(12) NOT NULL,   -- 单号前缀（可与 seq_type 不同：CT 的前缀是 HT）
    date_pattern VARCHAR(8)  NOT NULL,   -- yyyyMMdd / yyyyMM / none（当前全部 yyyyMMdd）
    period_key   VARCHAR(8)  NOT NULL,   -- 当前计数周期键；换天 → 流水归零
    current_no   BIGINT      NOT NULL,   -- 已用最大流水号
    step         INT         NOT NULL,   -- 步长（批量取连号用）
    separator    VARCHAR(4)  NOT NULL,
    status       SMALLINT    NOT NULL,
    deleted_at   TIMESTAMPTZ
);
CREATE UNIQUE INDEX uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL;
```

**关键点**：`seq_type` 唯一 ⇒ 一张单一种号段，`PO` 取号永远不会推进 `TK`。
不存在「所有单据共用一个计数器」的实现，也不存在按 `(doc_type, period)` 再分行的必要
（周期滚动已经是行内 `period_key` 的职责，换天归零而不是开新行）。

## 3. 号段清单（145 测试库 `cloudpivot_dev` 实测 `seq_name`，25 行）

| `seq_type` | 前缀 | `seq_name`（单据） | 取号入口 |
|-----------|------|-------------------|----------|
| `PO` | `PO` | 采购订单号 | `purchase_model::next_order_no` |
| `IN` | `IN` | 收货入库单号 | `stockin_base::next_in_no` |
| `STOCK_OUT` | `OUT` | 出库单号 | `stockout_base` |
| `TK` | `TK` | 库存调拨单号 | `transfer_base::next_transfer_no` |
| `STOCK_TAKE` | `ST` | 盘点单号 | `stocktake_model::next_take_no` |
| `PZ` | `PZ` | 凭证号 | `voucher::next_voucher_no` |
| `PZR`（**惰性建行**） | `PZR` | 红字凭证号（与 `PZ` 独立号段） | `voucher::next_red_voucher_no` |
| `SN` | `SN` | 单品序列号（批量取连号 `current_no + step*N`） | `serial_model` |
| `AP` | `AP` | 应付单号 | `payable::next_payable_no` |
| `PM` | `PM` | 付款单号 | `payable_payment` |
| `AR` | `AR` | 应收单号 | `receivable::next_receivable_no` |
| `CT` | `HT` | 合同号（**前缀与 `seq_type` 不同**） | `contract_model::next_contract_no` |
| `SH` | `SH` | 发货单号 | `shipment_model` |
| `SO` | `SO` | 销售订单号 | `sales_model::next_so_no` |
| `SR` | `SR` | 销售退货单号 | `sales_return_model` |
| `RC` | `RC` | 收款单号 | `receipt` |
| `RR` | `RR` | 收款退款单号 | `receipt_refund` |
| `IV` | `IV` | 销售发票号（手填税控发票号时不取号） | `invoice` |
| `EXP` | `EXP` | 报销单号 | `expense_model::SEQ` |
| `EXP_PAY` | `PAY-EXP` | 报销付款单号 | `expense_model::PAY_SEQ` |
| `BDG` | `BDG` | 预算单号 | `budget_model::next_budget_no` |
| `CM` | `CM` | 佣金结算单号（**按月** `yyyyMM` 滚动） | `commission_model::next_settlement_no` |
| `FA` | `FA` | 固定资产编号 | `asset_model::next_asset_no` |
| `DEP` | `DEP` | 折旧批次号 | `depreciation_model` |
| `SFC` | `SFC` | 销售预测编号 | `sales_target_model` |
| `STG` | `STG` | 销售目标编号 | `sales_target_model` |

> `PZR` 在空库 / 没开过红冲的库里**还没有行**：首次 `next_red_voucher_no` 时
> `ensure_seq_row` 才补一行（幂等，并发唯一键冲突视为已存在）。这正说明「缺行不是错」——
> 取号入口自己会补。
>
> 查全量真值：`SELECT seq_type, prefix, seq_name, period_key, current_no FROM biz_seq WHERE deleted_at IS NULL ORDER BY seq_type;`
> 表里 26 个 `seq_type`（含惰性的 `PZR`），145 当前实存 25 行。

## 4. 取号语义（`voucher::next_doc_no`，所有单据共用）

```text
period_key = CST(now) 的 yyyyMMdd
ensure_seq_row(seq_type)                     -- 行不存在就幂等补一行（唯一键冲突视为已存在）
UPDATE biz_seq
   SET current_no = CASE WHEN period_key = ? THEN current_no + step      -- 同一周期：接着发
                         WHEN period_key < ? THEN step                   -- 换新一天：归零重来
                         ELSE current_no + step END                     -- 周期回退（时钟回拨）：绝不归零
     , period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END
 WHERE seq_type = ? AND deleted_at IS NULL
 RETURNING current_no                        -- 单语句：取号与占号同一个原子动作
```

三条设计约束：

1. **原子**：取号与占号是同一条语句。改成「`SELECT current_no` → 算 → `UPDATE`」就会在并发下重号。
2. **不许回退**：`period_key > 本次`（NTP 回拨 / 单测假日期）时**继续累加**，不归零 ——
   归零会把当天已发出的号整段撞掉（W2b 实测踩过）。
3. **失败不落号**：取号在业务事务内，业务回滚时号也回滚（这次不占号）；已提交再作废的单据
   则永久占号 —— 这就是「跳号」的唯一来源。

## 5. 跳号 vs 重号

| 现象 | 判定 | 处理 |
|------|------|------|
| 号不连续（`TK-…-000001` 下一个是 `…-000093`） | **正常**。号被回滚 / 被删单 / 被并行的测试取走 | 不处理；审计按单查，不按号查 |
| 同一天同一号段的**同一张单号出现两次** | **事故**。说明取号退化成读-改-写，或 `biz_seq` 行被软删后重建 | 立刻停写、按下面的巡检 SQL 定位、修实现而不是改数据 |
| `current_no` 比库内最大单号还大很多 | 正常（跳号），但**多半是测试在共享库上跑过** | 见 §6「测试污染」 |

**处置口径（本卡钉死）**：跳号不修、不清理、不手工往下调 `current_no`。
手工把 `current_no` 调小 = 下一次取号必然撞上已存在的单号（真·重号）。
真要收敛水位只能**往上对齐**：

```sql
-- 只读：把某号段的水位对齐到「库内实际最大流水号」，绝不往下调
-- （先在业务表算出该日最大流水号，再 GREATEST 进去）
UPDATE biz_seq s
   SET current_no = GREATEST(s.current_no, <该日库内最大流水号>)
 WHERE s.seq_type = 'TK' AND s.deleted_at IS NULL;
```

## 6. 巡检（只读，随时可跑）

### 6.1 重号扫描（真正的不变量：同一单据表内单号唯一）

```sql
-- 逐表找重号：对每个 biz_* 表里名字以 _no 结尾的 varchar 列做 GROUP BY … HAVING count(*) > 1
-- （表名/列名从 information_schema 现取，新增单据自动纳入；脚本实现见
--   docs/evidence/20261108-fix8-seq-and-stocktake/20_seq_e2e.py 的 dup_scan()）
SELECT c.table_name, c.column_name
  FROM information_schema.columns c
  JOIN information_schema.tables t ON t.table_name = c.table_name AND t.table_schema = 'public'
 WHERE c.table_schema = 'public' AND t.table_name LIKE 'biz\_%'
   AND c.column_name LIKE '%\_no' AND c.data_type LIKE 'character%'
 ORDER BY 1, 2;
```

### 6.2 水位 vs 实际单号（判断「是不是测试污染」）

```sql
-- 以调拨单为例：水位 98，库内只有 2 张单（1 和 93）→ 中间 96 个号被回滚/删单吃掉
SELECT s.seq_type, s.period_key, s.current_no,
       (SELECT count(*) FROM biz_stock_transfer t
         WHERE t.transfer_no LIKE 'TK-' || s.period_key || '-%') AS docs_in_db,
       (SELECT max(split_part(t.transfer_no, '-', 3)::bigint) FROM biz_stock_transfer t
         WHERE t.transfer_no LIKE 'TK-' || s.period_key || '-%') AS max_no_in_db
  FROM biz_seq s WHERE s.seq_type = 'TK' AND s.deleted_at IS NULL;
```

### 6.3 测试污染的处理

共享测试库（145 `cloudpivot_dev`）上跑真库用例 / 演练会推进 `biz_seq`，这是**预期行为**：
跳过号的单据不存在，取号器不会重发已提交过的号，所以没有数据风险。要干净只能换库
（建拷贝库跑，本卡与 FIX-5/6/7 都是这个做法），**不要**为了号段好看去改 `current_no`。

## 7. 新增一种单据要做什么

1. 定义 `SeqSpec { seq_type, prefix, seq_name, remark }`（`voucher.rs`），或在自己的 `*_base.rs` 里
   照 `stockin_base` / `transfer_base` 的 `ensure_seq_row` + `bump_seq` 写；
2. 建单事务里 `let no = next_doc_no(&tx, &SPEC, Utc::now()).await?;` —— **必须**在事务内取号；
3. 单号列加唯一约束（`uk_*_no`），不要把「不重号」只押在取号器上（人工改库也能被挡住）；
4. 不要复用别人的 `seq_type`：号段混用会让「按号段排查今天记了几张单」失效
   （红字凭证 `PZR` 与蓝字 `PZ` 分开就是这个原因）。
