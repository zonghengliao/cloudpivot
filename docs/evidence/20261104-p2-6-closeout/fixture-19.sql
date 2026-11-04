-- ============================================================
-- t_485a84dd 收口 D 验收夹具（**仅 cloudpivot_p26**，生产库绝不执行）
--   形态① 真悬空/非豁免：见 drill.sh 步骤 1a（软删真收货单）、1b（同 doc_type 不同行 id）
--   形态② PENDING 实例 → 软删任务   ⇒ 期望 DI-19 WARN
--   形态③ APPROVED 实例 → 软删任务  ⇒ 期望 DI-19 INFO
--   形态④ 豁免台账行 + 指向软删序列号的流水 ⇒ 有效期内 INFO / expires_on 改成昨天 ⇒ ERROR
-- ============================================================
BEGIN;

-- ---------- ②③ 两张「已软删」的任务（业务对象）----------
INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, deleted_at)
VALUES
  ('01a1f900-0000-7000-8000-000000000001', 'TSK-D4-19P', '形态②：PENDING 实例指向的任务（已软删）',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', '01a1a5a6-a445-758d-8695-2ef67fdb41b8', now()),
  ('01a1f900-0000-7000-8000-000000000002', 'TSK-D4-19T', '形态③：APPROVED 实例指向的任务（已软删）',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', '01a1a5a6-a445-758d-8695-2ef67fdb41b8', now());

-- ---------- ②③ 两个实例：一个 PENDING、一个 APPROVED（终态）----------
INSERT INTO biz_flow_instance
  (id, instance_no, def_id, version_id, version_no, flow_code, flow_name,
   biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
   form_data, started_at, ended_at, created_at, updated_at)
VALUES
  ('01a1f900-0000-7000-8000-0000000000a1', 'FLW-D4-0001',
   '20000000-0000-4000-8000-0000000b0182', '20000000-0000-4000-8000-0000000b0183', 1,
   'task_accept', '任务验收审批', 'TASK_ACCEPT',
   '01a1f900-0000-7000-8000-000000000001', 'TSK-D4-19P',
   '形态② PENDING 实例 → 已软删任务', 'PENDING',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', 'admin', '{}'::jsonb,
   now(), NULL, now(), now()),
  ('01a1f900-0000-7000-8000-0000000000a2', 'FLW-D4-0002',
   '20000000-0000-4000-8000-0000000b0182', '20000000-0000-4000-8000-0000000b0183', 1,
   'task_accept', '任务验收审批', 'TASK_ACCEPT',
   '01a1f900-0000-7000-8000-000000000002', 'TSK-D4-19T',
   '形态③ APPROVED（终态）实例 → 已软删任务', 'APPROVED',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', 'admin', '{}'::jsonb,
   now(), now(), now(), now());

COMMIT;
