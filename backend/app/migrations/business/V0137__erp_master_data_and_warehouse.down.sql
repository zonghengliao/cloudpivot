-- ============================================================
-- V0137__erp_master_data_and_warehouse.down.sql
-- 回滚：13 张表（主数据 6 + 基线 5 + 库位 2）
--
-- ── 安全阀（卡面验收 ④）──────────────────────────────────────────────────
-- 任何一张表里有「不是本迁移种子」的行 → **中止**，逐表报出行数，一行都不删。
-- 只对本迁移自己插的种子行（固定 UUID）放行：它们是 V0137 的产物，跟着迁移一起走。
--
-- 为什么「有数据就中止」而不是 CASCADE：
--   仓库 / 物料 / 库存 / 流水 是 ERP 的账本根基。回滚一次静默丢掉 3200 条流水，
--   事后只能靠备份——所以宁可让运维先显式处理（导出或改迁移），也不做「贴心的自动清空」。
--   真要强删，走 `迁移前先 DELETE` 或直接 DROP SCHEMA 的运维流程，别在这里开口子。
--
-- ── 已知边界（写清，别当 bug）─────────────────────────────────────────────
-- ① down 会连 5 张基线表（biz_seq/biz_warehouse/biz_material/biz_inventory/biz_inventory_txn）
--    一起 DROP。它们本是批次1 V0030/V0032 的资产，本次由 V0137 代建（见 up 头注释迁移债）：
--    回滚 V0137 就等于撤销这次代建。若届时批次1 已接管这些表，请**先**回滚批次1 的迁移。
-- ② 数据库的迁移器按版本逆序回滚：V0138/V0139/V0140 必须先于 V0137 回滚
--    （它们的数据/权限行依赖这里建的表）。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

DO $$
DECLARE
    -- 本迁移插的种子行 id（up §5），down 时视为「自家产物」放行
    seeds UUID[] := ARRAY[
        'd1370000-0000-4000-8000-000000000001'::uuid,  -- biz_unit PCS
        'd1370000-0000-4000-8000-000000000002'::uuid,  -- biz_unit KG
        'd1370000-0000-4000-8000-000000000003'::uuid,  -- biz_unit BOX
        'd1370004-0000-4000-8000-000000000001'::uuid,  -- biz_account 1405
        'd1370004-0000-4000-8000-000000000002'::uuid,  -- biz_account 2202
        'd1370005-0000-4000-8000-000000000001'::uuid,  -- biz_supplier SUP-0001
        'd1370007-0000-4000-8000-000000000001'::uuid,  -- biz_accounting_period 2026-10
        'd1370006-0000-4000-8000-000000000001'::uuid,  -- biz_seq PO
        'd1370001-0000-4000-8000-000000000001'::uuid,  -- biz_warehouse WH01 总仓
        'd1370001-0000-4000-8000-000000000002'::uuid,  -- biz_warehouse WH02 分仓
        'd1370002-0000-4000-8000-000000000001'::uuid,  -- biz_location A-01-01
        'd1370002-0000-4000-8000-000000000002'::uuid,  -- biz_location A-01-02
        'd1370002-0000-4000-8000-000000000003'::uuid,  -- biz_location B-01-01
        'd1370002-0000-4000-8000-000000000004'::uuid,  -- biz_location RECV-01
        'd1370003-0000-4000-8000-000000000001'::uuid,  -- biz_material M-W1A-001
        'd1370003-0000-4000-8000-000000000002'::uuid,  -- biz_material M-W1A-002
        'd1370003-0000-4000-8000-000000000003'::uuid,  -- biz_material M-W1A-003
        'd1370003-0000-4000-8000-000000000004'::uuid,  -- biz_material M-W1A-004
        'd1370003-0000-4000-8000-000000000005'::uuid   -- biz_material M-W1A-005
    ];
    tbl      TEXT;
    n        BIGINT;
    total    BIGINT := 0;
    report   TEXT := '';
BEGIN
    -- 逐表统计「非种子行」。表不存在（半途失败/已被批次1 接管）就跳过，不因缺表而误报。
    FOREACH tbl IN ARRAY ARRAY[
        'biz_purchase_order_item','biz_purchase_order','biz_accounting_period','biz_supplier',
        'biz_account','biz_unit','biz_inventory_location','biz_location','biz_inventory_txn',
        'biz_inventory','biz_material','biz_warehouse','biz_seq'
    ] LOOP
        CONTINUE WHEN to_regclass('public.' || tbl) IS NULL;
        EXECUTE format('SELECT count(*) FROM public.%I WHERE NOT (id = ANY($1))', tbl)
           INTO n USING seeds;
        IF n > 0 THEN
            total  := total + n;
            report := report || format('  · %s：%s 行', tbl, n) || E'\n';
        END IF;
    END LOOP;

    IF total > 0 THEN
        RAISE EXCEPTION '%', format(
            E'V0137 回滚中止：表里有 %s 行业务数据（非本迁移种子），拒绝静默丢数据。\n'
            '逐表明细：\n%s'
            '处置方式：① 先备份/导出这些行；② 或先 DELETE 掉自己的业务数据；'
            '③ 确需整域重置时走运维流程（人工 DROP），不要靠迁移 down。',
            total, report);
    END IF;

    RAISE NOTICE 'V0137 回滚检查通过：13 张表里只有本迁移的种子行（19 行），无业务数据，可以 DROP';
END
$$;

-- 逆序 DROP（明细/子表先行；本迁移内唯一的外键是 po_item.order_id → po.id）
DROP TABLE IF EXISTS biz_purchase_order_item;
DROP TABLE IF EXISTS biz_inventory_location;
DROP TABLE IF EXISTS biz_location;
DROP TABLE IF EXISTS biz_purchase_order;
DROP TABLE IF EXISTS biz_inventory_txn;
DROP TABLE IF EXISTS biz_inventory;
DROP TABLE IF EXISTS biz_material;
DROP TABLE IF EXISTS biz_warehouse;
DROP TABLE IF EXISTS biz_seq;
DROP TABLE IF EXISTS biz_accounting_period;
DROP TABLE IF EXISTS biz_supplier;
DROP TABLE IF EXISTS biz_account;
DROP TABLE IF EXISTS biz_unit;

-- 零残留断言（回滚必须干净：表、约束、索引一起走）
DO $$
DECLARE n INT;
BEGIN
    SELECT count(*) INTO n
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public' AND c.relkind = 'r'
       AND c.relname IN ('biz_unit','biz_account','biz_supplier','biz_accounting_period',
                         'biz_purchase_order','biz_purchase_order_item',
                         'biz_seq','biz_warehouse','biz_material','biz_inventory','biz_inventory_txn',
                         'biz_location','biz_inventory_location');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0137 回滚失败：还剩 % 张表未删除', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_warehouse_path_pattern','idx_biz_warehouse_path','uk_biz_location_default',
                         'uk_biz_inv_loc','uk_biz_inv_mat_wh','uk_biz_unit_code','uk_biz_material_code',
                         'uk_biz_po_no','uk_biz_po_item_line','uk_biz_account_code','uk_biz_period_code',
                         'uk_biz_seq_type','uk_biz_warehouse_code','uk_biz_location_code','uk_biz_supplier_code');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0137 回滚失败：还剩 % 个索引未删除', n;
    END IF;

    RAISE NOTICE 'V0137 回滚完成：13 张表 + 全部索引零残留';
END
$$;
