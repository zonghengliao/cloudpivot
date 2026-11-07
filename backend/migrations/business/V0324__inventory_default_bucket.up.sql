-- ============================================================
-- V0324：库存两连（FIX-4 · t_763677cd）—— 期初桶与收货桶统一
--
-- 依据：`backend/src/business/erp/opening.rs`（`opening_location_row`）
--       + `backend/src/business/erp/transfer_settle.rs`（`pick_bucket`）
--       + `docs/evidence/20261107-p003-inventory-chain/`（P0-03 分库位漂移演练）
--
-- # 为什么要这张迁移
--
--   修复前：库存期初（`POST /business/erp/opening-balance/inventory`）一律写
--   `location_id = NULL` 的「未分配库位桶」，而**收货过账**写的是「仓库默认库位桶」
--   （`biz_location.is_default = 1` 经 `ensure_location_row` 建出来的库位账行）。
--   同仓同物料两桶并存 ⇒ 任何按「汇总桶优先」取数的路径都会扣错桶，这就是
--   P0-03 实测「分库位库存永久漂移」的触发条件；汇总不变式只比合计，对错位完全无感。
--
--   代码侧已改为「期初也落默认库位桶」（与收货同口径）。**存量数据**必须一起并过来，
--   否则老库里两桶并存的状态会一直留着。
--
-- # 做什么（全部幂等：找目标 → 合并 → 删源，跑第二遍选集为空）
--
--   ① 对每个「有默认库位（is_default = 1 且启用、未软删）的仓库」的每个物料：
--      · find-or-create 目标库位账行 `(物料, 仓库, 默认库位字典)`；
--      · 成本账：目标桶已有行 → 数量/金额/锁定量相加后删源行；没有 → 源行直接搬到目标桶；
--      · 库位账：目标行 += 源行（数量/锁定量），然后删源行；
--      · 引用（序列号 / 收货明细 / 调拨明细的调出、调入库位）全部改指向目标库位账行
--        —— 否则 FK 挡着删不掉源行，删掉也会让历史单据指向不存在的行。
--      · 没有默认库位的仓库**原样不动**：代码侧兜底仍落「未分配桶」（与收货一致）。
--   ② `biz_opening_inventory.location_row_id`（新增列）：记录这笔期初落在哪个库位桶，
--      重录 / 取消时按差额冲回**同一个桶**（否则中途改默认库位会把 Δ 冲到别的桶上）。
--      存量行按迁移后的实际落点回填。
--
-- # 自检（任何一条不满足就 RAISE EXCEPTION 整笔回滚，不留下半合并的数据）
--
--   · 有默认库位的仓库里，不再有 `location_id IS NULL` 的库位账行；
--   · 成本账总数量、库位账总数量、总金额与迁移前**逐分相等**（库存总数不变）；
--   · 迁移后仍然逐 (物料, 仓库) 满足 `SUM(成本账) = SUM(库位账)`。
--
-- # 为什么不改「没有默认库位」的仓库
--
--   代码侧的兜底是「该仓没有默认库位 → 落未分配桶，并提示先设默认库位」（与收货同口径）。
--   迁移替用户造一个默认库位属于越界（库位要容量、要编码、要业务确认），所以只搬有主的数据。
-- ============================================================

-- ---------- §1 期初台账记「落在哪个库位桶」 ----------
ALTER TABLE biz_opening_inventory ADD COLUMN IF NOT EXISTS location_row_id UUID;

COMMENT ON COLUMN biz_opening_inventory.location_row_id IS
    '期初落在哪个库位桶（biz_inventory_location.id，弱引用）；NULL = 未分配桶 / 存量行。重录、取消按它冲回同一个桶（V0324 / FIX-4）';

-- ---------- §2 迁移前的总量快照（自检用） ----------
DROP TABLE IF EXISTS _v0324_before;
CREATE TEMP TABLE _v0324_before AS
SELECT (SELECT COALESCE(SUM(qty), 0) FROM biz_inventory)              AS cost_qty,
       (SELECT COALESCE(SUM(amount), 0) FROM biz_inventory)           AS cost_amount,
       (SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location)     AS loc_qty,
       (SELECT COUNT(*) FROM biz_inventory_location WHERE location_id IS NULL) AS null_buckets;

-- ---------- §3 候选清单：(物料, 仓库) + 源库位账行 + 目标默认库位字典 ----------
DROP TABLE IF EXISTS _v0324_cand;
CREATE TEMP TABLE _v0324_cand AS
SELECT il.id            AS src_row_id,
       il.material_id   AS material_id,
       il.warehouse_id  AS warehouse_id,
       d.dict_id        AS dict_id
  FROM biz_inventory_location il
  JOIN LATERAL (
        -- 每仓最多一个默认库位（部分唯一索引 uk_biz_location_default 兜底），这里仍按 id 稳定取一行
        SELECT l.id AS dict_id
          FROM biz_location l
         WHERE l.warehouse_id = il.warehouse_id
           AND l.is_default = 1
           AND l.status = 1
           AND l.deleted_at IS NULL
         ORDER BY l.id
         LIMIT 1
       ) d ON TRUE
 WHERE il.location_id IS NULL;

-- ---------- §4 目标库位账行 find-or-create ----------
INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty, locked_qty)
SELECT material_id, warehouse_id, dict_id, 0, 0
  FROM _v0324_cand
ON CONFLICT DO NOTHING;

ALTER TABLE _v0324_cand ADD COLUMN IF NOT EXISTS tgt_row_id UUID;

UPDATE _v0324_cand c
   SET tgt_row_id = il.id
  FROM biz_inventory_location il
 WHERE il.material_id = c.material_id
   AND il.warehouse_id = c.warehouse_id
   AND il.location_id = c.dict_id;

-- 目标行与源行同一行（理论上不会：源行 location_id 为 NULL、目标行非 NULL）→ 排除，防误删
DELETE FROM _v0324_cand WHERE tgt_row_id IS NULL OR tgt_row_id = src_row_id;

-- ---------- §5 成本账：目标桶已有行 → 相加（数量 / 金额 / 锁定量 / 均价） ----------
UPDATE biz_inventory t
   SET qty        = t.qty + s.qty,
       amount     = t.amount + s.amount,
       locked_qty = t.locked_qty + s.locked_qty,
       avg_cost   = CASE WHEN t.qty + s.qty > 0
                         THEN ROUND((t.amount + s.amount) / (t.qty + s.qty), 6)
                         ELSE t.avg_cost END,
       last_txn_at = GREATEST(t.last_txn_at, s.last_txn_at),
       updated_at  = CURRENT_TIMESTAMP
  FROM biz_inventory s, _v0324_cand c
 WHERE s.material_id = c.material_id
   AND s.warehouse_id = c.warehouse_id
   AND s.location_id IS NULL
   AND t.material_id = c.material_id
   AND t.warehouse_id = c.warehouse_id
   AND t.location_id = c.tgt_row_id
   AND t.id <> s.id;

-- ---------- §6 成本账：已并入的源行删掉（否则唯一键不允许两行同桶） ----------
DELETE FROM biz_inventory s
 USING _v0324_cand c
 WHERE s.material_id = c.material_id
   AND s.warehouse_id = c.warehouse_id
   AND s.location_id IS NULL
   AND EXISTS (SELECT 1 FROM biz_inventory t
                WHERE t.material_id = c.material_id
                  AND t.warehouse_id = c.warehouse_id
                  AND t.location_id = c.tgt_row_id);

-- ---------- §7 成本账：没有目标行的源行直接搬到目标桶 ----------
UPDATE biz_inventory s
   SET location_id = c.tgt_row_id,
       updated_at  = CURRENT_TIMESTAMP
  FROM _v0324_cand c
 WHERE s.material_id = c.material_id
   AND s.warehouse_id = c.warehouse_id
   AND s.location_id IS NULL;

-- ---------- §8 引用改指向目标库位账行（先改引用，再删源行；FK 挡着就删不掉） ----------
UPDATE biz_material_serial x
   SET location_id = c.tgt_row_id
  FROM _v0324_cand c
 WHERE x.location_id = c.src_row_id;

UPDATE biz_stock_in_item x
   SET location_id = c.tgt_row_id
  FROM _v0324_cand c
 WHERE x.location_id = c.src_row_id;

UPDATE biz_stock_transfer_item x
   SET from_location_id = c.tgt_row_id
  FROM _v0324_cand c
 WHERE x.from_location_id = c.src_row_id;

UPDATE biz_stock_transfer_item x
   SET to_location_id = c.tgt_row_id
  FROM _v0324_cand c
 WHERE x.to_location_id = c.src_row_id;

-- ---------- §9 库位账：相加后删源行 ----------
UPDATE biz_inventory_location t
   SET qty         = t.qty + s.qty,
       locked_qty  = t.locked_qty + s.locked_qty,
       last_txn_at = GREATEST(t.last_txn_at, s.last_txn_at),
       updated_at  = CURRENT_TIMESTAMP
  FROM biz_inventory_location s, _v0324_cand c
 WHERE s.id = c.src_row_id
   AND t.id = c.tgt_row_id;

DELETE FROM biz_inventory_location s
 USING _v0324_cand c
 WHERE s.id = c.src_row_id;

-- ---------- §10 期初台账：回填「这笔期初落在哪个库位桶」 ----------
-- 只回填迁移后**实际持有该期初库存**的那个桶：默认库位桶存在就指向它
-- （§3-§9 已把未分配桶的库存并进去）；没有默认库位的仓保持 NULL（库存仍在未分配桶）。
UPDATE biz_opening_inventory o
   SET location_row_id = il.id,
       updated_at      = CURRENT_TIMESTAMP
  FROM biz_inventory_location il
  JOIN biz_location l ON l.id = il.location_id
 WHERE o.location_row_id IS NULL
   AND l.is_default = 1
   AND l.status = 1
   AND l.deleted_at IS NULL
   AND il.material_id = o.material_id
   AND il.warehouse_id = o.warehouse_id;

COMMENT ON COLUMN biz_opening_inventory.location_row_id IS
    '期初落在哪个库位桶（biz_inventory_location.id，弱引用）；NULL = 未分配桶 / 存量行。重录、取消按它冲回同一个桶（V0324 / FIX-4）';

-- ---------- §11 自检 ----------
DO $$
DECLARE
    before_cost_qty   NUMERIC;
    before_amount     NUMERIC;
    before_loc_qty    NUMERIC;
    before_nulls      BIGINT;
    after_cost_qty    NUMERIC;
    after_amount      NUMERIC;
    after_loc_qty     NUMERIC;
    left_nulls        BIGINT;
    two_books_broken  BIGINT;
    merged            BIGINT;
    backfilled        BIGINT;
BEGIN
    SELECT cost_qty, cost_amount, loc_qty, null_buckets
      INTO before_cost_qty, before_amount, before_loc_qty, before_nulls
      FROM _v0324_before;

    SELECT COALESCE(SUM(qty), 0), COALESCE(SUM(amount), 0) INTO after_cost_qty, after_amount
      FROM biz_inventory;
    SELECT COALESCE(SUM(qty), 0) INTO after_loc_qty FROM biz_inventory_location;

    -- ① 有默认库位的仓库里不许再留「未分配桶」库位账行
    SELECT COUNT(*) INTO left_nulls
      FROM biz_inventory_location il
     WHERE il.location_id IS NULL
       AND EXISTS (SELECT 1 FROM biz_location l
                    WHERE l.warehouse_id = il.warehouse_id
                      AND l.is_default = 1 AND l.status = 1 AND l.deleted_at IS NULL);

    -- ② 逐 (物料, 仓库)：成本账合计 == 库位账合计（巡检 B1 的判据，容差 0.0001）
    SELECT COUNT(*) INTO two_books_broken
      FROM (SELECT material_id, warehouse_id,
                   SUM(qty)::numeric(18,4) AS qty
              FROM biz_inventory GROUP BY 1, 2) c
      FULL JOIN (SELECT material_id, warehouse_id,
                        SUM(qty)::numeric(18,4) AS qty
                   FROM biz_inventory_location GROUP BY 1, 2) l
             ON c.material_id = l.material_id AND c.warehouse_id = l.warehouse_id
     WHERE abs(COALESCE(c.qty, 0) - COALESCE(l.qty, 0)) > 0.0001;

    SELECT COUNT(*) INTO merged FROM _v0324_cand;
    SELECT COUNT(*) INTO backfilled FROM biz_opening_inventory
     WHERE location_row_id IS NOT NULL;

    IF after_cost_qty <> before_cost_qty THEN
        RAISE EXCEPTION 'V0324 自检失败：成本账总数量被改变（迁移前 % / 迁移后 %）',
            before_cost_qty, after_cost_qty;
    END IF;
    IF after_loc_qty <> before_loc_qty THEN
        RAISE EXCEPTION 'V0324 自检失败：库位账总数量被改变（迁移前 % / 迁移后 %）',
            before_loc_qty, after_loc_qty;
    END IF;
    IF after_amount <> before_amount THEN
        RAISE EXCEPTION 'V0324 自检失败：库存总金额被改变（迁移前 % / 迁移后 %）',
            before_amount, after_amount;
    END IF;
    IF left_nulls > 0 THEN
        RAISE EXCEPTION 'V0324 自检失败：仍有 % 条「未分配桶」库位账行落在有默认库位的仓库里',
            left_nulls;
    END IF;
    IF two_books_broken > 0 THEN
        RAISE EXCEPTION 'V0324 自检失败：迁移后仍有 % 个「物料×仓库」两本账不相等', two_books_broken;
    END IF;

    RAISE NOTICE 'V0324：并入 % 个「未分配桶」到默认库位桶（原未分配桶 % 条），回填期初桶 % 行；'
                 '总数量 % / 总金额 % / 库位账合计 % 与迁移前一致，两本账 0 差异',
        merged, before_nulls, backfilled, after_cost_qty, after_amount, after_loc_qty;
END
$$;

DROP TABLE IF EXISTS _v0324_cand;
DROP TABLE IF EXISTS _v0324_before;
