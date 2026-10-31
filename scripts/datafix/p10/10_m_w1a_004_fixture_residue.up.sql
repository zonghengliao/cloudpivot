-- ============================================================================
-- P10 / C3：M-W1A-004「无单据的验收夹具」账实残留清理
-- ----------------------------------------------------------------------------
-- 背景：`scripts/verify/erp_invariants.py` 在 prod 上剩最后一条 WARN ——
--       C3 有库存则成本价非零：`M-W1A-004 / WH01` 2 件、amount = 0、avg_cost = 0。
--       P9 把 20 元的无单据价值归零（`50_b2_fixture_value.up.sql`）之后必然命中 C3。
--
-- 证据链（先查清来源，再决定处置；卡面 §二 要求「查清这 8 条 SN 的来源」）：
--   ① 该物料**没有任何收货单 / 采购单明细**：
--        select count(*) from biz_stock_in_item        where material_id = <M-W1A-004>  → 0
--        select count(*) from biz_purchase_order_item  where material_id = <M-W1A-004>  → 0
--      ⇒ 这批货不是采购进来的。
--   ② 11 条序列号全部是 `source_type='PURCHASE'` 且 **`source_id IS NULL`**：
--      只有 `serial_model::batch_generate`（`POST /business/erp/serial/batch-generate`）
--      会写出「有 source_type、无 source_id」的形态；收货过账走
--      `stockin_model::insert_serials`，**必定**回填 `source_id = 收货单 id`。
--      ⇒ 号是「批量生成接口」造的验收夹具（W2a/W2b/W2c1/W3a 几张卡的 e2e 都用 M-W1A-004 当测试物料）。
--   ③ 流水也印证同一路径：`biz_inventory_txn` 里该物料只有
--      `SERIAL_IN 3（SERIAL_BATCH / doc_no=SN-20261030-000009）`、`SERIAL_OUT 1`、
--      以及夹具调拨单 `TK-20261030-000001` 的 `TRANSFER_IN/OUT 2`（unit_cost 10 是手工填的，
--      `biz_inventory.amount` 的 20 元也是手工改库进来的 —— P9 §1 ④ 已逐笔归因）。
--   ④ 因此**没有「真采购来源」**：卡面 §二 第 3 条（补做收货单）不适用 ——
--      补一张没有供应商/价格/合同依据的采购单才是造账。
--
-- 口径（按卡面 §二 第 2 条「若是验收夹具 → 清理」）：
--   * 序列号：**软删**（`deleted_at`）—— 行保留：`biz_stock_transfer_item_serial.serial_id`
--     是硬外键（TK-20261030-000001 引用 SN-…0010/0011），物理删会撞 FK；
--     软删后 `recompute_quantity` / 分页 / 扫码等一律按 `deleted_at IS NULL` 过滤，
--     在库件数归零，同号也能被重新贴用（W2a 口径）。
--   * 成本账 / 库位账 / 流水：**物理删**这 4+1+2 行夹具 —— 它们的起点是无单据的批量生成，
--     中间的价值是手工改库，终点是手工造的调拨单，删掉才让「流水累计 = 库存」（B4）、
--     「库位账 = 成本账」（B1）、「作废单库存已回滚」（E4）三条同时成立。
--     **不写反向流水**：反向流水是给「有单据的真实业务」用的，这里从来没有业务。
--   * **不动**：调拨单 `TK-20261030-000001` / `TK-20261031-000001` 及其明细、序列号链接行
--     （单据与审批历史保留原样；`serial_id` 指向的号现在是软删态，链接行不删以保 FK 与审计）。
--   * **不动**：`biz_seq` 的 SN 号段（只前进不回退，宁可留空号也不撞号）。
--
-- 结果（应满足）：
--   C3 有库存则成本价非零      → 该物料不再有 qty > 0 的行          ✅
--   C5 一物一码在库件数=库存量  → 在库序列号 0 件 / 库存 0 件（P10 新增的不变量）
--   B1 / B4 / E4 / C4          → 三账同时少这批夹具，等式不变
--   B2 库存金额 = 1405 余额    → 金额本来就是 0，删 0 元行不改变合计
--
-- 幂等：全部 DELETE / UPDATE 都带「本物料 + 尚未清理」的谓词，第二遍命中 0 行。
-- 安全阀：若该物料有收货/采购明细，或库存金额 ≠ 0，**拒绝执行**（RAISE EXCEPTION），
--         因为那说明这批货有业务来源，应该补做收货单而不是清理。
-- ============================================================================
DO $$
DECLARE
    v_material    uuid;
    v_doc_rows    integer;
    v_value       numeric;
    v_serial_rows integer;
    v_txn_rows    integer;
    v_loc_rows    integer;
    v_inv_rows    integer;
    v_link_rows   integer;
BEGIN
    SELECT m.id INTO v_material
      FROM biz_material m
     WHERE m.material_code = 'M-W1A-004' AND m.deleted_at IS NULL
     ORDER BY m.created_at
     LIMIT 1;

    IF v_material IS NULL THEN
        RAISE NOTICE 'P10: 物料 M-W1A-004 不存在，跳过';
        RETURN;
    END IF;

    -- ---------- 安全阀 1：有业务单据 ⇒ 这是真采购，不许清理 ----------
    SELECT (SELECT count(*) FROM biz_stock_in_item       WHERE material_id = v_material)
         + (SELECT count(*) FROM biz_purchase_order_item WHERE material_id = v_material)
      INTO v_doc_rows;
    IF v_doc_rows > 0 THEN
        RAISE EXCEPTION
            'P10: M-W1A-004 有 % 条收货/采购明细 —— 这批货有业务来源，应补做收货单，不得清理（已中止）',
            v_doc_rows;
    END IF;

    -- ---------- 安全阀 2：库存带金额 ⇒ 价值已有账载，不许无差别删 ----------
    SELECT coalesce(sum(amount), 0) INTO v_value
      FROM biz_inventory WHERE material_id = v_material;
    IF v_value <> 0 THEN
        RAISE EXCEPTION
            'P10: M-W1A-004 库存金额 = %（≠ 0）—— 价值已有单据承载，请人工核对后再决定，已中止',
            v_value;
    END IF;

    -- ---------- ① 序列号：软删（保留审计痕迹与外键） ----------
    UPDATE biz_material_serial
       SET deleted_at = now(), updated_at = now()
     WHERE material_id = v_material AND deleted_at IS NULL;
    GET DIAGNOSTICS v_serial_rows = ROW_COUNT;

    -- ---------- ② 流水：SERIAL_IN / SERIAL_OUT / 夹具调拨的 TRANSFER_IN/OUT ----------
    DELETE FROM biz_inventory_txn WHERE material_id = v_material;
    GET DIAGNOSTICS v_txn_rows = ROW_COUNT;

    -- ---------- ③ 库位账（未分配库位桶）----------
    DELETE FROM biz_inventory_location WHERE material_id = v_material;
    GET DIAGNOSTICS v_loc_rows = ROW_COUNT;

    -- ---------- ④ 成本账 ----------
    DELETE FROM biz_inventory WHERE material_id = v_material;
    GET DIAGNOSTICS v_inv_rows = ROW_COUNT;

    SELECT count(*) INTO v_link_rows
      FROM biz_stock_transfer_item_serial ts
      JOIN biz_material_serial s ON s.id = ts.serial_id
     WHERE s.material_id = v_material;

    RAISE NOTICE 'P10: M-W1A-004 夹具残留已清理 —— 序列号软删 % 条 / 流水 % 条 / 库位账 % 行 / 成本账 % 行',
        v_serial_rows, v_txn_rows, v_loc_rows, v_inv_rows;
    RAISE NOTICE 'P10: 保留的夹具单据：调拨明细链接行 % 条（serial_id 现在指向软删号，FK 与审计都在）',
        v_link_rows;
END
$$;
