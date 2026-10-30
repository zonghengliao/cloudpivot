-- ============================================================
-- V0140__erp_stock_transfer.down.sql
-- 回滚 V0140：按依赖逆序 DROP 三张调拨表
--             serial（子）→ item（明细）→ transfer（主表）
--
-- ── 顺序为什么不能乱 ──────────────────────────────────────────────────────
--   `biz_stock_transfer_item_serial.item_id → biz_stock_transfer_item(id)`
--   `biz_stock_transfer_item.transfer_id     → biz_stock_transfer(id)`
--   `biz_stock_transfer_item_serial.serial_id → biz_material_serial(id)`（V0138 的表，本迁移不删）
--   反过来 DROP 主表会被 PG 的依赖保护挡住（安全失败），所以按子 → 父走。
--   卡面验收 ③ 的口径就是这条顺序。
--
-- ── 数据丢失说明（有意为之）────────────────────────────────────────────────
--   三张表都是本迁移新建的，DROP 即全丢。与 V0137 down 的「有数据就中止」不同：
--   卡面要求 down **真能回滚**，验收脚本就是带数据回滚这条路径。行数会打进 NOTICE 留痕。
--   想保留：回滚前 `COPY biz_stock_transfer TO …` 或走运维备份。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────────
--   全 `DROP TABLE IF EXISTS`，重复执行不报错。
--
-- ── 与 V0139 的关系 ──────────────────────────────────────────────────────
--   迁移器逆序回滚，本文件一定先于 `V0139__erp_inventory_location.down.sql` 执行；
--   手工只回滚 V0139 时若调拨表还在并引用了在途仓，V0139 down 会明确报错（不静默丢数据）。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

-- ---------- ① 先报行数留痕（表不存在则跳过）----------
DO $$
DECLARE
    tbl   TEXT;
    n     BIGINT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['biz_stock_transfer_item_serial',
                               'biz_stock_transfer_item',
                               'biz_stock_transfer'] LOOP
        CONTINUE WHEN to_regclass('public.' || tbl) IS NULL;
        EXECUTE format('SELECT count(*) FROM public.%I', tbl) INTO n;
        RAISE NOTICE 'V0140 回滚：DROP %（丢弃 % 行）', tbl, n;
    END LOOP;
END
$$;

-- ---------- ② 逆序 DROP ----------
DROP TABLE IF EXISTS biz_stock_transfer_item_serial;
DROP TABLE IF EXISTS biz_stock_transfer_item;
DROP TABLE IF EXISTS biz_stock_transfer;

-- ---------- ③ 零残留断言（表 + 索引 + 约束一起走）----------
DO $$
DECLARE
    left_tbl INT;
    left_idx INT;
    left_ck  INT;
BEGIN
    SELECT count(*) INTO left_tbl
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public' AND c.relkind = 'r'
       AND c.relname IN ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial');
    IF left_tbl > 0 THEN
        RAISE EXCEPTION 'V0140 回滚失败：仍残留 % 张调拨表', left_tbl;
    END IF;

    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_stock_transfer_no','idx_stock_transfer_status','idx_stock_transfer_from_wh',
                         'idx_stock_transfer_to_wh','idx_stock_transfer_approval','idx_transfer_item_transfer',
                         'idx_transfer_item_material','uk_transfer_item_serial','idx_transfer_item_serial_serial');
    IF left_idx > 0 THEN
        RAISE EXCEPTION 'V0140 回滚失败：仍残留 % 个本迁移建的索引', left_idx;
    END IF;

    SELECT count(*) INTO left_ck
      FROM pg_constraint
     WHERE conname IN ('ck_transfer_status','ck_transfer_diff_wh','ck_transfer_item_qty',
                       'ck_biz_transfer_state','ck_biz_transfer_item_ship','ck_biz_transfer_item_recv',
                       'ck_biz_transfer_total','ck_biz_transfer_item_line','ck_biz_transfer_item_cost',
                       'uk_transfer_item_serial');
    IF left_ck > 0 THEN
        RAISE EXCEPTION 'V0140 回滚失败：仍残留 % 个本迁移建的约束', left_ck;
    END IF;

    -- V0138 的序列号表必须**还在**（本迁移只引用它，不负责删）
    IF to_regclass('public.biz_material_serial') IS NULL THEN
        RAISE EXCEPTION 'V0140 回滚失败：biz_material_serial 不该被本迁移删掉（它归 V0138）';
    END IF;

    RAISE NOTICE 'V0140 回滚完成：3 张调拨表 / 9 个索引 / 10 个约束零残留（biz_material_serial 未受影响）';
END
$$;
