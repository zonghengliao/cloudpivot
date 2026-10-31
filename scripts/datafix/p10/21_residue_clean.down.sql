-- ============================================================================
-- P11-followup / 21-down：把最近一批未还原的通用夹具清理**原样写回**
-- ----------------------------------------------------------------------------
-- 快照来源：`public.datafix_residue_batch`（21_residue_clean.up.sql 删除前写入的整行 jsonb）。
-- 写回口径：
--   * 库位账 → 成本账 → 流水：按快照整行 INSERT（`jsonb_populate_record`，id / 时间戳 / 金额都用原值），
--     `ON CONFLICT DO NOTHING` 保证幂等（主键就是快照 id）。
--   * 序列号：只改回被 up 动过的三列（`deleted_at` / `updated_at` / `location_id`），
--     不再是「无条件置 NULL」——快照里本来就软删的行还原后仍是软删态。
-- 还原后回填 `restored_at`，同一批不会被回滚两次（第二遍 NOTICE 跳过）。
-- ============================================================================
DO $$
DECLARE
    v_batch uuid;
    v_note  text;
    v_inv   jsonb;
    v_loc   jsonb;
    v_txn   jsonb;
    v_ser   jsonb;
    n_inv   integer := 0;
    n_loc   integer := 0;
    n_txn   integer := 0;
    n_ser   integer := 0;
BEGIN
    IF to_regclass('public.datafix_residue_batch') IS NULL THEN
        RAISE NOTICE 'P10-21-down: 没有 datafix_residue_batch（本库没跑过通用清理），跳过';
        RETURN;
    END IF;

    SELECT batch_id, note, inventory, inventory_location, txn, serials
      INTO v_batch, v_note, v_inv, v_loc, v_txn, v_ser
      FROM public.datafix_residue_batch
     WHERE restored_at IS NULL
     ORDER BY run_at DESC
     LIMIT 1;

    IF v_batch IS NULL THEN
        RAISE NOTICE 'P10-21-down: 没有未还原的批次（幂等：第二遍 0 行），跳过';
        RETURN;
    END IF;

    -- ---------- ① 库位账（先写，成本账与序列号都指向它）----------
    INSERT INTO biz_inventory_location
    SELECT (jsonb_populate_record(NULL::biz_inventory_location, e)).*
      FROM jsonb_array_elements(v_loc) e
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS n_loc = ROW_COUNT;

    -- ---------- ② 成本账 ----------
    INSERT INTO biz_inventory
    SELECT (jsonb_populate_record(NULL::biz_inventory, e)).*
      FROM jsonb_array_elements(v_inv) e
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS n_inv = ROW_COUNT;

    -- ---------- ③ 流水 ----------
    INSERT INTO biz_inventory_txn
    SELECT (jsonb_populate_record(NULL::biz_inventory_txn, e)).*
      FROM jsonb_array_elements(v_txn) e
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS n_txn = ROW_COUNT;

    -- ---------- ④ 序列号：把 up 动过的三列改回快照值 ----------
    UPDATE biz_material_serial s
       SET deleted_at  = (e ->> 'deleted_at')::timestamptz,
           updated_at  = (e ->> 'updated_at')::timestamptz,
           location_id = (e ->> 'location_id')::uuid
      FROM jsonb_array_elements(v_ser) e
     WHERE s.id = (e ->> 'id')::uuid;
    GET DIAGNOSTICS n_ser = ROW_COUNT;

    UPDATE public.datafix_residue_batch SET restored_at = now() WHERE batch_id = v_batch;

    RAISE NOTICE 'P10-21-down: 已还原批次 % —— 库位账 % / 成本账 % / 流水 % / 序列号 % / 批次说明 %',
        v_batch, n_loc, n_inv, n_txn, n_ser, v_note;
END
$$;
