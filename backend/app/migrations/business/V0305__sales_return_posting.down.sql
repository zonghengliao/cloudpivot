-- ======================================================
-- V0305 down：撤销退货过账的结构补丁（SO-30）
--
-- 顺序与 up 相反：先还原发票来源白名单，再删索引与两列。
-- ⚠ 若库里已经存在 source_type='SALES_RETURN' 的入库单，删列会连业务事实一起丢 ——
--   本文件先断言「没有退货入库单」，有就拒绝（down 是演练用的，不许静默毁数据）。
-- ======================================================

DO $$
DECLARE
    n INTEGER;
BEGIN
    SELECT count(*) INTO n FROM biz_stock_in WHERE source_type = 'SALES_RETURN';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0305 down 拒绝执行：已有 % 张退货入库单（source_type=SALES_RETURN），删列会丢业务事实', n;
    END IF;
    -- 红字发票同样依赖本迁移补的来源域值：白名单回退会让这些行违反 ck_biz_invoice_source
    SELECT count(*) INTO n FROM biz_invoice WHERE source_type = 'SALES_RETURN';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0305 down 拒绝执行：已有 % 张来源为 SALES_RETURN 的红字发票，回退白名单会让它们违约（先冲销 / 清理这些发票）', n;
    END IF;
END
$$;

ALTER TABLE biz_invoice DROP CONSTRAINT IF EXISTS ck_biz_invoice_source;
ALTER TABLE biz_invoice
    ADD CONSTRAINT ck_biz_invoice_source
    CHECK (source_type IS NULL
        OR source_type IN ('SALES_ORDER','SHIPMENT','SALES_OUT','MANUAL'));

DROP INDEX IF EXISTS uk_biz_si_source_doc;
ALTER TABLE biz_stock_in DROP COLUMN IF EXISTS source_id;
ALTER TABLE biz_stock_in DROP COLUMN IF EXISTS source_type;
