-- ======================================================
-- V0270 down：up 的**真逆**（两张价格表 + biz_material.standard_price 列）
--
-- 为什么是全量反向：down 必须能「up → down → up」第二轮跑通，只删一半会让第二轮
-- 测不到建表/加列分支，回滚验证就成了空转。
--
-- ⚠ 不可逆点（数据丢失，回滚前必须备份）：
--   ① 两张价格表里的**全部价格配置**（客户协议价 / 阶梯促销价）随表消失；
--   ② biz_material.standard_price 里维护过的标准售价随列消失。
--   这两份数据都没有历史副本（不像单据有凭证/快照可重建），生产回滚前先：
--     pg_dump -Fc -t biz_customer_price -t biz_price_tier <db> > /backup/erp_before_v0270_price.dump
--     psql -c "\copy (SELECT id, material_code, standard_price FROM biz_material WHERE standard_price IS NOT NULL) TO '/backup/material_standard_price.csv' CSV HEADER"
--
-- 幂等：全部 IF EXISTS，重复执行 no-op。
-- ======================================================

-- 1) 先删子表（外键指向 biz_customer / biz_material，删除顺序不强制，但先子后父更直观）
DROP TABLE IF EXISTS biz_price_tier;
DROP TABLE IF EXISTS biz_customer_price;

-- 2) 删 biz_material.standard_price（先删约束再删列；约束随列删除也会消失，显式删更清晰）
ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_standard_price;
ALTER TABLE biz_material DROP COLUMN IF EXISTS standard_price;

-- 3) 反向自检：结构必须回到 V0270 之前的样子
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_customer_price') IS NOT NULL THEN
        RAISE EXCEPTION 'V0270 down 自检失败：biz_customer_price 表仍残留';
    END IF;
    IF to_regclass('public.biz_price_tier') IS NOT NULL THEN
        RAISE EXCEPTION 'V0270 down 自检失败：biz_price_tier 表仍残留';
    END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_material'
       AND column_name = 'standard_price';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0270 down 自检失败：biz_material.standard_price 列仍残留';
    END IF;

    SELECT count(*) INTO n FROM pg_constraint WHERE conname = 'ck_biz_material_standard_price';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0270 down 自检失败：ck_biz_material_standard_price 仍残留';
    END IF;

    SELECT count(*) INTO n FROM pg_class
     WHERE relname IN ('biz_customer_price','biz_price_tier') AND relkind = 'i';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0270 down 自检失败：价格表的索引仍有 % 个残留', n;
    END IF;

    RAISE NOTICE 'V0270 down 完成：两张价格表 + standard_price 列已移除（物料主数据 % 行一行未改）',
                 (SELECT count(*) FROM biz_material);
END $$;
