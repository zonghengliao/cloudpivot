-- ======================================================
-- V0272 down：逆序撤销「币种 + 汇率表」（SO-39 · ARCH-7 §15.6.3）
--
--   ① 先删引用方 biz_exchange_rate（它对 biz_currency 有两条 ON DELETE RESTRICT 外键，
--      不先删就删不掉被引用表）
--   ② 再删被引用方 biz_currency
--
-- 顺序反了会直接撞 23001 restrict_violation，这是本 down 唯一的正确顺序。
--
-- ⚠ 数据丢失提示：本卡的两张表是**主数据**，回滚会丢掉全部汇率历史（月末重估的取数依据）。
--   生产上执行前先 `COPY (SELECT * FROM biz_exchange_rate) TO '/tmp/…csv' CSV HEADER`。
--   下游卡（SO-40）给单据加了 currency_code / exchange_rate 之后，单据上的**已锁定汇率**
--   不受本 down 影响（那是单据自己的列），回滚是安全的「只回到没有汇率表」的状态。
--
-- 幂等：DROP TABLE IF EXISTS，重复执行 no-op。
-- ======================================================

-- ---------- ① 引用方：汇率表 ----------
DO $$
DECLARE
    n bigint := 0;
BEGIN
    IF to_regclass('public.biz_exchange_rate') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_exchange_rate' INTO n;
        RAISE NOTICE 'SO-39 down: 删除 biz_exchange_rate（丢弃 % 行汇率历史）', n;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_exchange_rate;

-- ---------- ② 被引用方：币种表 ----------
DO $$
DECLARE
    n bigint := 0;
BEGIN
    IF to_regclass('public.biz_currency') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_currency' INTO n;
        RAISE NOTICE 'SO-39 down: 删除 biz_currency（丢弃 % 行币种主数据）', n;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_currency;

-- ---------- ③ 反向自检：两张表必须真的消失（索引/约束随表一并消失）----------
DO $$
BEGIN
    IF to_regclass('public.biz_currency') IS NOT NULL THEN
        RAISE EXCEPTION 'SO-39 down: biz_currency 仍然存在';
    END IF;
    IF to_regclass('public.biz_exchange_rate') IS NOT NULL THEN
        RAISE EXCEPTION 'SO-39 down: biz_exchange_rate 仍然存在';
    END IF;
    IF to_regclass('public.uk_biz_currency_single_base') IS NOT NULL
       OR to_regclass('public.uk_biz_exchange_rate') IS NOT NULL THEN
        RAISE EXCEPTION 'SO-39 down: 唯一索引残留（表的索引应随表删除）';
    END IF;
    RAISE NOTICE 'SO-39 down 自检通过：两张表与其索引/约束均已移除';
END $$;
