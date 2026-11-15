-- ============================================================================
-- V0339__approval_sign_mode_freeze.down（回滚：把**草稿**的签名模式清回 NULL）
-- ============================================================================
--
-- ⚠️ 已知损失（不保证逐行还原，卡面 D2 允许）
--   · 只清 **DRAFT** 版本的 APPROVAL 节点（本迁移 up 只改了这一档）。
--     被 up 冻结成 `ALL` 的草稿节点会回到「或签」语义 —— 重新编辑发布前请人工确认。
--   · **已发布 / 归档版本不动**：它们的冻结是启动期 `flow::sign_freeze` 做的，
--     快照 + hash + 行 + 实例节点四者同改；单纯把行清回 NULL 会让
--     `graph_hash` 与行不一致（快照自检天天报警）。要回滚那一档只能「另存为新草稿 →
--     手工把节点改回或签 → 重新发布」，没有一键反向。
--   · 迁移前本来就是显式 `ANY` 的草稿节点也会被清成 NULL —— 语义等价（NULL ≡ ANY）。
--
-- 清完后打印被改过的行数（RAISE NOTICE），便于在日志/证据里核对。
-- ============================================================================

DO $$
DECLARE
    cleared INTEGER;
BEGIN
    WITH touched AS (
        UPDATE biz_flow_node n
           SET approval_mode = NULL
          FROM biz_flow_def_version v
         WHERE v.id = n.version_id
           AND v.version_status = 'DRAFT'
           AND n.node_type = 'APPROVAL'
           AND n.approval_mode IS NOT NULL
        RETURNING 1
    )
    SELECT count(*) INTO cleared FROM touched;
    RAISE NOTICE 'V0339 down：已把 % 个草稿 APPROVAL 节点的 approval_mode 清回 NULL', cleared;
END $$;

-- 回滚后的补救 SQL（需要时手工执行，把某个草稿节点重设成全员签）：
--   UPDATE biz_flow_node n SET approval_mode = 'ALL'
--     FROM biz_flow_def_version v
--    WHERE v.id = n.version_id AND v.version_status = 'DRAFT' AND n.node_key = '<node>';
