-- ======================================================
-- V0309 down：up 的**真逆**（明细 → 结算单 → 规则 → 订单列/外键/索引）
--
-- 为什么是全量反向：down 必须能「up → down → up」第二轮跑通，
-- 只删一半会让第二轮测不到加列分支，回滚验证就成了空转。
--
-- ⚠ 不可逆点（生产回滚前先备份）：
--   · 删三表会丢掉已配的佣金规则；
--   · 删 biz_sales_order.salesperson_id 会丢掉「这张订单算在谁头上」这一事实
--     （SO-36 的计提依赖它 ⇒ 回滚本迁移后已生成的结算单无法复算）。
--     pg_dump -Fc -t biz_sales_order -t biz_commission_rule \
--       -t biz_commission_settlement -t biz_commission_item <db> \
--       > /backup/erp_before_v0309_rollback.dump
--
-- 幂等：全部 IF EXISTS，重复执行 no-op。
-- ======================================================

-- 1) 明细表先删（它挂 CASCADE 到结算单，先删免得依赖级联顺序）
DROP TABLE IF EXISTS biz_commission_item;

-- 2) 结算单（唯一索引 / 普通索引随表一起消失）
DROP TABLE IF EXISTS biz_commission_settlement;

-- 3) 规则
DROP TABLE IF EXISTS biz_commission_rule;

-- 4) 订单：索引 → 外键 → 列
DROP INDEX IF EXISTS idx_biz_sales_order_salesperson;
ALTER TABLE biz_sales_order DROP CONSTRAINT IF EXISTS fk_biz_sales_order_salesperson;
ALTER TABLE biz_sales_order DROP COLUMN IF EXISTS salesperson_id;

-- 5) 反向自检：结构必须回到 V0308 的样子
DO $$
DECLARE
    n INT;
BEGIN
    SELECT count(*) INTO n FROM pg_class c
      JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public'
       AND c.relname IN ('biz_commission_rule','biz_commission_settlement','biz_commission_item');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0309 down 自检失败：佣金表仍残留 % 张', n;
    END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order'
       AND column_name = 'salesperson_id';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0309 down 自检失败：salesperson_id 列仍残留';
    END IF;

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conname = 'fk_biz_sales_order_salesperson';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0309 down 自检失败：fk_biz_sales_order_salesperson 仍残留';
    END IF;

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'idx_biz_sales_order_salesperson';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0309 down 自检失败：idx_biz_sales_order_salesperson 仍残留';
    END IF;

    RAISE NOTICE 'V0309 down 完成：三表 + 销售员列/外键/索引已全部移除（订单行 % 张，一行未改）',
                 (SELECT count(*) FROM biz_sales_order);
END $$;
