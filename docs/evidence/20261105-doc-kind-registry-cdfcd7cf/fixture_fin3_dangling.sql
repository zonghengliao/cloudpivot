-- FIN 三类对象的悬空引用夹具（只读验证，最后整体 ROLLBACK）
\set ON_ERROR_STOP on
BEGIN;

-- ① 预算审批实例：biz_id 指一个不存在的预算行（PENDING ⇒ DI-19 应报 WARN）
--    def_id / version_id / initiator_id 走真表，避免撞外键；biz_type 与 biz_id 才是被测对象
INSERT INTO biz_flow_instance
    (instance_no, def_id, version_id, version_no, flow_code, flow_name,
     biz_type, biz_id, title, status, initiator_id)
SELECT 'FIX-DI-BUD-1', d.id, d.current_version_id, d.current_version_no,
       'budget_approve', '预算审批', 'BUDGET_APPROVE', gen_random_uuid(),
       '夹具：预算审批', 'PENDING', (SELECT id FROM sys_user ORDER BY id LIMIT 1)
  FROM biz_flow_def d
 WHERE d.current_version_id IS NOT NULL
 ORDER BY d.id LIMIT 1;

-- ② 资产处置审批实例：biz_id 指一个不存在的资产卡片
INSERT INTO biz_flow_instance
    (instance_no, def_id, version_id, version_no, flow_code, flow_name,
     biz_type, biz_id, title, status, initiator_id)
SELECT 'FIX-DI-AST-1', d.id, d.current_version_id, d.current_version_no,
       'asset_disposal', '资产处置审批', 'ASSET_DISPOSAL', gen_random_uuid(),
       '夹具：资产处置审批', 'PENDING', (SELECT id FROM sys_user ORDER BY id LIMIT 1)
  FROM biz_flow_def d
 WHERE d.current_version_id IS NOT NULL
 ORDER BY d.id LIMIT 1;

-- ③ 状态日志：预算 / 资产 / 折旧三类实体各一条，entity_id 都不存在
INSERT INTO biz_status_log (entity_type, entity_id, entity_no, to_status) VALUES
    ('BUDGET',        gen_random_uuid(), 'FIX-DI-BUD-LOG', 'APPROVED'),
    ('FIXED_ASSET',   gen_random_uuid(), 'FIX-DI-AST-LOG', 'IN_USE'),
    ('DEPRECIATION',  gen_random_uuid(), 'FIX-DI-DEP-LOG', 'POSTED');

-- ④ 凭证：购入 / 处置 / 折旧三个来源各一张，source_id 指向不存在的行
INSERT INTO biz_voucher
    (voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no, summary,
     total_debit, total_credit, entry_count)
VALUES
    ('FIX-DI-V-ACQ', '2026-11', DATE '2026-11-30', 'AUTO', 'FIXED_ASSET_ACQ',      gen_random_uuid(), 'FIX-A-1', '夹具：资产购入', 100, 100, 2),
    ('FIX-DI-V-DSP', '2026-11', DATE '2026-11-30', 'AUTO', 'FIXED_ASSET_DISPOSAL', gen_random_uuid(), 'FIX-A-2', '夹具：资产处置', 100, 100, 2),
    ('FIX-DI-V-DEP', '2026-11', DATE '2026-11-30', 'AUTO', 'DEPRECIATION',         gen_random_uuid(), '2026-11', '夹具：折旧计提', 100, 100, 2);

\echo '--- DI-17 凭证来源（只看夹具） ---'
SELECT issue_code, entity_no, snapshot_value, truth_value, severity
  FROM di.c_di17 WHERE entity_no LIKE 'FIX-DI-V-%' ORDER BY entity_no;

\echo '--- DI-19 流程实例（只看夹具） ---'
SELECT issue_code, entity_no, snapshot_value, truth_value, severity
  FROM di.c_di19 WHERE entity_no LIKE 'FIX-DI-%' ORDER BY entity_no;

\echo '--- DI-20 状态日志实体（只看夹具） ---'
SELECT issue_code, entity_no, snapshot_value, truth_value, severity
  FROM di.c_di20 WHERE entity_no LIKE 'FIX-DI-%' ORDER BY entity_no;

ROLLBACK;
