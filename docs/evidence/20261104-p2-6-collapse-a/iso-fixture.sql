-- ============================================================
-- t_237f1caa 隔离库验收夹具（仅 cloudpivot_p26，生产库绝不执行）
--   ① PENDING 实例 + 软删业务对象    ⇒ 期望 DI-19 WARN
--   ② 终态实例 + 软删业务对象        ⇒ 期望 DI-19 INFO
--   ③ 流水指向软删序列号 + 有效豁免  ⇒ 期望 DI-18 INFO（带「已核查豁免#」）
--      然后 b) 步骤把豁免 expires_on 改成昨天 ⇒ 期望 DI-18 回 ERROR
-- ============================================================
BEGIN;

-- ---------- 共同引用 ----------
-- admin 用户：01a1a5a6-a445-758d-8695-2ef67fdb41b8
-- 流程定义/版本：20000000-0000-4000-8000-0000000b0182 / ...b0183（沿用 p26 已有实例）
-- 物料 / 仓库：e2040000-0000-4000-8000-000000000011 / ...003

-- ---------- ①② 两张「已软删」的任务（业务对象）----------
INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, deleted_at)
VALUES
  ('01a1f900-0000-7000-8000-000000000001', 'TSK-ISO-19P', 'DI-19 场景①：PENDING 实例指向的任务（已软删）',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', '01a1a5a6-a445-758d-8695-2ef67fdb41b8', now()),
  ('01a1f900-0000-7000-8000-000000000002', 'TSK-ISO-19T', 'DI-19 场景②：终态实例指向的任务（已软删）',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', '01a1a5a6-a445-758d-8695-2ef67fdb41b8', now());

-- ---------- ①② 两个实例：一个 PENDING、一个 CANCELLED（终态）----------
INSERT INTO biz_flow_instance
  (id, instance_no, def_id, version_id, version_no, flow_code, flow_name,
   biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
   form_data, started_at, ended_at, created_at, updated_at)
VALUES
  ('01a1f900-0000-7000-8000-0000000000a1', 'FLW-ISO-0001',
   '20000000-0000-4000-8000-0000000b0182', '20000000-0000-4000-8000-0000000b0183', 1,
   'task_accept', '任务验收审批', 'TASK_ACCEPT',
   '01a1f900-0000-7000-8000-000000000001', 'TSK-ISO-19P',
   'DI-19 场景① PENDING 实例 → 已软删任务', 'PENDING',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', 'admin', '{}'::jsonb,
   now(), NULL, now(), now()),
  ('01a1f900-0000-7000-8000-0000000000a2', 'FLW-ISO-0002',
   '20000000-0000-4000-8000-0000000b0182', '20000000-0000-4000-8000-0000000b0183', 1,
   'task_accept', '任务验收审批', 'TASK_ACCEPT',
   '01a1f900-0000-7000-8000-000000000002', 'TSK-ISO-19T',
   'DI-19 场景② CANCELLED（终态）实例 → 已软删任务', 'CANCELLED',
   '01a1a5a6-a445-758d-8695-2ef67fdb41b8', 'admin', '{}'::jsonb,
   now(), now(), now(), now());

-- ---------- ③ 一条已软删的序列号 + 一条指向它的流水（id = 生产豁免台账里的那一行）----------
INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, deleted_at)
VALUES ('01a1f900-0000-7000-8000-0000000000d1', 'SN-ISO-DI18-000001',
        'e2040000-0000-4000-8000-000000000011', 'e2040000-0000-4000-8000-000000000003',
        'IN_STOCK', now());

INSERT INTO biz_inventory_txn
  (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
   qty, unit_cost, amount, before_qty, before_amount, before_cost,
   after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at)
VALUES
  ('01a19108-ee53-73c6-bdaa-0109614ea963',
   'e2040000-0000-4000-8000-000000000011', 'e2040000-0000-4000-8000-000000000003',
   'IN', 'PURCHASE_IN', 'SERIAL_BATCH', '01a1f900-0000-7000-8000-0000000000d1',
   'ISO-DI18-WAIVER-0001', 1, 10, 0, 0, 0, 0, 0, 10, 0, 0, '2026-11', NULL,
   '隔离库夹具：验证 DI-18 已核查豁免/到期回红（非生产数据）', now());

COMMIT;
