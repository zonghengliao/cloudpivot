-- ============================================================
-- V0215：P2-2 摆设字段处置（ARCH-3 §5 DC-12）
--
-- 「填了以为生效」是软件最伤信任的一类 bug。本迁移把三个摆设字段
-- 的**处置结论**落到 schema 层（代码侧见 `purchase_credit.rs` /
-- `reminder_stock.rs`，口径文档见 `docs/ERP-摆设字段口径.md`）：
--
--   ① biz_supplier.credit_limit     → **开始生效**：采购单提交时现算
--       「该供应商累计未结采购额（含本单）」比较额度；超限则单据带
--       `over_credit_limit = true`，且**必须走审批**（不许 skip-approval）。
--       评估结果三列落库（见下），供列表/详情/审批流条件直接读。
--
--   ② biz_material.safety_stock     → 消费者是 `erp_low_stock_scan`
--       （V0210）。口径写进列注释：当前库存 = `biz_inventory.qty`
--       （已过账结存，**不含在途**），`0` = 未设 = 不预警。
--
--   ③ biz_inventory.locked_qty      → **本版本无占用流程，恒为 0**。
--       不删列（未来「销售订单占用/生产预占」会用它），前端已隐藏该列，
--       列注释明确标注「预留」，免得运维/二开以为它在工作。
--
-- # 为什么评估结果要落库（而「已用额度」不落库）
--
-- `credit_limit` 的判定必须**现算**（R1 单一数据源：缓存列必漂移）。
-- 但「这张单提交那一刻查到什么」是**审计事实**，必须冻结 —— 供应商额度
-- 之后被调高/调低，不能让历史单据的「当时超没超」跟着变。两者的区别是
-- 「当前值」vs「当时值」：前者现算，后者是一次性快照。
--
-- # 幂等
--
-- 全部 `IF NOT EXISTS` / `COMMENT ON`（后者天然幂等），可重复执行。
-- 存量数据**零回填**：三列默认 `FALSE` / `NULL`，`NULL` 的语义是
-- 「从未提交过（未评估）」，不是「没超限」—— 回填一个假的 FALSE
-- 会把「没查过」伪装成「查过且没问题」。
-- ============================================================

-- ---------- ① 采购单：信用额度评估结果（提交时点快照） ----------
ALTER TABLE biz_purchase_order
    ADD COLUMN IF NOT EXISTS over_credit_limit   BOOLEAN       NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS credit_exposure     NUMERIC(18,4),
    ADD COLUMN IF NOT EXISTS credit_limit_snapshot NUMERIC(18,4);

COMMENT ON COLUMN biz_purchase_order.over_credit_limit IS
    '提交时点是否超供应商信用额度：TRUE = 该供应商累计未结采购额（含本单）> credit_limit_snapshot。超限单**必须走审批**（不许 skip-approval），审批流可按 form.over_credit_limit 配条件分支';
COMMENT ON COLUMN biz_purchase_order.credit_exposure IS
    '提交时点「该供应商累计未结采购额（含本单）」快照：SUM(total_with_tax) WHERE supplier_id 相同 AND status <> CANCELLED AND deleted_at IS NULL。NULL = 从未提交过（未评估），不是 0';
COMMENT ON COLUMN biz_purchase_order.credit_limit_snapshot IS
    '提交时点供应商额度快照（0 = 不限）。额度之后被改不影响历史单据的评审结论';

-- ---------- ② 安全库存：口径写进列注释 ----------
COMMENT ON COLUMN biz_material.safety_stock IS
    '安全库存：低库存提醒（erp_low_stock_scan）的阈值。口径 = biz_inventory.qty（该物料×该仓库的已过账结存，不含在途、不含未过账收货）< safety_stock 时提醒。0 = 未设 = 不预警（不是「安全库存为 0」）';

-- ---------- ③ 锁定量：标注预留 ----------
COMMENT ON COLUMN biz_inventory.locked_qty IS
    '锁定量（预留）：本版本没有任何占用/拣货流程写它，恒为 0。不参与低库存判定、不参与可用量（可用 = qty）。等「销售订单占用/生产预占」落地后再启用，见 docs/ERP-摆设字段口径.md';
COMMENT ON COLUMN biz_inventory_location.locked_qty IS
    '库位锁定量（预留）：同 biz_inventory.locked_qty，本版本恒为 0，无占用流程';

-- ---------- ④ 信用额度列注释对齐现行语义 ----------
COMMENT ON COLUMN biz_supplier.credit_limit IS
    '信用额度（0 = 不限）：采购单**提交时**现算「累计未结采购额（含本单，排除 CANCELLED）」比较；超限只标记 + 强制审批，不直接拒绝（策略 B）。';

-- ---------- ⑤ 自检：列必须存在、类型/可空性符合预期 ----------
DO $$
DECLARE
    n_ok integer;
    n_total integer;
BEGIN
    SELECT count(*) INTO n_total
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name IN ('over_credit_limit', 'credit_exposure', 'credit_limit_snapshot');
    IF n_total <> 3 THEN
        RAISE EXCEPTION 'V0215 自检失败：biz_purchase_order 应有 3 个信用额度列，实际 %', n_total;
    END IF;

    SELECT count(*) INTO n_ok
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND (   (column_name = 'over_credit_limit' AND data_type = 'boolean' AND is_nullable = 'NO')
            OR (column_name = 'credit_exposure' AND data_type = 'numeric' AND is_nullable = 'YES')
            OR (column_name = 'credit_limit_snapshot' AND data_type = 'numeric' AND is_nullable = 'YES'));
    IF n_ok <> 3 THEN
        RAISE EXCEPTION 'V0215 自检失败：信用额度列的类型/可空性不符（应 3 条符合，实际 %）', n_ok;
    END IF;

    RAISE NOTICE 'V0215：摆设字段处置已落地（采购单 3 列 + 4 条列注释）';
END $$;
