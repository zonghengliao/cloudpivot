-- ============================================================
-- V0198 down：采购单绑定合同的**完整反向**（约束 + 索引 + 注释 + 四列）
--
-- 为什么是全量反向（而不是设计 §8.3 的「档 A：只删约束/索引、保留四列」）：
--   迁移文件里的 down 必须是 up 的**真逆**，否则「up → down → up」第二轮测不到加列分支，
--   回滚验证就成了空转。生产应急处置仍用档 A（无损、重上线即 no-op），写法见文件末的注释块。
--
-- ⚠ 不可逆点：`purchase_type` 区分「历史零星」与「曾经合同采购」依靠的就是这一列，
--   删列之前**必须**先 `pg_dump` 备份（档 B/C 的纪律，见设计 §8.3）：
--     pg_dump -Fc -t biz_purchase_order -t biz_stock_in <db> > /backup/erp_before_v0198_rollback.dump
--
-- 幂等：全部 `IF EXISTS` / existence-guard，重复执行 no-op。
-- ============================================================

-- 1) 删约束（先删引用这些列的 CHECK，再删列）
ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_contract_pair;
ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_purchase_type;
ALTER TABLE biz_stock_in        DROP CONSTRAINT IF EXISTS ck_biz_si_supplier_scope;

-- 2) 删索引
DROP INDEX IF EXISTS idx_biz_po_contract;

-- 3) 删列（COMMENT 随列一起消失，无需单独 DROP COMMENT）
ALTER TABLE biz_purchase_order
    DROP COLUMN IF EXISTS supplier_name_snapshot,
    DROP COLUMN IF EXISTS contract_no_snapshot,
    DROP COLUMN IF EXISTS purchase_type,
    DROP COLUMN IF EXISTS contract_id;

-- 4) 反向自检：结构必须回到 V0197 的样子
DO $$
DECLARE
    col_count bigint;
    con_count bigint;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_name = 'biz_purchase_order'
       AND column_name IN ('contract_id', 'purchase_type', 'contract_no_snapshot', 'supplier_name_snapshot');
    IF col_count <> 0 THEN
        RAISE EXCEPTION 'down 自检失败：仍残留 % 个合同相关列', col_count;
    END IF;

    SELECT count(*) INTO con_count
      FROM pg_constraint
     WHERE conname IN ('ck_biz_po_purchase_type', 'ck_biz_po_contract_pair', 'ck_biz_si_supplier_scope');
    IF con_count <> 0 THEN
        RAISE EXCEPTION 'down 自检失败：仍残留 % 个约束', con_count;
    END IF;

    RAISE NOTICE 'down 完成：四列 / 三约束 / 一索引已全部移除，业务数据一行未改（采购单 % 张）',
                 (SELECT count(*) FROM biz_purchase_order);
END $$;

-- ------------------------------------------------------------
-- 【生产应急档 A（无损）】只删约束与索引、保留四列 —— 回退二进制或约束误伤时用：
--
--   ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_contract_pair;
--   ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_purchase_type;
--   ALTER TABLE biz_stock_in        DROP CONSTRAINT IF EXISTS ck_biz_si_supplier_scope;
--   DROP INDEX IF EXISTS idx_biz_po_contract;
--   -- 四列保留：重新上线时 up 的 ADD COLUMN 是 no-op，约束重建也不会丢数据
-- ------------------------------------------------------------
