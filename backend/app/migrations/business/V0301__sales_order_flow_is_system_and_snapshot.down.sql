-- ============================================================================
-- V0301__sales_order_flow_is_system_and_snapshot.down.sql
--
-- 回滚 V0301：把 `so_approve` 的 `is_system` 改回 FALSE，并把 v1 的快照 / hash 还原成
-- V0281 种下时的字节（边按 key 字母序，hash = da3c2eed…）。
--
-- 说明：
--   · 两处都是精确逆操作，且各自带守卫：打标只回退到「本来就是 FALSE」的状态；
--     快照只在「仍是本迁移写的那份 hash」时才还原 —— 客户若已重新发布 / 手工改图，
--     整段跳过（回滚不许覆盖别人的配置，与 V0248 同口径）；
--   · 幂等：重复执行 0 行变更；
--   · **回滚后 pflow1a 的发布回归会重新变红**（存量 v1 hash 不可由当前序列化器重放），
--     所以 down 只用于「回退本迁移 + 回退依赖它的验收」的完整回滚，不要单独在生产跑。
-- ============================================================================

DO $$
DECLARE
    c_def        CONSTANT uuid := '20000000-0000-4000-8000-0000000d0281';
    c_ver        CONSTANT uuid := '20000000-0000-4000-8000-0000000d0282';
    c_seed_hash  CONSTANT text := 'da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2';
    c_fixed_hash CONSTANT text := '97fe937752c881c0c9b36f7aa8e2a410c2891a4327711fde96fcbf16a0bc6e80';
    c_snapshot   CONSTANT jsonb := $snapshot${"edges":[{"condition":null,"from":"n_finance","is_default":false,"key":"e_finance_mid","label":null,"priority":0,"to":"c_mid"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":10000},"from":"c_low","is_default":false,"key":"e_low","label":"金额 < 10000","priority":0,"to":"e"},{"condition":null,"from":"c_low","is_default":true,"key":"e_low_else","label":"金额 ≥ 10000","priority":10,"to":"n_finance"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":100000},"from":"c_mid","is_default":false,"key":"e_mid","label":"10000 ≤ 金额 < 100000","priority":0,"to":"e"},{"condition":null,"from":"c_mid","is_default":true,"key":"e_mid_else","label":"金额 ≥ 100000","priority":10,"to":"n_gm"},{"condition":null,"from":"n_sales","is_default":false,"key":"e_sales_low","label":null,"priority":0,"to":"c_low"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sales"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_low","max_remind_times":3,"name":"金额分级①（< 1 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_mid","max_remind_times":3,"name":"金额分级②（< 10 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":7,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1300,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_finance","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":1080,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0007","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sales","max_remind_times":2,"name":"销售主管审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb;
BEGIN
    UPDATE biz_flow_def
       SET is_system = FALSE
     WHERE flow_code = 'so_approve'
       AND id = c_def
       AND deleted_at IS NULL
       AND is_system;

    UPDATE biz_flow_def_version
       SET graph_snapshot = c_snapshot,
           graph_hash     = c_seed_hash,
           updated_at     = now()
     WHERE id = c_ver
       AND def_id = c_def
       AND version_no = 1
       AND btrim(graph_hash) = c_fixed_hash;
END $$;
