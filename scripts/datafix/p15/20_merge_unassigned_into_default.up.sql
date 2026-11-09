-- ============================================================
-- P15 数据修复 ②：把「未分配桶」并进「默认库位桶」（E2E-FIX-6 ③ 存量口径）
--
-- 修的是**存量**（E2E-FIX-6 之前批量生成造出来的两桶并存）；新数据由代码保证落同一桶。
--
-- 合并动作（每个 (物料, 仓库) 一轮，全部在一个事务里）：
--   ① 找该仓的默认库位（`biz_location.is_default = 1` 且未软删）；**没有就跳过**
--      —— 没有默认库位的仓，收货也落未分配桶，这是正常形态，不该硬塞一个库位。
--   ② find-or-create 库位账行 `(物料, 仓库, 默认库位)` → `biz_inventory_location.id`
--   ③ 把落未分配桶的**在库**序列号改指这一行（否则扫码仍然看不到库位）
--   ④ 成本账：`qty / amount` 相加，`avg_cost = round(新 amount / 新 qty, 6)`
--   ⑤ 库位账：同一行 `qty` 相加
--   ⑥ 未分配桶清零（**保留行**：行 id 仍被历史流水 / 审计引用，清零比删行安全）
--
-- 安全阀：
--   · 幂等：未分配桶清零后下一轮不再命中（`qty <> 0 OR amount <> 0`）
--   · 守恒：合并前后按 (物料, 仓库) 比对 `SUM(qty)` / `SUM(amount)`，不一致直接 RAISE EXCEPTION 回滚
--   · 只动「有默认库位」的仓；只动未分配桶；不删任何行、不动已分配库位桶的均价口径
--
-- 执行（**必须先跑只读报告看清范围**）：
--   bash scripts/datafix/p15/apply.sh --check     # 只读：列出会被合并的记录
--   bash scripts/datafix/p15/apply.sh --apply     # 真写库（事务内，守恒校验失败即回滚）
-- ============================================================

BEGIN;

-- 合并前按 (物料, 仓库) 的仓级合计，供守恒校验用
CREATE TEMP TABLE p15_before ON COMMIT DROP AS
SELECT material_id,
       warehouse_id,
       SUM(qty)::numeric(18,4)    AS qty,
       SUM(amount)::numeric(18,4) AS amount
  FROM biz_inventory
 GROUP BY material_id, warehouse_id;

DO $$
DECLARE
    rec            RECORD;
    ledger_row_id  UUID;
    def_loc_id     UUID;
    moved_qty      NUMERIC(18,4) := 0;
    moved_amount   NUMERIC(18,4) := 0;
    moved_serials  BIGINT := 0;
    row_cnt        BIGINT := 0;
    merged_rows    INT := 0;
    skipped_no_loc INT := 0;
BEGIN
    FOR rec IN
        SELECT i.material_id,
               i.warehouse_id,
               i.qty    AS null_qty,
               i.amount AS null_amount
          FROM biz_inventory i
         WHERE i.location_id IS NULL
           AND (i.qty <> 0 OR i.amount <> 0)
         ORDER BY i.material_id, i.warehouse_id
           FOR UPDATE
    LOOP
        -- ① 该仓的默认库位（每仓最多一个，库侧部分唯一索引兜底）
        SELECT l.id INTO def_loc_id
          FROM biz_location l
         WHERE l.warehouse_id = rec.warehouse_id
           AND l.is_default = 1
           AND l.deleted_at IS NULL
         LIMIT 1;

        IF def_loc_id IS NULL THEN
            RAISE NOTICE '跳过 物料% 仓库%（该仓没有默认库位，未分配桶是正常形态）',
                rec.material_id, rec.warehouse_id;
            skipped_no_loc := skipped_no_loc + 1;
            CONTINUE;
        END IF;

        -- ② find-or-create 库位账行
        INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
        VALUES (rec.material_id, rec.warehouse_id, def_loc_id, 0)
        ON CONFLICT (material_id, warehouse_id, location_id) DO NOTHING;

        SELECT il.id INTO ledger_row_id
          FROM biz_inventory_location il
         WHERE il.material_id = rec.material_id
           AND il.warehouse_id = rec.warehouse_id
           AND il.location_id = def_loc_id;

        -- ③ 在库序列号改指默认库位账行（扫码 / 列表才看得到库位）
        UPDATE biz_material_serial
           SET location_id = ledger_row_id,
               updated_at  = now()
         WHERE material_id  = rec.material_id
           AND warehouse_id = rec.warehouse_id
           AND location_id IS NULL
           AND status = 'IN_STOCK'
           AND deleted_at IS NULL;
        GET DIAGNOSTICS row_cnt = ROW_COUNT;
        moved_serials := moved_serials + row_cnt;

        -- ④ 成本账：并进目标桶（加法 + 移动加权，舍入口径与 SQL 侧一致：round(…,6)）
        UPDATE biz_inventory tgt
           SET qty         = tgt.qty + rec.null_qty,
               amount      = tgt.amount + rec.null_amount,
               avg_cost    = CASE
                                 WHEN (tgt.qty + rec.null_qty) = 0 THEN 0
                                 ELSE round((tgt.amount + rec.null_amount) / (tgt.qty + rec.null_qty), 6)
                             END,
               last_txn_at = now(),
               updated_at  = now()
         WHERE tgt.material_id  = rec.material_id
           AND tgt.warehouse_id = rec.warehouse_id
           AND tgt.location_id  = ledger_row_id;

        -- ⑤ 库位账：同一行 qty 相加
        UPDATE biz_inventory_location
           SET qty        = qty + rec.null_qty,
               last_txn_at = now(),
               updated_at  = now()
         WHERE id = ledger_row_id;

        -- ⑥ 库位账的「未分配」行同样清零（**必须做**：只清成本账会把 B1b 逐桶对账打红 ——
        --    成本账 0 / 库位账 2 的错位，实测就是这么来的）
        UPDATE biz_inventory_location
           SET qty        = 0,
               last_txn_at = now(),
               updated_at  = now()
         WHERE material_id  = rec.material_id
           AND warehouse_id = rec.warehouse_id
           AND location_id IS NULL
           AND id <> ledger_row_id;

        -- ⑦ 成本账的未分配桶清零（保留行）
        UPDATE biz_inventory
           SET qty      = 0,
               amount   = 0,
               avg_cost = 0,
               updated_at = now()
         WHERE material_id  = rec.material_id
           AND warehouse_id = rec.warehouse_id
           AND location_id IS NULL;

        moved_qty    := moved_qty + rec.null_qty;
        moved_amount := moved_amount + rec.null_amount;
        merged_rows  := merged_rows + 1;

        RAISE NOTICE '合并 物料% 仓库% → 库位账行%：qty % / amount %',
            rec.material_id, rec.warehouse_id, ledger_row_id, rec.null_qty, rec.null_amount;
    END LOOP;

    -- 守恒校验：仓级合计必须一分不动
    IF EXISTS (
        SELECT 1
          FROM p15_before b
          FULL JOIN (
              SELECT material_id,
                     warehouse_id,
                     SUM(qty)::numeric(18,4)    AS qty,
                     SUM(amount)::numeric(18,4) AS amount
                FROM biz_inventory
               GROUP BY material_id, warehouse_id
          ) a USING (material_id, warehouse_id)
         WHERE COALESCE(b.qty, 0)    <> COALESCE(a.qty, 0)
            OR COALESCE(b.amount, 0) <> COALESCE(a.amount, 0)
    ) THEN
        RAISE EXCEPTION 'P15 合并破坏了仓级合计（qty/amount 前后不一致），已回滚：请先用 10_report_two_bucket 复核';
    END IF;

    RAISE NOTICE 'P15 完成：合并 % 个 (物料, 仓库)，移动 qty % / amount %，改指序列号 % 件；跳过 % 个无默认库位的仓',
        merged_rows, moved_qty, moved_amount, moved_serials, skipped_no_loc;
END
$$;

COMMIT;
