-- ============================================================================
-- V0361：收款退款单补 `approval_instance_id`（E2E-FIX-8 ②，P4）
--
-- 卡：t_e220697e（E2E-FIX-8【P4】杂项 5 条 ②）
--
-- # 一、现场（E2E-02 演练）
--
--   `biz_receipt_refund` 只有 `status` 四个值（DRAFT/CONFIRMED/POSTED/CANCELLED），
--   审批不是状态（见 `receipt_refund.rs` 模块文档：审批是 CONFIRMED → POSTED 的**前置条件**）。
--   结果：
--     · 详情接口靠 `receipt_refund_bridge::latest_approval_of` 现查审批实例表（1 次额外查询）；
--     · **列表接口完全没有审批状态** —— 前端要显示「审批中 / 已驳回」只能逐行回查详情，
--       N 行 N 次查询（又有 N+1），所以 E2E-02 的退款单列表只能看到「已确认」。
--
-- # 二、修法：加一列弱引用 + 一次性回填
--
--   `approval_instance_id UUID` 记录**最近一次提交的审批实例**（弱引用
--   `biz_flow_instance.id`，与 `biz_stock_in` / `biz_contract` / `biz_purchase_order` 同款）。
--   列表接口用一条 `LEFT JOIN biz_flow_instance fi ON fi.id = p.approval_instance_id`
--   就把 `approval_status` / `approval_instance_no` 一起带出来 —— 一条 SQL，零 N+1。
--
--   为什么不加外键：审批实例可被流程管理侧清理（历史卡有「删掉的单据留下孤儿审批」的教训，
--   反过来「审批实例没了也不能把退款单卡住」）。弱引用 + JOIN 时 `deleted_at IS NULL`
--   由读侧兜底，与本仓其它单据的审批弱引用口径一致。
--
--   为什么存「最近一次」而不是「已通过的那一次」：前端要看的是**当前这一轮**——
--   驳回后重新提交会留下多条实例，存最近一条才能显示「审批中（FLW-…）」；
--   过账闸仍然走 `approved_instance_of`（认任何一条 APPROVED），两者互不影响。
--
-- # 三、回填
--
--   存量退款单（145 dev 上已提交过审批的那些）按「同一 (biz_type, biz_id) 最近一条实例」
--   回填，排序口径与 `latest_approval_of` 逐字一致（`started_at DESC, created_at DESC`）。
--   未提交过审批的单保持 NULL（前端显示「从未提交」）。
--
-- # 四、幂等
--
--   · `ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`；
--   · 回填只动 `approval_instance_id IS NULL` 的行 —— 重复执行 0 行；
--   · 末尾自检：该列必须存在，且不得出现「指向非 RECEIPT_REFUND 实例」的行。
--
-- # 五、回滚
--
--   down 删索引 + 删列（列是本卡新增的唯一东西，删掉即回到 V0359 的结构）。
-- ============================================================================

-- ---------- ① 加列 ----------
ALTER TABLE biz_receipt_refund
    ADD COLUMN IF NOT EXISTS approval_instance_id UUID;

COMMENT ON COLUMN biz_receipt_refund.approval_instance_id IS
    '最近一次提交的审批实例（弱引用 biz_flow_instance.id；NULL = 从未提交过审批）。列表用它 LEFT JOIN 出审批状态，审批本身仍不是单据状态（E2E-FIX-8 ②）';

-- ---------- ② 回填存量（只补 NULL，幂等） ----------
UPDATE biz_receipt_refund r
   SET approval_instance_id = latest.id
  FROM (
        SELECT DISTINCT ON (i.biz_id) i.biz_id, i.id
          FROM biz_flow_instance i
         WHERE i.biz_type = 'RECEIPT_REFUND'
           AND i.biz_id IS NOT NULL
           AND i.deleted_at IS NULL
         ORDER BY i.biz_id, i.started_at DESC, i.created_at DESC
       ) AS latest
 WHERE latest.biz_id = r.id
   AND r.approval_instance_id IS NULL
   AND r.deleted_at IS NULL;

-- ---------- ③ 索引（列表 LEFT JOIN 按 id 命中；只索引有值的行） ----------
CREATE INDEX IF NOT EXISTS ix_biz_receipt_refund_approval_instance
    ON biz_receipt_refund (approval_instance_id)
 WHERE approval_instance_id IS NOT NULL;

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    v_column_exists BOOLEAN;
    v_wrong_biz_type BIGINT;
    v_backfilled     BIGINT;
BEGIN
    SELECT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_receipt_refund'
           AND column_name = 'approval_instance_id'
    ) INTO v_column_exists;
    IF NOT v_column_exists THEN
        RAISE EXCEPTION 'V0361 自检失败：biz_receipt_refund.approval_instance_id 未加上';
    END IF;

    -- 回填/写入都只认 RECEIPT_REFUND 实例；指错类型的行会让列表显示别人的审批状态
    SELECT count(*) INTO v_wrong_biz_type
      FROM biz_receipt_refund r
      JOIN biz_flow_instance i ON i.id = r.approval_instance_id
     WHERE i.biz_type <> 'RECEIPT_REFUND';
    IF v_wrong_biz_type > 0 THEN
        RAISE EXCEPTION 'V0361 自检失败：% 行 approval_instance_id 指向的不是收款退款审批实例',
            v_wrong_biz_type;
    END IF;

    -- 指向了已被清理的实例（弱引用允许，但要在日志里可见，方便运维核对）
    SELECT count(*) INTO v_backfilled
      FROM biz_receipt_refund r
     WHERE r.approval_instance_id IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM biz_flow_instance i WHERE i.id = r.approval_instance_id);
    IF v_backfilled > 0 THEN
        RAISE NOTICE 'V0361 提示：% 行 approval_instance_id 指向的审批实例已不存在（列表审批状态为空，属预期）',
            v_backfilled;
    END IF;

    RAISE NOTICE 'V0361 完成：biz_receipt_refund.approval_instance_id 已就绪';
END $$;
