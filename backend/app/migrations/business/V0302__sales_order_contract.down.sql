-- ======================================================
-- V0302 down：up 的**真逆**（索引 + 外键 + 列）
--
-- 为什么是全量反向：迁移文件里的 down 必须能「up → down → up」第二轮跑通，
-- 只删一半会让第二轮测不到加列分支，回滚验证就成了空转。
--
-- ⚠ 不可逆点：删列会丢掉「这批订单挂过哪张合同」这一事实。
--   生产回滚前先备份：
--     pg_dump -Fc -t biz_sales_order <db> > /backup/erp_before_v0302_rollback.dump
--
-- 幂等：全部 IF EXISTS，重复执行 no-op。
-- ======================================================

-- 1) 删索引
DROP INDEX IF EXISTS idx_biz_sales_order_contract;

-- 2) 删外键（先删约束再删列）
ALTER TABLE biz_sales_order DROP CONSTRAINT IF EXISTS fk_biz_sales_order_contract;

-- 3) 删列（COMMENT 随列一起消失）
ALTER TABLE biz_sales_order DROP COLUMN IF EXISTS contract_id;

-- 4) 反向自检：结构必须回到 V0301 的样子
DO $$
DECLARE
    col_count bigint;
    fk_count  bigint;
    idx_count bigint;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_name = 'biz_sales_order' AND column_name = 'contract_id';
    IF col_count <> 0 THEN
        RAISE EXCEPTION 'down 自检失败：contract_id 列仍残留';
    END IF;

    SELECT count(*) INTO fk_count
      FROM pg_constraint
     WHERE conname = 'fk_biz_sales_order_contract';
    IF fk_count <> 0 THEN
        RAISE EXCEPTION 'down 自检失败：fk_biz_sales_order_contract 仍残留';
    END IF;

    SELECT count(*) INTO idx_count
      FROM pg_indexes
     WHERE tablename = 'biz_sales_order' AND indexname = 'idx_biz_sales_order_contract';
    IF idx_count <> 0 THEN
        RAISE EXCEPTION 'down 自检失败：idx_biz_sales_order_contract 仍残留';
    END IF;

    RAISE NOTICE 'down 完成：列 / 外键 / 索引已全部移除，订单行一行未改（% 张）',
                 (SELECT count(*) FROM biz_sales_order);
END $$;
