-- ============================================================================
-- V0248__stock_in_cancel_reviewer_role.down.sql
--
-- 回滚 V0248：作废复核节点回到 V0174 的种子口径（ADMIN / FALLBACK_ADMIN / FORBID），
-- 摘掉 WAREHOUSE_MANAGER 绑定，快照与 hash 回 78d34c…。
--
-- 守卫：只有「当前 hash = 026766…」（= 本卡改过）才会动；客户在此基础上又改过
-- （重新发布 / 手工改图）则整段跳过 —— 回滚不许覆盖别人的配置。
-- 回滚后 `self_approval=FORBID` 仍在（内控不放宽），只是审批人来源退回「全部超管」。
-- ============================================================================

DO $$
DECLARE
    c_def      CONSTANT uuid := '20000000-0000-4000-8000-00000000d174';
    c_ver      CONSTANT uuid := '20000000-0000-4000-8000-00000000e174';
    c_node     CONSTANT uuid := '20000000-0000-4000-8000-00000000a175';
    c_old_hash CONSTANT text := '0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0';
    c_new_hash CONSTANT text := '78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c';
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM biz_flow_def_version
         WHERE id = c_ver AND btrim(graph_hash) = c_old_hash
    ) THEN
        RAISE NOTICE 'V0248 down：当前不是本卡改过的图（hash ≠ 026766…），跳过回滚';
        RETURN;
    END IF;

    UPDATE biz_flow_node
       SET assignee_type  = 'ADMIN',
           empty_assignee = 'FALLBACK_ADMIN'
     WHERE id = c_node AND version_id = c_ver AND node_key = 'n_review';

    DELETE FROM biz_flow_node_assignee WHERE node_id = c_node;

    UPDATE biz_flow_def_version
       SET graph_snapshot = '{"edges":[{"condition":null,"from":"n_review","is_default":false,"key":"e2","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e1","label":null,"priority":0,"to":"n_review"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":480,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ADMIN","assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_review","max_remind_times":2,"name":"作废复核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"FORBID","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":240,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}'::jsonb,
           graph_hash     = c_new_hash,
           updated_at     = now()
     WHERE id = c_ver AND btrim(graph_hash) = c_old_hash;

    UPDATE biz_flow_def
       SET description = '收货入库单作废的复核流程：发起作废 → 作废复核（超管，禁止自审） → 结束。'
                         '审批通过才执行六本账（库存/库位账/流水/凭证/应付/序列号）回滚；驳回或撤回则单据回到已过账，账不动。',
           updated_at  = now()
     WHERE id = c_def;

    RAISE NOTICE 'V0248 down：作废复核已回滚为 ADMIN + FALLBACK_ADMIN，hash 026766… → 78d34c…';
END $$;

DO $$
DECLARE
    c_ver CONSTANT uuid := '20000000-0000-4000-8000-00000000e174';
    c_node CONSTANT uuid := '20000000-0000-4000-8000-00000000a175';
    c_new_hash CONSTANT text := '78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c';
    v_hash text;
    n int;
BEGIN
    SELECT btrim(graph_hash) INTO v_hash FROM biz_flow_def_version WHERE id = c_ver;
    IF v_hash IS DISTINCT FROM c_new_hash THEN
        RAISE NOTICE 'V0248 down 自检：hash = %（守卫跳过或已回滚），不做内容断言', coalesce(v_hash, '<null>');
        RETURN;
    END IF;
    SELECT count(*) INTO n FROM biz_flow_node
     WHERE id = c_node AND assignee_type = 'ADMIN' AND empty_assignee = 'FALLBACK_ADMIN'
       AND self_approval = 'FORBID';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0248 down 自检失败：作废复核节点没回到 ADMIN + FALLBACK_ADMIN + FORBID';
    END IF;
    SELECT count(*) INTO n FROM biz_flow_node_assignee WHERE node_id = c_node;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0248 down 自检失败：审批人绑定没清干净（残留 % 行）', n;
    END IF;
    RAISE NOTICE 'V0248 down 自检通过：节点与 hash 均已回到 V0174 种子口径';
END $$;
