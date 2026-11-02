-- ============================================================
-- V0194 回滚（down）：ARCH-2 采购单↔合同勾稽 + 入库单供应商快照
--
-- 回滚分三档，默认执行「档 A」——它把结构恢复到 V0193 之后（即合同表在、采购/入库勾稽不在），
-- 且**不丢任何业务数据**。档 B / 档 C 会丢列数据，必须显式人工确认后再跑（本文件不自动执行）。
--
-- 档 A（默认，无损）：删本卡新增的约束与索引，保留四列（列还在 = 数据还在，重新上线时
--                     ADD COLUMN IF NOT EXISTS 直接 no-op，不需要重跑回填）
-- 档 B（列级回滚）：再删 contract_no_snapshot / supplier_name_snapshot / purchase_type ——
--                    **丢弃快照与类型**，执行前必须先 pg_dump 这两列
-- 档 C（完全回滚）：最后删 contract_id —— **丢弃合同关联**，执行前必须 pg_dump 该列
--
-- 顺序与环境：档 A 可在任意环境直接跑；档 B/C 只在「确认没有代码读这几列」的环境跑
--            （即二进制已回退到 V0194 之前的版本），否则 NIK 启动自检/查询会报缺列。
-- ============================================================

-- ---------- 档 A：约束 + 索引 ----------
DROP INDEX IF EXISTS idx_biz_si_no_source;
DROP INDEX IF EXISTS idx_biz_po_contract;

ALTER TABLE biz_stock_in       DROP CONSTRAINT IF EXISTS ck_biz_si_supplier_scope;
ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_contract_pair;
ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_purchase_type;

-- 注释回滚到 V0145 口径（这两列在 V0145 时没有 COMMENT，删掉即回到「无注释」）
COMMENT ON COLUMN biz_stock_in.source_order_id IS NULL;
COMMENT ON COLUMN biz_stock_in.supplier_id     IS NULL;
COMMENT ON COLUMN biz_purchase_order.contract_id            IS NULL;
COMMENT ON COLUMN biz_purchase_order.purchase_type          IS NULL;
COMMENT ON COLUMN biz_purchase_order.contract_no_snapshot   IS NULL;
COMMENT ON COLUMN biz_purchase_order.supplier_name_snapshot IS NULL;
COMMENT ON COLUMN biz_purchase_order.supplier_id            IS NULL;

-- ---------- 档 B：三列（有损，需人工确认后手工执行） ----------
-- ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS supplier_name_snapshot;
-- ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS contract_no_snapshot;
-- ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS purchase_type;

-- ---------- 档 C：合同关联（有损，需人工确认后手工执行） ----------
-- ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS contract_id;

-- ---------- 回滚自检：档 A 之后的状态必须满足 ----------
DO $$
DECLARE
    n_chk INT;
    n_idx INT;
    n_col INT;
BEGIN
    SELECT count(*) INTO n_chk FROM pg_constraint
     WHERE conname IN ('ck_biz_po_purchase_type','ck_biz_po_contract_pair','ck_biz_si_supplier_scope');
    IF n_chk <> 0 THEN
        RAISE EXCEPTION 'V0194 回滚失败：仍有 % 条本卡约束在位', n_chk;
    END IF;

    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname = 'public' AND indexname IN ('idx_biz_po_contract','idx_biz_si_no_source');
    IF n_idx <> 0 THEN
        RAISE EXCEPTION 'V0194 回滚失败：仍有 % 条本卡索引在位', n_idx;
    END IF;

    -- 档 A 的承诺：四列仍在（无损）；若这里变成 0，说明有人接着跑了档 B/C，属预期外的库形态
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_purchase_order'
       AND column_name IN ('contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot');
    IF n_col <> 4 THEN
        RAISE EXCEPTION 'V0194 回滚自检：期望档 A 后四列仍在，实际 % / 4（若已手工执行档 B/C 属预期）', n_col;
    END IF;

    RAISE NOTICE 'V0194 回滚完成（档 A）：约束与索引已删，四列保留（无损），业务数据一行未动';
END
$$;
