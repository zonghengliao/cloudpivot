-- ============================================================================
-- probe_design_v2.sql · 「批量生成 = 无来源入库」目标设计的**端到端真跑取证**
--
-- 目的：把《技术方案-20261031-批量生成无来源入库》§3/§5/§6 的三条决定
--   A1 生成 + 出库 + 报废都在同事务生成成本凭证（借 1405 / 贷 1901 或 6401）
--   A2 成本账与库位账都以「物料×仓库×biz_inventory_location 行」为桶（NULL = 仓级汇总桶）
--   A3 出库/报废侧按桶重算两本账（不是 ±1，也不是只写 NULL 桶）
-- 用**与 Rust 侧逐字同形的 SQL** 在真库上跑一遍完整生命周期，每一步都断言
-- 6 条巡检判据（B1/B2/B4/C3/C4/C5）全绿。
--
-- 跑法（本机 PG，库由 cloudpivot_test 克隆；本脚本全程在一个事务里，末尾 ROLLBACK，
-- 不改任何数据 —— 中途失败时 psql 断开也会自动回滚）：
--   su postgres -c "psql -c 'CREATE DATABASE arch_spec_v2 TEMPLATE cloudpivot_test'"
--   su postgres -c "psql -X -d arch_spec_v2 -f probe_design_v2.sql"
-- 期望：最后一行为 `PROBE OK —— 5 步全部断言通过`，退出码 0。
--
-- 与 probe_s1.sql / probe_s2.sql 的关系：那两个是**现状**（改判前）的负向取证；
-- 本脚本是**目标设计**的正向取证。三个脚本合计构成「改前红 / 改后绿」的完整证据链。
-- ============================================================================
\set ON_ERROR_STOP on
\pset pager off

BEGIN;

-- 巡检判据断言器：每一步都按 erp_invariants.py 的 B1/B2/B4/C3/C4/C5 口径复核（按 物料×仓库 限定）
CREATE FUNCTION public.probe_assert(p_mat uuid, p_wh uuid, p_base numeric, p_label text)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    v_cost  numeric; v_loc numeric; v_txn numeric; v_ser numeric;
    v_b2    numeric; v_bad int;
BEGIN
    SELECT coalesce(sum(qty), 0) INTO v_cost FROM biz_inventory WHERE material_id = p_mat AND warehouse_id = p_wh;
    SELECT coalesce(sum(qty), 0) INTO v_loc  FROM biz_inventory_location WHERE material_id = p_mat AND warehouse_id = p_wh;
    IF abs(v_cost - v_loc) > 0.0001 THEN
        RAISE EXCEPTION '[%] B1 红：成本账 % / 库位账 %', p_label, v_cost, v_loc;
    END IF;

    SELECT coalesce(sum(case when txn_type = 'IN' then qty else -qty end), 0) INTO v_txn
      FROM biz_inventory_txn WHERE material_id = p_mat AND warehouse_id = p_wh;
    IF abs(v_txn - v_cost) > 0.0001 THEN
        RAISE EXCEPTION '[%] B4 红：流水累计 % / 库存 %', p_label, v_txn, v_cost;
    END IF;

    SELECT coalesce(count(*), 0) INTO v_bad FROM biz_inventory
     WHERE material_id = p_mat AND qty > 0 AND avg_cost = 0;
    IF v_bad > 0 THEN
        RAISE EXCEPTION '[%] C3 红：% 行有库存但均价为 0', p_label, v_bad;
    END IF;

    SELECT coalesce(count(*), 0) INTO v_bad FROM biz_inventory
     WHERE material_id = p_mat AND qty > 0 AND abs(amount - qty * avg_cost) > 0.01;
    IF v_bad > 0 THEN
        RAISE EXCEPTION '[%] C4 红：% 行 金额 ≠ 数量×均价', p_label, v_bad;
    END IF;

    SELECT count(*)::numeric INTO v_ser FROM biz_material_serial
     WHERE material_id = p_mat AND warehouse_id = p_wh AND status = 'IN_STOCK' AND deleted_at IS NULL;
    IF abs(v_ser - v_cost) > 0.0001 THEN
        RAISE EXCEPTION '[%] C5 红：在库号 % / 帐面 %', p_label, v_ser, v_cost;
    END IF;

    SELECT (SELECT coalesce(sum(amount), 0) FROM biz_inventory)
         - (SELECT coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)), 0)
              FROM biz_voucher_entry e
              JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
              JOIN biz_account a ON a.id = e.account_id
             WHERE a.account_code LIKE '1405%')
      INTO v_b2;
    IF abs(v_b2 - p_base) > 0.0001 THEN
        RAISE EXCEPTION '[%] B2 红：库存金额与 1405 余额差 %（基线 %，增量 %）', p_label, v_b2, p_base, v_b2 - p_base;
    END IF;
END $$;

-- 0. 基线：全局 B2 差值（本库可能已有历史差额，所以断言用「增量」而不是绝对值）
CREATE TEMP TABLE probe_baseline AS
SELECT (SELECT coalesce(sum(amount), 0) FROM biz_inventory)                                    AS inv_amount,
       (SELECT coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)), 0)
          FROM biz_voucher_entry e
          JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
          JOIN biz_account a ON a.id = e.account_id
         WHERE a.account_code LIKE '1405%')                                                     AS gl_1405;

DO $$
DECLARE
    v_mat        uuid;
    v_mat_code   text;
    v_wh         uuid;
    v_wh_code    text;
    v_loc_row    uuid;      -- biz_inventory_location.id（= 请求体里的 location_id）
    v_loc_dict   uuid;      -- biz_location.id（库位字典，仅用于让库位行看起来像真的）
    v_out_id     uuid;
    v_out_no     text;
    v_scrap_id   uuid;
    v_scrap_no   text;
    v_out2_id    uuid;
    v_out2_no    text;
    v_doc1       uuid := gen_random_uuid();
    v_doc2       uuid := gen_random_uuid();
    v_v1         uuid := gen_random_uuid();
    v_v2         uuid := gen_random_uuid();
    v_v3         uuid := gen_random_uuid();
    v_v4         uuid := gen_random_uuid();
    v_v5         uuid := gen_random_uuid();
    v_a1405      uuid;
    v_a1901      uuid;
    v_a6401      uuid;
    v_base       numeric;
    v_now        timestamptz := now();
    -- 断言辅助
    v_qty        numeric;
    v_amt        numeric;
    v_avg        numeric;
    v_locq       numeric;
    v_txnq       numeric;
    v_serq       numeric;
    v_diff       numeric;
    v_rows       int;
BEGIN
    -- ---------- 0. 选一个「一物一码 + 该仓无历史库存/无历史号」的物料，隔离干净 ----------
    SELECT m.id, m.material_code INTO v_mat, v_mat_code
      FROM biz_material m
     WHERE m.serial_required AND m.deleted_at IS NULL
       AND NOT EXISTS (SELECT 1 FROM biz_inventory i WHERE i.material_id = m.id)
       AND NOT EXISTS (SELECT 1 FROM biz_material_serial s WHERE s.material_id = m.id)
     ORDER BY m.material_code
     LIMIT 1;
    IF v_mat IS NULL THEN
        RAISE EXCEPTION '取证失败：库里找不到「serial_required=TRUE 且无库存无号」的物料，无法隔离取证';
    END IF;

    SELECT w.id, w.warehouse_code INTO v_wh, v_wh_code
      FROM biz_warehouse w
     WHERE w.status = 1 AND w.warehouse_type = 'NORMAL'
       AND NOT EXISTS (SELECT 1 FROM biz_inventory i WHERE i.material_id = v_mat AND i.warehouse_id = w.id)
     ORDER BY w.warehouse_code
     LIMIT 1;
    IF v_wh IS NULL THEN
        RAISE EXCEPTION '取证失败：找不到可用仓库';
    END IF;

    SELECT id INTO v_loc_dict FROM biz_location WHERE warehouse_id = v_wh AND deleted_at IS NULL LIMIT 1;

    RAISE NOTICE '── 取证对象：物料 % / 仓库 % / 基线 B2 差值 %',
        v_mat_code, v_wh_code,
        (SELECT inv_amount - gl_1405 FROM probe_baseline);
    SELECT inv_amount - gl_1405 INTO v_base FROM probe_baseline;

    -- ---------- 0.1 补种科目（= 迁移 V0159 的内容，与 V0149/V0137 同一写法） ----------
    INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                             account_category, balance_direction, is_leaf, status, remark)
    VALUES ('d1370004-0000-4000-8000-000000000901', '1901', '待处理财产损溢', NULL, 1,
            'ASSET', 'DEBIT', 1, 1, 'V0159 补种：期初/其他入库的贷方、报废损失的借方')
    ON CONFLICT DO NOTHING;
    INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                             account_category, balance_direction, is_leaf, status, remark)
    VALUES ('d1370004-0000-4000-8000-000000000902', '6401', '主营业务成本', NULL, 1,
            'PROFIT_LOSS', 'DEBIT', 1, 1, 'V0159 补种：序列号出库结转成本')
    ON CONFLICT DO NOTHING;

    SELECT id INTO v_a1405 FROM biz_account WHERE account_code = '1405' AND deleted_at IS NULL;
    SELECT id INTO v_a1901 FROM biz_account WHERE account_code = '1901' AND deleted_at IS NULL;
    SELECT id INTO v_a6401 FROM biz_account WHERE account_code = '6401' AND deleted_at IS NULL;
    IF v_a1405 IS NULL OR v_a1901 IS NULL OR v_a6401 IS NULL THEN
        RAISE EXCEPTION '科目种子缺失：1405=% / 1901=% / 6401=%', v_a1405, v_a1901, v_a6401;
    END IF;

    -- ========================================================================
    -- 步骤 1：批量生成 5 件 @ 10.00（无来源入库，桶 = NULL 仓级汇总）
    --   落号 → apply_cost（加法）→ apply_location_qty（加法）→ 流水 → 凭证
    -- ========================================================================
    INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, location_id,
                                     status, inbound_at, source_type, source_id)
    SELECT gen_random_uuid(), 'PROBE-' || v_mat_code || '-' || g, v_mat, v_wh, NULL,
           'IN_STOCK', v_now, NULL, NULL
      FROM generate_series(1, 5) AS g;

    -- 成本账：stockin_model::apply_cost 的 SQL 逐字照抄（桶 = NULL；qty/amount 加法；avg 在 SQL 里 ROUND(..,6)）
    INSERT INTO biz_inventory AS i
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
    VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 5, 0,
            CASE WHEN 50 > 0 THEN ROUND(50::numeric / 5, 6) ELSE 0 END, 50, v_now)
    ON CONFLICT (material_id, warehouse_id, coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
    DO UPDATE SET
        qty      = i.qty + EXCLUDED.qty,
        amount   = i.amount + EXCLUDED.amount,
        avg_cost = CASE WHEN i.qty > 0 AND (i.qty + EXCLUDED.qty) > 0
                        THEN ROUND((i.amount + EXCLUDED.amount) / (i.qty + EXCLUDED.qty), 6)
                        WHEN EXCLUDED.qty > 0 THEN ROUND(EXCLUDED.amount / EXCLUDED.qty, 6)
                        ELSE 0 END,
        last_txn_at = EXCLUDED.last_txn_at;

    -- 库位账：stockin_model::apply_location_qty 分支 B（桶 = NULL）逐字照抄
    INSERT INTO biz_inventory_location AS il
        (id, material_id, warehouse_id, location_id, qty, locked_qty, last_txn_at)
    VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 5, 0, v_now)
    ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET
        qty = il.qty + EXCLUDED.qty,
        last_txn_at = EXCLUDED.last_txn_at;

    -- 流水：biz_type/doc_type 不变（P10 判据①依赖），unit_cost/amount 写实值；before/after 是**仓级**聚合
    INSERT INTO biz_inventory_txn
        (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, remark, created_at)
    VALUES (v_mat, v_wh, 'IN', 'SERIAL_IN', 'SERIAL_BATCH', v_doc1, 'PROBE-BATCH-1', 1,
            5, 10, 50, 0, 0, 0,
            5, 50, 10, '2026-10',
            '批量生成 5 件（PROBE-BATCH-1 起）｜单价 10.000000｜金额 50.0000', v_now);

    -- 凭证 A1：借 1405 库存商品 50 / 贷 1901 待处理财产损溢 50
    INSERT INTO biz_voucher
        (id, voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no,
         summary, total_debit, total_credit, entry_count, status, posted_at)
    VALUES (v_v1, 'PROBE-0001', '2026-10', DATE '2026-10-31', 'AUTO', 'SERIAL_BATCH',
            v_doc1, 'PROBE-BATCH-1', '期初入库：序列号批量生成 5 件', 50, 50, 2, 'POSTED', v_now);
    INSERT INTO biz_voucher_entry
        (voucher_id, line_no, summary, account_id, account_code, account_name,
         debit_amount, credit_amount, aux_material_id, aux_warehouse_id)
    VALUES
        (v_v1, 1, '期初入库：序列号批量生成 5 件', v_a1405, '1405', '库存商品', 50, 0, v_mat, v_wh),
        (v_v1, 2, '期初入库：序列号批量生成 5 件', v_a1901, '1901', '待处理财产损溢', 0, 50, NULL, NULL);

    -- ---- 断言：生成后 6 条判据 ----
    SELECT qty, amount, avg_cost INTO v_qty, v_amt, v_avg
      FROM biz_inventory WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id IS NULL;
    IF v_qty <> 5 OR v_amt <> 50 OR v_avg <> 10.000000 THEN
        RAISE EXCEPTION '步骤1 失败：成本账 qty/amount/avg = % / % / %（期望 5 / 50 / 10.000000）', v_qty, v_amt, v_avg;
    END IF;
    SELECT coalesce(sum(qty), 0) INTO v_locq FROM biz_inventory_location WHERE material_id = v_mat;
    IF v_locq <> 5 THEN
        RAISE EXCEPTION '步骤1 失败：库位账合计 = %（期望 5）', v_locq;
    END IF;
    PERFORM probe_assert(v_mat, v_wh, v_base, '步骤1 生成 5 件 @10');
    RAISE NOTICE '✔ 步骤1 生成 5 件 @10.00：成本账 5 / 50.0000 / 10.000000，库位账 5，流水 IN 5@10=50，凭证 借1405 50 / 贷1901 50';

    -- ========================================================================
    -- 步骤 2：出库 1 件（按桶重算两本账 → 成本结转凭证）
    -- ========================================================================
    SELECT id, serial_no INTO v_out_id, v_out_no
      FROM biz_material_serial
     WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id IS NULL
       AND status = 'IN_STOCK' AND deleted_at IS NULL
     ORDER BY serial_no LIMIT 1
       FOR UPDATE;
    UPDATE biz_material_serial SET status = 'OUTBOUND', outbound_at = v_now
     WHERE id = v_out_id;

    -- 成本账按桶重算（绝对值 = 该桶 IN_STOCK 件数；amount = qty × 桶 avg，avg 不动）
    INSERT INTO biz_inventory AS i
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
    SELECT gen_random_uuid(), v_mat, v_wh, s.bucket, s.q, 0, 0, 0, v_now
      FROM (SELECT location_id AS bucket, count(*)::numeric AS q
              FROM biz_material_serial
             WHERE material_id = v_mat AND warehouse_id = v_wh
               AND status = 'IN_STOCK' AND deleted_at IS NULL
             GROUP BY 1) s
    ON CONFLICT (material_id, warehouse_id, coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
    DO UPDATE SET qty = EXCLUDED.qty,
                  amount = EXCLUDED.qty * i.avg_cost,
                  last_txn_at = now();

    -- 库位账按桶重算（同口径）
    -- ★ 库位账的重算**不能**按「序列号的 location_id 去 upsert」：序列号存的是
    --   biz_inventory_location 的**行 id**，而本表的 location_id 列存的是**库位字典 id**
    --   （`biz_location.id`）。两个 id 混用会插出一条「拿行 id 当字典 id」的新行（实测 13 ≠ 10）。
    --   正确口径：以库位账自己的行为驱动，按行回填「落在该行的在库件数」。
    UPDATE biz_inventory_location il
       SET qty = (SELECT count(*)
                    FROM biz_material_serial s
                   WHERE s.material_id = il.material_id
                     AND s.warehouse_id = il.warehouse_id
                     AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL
                     AND ((il.location_id IS NULL AND s.location_id IS NULL) OR s.location_id = il.id)),
           last_txn_at = now()
     WHERE il.material_id = v_mat AND il.warehouse_id = v_wh;

    -- 流水（仓级 before/after；单价/金额 = 本次出库的实际成本）
    INSERT INTO biz_inventory_txn
        (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, remark, created_at)
    VALUES (v_mat, v_wh, 'OUT', 'SERIAL_OUT', 'SERIAL', v_out_id, v_out_no, 1,
            1, 10, 10, 5, 50, 10,
            4, 40, 10, '2026-10', '序列号出库：' || v_out_no, v_now);

    -- 凭证：借 6401 主营业务成本 10 / 贷 1405 库存商品 10（金额 = 账上 amount 的实际差额）
    INSERT INTO biz_voucher
        (id, voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no,
         summary, total_debit, total_credit, entry_count, status, posted_at)
    VALUES (v_v2, 'PROBE-0002', '2026-10', DATE '2026-10-31', 'AUTO', 'SERIAL_OUT',
            v_out_id, v_out_no,
            '序列号出库结转成本：' || v_out_no, 10, 10, 2, 'POSTED', v_now);
    INSERT INTO biz_voucher_entry
        (voucher_id, line_no, summary, account_id, account_code, account_name,
         debit_amount, credit_amount, aux_material_id, aux_warehouse_id)
    VALUES
        (v_v2, 1, '序列号出库结转成本', v_a6401, '6401', '主营业务成本', 10, 0, v_mat, v_wh),
        (v_v2, 2, '序列号出库结转成本', v_a1405, '1405', '库存商品', 0, 10, v_mat, v_wh);

    PERFORM probe_assert(v_mat, v_wh, v_base, '步骤2 出库 1 件');
    RAISE NOTICE '✔ 步骤2 出库 1 件：成本账 4 / 40.0000 / 10.000000，库位账 4，凭证 借6401 10 / 贷1405 10';

    -- ========================================================================
    -- 步骤 3：第二次批量生成 5 件 @ 13.00（不同单价 → 移动加权）
    --   期望 avg = ROUND((40 + 65) / 9, 6) = 11.666667
    -- ========================================================================
    INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, location_id,
                                     status, inbound_at, source_type, source_id)
    SELECT gen_random_uuid(), 'PROBE-' || v_mat_code || '-B' || g, v_mat, v_wh, NULL,
           'IN_STOCK', v_now, NULL, NULL
      FROM generate_series(1, 5) AS g;

    INSERT INTO biz_inventory AS i
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
    VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 5, 0,
            CASE WHEN 65 > 0 THEN ROUND(65::numeric / 5, 6) ELSE 0 END, 65, v_now)
    ON CONFLICT (material_id, warehouse_id, coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
    DO UPDATE SET
        qty      = i.qty + EXCLUDED.qty,
        amount   = i.amount + EXCLUDED.amount,
        avg_cost = CASE WHEN i.qty > 0 AND (i.qty + EXCLUDED.qty) > 0
                        THEN ROUND((i.amount + EXCLUDED.amount) / (i.qty + EXCLUDED.qty), 6)
                        WHEN EXCLUDED.qty > 0 THEN ROUND(EXCLUDED.amount / EXCLUDED.qty, 6)
                        ELSE 0 END,
        last_txn_at = EXCLUDED.last_txn_at;

    INSERT INTO biz_inventory_location AS il
        (id, material_id, warehouse_id, location_id, qty, locked_qty, last_txn_at)
    VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 5, 0, v_now)
    ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET
        qty = il.qty + EXCLUDED.qty, last_txn_at = EXCLUDED.last_txn_at;

    INSERT INTO biz_inventory_txn
        (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, remark, created_at)
    VALUES (v_mat, v_wh, 'IN', 'SERIAL_IN', 'SERIAL_BATCH', v_doc2, 'PROBE-BATCH-2', 1,
            5, 13, 65, 4, 40, 10,
            9, 105, ROUND(105::numeric / 9, 6), '2026-10',
            '批量生成 5 件（PROBE-BATCH-2 起）｜单价 13.000000｜金额 65.0000', v_now);

    INSERT INTO biz_voucher
        (id, voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no,
         summary, total_debit, total_credit, entry_count, status, posted_at)
    VALUES (v_v3, 'PROBE-0003', '2026-10', DATE '2026-10-31', 'AUTO', 'SERIAL_BATCH',
            v_doc2, 'PROBE-BATCH-2', '期初入库：序列号批量生成 5 件', 65, 65, 2, 'POSTED', v_now);
    INSERT INTO biz_voucher_entry
        (voucher_id, line_no, summary, account_id, account_code, account_name,
         debit_amount, credit_amount, aux_material_id, aux_warehouse_id)
    VALUES
        (v_v3, 1, '期初入库：序列号批量生成 5 件', v_a1405, '1405', '库存商品', 65, 0, v_mat, v_wh),
        (v_v3, 2, '期初入库：序列号批量生成 5 件', v_a1901, '1901', '待处理财产损溢', 0, 65, NULL, NULL);

    SELECT qty, amount, avg_cost INTO v_qty, v_amt, v_avg
      FROM biz_inventory WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id IS NULL;
    IF v_qty <> 9 OR v_amt <> 105 OR v_avg <> ROUND(105::numeric / 9, 6) THEN
        RAISE EXCEPTION '步骤3 失败：qty/amount/avg = % / % / %（期望 9 / 105 / 11.666667）', v_qty, v_amt, v_avg;
    END IF;
    PERFORM probe_assert(v_mat, v_wh, v_base, '步骤3 二次生成 5 件 @13');
    RAISE NOTICE '✔ 步骤3 二次生成 5 件 @13.00：成本账 9 / 105.0000 / %（= ROUND((40+65)/9, 6)），库位账 9', v_avg;

    -- ========================================================================
    -- 步骤 4：报废 1 件（借 1901 / 贷 1405，金额 = 账上 amount 实际差额）
    --   105 - ROUND(8 × 11.666667, 4) = 105 - 93.3333 = 11.6667
    -- ========================================================================
    SELECT id, serial_no INTO v_scrap_id, v_scrap_no
      FROM biz_material_serial
     WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id IS NULL
       AND status = 'IN_STOCK' AND deleted_at IS NULL
     ORDER BY serial_no LIMIT 1
       FOR UPDATE;
    UPDATE biz_material_serial SET status = 'SCRAPPED', outbound_at = v_now
     WHERE id = v_scrap_id;

    INSERT INTO biz_inventory AS i
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
    SELECT gen_random_uuid(), v_mat, v_wh, s.bucket, s.q, 0, 0, 0, v_now
      FROM (SELECT location_id AS bucket, count(*)::numeric AS q
              FROM biz_material_serial
             WHERE material_id = v_mat AND warehouse_id = v_wh
               AND status = 'IN_STOCK' AND deleted_at IS NULL
             GROUP BY 1) s
    ON CONFLICT (material_id, warehouse_id, coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
    DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.qty * i.avg_cost, last_txn_at = now();

    -- ★ 库位账的重算**不能**按「序列号的 location_id 去 upsert」：序列号存的是
    --   biz_inventory_location 的**行 id**，而本表的 location_id 列存的是**库位字典 id**
    --   （`biz_location.id`）。两个 id 混用会插出一条「拿行 id 当字典 id」的新行（实测 13 ≠ 10）。
    --   正确口径：以库位账自己的行为驱动，按行回填「落在该行的在库件数」。
    UPDATE biz_inventory_location il
       SET qty = (SELECT count(*)
                    FROM biz_material_serial s
                   WHERE s.material_id = il.material_id
                     AND s.warehouse_id = il.warehouse_id
                     AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL
                     AND ((il.location_id IS NULL AND s.location_id IS NULL) OR s.location_id = il.id)),
           last_txn_at = now()
     WHERE il.material_id = v_mat AND il.warehouse_id = v_wh;

    INSERT INTO biz_inventory_txn
        (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, remark, created_at)
    VALUES (v_mat, v_wh, 'OUT', 'SERIAL_SCRAP', 'SERIAL', v_scrap_id, v_scrap_no, 1,
            1, ROUND(105::numeric / 9, 6), 11.6667, 9, 105, ROUND(105::numeric / 9, 6),
            8, 93.3333, ROUND(105::numeric / 9, 6), '2026-10', '序列号报废：' || v_scrap_no, v_now);

    INSERT INTO biz_voucher
        (id, voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no,
         summary, total_debit, total_credit, entry_count, status, posted_at)
    VALUES (v_v4, 'PROBE-0004', '2026-10', DATE '2026-10-31', 'AUTO', 'SERIAL_SCRAP',
            v_scrap_id, v_scrap_no,
            '序列号报废损失：' || v_scrap_no, 11.6667, 11.6667, 2, 'POSTED', v_now);
    INSERT INTO biz_voucher_entry
        (voucher_id, line_no, summary, account_id, account_code, account_name,
         debit_amount, credit_amount, aux_material_id, aux_warehouse_id)
    VALUES
        (v_v4, 1, '序列号报废损失', v_a1901, '1901', '待处理财产损溢', 11.6667, 0, v_mat, v_wh),
        (v_v4, 2, '序列号报废损失', v_a1405, '1405', '库存商品', 0, 11.6667, v_mat, v_wh);

    PERFORM probe_assert(v_mat, v_wh, v_base, '步骤4 报废 1 件');
    RAISE NOTICE '✔ 步骤4 报废 1 件：成本账 8 / 93.3333 / 11.666667，凭证 借1901 11.6667 / 贷1405 11.6667';

    -- ========================================================================
    -- 步骤 5：混桶 —— 再生成 3 件 @15.00 落到**指定库位行**（location_id = biz_inventory_location.id）
    --   这一桶的成本账/库位账都落该库位行；仓级合计仍 = 在库号数（不变式① 的按桶版本）
    -- ========================================================================
    INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty)
    VALUES (gen_random_uuid(), v_mat, v_wh, v_loc_dict, 0, 0)
    ON CONFLICT (material_id, warehouse_id, location_id) DO NOTHING;
    SELECT id INTO v_loc_row FROM biz_inventory_location
     WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id IS NOT DISTINCT FROM v_loc_dict;

    INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, location_id,
                                     status, inbound_at, source_type, source_id)
    SELECT gen_random_uuid(), 'PROBE-' || v_mat_code || '-L' || g, v_mat, v_wh, v_loc_row,
           'IN_STOCK', v_now, NULL, NULL
      FROM generate_series(1, 3) AS g;

    INSERT INTO biz_inventory AS i
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
    VALUES (gen_random_uuid(), v_mat, v_wh, v_loc_row, 3, 0,
            CASE WHEN 45 > 0 THEN ROUND(45::numeric / 3, 6) ELSE 0 END, 45, v_now)
    ON CONFLICT (material_id, warehouse_id, coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
    DO UPDATE SET
        qty      = i.qty + EXCLUDED.qty,
        amount   = i.amount + EXCLUDED.amount,
        avg_cost = CASE WHEN i.qty > 0 AND (i.qty + EXCLUDED.qty) > 0
                        THEN ROUND((i.amount + EXCLUDED.amount) / (i.qty + EXCLUDED.qty), 6)
                        WHEN EXCLUDED.qty > 0 THEN ROUND(EXCLUDED.amount / EXCLUDED.qty, 6)
                        ELSE 0 END,
        last_txn_at = EXCLUDED.last_txn_at;

    -- 库位账分支 A：按行 id 加（stockin_model::apply_location_qty 的 Some(row_id) 分支）
    UPDATE biz_inventory_location SET qty = qty + 3, last_txn_at = v_now WHERE id = v_loc_row;

    INSERT INTO biz_inventory_txn
        (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, remark, created_at)
    VALUES (v_mat, v_wh, 'IN', 'SERIAL_IN', 'SERIAL_BATCH', gen_random_uuid(), 'PROBE-BATCH-3', 1,
            3, 15, 45, 8, 93.3333, ROUND(105::numeric / 9, 6),
            11, 138.3333, ROUND(138.3333 / 11, 6), '2026-10',
            '批量生成 3 件（PROBE-BATCH-3 起）｜单价 15.000000｜金额 45.0000', v_now);

    INSERT INTO biz_voucher
        (id, voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no,
         summary, total_debit, total_credit, entry_count, status, posted_at)
    VALUES (v_v5, 'PROBE-0005', '2026-10', DATE '2026-10-31', 'AUTO', 'SERIAL_BATCH',
            gen_random_uuid(), 'PROBE-BATCH-3', '期初入库：序列号批量生成 3 件（库位）', 45, 45, 2, 'POSTED', v_now);
    INSERT INTO biz_voucher_entry
        (voucher_id, line_no, summary, account_id, account_code, account_name,
         debit_amount, credit_amount, aux_material_id, aux_warehouse_id)
    VALUES
        (v_v5, 1, '期初入库：序列号批量生成 3 件（库位）', v_a1405, '1405', '库存商品', 45, 0, v_mat, v_wh),
        (v_v5, 2, '期初入库：序列号批量生成 3 件（库位）', v_a1901, '1901', '待处理财产损溢', 0, 45, NULL, NULL);

    SELECT count(*) INTO v_rows FROM biz_inventory WHERE material_id = v_mat;
    IF v_rows <> 2 THEN
        RAISE EXCEPTION '步骤5 失败：成本账桶数 = %（期望 2：NULL 汇总桶 + 库位桶）', v_rows;
    END IF;
    PERFORM probe_assert(v_mat, v_wh, v_base, '步骤5 混桶生成 3 件 @15');
    RAISE NOTICE '✔ 步骤5 混桶生成 3 件 @15.00：成本账 % 个桶（NULL=8/93.3333/11.666667，库位=3/45/15.000000），库位账 8+3=11', v_rows;

    -- ========================================================================
    -- 步骤 6：从库位桶出库 1 件 —— 按桶重算必须只动该桶，仓级合计跟着变 11 → 10
    -- ========================================================================
    SELECT id, serial_no INTO v_out2_id, v_out2_no
      FROM biz_material_serial
     WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id = v_loc_row
       AND status = 'IN_STOCK' AND deleted_at IS NULL
     ORDER BY serial_no LIMIT 1
       FOR UPDATE;
    UPDATE biz_material_serial SET status = 'OUTBOUND', outbound_at = v_now
     WHERE id = v_out2_id;

    INSERT INTO biz_inventory AS i
        (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
    SELECT gen_random_uuid(), v_mat, v_wh, s.bucket, s.q, 0, 0, 0, v_now
      FROM (SELECT location_id AS bucket, count(*)::numeric AS q
              FROM biz_material_serial
             WHERE material_id = v_mat AND warehouse_id = v_wh
               AND status = 'IN_STOCK' AND deleted_at IS NULL
             GROUP BY 1) s
    ON CONFLICT (material_id, warehouse_id, coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid))
    DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.qty * i.avg_cost, last_txn_at = now();

    -- ★ 库位账的重算**不能**按「序列号的 location_id 去 upsert」：序列号存的是
    --   biz_inventory_location 的**行 id**，而本表的 location_id 列存的是**库位字典 id**
    --   （`biz_location.id`）。两个 id 混用会插出一条「拿行 id 当字典 id」的新行（实测 13 ≠ 10）。
    --   正确口径：以库位账自己的行为驱动，按行回填「落在该行的在库件数」。
    UPDATE biz_inventory_location il
       SET qty = (SELECT count(*)
                    FROM biz_material_serial s
                   WHERE s.material_id = il.material_id
                     AND s.warehouse_id = il.warehouse_id
                     AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL
                     AND ((il.location_id IS NULL AND s.location_id IS NULL) OR s.location_id = il.id)),
           last_txn_at = now()
     WHERE il.material_id = v_mat AND il.warehouse_id = v_wh;

    INSERT INTO biz_inventory_txn
        (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
         qty, unit_cost, amount, before_qty, before_amount, before_cost,
         after_qty, after_amount, after_cost, period_code, remark, created_at)
    VALUES (v_mat, v_wh, 'OUT', 'SERIAL_OUT', 'SERIAL', v_out2_id, v_out2_no, 1,
            1, 15, 15, 11, 138.3333, ROUND(138.3333 / 11, 6),
            10, 123.3333, ROUND(123.3333 / 10, 6), '2026-10', '序列号出库：' || v_out2_no, v_now);

    INSERT INTO biz_voucher
        (id, voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no,
         summary, total_debit, total_credit, entry_count, status, posted_at)
    VALUES (gen_random_uuid(), 'PROBE-0006', '2026-10', DATE '2026-10-31', 'AUTO', 'SERIAL_OUT',
            v_out2_id, v_out2_no, '序列号出库结转成本：' || v_out2_no,
            15, 15, 2, 'POSTED', v_now);
    INSERT INTO biz_voucher_entry
        (voucher_id, line_no, summary, account_id, account_code, account_name,
         debit_amount, credit_amount, aux_material_id, aux_warehouse_id)
    SELECT vv.id, x.line_no, '序列号出库结转成本', x.account_id, x.code, x.name, x.dr, x.cr, v_mat, v_wh
      FROM biz_voucher vv,
           (VALUES (1, v_a6401, '6401', '主营业务成本', 15::numeric, 0::numeric),
                   (2, v_a1405, '1405', '库存商品', 0::numeric, 15::numeric)) AS x(line_no, account_id, code, name, dr, cr)
     WHERE vv.voucher_no = 'PROBE-0006';

    SELECT qty, amount, avg_cost INTO v_qty, v_amt, v_avg
      FROM biz_inventory WHERE material_id = v_mat AND warehouse_id = v_wh AND location_id = v_loc_row;
    IF v_qty <> 2 OR v_amt <> 30 THEN
        RAISE EXCEPTION '步骤6 失败：库位桶 qty/amount = % / %（期望 2 / 30）', v_qty, v_amt;
    END IF;
    SELECT coalesce(sum(qty), 0) INTO v_qty FROM biz_inventory WHERE material_id = v_mat;
    IF v_qty <> 10 THEN
        RAISE EXCEPTION '步骤6 失败：仓级成本账 qty = %（期望 10）', v_qty;
    END IF;
    PERFORM probe_assert(v_mat, v_wh, v_base, '步骤6 从库位桶出库 1 件');
    RAISE NOTICE '✔ 步骤6 从库位桶出库 1 件：库位桶 2/30.0000，仓级 10，凭证 借6401 15 / 贷1405 15';

    RAISE NOTICE '════════════════════════════════════════════════════════════';
    RAISE NOTICE 'PROBE OK —— 6 步全部断言通过（B1/B2/B4/C3/C4/C5 每步全绿）';
    RAISE NOTICE '════════════════════════════════════════════════════════════';
END $$;

-- 汇总打印最终状态（肉眼复核用）
SELECT i.location_id IS NULL AS 汇总桶, i.qty, i.amount, i.avg_cost
  FROM biz_inventory i
 WHERE i.material_id = (SELECT material_id FROM biz_material_serial WHERE serial_no LIKE 'PROBE-%' LIMIT 1)
 ORDER BY 1 DESC;
SELECT (SELECT sum(amount) FROM biz_inventory)
       - (SELECT coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)), 0)
            FROM biz_voucher_entry e
            JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
            JOIN biz_account a ON a.id = e.account_id
           WHERE a.account_code LIKE '1405%') AS b2_差值;

ROLLBACK;
\echo '已 ROLLBACK —— 库内数据未变'
