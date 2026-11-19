-- ============================================================================
-- V0465__cost_standard_publish.down.sql
--
-- 回滚标准成本「发布（审核后生效）」：拆流程种子 + 拆发布轴四列。
--
-- 三道安全阀（拒绝而不是静默丢数据）：
--   ① 库里还有 COST_STANDARD_PUBLISH 的审批实例 → 拒绝（删版本会留下指向空气的审批单）；
--   ② 还有卡停在 PENDING（审批中）→ 拒绝（删列等于把「审批中」的卡变成无主单，只能改库）；
--   ③ 还有卡是 DRAFT（草稿）→ 拒绝（降级成 PUBLISHED 会让未经审批的标准突然参与取数，
--      静默改变差异分析口径 —— 这比回滚失败严重得多；请先让人工把草稿处理掉）。
-- 都为空时才拆：assignee → edge → node → version → def，最后 DROP COLUMN。
-- ============================================================================

DO $$
DECLARE
    v_cnt integer;
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_schema = current_schema() AND table_name = 'biz_cost_standard'
                  AND column_name = 'publish_state') THEN
        -- ① 审批实例
        SELECT count(*) INTO v_cnt FROM biz_flow_instance WHERE biz_type = 'COST_STANDARD_PUBLISH';
        IF v_cnt > 0 THEN
            RAISE EXCEPTION 'M5-10 down 拒绝执行：还有 % 张 COST_STANDARD_PUBLISH 审批单（删流程定义会让它们指向空气）', v_cnt;
        END IF;
        -- ② 审批中的卡
        SELECT count(*) INTO v_cnt FROM biz_cost_standard WHERE publish_state = 'PENDING' AND deleted_at IS NULL;
        IF v_cnt > 0 THEN
            RAISE EXCEPTION 'M5-10 down 拒绝执行：还有 % 版标准成本卡在审批中（拆列会留下无主单）', v_cnt;
        END IF;
        -- ③ 草稿卡
        SELECT count(*) INTO v_cnt FROM biz_cost_standard WHERE publish_state = 'DRAFT' AND deleted_at IS NULL;
        IF v_cnt > 0 THEN
            RAISE EXCEPTION 'M5-10 down 拒绝执行：还有 % 版标准成本卡是草稿（拆列会把它们当成已发布，静默改变取数口径）', v_cnt;
        END IF;
    END IF;
END $$;

-- ---------- 拆流程种子（节点 / 边 / 审批人 → 版本 → 定义）----------
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id IN (SELECT id FROM biz_flow_def_version
                                          WHERE def_id IN (SELECT id FROM biz_flow_def
                                                            WHERE id = '20000000-0000-4000-8000-0000000d0465'::uuid
                                                               OR flow_code = 'cost_standard_publish_approve')));
DELETE FROM biz_flow_edge
 WHERE version_id IN (SELECT id FROM biz_flow_def_version
                       WHERE def_id IN (SELECT id FROM biz_flow_def
                                         WHERE id = '20000000-0000-4000-8000-0000000d0465'::uuid
                                            OR flow_code = 'cost_standard_publish_approve'));
DELETE FROM biz_flow_node
 WHERE version_id IN (SELECT id FROM biz_flow_def_version
                       WHERE def_id IN (SELECT id FROM biz_flow_def
                                         WHERE id = '20000000-0000-4000-8000-0000000d0465'::uuid
                                            OR flow_code = 'cost_standard_publish_approve'));
DELETE FROM biz_flow_def_version
 WHERE def_id IN (SELECT id FROM biz_flow_def
                   WHERE id = '20000000-0000-4000-8000-0000000d0465'::uuid
                      OR flow_code = 'cost_standard_publish_approve');
DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000d0465'::uuid
    OR flow_code = 'cost_standard_publish_approve';

-- ---------- 拆发布轴四列 ----------
ALTER TABLE biz_cost_standard DROP CONSTRAINT IF EXISTS ck_biz_cost_std_publish_inst;
ALTER TABLE biz_cost_standard DROP CONSTRAINT IF EXISTS ck_biz_cost_std_publish;
DROP INDEX IF EXISTS idx_biz_cost_std_publish_state;
ALTER TABLE biz_cost_standard
    DROP COLUMN IF EXISTS published_by,
    DROP COLUMN IF EXISTS published_at,
    DROP COLUMN IF EXISTS publish_instance_id,
    DROP COLUMN IF EXISTS publish_state;

-- ---------- 自检 ----------
DO $$
DECLARE
    v_cnt integer;
BEGIN
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_cost_standard'
       AND column_name LIKE 'publish%';
    IF v_cnt > 0 THEN
        RAISE EXCEPTION 'M5-10 down 未干净：biz_cost_standard 还剩 % 个 publish* 列', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt FROM biz_flow_def WHERE flow_code = 'cost_standard_publish_approve';
    IF v_cnt > 0 THEN
        RAISE EXCEPTION 'M5-10 down 未干净：cost_standard_publish_approve 定义还在（% 行）', v_cnt;
    END IF;
END $$;
