-- ============================================================
-- V0145__purchase_order_amounts_and_stock_in.down.sql
-- 回滚 V0145：收货两表整体撤销 + 采购两表删掉本迁移新增的列/约束
--
-- ── 顺序为什么不能乱（卡面验收 ③ 就是这条顺序）────────────────────────────
--   ① `biz_stock_in_item.stock_in_id → biz_stock_in(id) ON DELETE CASCADE` 是硬外键，
--      必须先 DROP 子表再 DROP 主表（反过来会被 PG 依赖保护挡住）。
--   ② 采购两表是**改造**而不是新建：只删掉本迁移加的东西
--        biz_purchase_order      ← price_mode（+ 依赖它的 ck_biz_po_price_mode，随列自动删）
--        biz_purchase_order_item ← price_incl
--        biz_purchase_order_item ← ck_biz_po_item_tax_rate（建在既有列 tax_rate 上，必须显式 DROP）
--      `total_amount / total_tax / total_with_tax / amount / price` 等是 V0031 时代的既有列，
--      **绝不动**（动了就等于把采购订单打回更早的形态，属于破坏性回滚）。
--
-- ── 数据丢失说明（有意为之）────────────────────────────────────────────────
--   收货两表是本迁移新建的，DROP 即全丢；本批还没有任何业务代码写入，实际损失为 0。
--   行数会打进 NOTICE 留痕；想保留就先 `COPY biz_stock_in TO …`。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────────
--   全 `DROP TABLE IF EXISTS` / `DROP COLUMN IF EXISTS` / `DROP CONSTRAINT IF EXISTS`，
--   重复执行不报错（验收 ② 的 down 反向场景）。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

-- ---------- ① 先报行数留痕（表不存在则跳过）----------
DO $$
DECLARE
    tbl TEXT;
    n   BIGINT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['biz_stock_in_item', 'biz_stock_in'] LOOP
        CONTINUE WHEN to_regclass('public.' || tbl) IS NULL;
        EXECUTE format('SELECT count(*) FROM public.%I', tbl) INTO n;
        RAISE NOTICE 'V0145 回滚：DROP %（丢弃 % 行）', tbl, n;
    END LOOP;
END
$$;

-- ---------- ② 收货两表：子 → 父 ----------
DROP TABLE IF EXISTS biz_stock_in_item;
DROP TABLE IF EXISTS biz_stock_in;

-- ---------- ③ 采购两表：删本迁移新增的列与约束 ----------
ALTER TABLE biz_purchase_order_item
    DROP CONSTRAINT IF EXISTS ck_biz_po_item_tax_rate;
ALTER TABLE biz_purchase_order_item
    DROP COLUMN IF EXISTS price_incl;

ALTER TABLE biz_purchase_order
    DROP CONSTRAINT IF EXISTS ck_biz_po_price_mode;
ALTER TABLE biz_purchase_order
    DROP COLUMN IF EXISTS price_mode;

-- ---------- ④ 零残留断言 ----------
DO $$
DECLARE
    left_tbl INT;
    left_idx INT;
    left_ck  INT;
    left_col INT;
    keep_col INT;
BEGIN
    -- 4.1 收货两表必须gone
    SELECT count(*) INTO left_tbl
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public' AND c.relkind = 'r'
       AND c.relname IN ('biz_stock_in', 'biz_stock_in_item');
    IF left_tbl > 0 THEN
        RAISE EXCEPTION 'V0145 回滚失败：仍残留 % 张收货表', left_tbl;
    END IF;

    -- 4.2 本迁移建的索引必须gone
    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_si_no', 'idx_biz_si_source', 'idx_biz_si_supplier', 'idx_biz_si_status',
                         'idx_biz_si_date', 'idx_biz_si_warehouse', 'idx_biz_si_voucher',
                         'idx_biz_si_item_in', 'idx_biz_si_item_material', 'idx_biz_si_item_src');
    IF left_idx > 0 THEN
        RAISE EXCEPTION 'V0145 回滚失败：仍残留 % 个本迁移建的索引', left_idx;
    END IF;

    -- 4.3 本迁移建的约束必须gone
    SELECT count(*) INTO left_ck
      FROM pg_constraint
     WHERE conname IN ('ck_biz_po_price_mode', 'ck_biz_po_item_tax_rate',
                       'ck_biz_si_status', 'ck_biz_si_type', 'ck_biz_si_tax', 'ck_biz_si_qty',
                       'ck_biz_si_supplier', 'ck_biz_si_item_qty', 'ck_biz_si_item_tax',
                       'ck_biz_si_item_rate', 'uk_biz_si_item_line');
    IF left_ck > 0 THEN
        RAISE EXCEPTION 'V0145 回滚失败：仍残留 % 个本迁移建的约束', left_ck;
    END IF;

    -- 4.4 新增列必须gone
    SELECT count(*) INTO left_col
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_purchase_order'      AND column_name = 'price_mode')
         OR (table_name = 'biz_purchase_order_item' AND column_name = 'price_incl'));
    IF left_col > 0 THEN
        RAISE EXCEPTION 'V0145 回滚失败：仍残留 % 个新增列', left_col;
    END IF;

    -- 4.5 既有列必须**还在**（防「回滚顺手把采购订单打回 V0031 之前」）
    SELECT count(*) INTO keep_col
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_purchase_order'
             AND column_name IN ('order_no','supplier_id','order_date','status','total_qty',
                                 'total_amount','total_tax','total_with_tax'))
         OR (table_name = 'biz_purchase_order_item'
             AND column_name IN ('order_id','line_no','material_id','unit_id','qty','price',
                                 'tax_rate','amount','tax_amount','with_tax_amount')));
    IF keep_col <> 18 THEN
        RAISE EXCEPTION 'V0145 回滚失败：采购两表的既有列只剩 %/18 —— 回滚越界了', keep_col;
    END IF;

    RAISE NOTICE 'V0145 回滚完成：收货 2 表 / 10 索引 / 11 约束零残留，采购两表既有 18 列完好（total_amount/total_tax/total_with_tax 未动）';
END
$$;
