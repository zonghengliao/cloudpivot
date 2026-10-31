-- ============================================================================
-- P11-followup / 21：「批量生成零成本夹具」清理（**默认不跑**，只在显式清理时跑）
-- ----------------------------------------------------------------------------
-- 前置：先跑 `20_residue_report.up.sql` 生成 `datafix_residue_report`（本脚本按报告清单逐条处理，
--       并在删除前**再复核一遍判据**，报告之后状态变了的物料自动跳过）。
--
-- 处置口径（与 P10 的 10_ 完全一致，只是不绑定物料编码）：
--   * 序列号：**软删**（`deleted_at`）—— `biz_stock_transfer_item_serial.serial_id` 是硬外键，
--     物理删会撞 FK；软删后分页/扫码/在库件数一律按 `deleted_at IS NULL` 过滤，同号也能重发。
--     顺手把 `location_id` 置空：库位账行马上要被删，序列号上留着指针会挡删除（FK）。
--   * 成本账 / 库位账 / 流水：**物理删** —— 它们的起点是无单据的批量生成、价值从来没有账载，
--     只有删掉才能让 B1（库位账 = 成本账）、B4（流水累计 = 库存）、C3（有库存则成本非零）同时成立。
--   * **不动**：调拨单/单据与序列号链接行、`biz_seq` 号段（只前进不回退）。
--
-- 回滚：本脚本把删掉/改掉的行整行快照进 `public.datafix_residue_batch`（jsonb），
--       `21_residue_clean.down.sql` 按最近一批未还原的快照原样写回（id / 时间戳都用快照值）。
--
-- 幂等：报告为空 ⇒ 一行不动；第二遍跑报告仍为空 ⇒ 0 行。
-- 安全阀（任一命中该物料就跳过，不删）：有收货/采购明细、库存金额或均价非零、没有 SERIAL_BATCH 流水。
-- ============================================================================

CREATE TABLE IF NOT EXISTS public.datafix_residue_batch (
    batch_id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    run_at             timestamptz NOT NULL DEFAULT now(),
    note               text,
    materials          jsonb       NOT NULL,   -- [{material_id, material_code}]
    inventory          jsonb       NOT NULL,   -- 删掉的 biz_inventory 整行
    inventory_location jsonb       NOT NULL,   -- 删掉的 biz_inventory_location 整行
    txn                jsonb       NOT NULL,   -- 删掉的 biz_inventory_txn 整行
    serials            jsonb       NOT NULL,   -- 改动前的 biz_material_serial 整行
    restored_at        timestamptz             -- down 还原后回填，保证同一批不会被回滚两次
);

COMMENT ON TABLE public.datafix_residue_batch IS
    'P11-followup：通用夹具清理的整行快照（供 21_residue_clean.down.sql 精确回滚）';

DO $$
DECLARE
    r          record;
    n          integer;
    m_ser      integer;
    m_inv      integer;
    m_loc      integer;
    m_txn      integer;
    v_skip     integer := 0;
    v_done     integer := 0;
    v_mat      jsonb := '[]'::jsonb;
    v_inv      jsonb := '[]'::jsonb;
    v_loc      jsonb := '[]'::jsonb;
    v_txn      jsonb := '[]'::jsonb;
    v_ser      jsonb := '[]'::jsonb;
    b_inv      integer := 0;
    b_loc      integer := 0;
    b_txn      integer := 0;
    b_ser      integer := 0;
BEGIN
    IF to_regclass('public.datafix_residue_report') IS NULL THEN
        RAISE NOTICE 'P10-21: 没有 datafix_residue_report —— 先跑 20_residue_report.up.sql（本脚本未改任何数据）';
        RETURN;
    END IF;

    FOR r IN SELECT * FROM public.datafix_residue_report ORDER BY material_code LOOP
        -- ---------- 删除前复核（报告是快照，可能已过期）----------
        IF EXISTS (SELECT 1 FROM biz_stock_in_item si       WHERE si.material_id = r.material_id)
        OR EXISTS (SELECT 1 FROM biz_purchase_order_item pi WHERE pi.material_id = r.material_id)
        OR EXISTS (SELECT 1 FROM biz_inventory i
                    WHERE i.material_id = r.material_id AND (i.amount <> 0 OR i.avg_cost <> 0))
        OR NOT EXISTS (SELECT 1 FROM biz_inventory_txn t
                        WHERE t.material_id = r.material_id
                          AND t.biz_type = 'SERIAL_IN' AND t.doc_type = 'SERIAL_BATCH')
        THEN
            RAISE NOTICE 'P10-21: 跳过 % —— 报告之后状态已变（有收货/采购明细、或已有成本价值、或批量生成流水已不在）',
                r.material_code;
            v_skip := v_skip + 1;
            CONTINUE;
        END IF;

        -- ---------- ① 快照（整行 jsonb，回滚靠它）----------
        v_mat := v_mat || jsonb_build_array(
                     jsonb_build_object('material_id', r.material_id, 'material_code', r.material_code));
        v_inv := v_inv || coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM biz_inventory t
                                     WHERE t.material_id = r.material_id), '[]'::jsonb);
        v_loc := v_loc || coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM biz_inventory_location t
                                     WHERE t.material_id = r.material_id), '[]'::jsonb);
        v_txn := v_txn || coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM biz_inventory_txn t
                                     WHERE t.material_id = r.material_id), '[]'::jsonb);
        v_ser := v_ser || coalesce((SELECT jsonb_agg(to_jsonb(t)) FROM biz_material_serial t
                                     WHERE t.material_id = r.material_id), '[]'::jsonb);

        -- ---------- ② 序列号：软删 + 解除库位引用 ----------
        UPDATE biz_material_serial
           SET deleted_at = now(), updated_at = now(), location_id = NULL
         WHERE material_id = r.material_id AND deleted_at IS NULL;
        GET DIAGNOSTICS m_ser = ROW_COUNT;
        b_ser := b_ser + m_ser;

        -- ---------- ③ 成本账（先删，解掉对库位账的外键）----------
        DELETE FROM biz_inventory WHERE material_id = r.material_id;
        GET DIAGNOSTICS m_inv = ROW_COUNT;
        b_inv := b_inv + m_inv;

        -- ---------- ④ 库位账 ----------
        DELETE FROM biz_inventory_location WHERE material_id = r.material_id;
        GET DIAGNOSTICS m_loc = ROW_COUNT;
        b_loc := b_loc + m_loc;

        -- ---------- ⑤ 流水 ----------
        DELETE FROM biz_inventory_txn WHERE material_id = r.material_id;
        GET DIAGNOSTICS m_txn = ROW_COUNT;
        b_txn := b_txn + m_txn;

        RAISE NOTICE 'P10-21: 已清理 % —— 序列号软删 % / 流水 % / 库位账 % / 成本账 %',
            r.material_code, m_ser, m_txn, m_loc, m_inv;
        v_done := v_done + 1;
    END LOOP;

    IF v_done = 0 THEN
        RAISE NOTICE 'P10-21: 没有需要清理的夹具（跳过 % 个物料），本脚本未改任何数据', v_skip;
        RETURN;
    END IF;

    INSERT INTO public.datafix_residue_batch
        (note, materials, inventory, inventory_location, txn, serials)
    VALUES
        (format('P11-followup 通用夹具清理：%s 个物料 / 序列号软删 %s / 流水 %s / 库位账 %s / 成本账 %s',
                v_done, b_ser, b_txn, b_loc, b_inv),
         v_mat, v_inv, v_loc, v_txn, v_ser);

    RAISE NOTICE 'P10-21: 完成 —— 物料 % 个（跳过 % 个）/ 序列号软删 % / 流水 % / 库位账 % / 成本账 %；快照已存 datafix_residue_batch',
        v_done, v_skip, b_ser, b_txn, b_loc, b_inv;
END
$$;
