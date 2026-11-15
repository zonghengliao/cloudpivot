-- ======================================================
-- V0160：补种无来源库存变动所需的两个会计科目（`1901` / `6401`）
--
-- ── 为什么必须补种（实测口径）────────────────────────────
-- `POST /business/erp/serial/batch-generate` 自 ADR-20261031 起改判为
-- 「无来源入库（期初 / 其他入库）」：成本账加金额的**同一个事务**里必须出一张凭证，
-- 否则 `scripts/verify/erp_invariants.py` 的 B2（BLOCKER，`:201-212`）会当场红：
-- `Σ(biz_inventory.amount)` 与 `Σ(1405 借 − 贷)` 的容差只有 0.0001。
--
-- 三个场景的科目（ADR §6 R2 已由总管批准，规格 v3 §3.10 / §5.3）：
--   * 生成（期初 / 其他入库）：借 `1405 库存商品` / 贷 `1901 待处理财产损溢`；
--   * 出库（序列号发货）    ：借 `6401 主营业务成本` / 贷 `1405 库存商品`；
--   * 报废（序列号报废）    ：借 `1901 待处理财产损溢` / 贷 `1405 库存商品`。
--
-- 而库内现有科目只有 5 个（`select account_code from biz_account` 实测）：
--   1002 银行存款 | 1405 库存商品 | 2202 应付账款 | 2221 应交税费(非末级) | 2221.01 应交税费-进项税
-- 缺 `1901` / `6401` 就写不出分录。本迁移只种这两行，**不改任何既有科目**。
--
-- ── 为什么贷方是 1901 不是 4104 ─────────────────────────
-- `1901 待处理财产损溢` 是《企业会计准则》对盘盈存货的标准过渡科目，无来源入库 = 期初盘盈；
-- 报废损失的借方也用它 —— 一个科目同时覆盖两个「无来源」方向，且不污染利润分配口径。
--
-- ── 表结构 0 DDL ───────────────────────────────────────
-- `biz_inventory_txn` 的 `unit_cost numeric(18,6)` / `amount numeric(18,4)` / 前后六列快照
-- （V0137:423-430）与 `biz_voucher` / `biz_voucher_entry`（V0149:42-109）都已存在，
-- 本卡**不新增任何列 / 表 / 索引**，只有本条种子数据。
--
-- ── 幂等 ───────────────────────────────────────────────
-- 两条 `INSERT … ON CONFLICT DO NOTHING`（不带冲突目标 ⇒ 撞主键或撞编码唯一索引都放过），
-- 自检块只断言「两行存在且是末级、启用、未删」，重复应用不会炸。
--
-- 依据：docs/技术方案-20261031-批量生成无来源入库.md §7.1、
--       docs/adr-20261031-erp-serial-batch-generate-semantics.md §3 / §6
-- ======================================================

INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status, remark)
VALUES ('d1370004-0000-4000-8000-000000000901', '1901', '待处理财产损溢', NULL, 1,
        'ASSET', 'DEBIT', 1, 1,
        'V0160 补种：无来源入库（期初盘盈）的贷方、报废损失的借方（ADR §6 R2）')
ON CONFLICT DO NOTHING;

INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status, remark)
VALUES ('d1370004-0000-4000-8000-000000000902', '6401', '主营业务成本', NULL, 1,
        'PROFIT_LOSS', 'DEBIT', 1, 1,
        'V0160 补种：序列号出库结转存货成本的借方（ADR §6 R2）')
ON CONFLICT DO NOTHING;

-- 自检：两条都必须存在、末级、启用、未删（否则凭证写不出来，B2 必红）
DO $$
DECLARE
    n INT;
BEGIN
    SELECT count(*) INTO n
      FROM biz_account
     WHERE account_code IN ('1901', '6401')
       AND is_leaf = 1
       AND status = 1
       AND deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0160 自检失败：1901/6401 科目应为 2 条末级启用行，实际 %', n;
    END IF;
END $$;
