-- ============================================================
-- V0138__material_serial.down.sql
-- 回滚 V0138：① 删表 biz_material_serial（4 个索引随表走）② 删 biz_material 两列
--             ③ 删本迁移种入的 SN 取号规则 ④ 零残留断言
--
-- ── 顺序为什么是「先表后列」（卡面验收 ③）────────────────────────────────
-- `biz_material_serial.material_id` 有指向 `biz_material(id)` 的硬外键，
-- 所以必须先删子表、再动父表（虽然 DROP COLUMN 不重建父表，但按 FK 依赖序走最稳，
-- 也保证「万一将来有人在列上挂索引/视图」时顺序仍然正确）。
--
-- ── 数据库迁移器的执行顺序（写清，别当 bug）───────────────────────────────
-- 迁移器按版本**逆序**回滚：V0141 → V0140 → V0139 → **V0138** → V0137。
-- 也就是说本文件一定在 `V0137__erp_master_data_and_warehouse.down.sql` 之前跑（
-- 那个文件会 DROP `biz_material` / `biz_warehouse` / `biz_inventory_location` 等）。
-- 所以：
--   · 手工只回滚 V0137 而不先回滚 V0138，会因本表的 3 条硬外键而 DROP TABLE 失败
--     （PG 的 dependency 保护，属于「安全失败」：不会静默丢数据）；
--   · 反过来「单独回滚 V0138、保留 V0137」是**受支持**的：删完本表与两列后，
--     `biz_material` 回到 V0137 建它时的形态，其它表不受影响。
--
-- ── 数据丢失说明（有意为之）──────────────────────────────────────────────
-- `biz_material_serial` 是一物一码的身份账本，DROP 即全丢；`serial_required` 的标记也一起丢。
-- 想保留，请在回滚前 `COPY biz_material_serial TO …` 或走运维备份流程 ——
-- 本文件**不做**「有数据就中止」的安全阀（与 V0137 不同）：卡面要求 down 必须能完整回滚，
-- 而验收脚本就是「带数据回滚」这一条路径。行数会打在 NOTICE 里留痕。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────────
-- 全用 `DROP … IF EXISTS`，重复执行不报错（卡面验收 ② 的 down 侧）。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

-- ---------- ① 删表：先报行数留痕，再 DROP（索引/约束随表一起消失）----------
DO $$
DECLARE
    n_ser BIGINT := 0;
    has   BOOLEAN;
BEGIN
    SELECT EXISTS (SELECT 1
                     FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
                    WHERE ns.nspname = 'public' AND c.relkind = 'r'
                      AND c.relname = 'biz_material_serial') INTO has;
    IF has THEN
        EXECUTE 'SELECT count(*) FROM biz_material_serial' INTO n_ser;
        RAISE NOTICE 'V0138 回滚：DROP biz_material_serial（丢弃 % 行单品序列号）', n_ser;
    ELSE
        RAISE NOTICE 'V0138 回滚：biz_material_serial 不存在，跳过 DROP（幂等）';
    END IF;
END
$$;

DROP TABLE IF EXISTS biz_material_serial;

-- ---------- ② 删 biz_material 两列（先删索引所在的 serial_required 也行：DROP COLUMN 会级联删掉依赖它的索引）----------
ALTER TABLE biz_material DROP COLUMN IF EXISTS serial_required;
ALTER TABLE biz_material DROP COLUMN IF EXISTS external_sn;

-- ---------- ③ 删本迁移种入的 SN 取号规则（按固定 UUID 认领，不动运维后加的行）----------
DELETE FROM biz_seq
 WHERE id = 'd1380000-0000-4000-8000-000000000001'::uuid
   AND seq_type = 'SN';

-- ---------- ④ 零残留断言：表 / 两列 / 索引 / SN 取号规则都必须消失 ----------
DO $$
DECLARE
    left_tbl INT;
    left_col INT;
    left_idx INT;
    left_seq INT;
BEGIN
    SELECT count(*) INTO left_tbl
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public' AND c.relkind = 'r' AND c.relname = 'biz_material_serial';
    IF left_tbl > 0 THEN
        RAISE EXCEPTION 'V0138 回滚失败：biz_material_serial 仍存在';
    END IF;

    SELECT count(*) INTO left_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_material'
       AND column_name IN ('serial_required','external_sn');
    IF left_col > 0 THEN
        RAISE EXCEPTION 'V0138 回滚失败：biz_material 仍残留 % 个本迁移新增的列', left_col;
    END IF;

    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_material_serial_no','idx_biz_material_serial_material',
                         'idx_biz_material_serial_wh_loc','idx_biz_material_serial_status',
                         'idx_biz_material_serial_req');
    IF left_idx > 0 THEN
        RAISE EXCEPTION 'V0138 回滚失败：仍残留 % 个本迁移建的索引', left_idx;
    END IF;

    SELECT count(*) INTO left_seq
      FROM biz_seq WHERE id = 'd1380000-0000-4000-8000-000000000001'::uuid AND seq_type = 'SN';
    IF left_seq > 0 THEN
        RAISE EXCEPTION 'V0138 回滚失败：SN 取号规则行仍残留';
    END IF;

    RAISE NOTICE 'V0138 回滚完成：表 / 两列 / 5 个索引 / SN 取号规则全部零残留';
END
$$;
