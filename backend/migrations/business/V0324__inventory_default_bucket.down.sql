-- ============================================================
-- V0324 down：库存两连（FIX-4）—— 只回退**结构**，不回退数据合并
--
-- # 为什么数据合并不回滚
--
--   ① 「未分配桶 → 默认库位桶」是**合并**：合并后无法逐条还原「哪些数量原本在未分配桶」。
--      硬拆回去需要一个迁移前的快照表，而这个表在生产上会永久的占着数据（并且没人会读它）。
--   ② 合并**不改变任何总量**（up.sql §11 自检：总数量 / 总金额 / 两本账差异全等），
--      只改「同一个仓库里库存记在哪个库位桶上」。回退代码而不回退数据是安全的：
--      旧代码在「有汇总桶就扣汇总桶」的前提下，遇到只剩库位桶的数据，会走「按明细库位精确匹配」
--      分支 —— 两本账仍然同一个桶，只是不再有「未分配桶」可选。
--   ③ 真要回到「期初落未分配桶」：新录一笔期初（或直接删掉该期初再录）即可，
--      不需要动历史数据。
--
-- 因此本 down 只做两件事：删掉新增列、把不变量再核一遍（防止手工改过库导致悄悄不平衡）。
-- ============================================================

-- ---------- §1 逐 (物料, 仓库) 两本账复核（删列之前先确认数据没被手工改坏） ----------
DO $$
DECLARE
    broken BIGINT;
BEGIN
    SELECT COUNT(*) INTO broken
      FROM (SELECT material_id, warehouse_id, SUM(qty)::numeric(18,4) AS qty
              FROM biz_inventory GROUP BY 1, 2) c
      FULL JOIN (SELECT material_id, warehouse_id, SUM(qty)::numeric(18,4) AS qty
                   FROM biz_inventory_location GROUP BY 1, 2) l
             ON c.material_id = l.material_id AND c.warehouse_id = l.warehouse_id
     WHERE abs(COALESCE(c.qty, 0) - COALESCE(l.qty, 0)) > 0.0001;
    IF broken > 0 THEN
        RAISE EXCEPTION 'V0324 down 中止：% 个「物料×仓库」两本账不相等，先跑库存巡检修正', broken;
    END IF;
    RAISE NOTICE 'V0324 down：两本账 0 差异；数据合并按设计**不回滚**（见文件头）';
END
$$;

-- ---------- §2 删除新增列 ----------
DROP INDEX IF EXISTS idx_biz_opening_inventory_location_row;
ALTER TABLE biz_opening_inventory DROP COLUMN IF EXISTS location_row_id;
