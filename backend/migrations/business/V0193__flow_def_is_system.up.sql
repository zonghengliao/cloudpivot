-- ======================================================
-- V0193：流程模板库 · `is_system` 标记 + 预置流程分类归一（P-FLOW-5）
--
-- 依据：卡面 t_84fbfea1「一、后端」段
--   > `is_system` 字段：`biz_flow_def` 加一列（迁移），预置的标 true。
--
-- ── 为什么用「按种子 UUID 打标」而不是「按 flow_code 打标」 ─────────
-- 种子的插入条件都是 `WHERE NOT EXISTS (flow_code = ...)`：客户若先建了同编码流程，
-- 种子整条跳过，那条流程是**客户自己建的**。按编码打标会把它错认成系统预置
-- （前端就会给客户的流程挂上「标准」标签，O1 的设计意图直接反了）。
-- 按种子固定 UUID 打标则只命中「确实由种子插入的那一行」。
--
-- ── 为什么顺手归一 category ──────────────────────────────────────
-- 早期种子（V0127/V0157/V0174/V0175）的 category 写的是 `task` / `erp` 这种**内部码**，
-- V0182/V0191 写的是中文业务域。模板库按 category 分组时会出现「erp」这种不该给用户看的分组名。
-- 本迁移把 8 条预置流程的 category 统一成中文业务域，**只改这一列**（不动 graph / 授权）：
--   采购管理：po_approve
--   仓库管理：stock_in_post / stock_transfer / stock_in_cancel
--   财务管理：payable_pay / payable_cancel / voucher_reverse
--   任务    ：task_accept
-- 幂等：`WHERE category IS DISTINCT FROM <新值>`，重复执行零行变更。
--
-- 回滚：`V0193__flow_def_is_system.down.sql`（删列；category 不回退 —— 回退成内部码
--       会让前端分组重新出现「erp」这种名字，且不承载任何业务语义）
-- ======================================================

-- ---------- 1) 加列 ----------
ALTER TABLE biz_flow_def
    ADD COLUMN IF NOT EXISTS is_system BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN biz_flow_def.is_system IS
    '是否系统预置模板（true = 随迁移种下的标准流程，前端标「标准」；客户另存为出来的副本恒 false）';

-- ---------- 2) 预置流程打标（按种子固定 UUID，见文件头） ----------
UPDATE biz_flow_def
   SET is_system = TRUE,
       updated_at = now()
 WHERE id IN (
       '20000000-0000-4000-8000-0000000a0182'::uuid, -- po_approve       采购订单审批
       '20000000-0000-4000-8000-0000000b0182'::uuid, -- stock_in_post    收货过账审批
       '20000000-0000-4000-8000-0000000c0182'::uuid, -- payable_cancel   应付单作废审批
       '20000000-0000-4000-8000-0000000d0182'::uuid, -- voucher_reverse  凭证红冲审批
       '20000000-0000-4000-8000-00000000d127'::uuid, -- task_accept      任务验收
       '20000000-0000-4000-8000-00000000d157'::uuid, -- stock_transfer   库存调拨审批
       '20000000-0000-4000-8000-00000000d174'::uuid, -- stock_in_cancel  收货单作废审批
       '20000000-0000-4000-8000-000000000717'::uuid  -- payable_pay      应付付款审批
   )
   AND deleted_at IS NULL
   AND is_system = FALSE;

-- ---------- 3) 分类归一（只动这 8 行的 category） ----------
UPDATE biz_flow_def AS d
   SET category = v.category_new,
       updated_at = now()
  FROM (VALUES
        ('20000000-0000-4000-8000-0000000a0182'::uuid, '采购管理'),
        ('20000000-0000-4000-8000-0000000b0182'::uuid, '仓库管理'),
        ('20000000-0000-4000-8000-0000000c0182'::uuid, '财务管理'),
        ('20000000-0000-4000-8000-0000000d0182'::uuid, '财务管理'),
        ('20000000-0000-4000-8000-00000000d127'::uuid, '任务'),
        ('20000000-0000-4000-8000-00000000d157'::uuid, '仓库管理'),
        ('20000000-0000-4000-8000-00000000d174'::uuid, '仓库管理'),
        ('20000000-0000-4000-8000-000000000717'::uuid, '财务管理')
       ) AS v(id, category_new)
 WHERE d.id = v.id
   AND d.deleted_at IS NULL
   AND d.is_system = TRUE
   AND d.category IS DISTINCT FROM v.category_new;

-- ---------- 4) 结构自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    col_ok   int;
    bad_flag int;
BEGIN
    SELECT count(*) INTO col_ok
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_flow_def'
       AND column_name = 'is_system'
       AND is_nullable = 'NO'
       AND data_type = 'boolean';
    IF col_ok <> 1 THEN
        RAISE EXCEPTION 'V0193 自检失败：biz_flow_def.is_system 不是 NOT NULL BOOLEAN';
    END IF;

    -- 预置流程不允许出现「is_system = TRUE 但 category 还是内部码」
    SELECT count(*) INTO bad_flag
      FROM biz_flow_def
     WHERE is_system
       AND deleted_at IS NULL
       AND (category IS NULL OR category IN ('erp', 'task'));
    IF bad_flag > 0 THEN
        RAISE EXCEPTION 'V0193 自检失败：% 条预置流程的 category 仍是内部码', bad_flag;
    END IF;

    RAISE NOTICE 'V0193 自检通过：is_system 列 + 预置流程分类归一';
END
$$;
