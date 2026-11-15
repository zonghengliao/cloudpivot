-- ============================================================
-- V0139__erp_inventory_location.down.sql
-- 回滚 V0139：① 三列唯一索引退回两列（有冲突数据则中止）
--             ② 删 biz_inventory.location_id（含外键与库位反查索引）
--             ③ 删在途仓种子（被引用则中止）
--             ④ 零残留断言
--
-- ── 顺序为什么是「先索引、再列、最后种子仓」─────────────────────────────────
--   · 两列唯一索引必须**先**重建：它建立在 (material_id, warehouse_id) 上，
--     与 location_id 无关，先建先拦，后面的 DROP COLUMN 才不会让表长时间无唯一键保护；
--   · 在途仓放最后：`biz_inventory.warehouse_id` / `biz_material_serial.warehouse_id` /
--     `biz_stock_transfer.{from,to,transit}_warehouse_id` 都可能指向它（V0140 已先回滚，
--     但库存/序列号的行还在），被引用时必须让人先处理数据，而不是级联删掉在途库存。
--
-- ── 迁移器顺序（写清，别当 bug）───────────────────────────────────────────
--   迁移器按版本**逆序**回滚：V0142 → V0141 → **V0140** → **V0139** → V0138 → V0137。
--   所以本文件运行时 `biz_stock_transfer*` 三张表已经不存在；手工只回滚 V0139 时若
--   调拨表还在且引用了在途仓，③ 会明确报错（安全失败，不静默丢数据）。
--
-- ── 数据丢失说明（有意为之）───────────────────────────────────────────────
--   `biz_inventory.location_id` 一旦删掉，**库位维度的账面行会合并**：
--   能回滚的前提是「每个物料×仓库只有一行」，所以 ① 先做冲突检测；
--   有冲突 → 中止并列出 (物料, 仓库) 组合与其行数，由运维决定「合并数量」还是「先出库」。
--   库位级数量账 `biz_inventory_location` 不受影响（那张表归 V0137）。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────────
--   全用 `IF EXISTS` / `WHERE NOT EXISTS`，重复执行不报错。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

-- ---------- ① 安全阀：同物料同仓出现多行（多库位）就无法退回两列唯一 ----------
DO $$
DECLARE
    n_dup     INT;
    n_loc     BIGINT;
    report    TEXT;
BEGIN
    IF to_regclass('public.biz_inventory') IS NULL THEN
        RAISE NOTICE 'V0139 回滚：biz_inventory 不存在，跳过冲突检测';
        RETURN;
    END IF;

    SELECT count(*) INTO n_dup
      FROM (SELECT 1 FROM biz_inventory GROUP BY material_id, warehouse_id HAVING count(*) > 1) d;

    IF n_dup > 0 THEN
        SELECT string_agg(format('  · 物料 %s / 仓库 %s：%s 行', material_id, warehouse_id, c), E'\n')
          INTO report
          FROM (SELECT material_id, warehouse_id, count(*) AS c
                  FROM biz_inventory GROUP BY material_id, warehouse_id
                 HAVING count(*) > 1 LIMIT 20) t;
        RAISE EXCEPTION E'V0139 回滚中止：% 组 (物料, 仓库) 存在多库位库存行，无法退回两列唯一索引。\n'
                         '冲突明细（最多 20 条）：\n%s\n'
                         '处置方式：① 先把库位级行合并/清零（数量并入 location_id IS NULL 的汇总行）；'
                         '② 或保留 V0139 不回滚。', n_dup, report;
    END IF;

    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'biz_inventory'
                  AND column_name = 'location_id') THEN
        EXECUTE 'SELECT count(*) FROM biz_inventory WHERE location_id IS NOT NULL' INTO n_loc;
    ELSE
        n_loc := 0;   -- 列已删（重复执行 down 的幂等路径）
    END IF;
    RAISE NOTICE 'V0139 回滚：冲突检测通过（每组恰 1 行）；其中带库位的行 % 条，将随列一起丢失', n_loc;
END
$$;

-- ---------- ② 索引：三列 → 退回两列（原定义：uk_biz_inv_mat_wh (material_id, warehouse_id)）----------
DROP INDEX IF EXISTS uk_biz_inv_mat_wh_loc;
DROP INDEX IF EXISTS idx_biz_inv_location;

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_inv_mat_wh ON biz_inventory (material_id, warehouse_id);

-- ---------- ③ 删列（外键先删，再删列；idx_biz_inv_location 已在 ② 显式删掉）----------
ALTER TABLE biz_inventory DROP CONSTRAINT IF EXISTS fk_biz_inv_location_id;
ALTER TABLE biz_inventory DROP COLUMN IF EXISTS location_id;

-- ---------- ④ 删在途仓种子（只删本迁移插的那一行；被引用则中止）----------
DO $$
DECLARE
    v_wh   UUID := 'd1390001-0000-4000-8000-000000000001'::uuid;
    n_ref  BIGINT := 0;
    n_tmp  BIGINT;
BEGIN
    IF to_regclass('public.biz_warehouse') IS NULL THEN
        RAISE NOTICE 'V0139 回滚：biz_warehouse 不存在，跳过在途仓删除';
        RETURN;
    END IF;

    IF NOT EXISTS (SELECT 1 FROM biz_warehouse WHERE id = v_wh) THEN
        RAISE NOTICE 'V0139 回滚：在途仓不存在，跳过（幂等）';
        RETURN;
    END IF;

    IF to_regclass('public.biz_inventory') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_inventory WHERE warehouse_id = $1' INTO n_tmp USING v_wh;
        n_ref := n_ref + n_tmp;
    END IF;
    IF to_regclass('public.biz_material_serial') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_material_serial WHERE warehouse_id = $1' INTO n_tmp USING v_wh;
        n_ref := n_ref + n_tmp;
    END IF;
    IF to_regclass('public.biz_stock_transfer') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_stock_transfer
                  WHERE from_warehouse_id = $1 OR to_warehouse_id = $1
                     OR transit_warehouse_id = $1' INTO n_tmp USING v_wh;
        n_ref := n_ref + n_tmp;
    END IF;

    IF n_ref > 0 THEN
        RAISE EXCEPTION 'V0139 回滚中止：在途仓 WH-TRANSIT 仍被 % 行库存/序列号/调拨单引用，'
                        '请先清零在途库存（或保留 V0139 不回滚）', n_ref;
    END IF;

    DELETE FROM biz_warehouse WHERE id = v_wh AND warehouse_code = 'WH-TRANSIT';
    RAISE NOTICE 'V0139 回滚：在途仓 WH-TRANSIT 已删除（未被任何库存/序列号/调拨单引用）';
END
$$;

-- ---------- ⑤ 零残留断言 ----------
DO $$
DECLARE
    left_col INT;
    left_fk  INT;
    left_idx INT;
    left_wh  INT;
    ok_uk    INT;
BEGIN
    SELECT count(*) INTO left_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_inventory' AND column_name = 'location_id';
    IF left_col > 0 THEN
        RAISE EXCEPTION 'V0139 回滚失败：biz_inventory.location_id 仍存在';
    END IF;

    SELECT count(*) INTO left_fk
      FROM pg_constraint
     WHERE conname = 'fk_biz_inv_location_id' AND conrelid = 'public.biz_inventory'::regclass;
    IF left_fk > 0 THEN
        RAISE EXCEPTION 'V0139 回滚失败：外键 fk_biz_inv_location_id 仍存在';
    END IF;

    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_inv_mat_wh_loc','idx_biz_inv_location');
    IF left_idx > 0 THEN
        RAISE EXCEPTION 'V0139 回滚失败：仍残留 % 个本迁移建的索引', left_idx;
    END IF;

    SELECT count(*) INTO ok_uk
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_inv_mat_wh'
       AND indexdef LIKE '%(material_id, warehouse_id)%';
    IF ok_uk <> 1 THEN
        RAISE EXCEPTION 'V0139 回滚失败：两列唯一索引 uk_biz_inv_mat_wh 未恢复';
    END IF;

    SELECT count(*) INTO left_wh
      FROM biz_warehouse WHERE id = 'd1390001-0000-4000-8000-000000000001'::uuid;
    IF left_wh > 0 THEN
        RAISE EXCEPTION 'V0139 回滚失败：在途仓种子行仍存在';
    END IF;

    RAISE NOTICE 'V0139 回滚完成：三列索引 → 两列索引已恢复 / location_id 与 FK 已删 / 在途仓已删 / 零残留';
END
$$;
