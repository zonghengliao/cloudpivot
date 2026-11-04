-- ============================================================
-- ARCH-5 · 数据模型负向测试（证明约束真的能拦脏数据）
--
-- 每个用例都在 DO 块里用 EXCEPTION 断言「这条 INSERT 必须失败」。
-- 全部通过 = 结束时打印 `ARCH-5 负向测试 20/20 通过`。
-- 任何一个用例意外成功 → 该 DO 块 RAISE EXCEPTION → psql 退出码非 0。
--
-- 在 arch5_poc_a（cloudpivot_p16md 的副本 + arch5_ddl_poc.sql）上执行。
-- ============================================================

\set ON_ERROR_STOP on
SET client_min_messages = notice;

-- ---------- 0) 造最小合法基线数据 ----------
INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status)
VALUES ('11111111-1111-4111-8111-111111111111', 'arch5_poc_flow', 'POC 流程', 'POC', 'DRAFT')
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status, graph_hash, graph_snapshot, published_at)
VALUES ('22222222-2222-4222-8222-222222222222',
        '11111111-1111-4111-8111-111111111111', 1, 'PUBLISHED',
        repeat('a', 64), '{}'::jsonb, now())
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_node (id, version_id, node_key, node_name, node_type)
VALUES ('33333333-3333-4333-8333-333333333333',
        '22222222-2222-4222-8222-222222222222', 'n1', 'POC 节点', 'TASK_NODE')
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_form_def (id, form_code, form_name, status, schema, schema_hash, published_at, is_current)
VALUES ('44444444-4444-4444-8444-444444444444', 'poc_form', 'POC 表单', 'PUBLISHED',
        '{"fields":[{"key":"amount","type":"MONEY"}]}'::jsonb, repeat('b', 64), now(), true)
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_checklist_def (id, checklist_code, checklist_name, status, published_at, is_current)
VALUES ('55555555-5555-4555-8555-555555555555', 'poc_ckl', 'POC 清单', 'PUBLISHED', now(), true)
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_checklist_item (id, checklist_def_id, item_key, item_text, input_type, pass_rule, min_value, max_value)
VALUES ('66666666-6666-4666-8666-666666666666',
        '55555555-5555-4555-8555-555555555555', 'i1', '压力在范围内',
        'NUMBER', 'RANGE', 0.5, 1.5)
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no,
                               flow_code, flow_name, biz_type, title, initiator_id)
SELECT '77777777-7777-4777-8777-777777777777', 'FLW-POC-0001',
       '11111111-1111-4111-8111-111111111111', '22222222-2222-4222-8222-222222222222', 1,
       'arch5_poc_flow', 'POC 流程', 'POC', 'POC 实例', u.id
  FROM sys_user u ORDER BY u.id LIMIT 1
ON CONFLICT DO NOTHING;

INSERT INTO biz_flow_instance_node (id, instance_id, node_key, node_name, node_type)
VALUES ('88888888-8888-4888-8888-888888888888',
        '77777777-7777-4777-8777-777777777777', 'n1', 'POC 节点', 'TASK_NODE')
ON CONFLICT DO NOTHING;

-- ---------- 1) 表单定义 ----------

-- 1. 空 fields 数组的表单不许存在
DO $$ BEGIN
  INSERT INTO biz_flow_form_def (form_code, form_name, schema)
  VALUES ('bad_form_1', '空表单', '{"fields":[]}'::jsonb);
  RAISE EXCEPTION 'CASE 1 未拦住：空 fields 数组的表单被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 01 ok  空 fields 数组被 ck_biz_flow_form_def_schema 拦住';
END $$;

-- 2. PUBLISHED 却没有 schema_hash
DO $$ BEGIN
  INSERT INTO biz_flow_form_def (form_code, form_name, status, schema)
  VALUES ('bad_form_2', '发布无指纹', 'PUBLISHED', '{"fields":[{"key":"a","type":"TEXT"}]}'::jsonb);
  RAISE EXCEPTION 'CASE 2 未拦住：PUBLISHED 无 schema_hash 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 02 ok  PUBLISHED 缺指纹被 ck_biz_flow_form_def_published 拦住';
END $$;

-- 3. DRAFT 版本不许 is_current（否则「当前版本」指向一个没人发布过的东西）
DO $$ BEGIN
  INSERT INTO biz_flow_form_def (form_code, form_name, status, is_current, schema)
  VALUES ('bad_form_3', '草稿当前', 'DRAFT', true, '{"fields":[{"key":"a","type":"TEXT"}]}'::jsonb);
  RAISE EXCEPTION 'CASE 3 未拦住：DRAFT 挂 is_current 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 03 ok  DRAFT 挂 is_current 被 ck_biz_flow_form_def_current 拦住';
END $$;

-- 4. 同 code 两个当前版本（两个「当前」= 不可判定）
DO $$ BEGIN
  INSERT INTO biz_flow_form_def (form_code, form_name, version_no, status, is_current, schema, schema_hash, published_at)
  VALUES ('poc_form', 'POC 表单 v2', 2, 'PUBLISHED', true,
          '{"fields":[{"key":"b","type":"TEXT"}]}'::jsonb, repeat('c', 64), now());
  RAISE EXCEPTION 'CASE 4 未拦住：同 code 出现第二个 is_current';
EXCEPTION WHEN unique_violation THEN
  RAISE NOTICE 'CASE 04 ok  第二个 is_current 被 uk_biz_flow_form_def_current 拦住';
END $$;

-- ---------- 2) 节点绑定 ----------

-- 5. FORM 节点不绑表单（画布上画了个「表单节点」但不知道填什么）
DO $$ BEGIN
  INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type)
  VALUES ('22222222-2222-4222-8222-222222222222', 'bad_form_node', '无表单的表单节点', 'FORM');
  RAISE EXCEPTION 'CASE 5 未拦住：FORM 节点无 form_def_id 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 05 ok  FORM 节点缺 form_def_id 被 ck_biz_flow_node_work_binding 拦住';
END $$;

-- 6. CHECKLIST 节点错绑表单（绑定列与类型错配）
DO $$ BEGIN
  INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type, form_def_id)
  VALUES ('22222222-2222-4222-8222-222222222222', 'bad_mix_node', '错配节点', 'CHECKLIST',
          '44444444-4444-4444-8444-444444444444');
  RAISE EXCEPTION 'CASE 6 未拦住：CHECKLIST 节点绑了 form_def_id';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 06 ok  作业形态绑定错配被 ck_biz_flow_node_work_binding 拦住';
END $$;

-- 7. 同一版本两个验证节点（闭环里「谁说了算」不可判定）
DO $$ BEGIN
  INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type, closure_role)
  VALUES ('22222222-2222-4222-8222-222222222222', 'v1', '验证节点1', 'TASK_NODE', 'VERIFIER');
  INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type, closure_role)
  VALUES ('22222222-2222-4222-8222-222222222222', 'v2', '验证节点2', 'TASK_NODE', 'VERIFIER');
  RAISE EXCEPTION 'CASE 7 未拦住：同版本出现两个 VERIFIER';
EXCEPTION WHEN unique_violation THEN
  RAISE NOTICE 'CASE 07 ok  第二个验证节点被 uk_biz_flow_node_verifier 拦住';
END $$;

-- 8. ESCALATE 但没配升级级数（「升级」不知道升到谁）
DO $$ BEGIN
  INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type, timeout_action, escalate_levels)
  VALUES ('22222222-2222-4222-8222-222222222222', 'bad_esc', '升级节点', 'APPROVAL', 'ESCALATE', 0);
  RAISE EXCEPTION 'CASE 8 未拦住：ESCALATE + escalate_levels=0 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 08 ok  ESCALATE 缺升级级数被 ck_biz_flow_node_escalate 拦住';
END $$;

-- 9. node_type 不在 15 值白名单
DO $$ BEGIN
  INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type)
  VALUES ('22222222-2222-4222-8222-222222222222', 'bad_type', '野节点', 'FOO');
  RAISE EXCEPTION 'CASE 9 未拦住：未知 node_type 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 09 ok  未知 node_type 被 biz_flow_node_node_type_check 拦住';
END $$;

-- ---------- 3) 检查项 / 检查结果 ----------

-- 10. RANGE 判合格却没有边界（判不了「合格」）
DO $$ BEGIN
  INSERT INTO biz_flow_checklist_item (checklist_def_id, item_key, item_text, input_type, pass_rule)
  VALUES ('55555555-5555-4555-8555-555555555555', 'bad_range', '空边界', 'NUMBER', 'RANGE');
  RAISE EXCEPTION 'CASE 10 未拦住：RANGE 无边界被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 10 ok  RANGE 无边界被 ck_biz_flow_checklist_item_judgeable 拦住';
END $$;

-- 11. 允许跳过却没给权限码（谁都能跳 = 防漏形同虚设）
DO $$ BEGIN
  INSERT INTO biz_flow_checklist_item (checklist_def_id, item_key, item_text, input_type, allow_skip)
  VALUES ('55555555-5555-4555-8555-555555555555', 'bad_skip', '随便跳', 'BOOLEAN', true);
  RAISE EXCEPTION 'CASE 11 未拦住：allow_skip 无权限码被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 11 ok  跳过无权限码被 ck_biz_flow_checklist_item_skip 拦住';
END $$;

-- 12. min_value > max_value
DO $$ BEGIN
  INSERT INTO biz_flow_checklist_item (checklist_def_id, item_key, item_text, input_type, pass_rule, min_value, max_value)
  VALUES ('55555555-5555-4555-8555-555555555555', 'bad_order', '倒挂区间', 'NUMBER', 'RANGE', 9, 1);
  RAISE EXCEPTION 'CASE 12 未拦住：min > max 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 12 ok  倒挂区间被 ck_biz_flow_checklist_item_range 拦住';
END $$;

-- 13. SKIPPED 不填原因
DO $$ BEGIN
  INSERT INTO biz_flow_checklist_result (instance_id, instance_node_id, node_key, round,
                                         checklist_def_id, item_id, item_key, result)
  VALUES ('77777777-7777-4777-8777-777777777777', '88888888-8888-4888-8888-888888888888',
          'n1', 1, '55555555-5555-4555-8555-555555555555',
          '66666666-6666-4666-8666-666666666666', 'i1', 'SKIPPED');
  RAISE EXCEPTION 'CASE 13 未拦住：SKIPPED 无原因被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 13 ok  跳过无原因被 ck_biz_flow_checklist_result_skip 拦住';
END $$;

-- 14. 同一节点同一轮同一项两条结果（防漏的库级拦点）
DO $$ BEGIN
  INSERT INTO biz_flow_checklist_result (instance_id, instance_node_id, node_key, round,
                                         checklist_def_id, item_id, item_key, result)
  VALUES ('77777777-7777-4777-8777-777777777777', '88888888-8888-4888-8888-888888888888',
          'n1', 1, '55555555-5555-4555-8555-555555555555',
          '66666666-6666-4666-8666-666666666666', 'i1', 'PASS');
  INSERT INTO biz_flow_checklist_result (instance_id, instance_node_id, node_key, round,
                                         checklist_def_id, item_id, item_key, result)
  VALUES ('77777777-7777-4777-8777-777777777777', '88888888-8888-4888-8888-888888888888',
          'n1', 1, '55555555-5555-4555-8555-555555555555',
          '66666666-6666-4666-8666-666666666666', 'i1', 'FAIL');
  RAISE EXCEPTION 'CASE 14 未拦住：同一项一轮写了两条结果';
EXCEPTION WHEN unique_violation THEN
  RAISE NOTICE 'CASE 14 ok  重复结果被 uk_biz_flow_checklist_result 拦住';
END $$;

-- ---------- 4) 证据 / 归档 ----------

-- 15. 证据要求 min_count > max_count
DO $$ BEGIN
  INSERT INTO biz_flow_evidence_req (node_id, req_key, req_name, min_count, max_count)
  VALUES ('33333333-3333-4333-8333-333333333333', 'r1', '照片', 5, 2);
  RAISE EXCEPTION 'CASE 15 未拦住：min>max 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 15 ok  证据数量倒挂被 ck_biz_flow_evidence_req_count 拦住';
END $$;

-- 16. SIGN 类型却不要签（自相矛盾）
DO $$ BEGIN
  INSERT INTO biz_flow_evidence_req (node_id, req_key, req_name, evidence_type, require_sign)
  VALUES ('33333333-3333-4333-8333-333333333333', 'r2', '签署件', 'SIGN', false);
  RAISE EXCEPTION 'CASE 16 未拦住：SIGN 类型 require_sign=false 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 16 ok  SIGN 不签被 ck_biz_flow_evidence_req_sign 拦住';
END $$;

-- 17. 归档包 READY 却没有任何产物
DO $$ BEGIN
  INSERT INTO biz_flow_archive_pack (instance_id, pack_no, status, format, sha256, built_at)
  VALUES ('77777777-7777-4777-8777-777777777777', 'PKG-POC-1', 'READY', 'ZIP', repeat('d',64), now());
  RAISE EXCEPTION 'CASE 17 未拦住：READY 空包被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 17 ok  READY 空包被 ck_biz_flow_archive_pack_ready 拦住';
END $$;

-- ---------- 5) 触发器 ----------

-- 18. CRON 触发器没有 cron 表达式
DO $$ BEGIN
  INSERT INTO biz_flow_trigger (def_id, trigger_name, trigger_kind)
  VALUES ('11111111-1111-4111-8111-111111111111', '空 cron', 'CRON');
  RAISE EXCEPTION 'CASE 18 未拦住：CRON 无 cron_expr 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 18 ok  空 cron 被 ck_biz_flow_trigger_shape 拦住';
END $$;

-- 19. 触发器配置里夹带 SQL（配置即代码的红线）
DO $$ BEGIN
  INSERT INTO biz_flow_trigger (def_id, trigger_name, trigger_kind, event_code, match_expr)
  VALUES ('11111111-1111-4111-8111-111111111111', '偷 SQL', 'EVENT', 'QC_FAIL',
          '{"sql":"delete from biz_task"}'::jsonb);
  RAISE EXCEPTION 'CASE 19 未拦住：match_expr 夹带 sql 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 19 ok  夹带 SQL 被 ck_biz_flow_trigger_no_sql 拦住';
END $$;

-- 20. 幂等台账重复（同一事件第二次触发必须被库拦住）
DO $$ BEGIN
  INSERT INTO biz_flow_trigger (id, def_id, trigger_name, trigger_kind, event_code)
  VALUES ('99999999-9999-4999-8999-999999999999',
          '11111111-1111-4111-8111-111111111111', '事件触发', 'EVENT', 'QC_FAIL');
  INSERT INTO biz_flow_trigger_fire (trigger_id, dedupe_key, status)
  VALUES ('99999999-9999-4999-8999-999999999999', 'QC_FAIL:batch-001', 'FIRED');
  INSERT INTO biz_flow_trigger_fire (trigger_id, dedupe_key, status)
  VALUES ('99999999-9999-4999-8999-999999999999', 'QC_FAIL:batch-001', 'FIRED');
  RAISE EXCEPTION 'CASE 20 未拦住：同一 dedupe_key 触发两次';
EXCEPTION WHEN unique_violation THEN
  RAISE NOTICE 'CASE 20 ok  重复触发被 uk_biz_flow_trigger_fire 拦住';
END $$;

-- 21. 闭环 ARCHIVE 却没有归档策略
DO $$ BEGIN
  INSERT INTO biz_flow_def (flow_code, flow_name, biz_type, closure_mode, archive_policy)
  VALUES ('bad_closure', '闭环无归档', 'POC', 'ARCHIVE', 'NONE');
  RAISE EXCEPTION 'CASE 21 未拦住：closure_mode=ARCHIVE + archive_policy=NONE 被接受';
EXCEPTION WHEN check_violation THEN
  RAISE NOTICE 'CASE 21 ok  闭环无归档被 ck_biz_flow_def_closure_archive 拦住';
END $$;

-- ---------- 汇总 ----------
DO $$ BEGIN
  RAISE NOTICE '================================================';
  RAISE NOTICE 'ARCH-5 负向测试完成：21 个用例，18 条约束逐条实测被拦';
  RAISE NOTICE '================================================';
END $$;
