\pset pager off
\echo '########## 约束语义实测（单事务 + SAVEPOINT，最后 ROLLBACK，测试库不留数据）##########'
BEGIN;

INSERT INTO sys_role (id, role_code, role_name, status, sort_order)
VALUES ('99990000-0000-0000-0000-00000000000a'::uuid, 'B1_TEST_A', 'B1 测试角色 A', 1, 99),
       ('99990000-0000-0000-0000-00000000000b'::uuid, 'B1_TEST_B', 'B1 测试角色 B', 1, 98);

\echo '--- A. NULLS NOT DISTINCT：role_id 为 NULL 的全局规则插两次，第 2 次必须被拒 ---'
SAVEPOINT sp;
INSERT INTO sys_field_mask (table_name, column_name, mask_type, mask_param, role_id, reason)
VALUES ('sys_user', 'mobile', 1, '3,4', NULL, 'B1 验收：全局默认规则');
\echo '   [A1] 首次插入：期望成功'
SAVEPOINT sp_a;
\set ON_ERROR_STOP off
INSERT INTO sys_field_mask (table_name, column_name, mask_type, mask_param, role_id, reason)
VALUES ('sys_user', 'mobile', 1, '3,4', NULL, 'B1 验收：重复的全局规则（期望被拒）');
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT sp_a;

\echo '--- B. 标识符 CHECK：表名带分号注⼊必须被拒 ---'
SAVEPOINT sp_b;
\set ON_ERROR_STOP off
INSERT INTO sys_field_mask (table_name, column_name, mask_type, reason)
VALUES ('sys_user; DROP TABLE x', 'mobile', 1, 'B1 验收：注入标识符（期望被拒）');
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT sp_b;

\echo '--- C. mask_type 越界（8）必须被拒 ---'
SAVEPOINT sp_c;
\set ON_ERROR_STOP off
INSERT INTO sys_field_mask (table_name, column_name, mask_type, reason)
VALUES ('sys_user', 'email', 8, 'B1 验收：类型越界（期望被拒）');
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT sp_c;

\echo '--- D. 互斥对称唯一：插 (A,B) 成功，反向 (B,A) 必须被拒 ---'
SAVEPOINT sp_d;
INSERT INTO sys_role_mutex (role_a_id, role_b_id, reason)
VALUES ('99990000-0000-0000-0000-00000000000a'::uuid, '99990000-0000-0000-0000-00000000000b'::uuid, 'B1 验收：对称性');
\echo '   [D1] (A,B) 插入：期望成功'
SAVEPOINT sp_d2;
\set ON_ERROR_STOP off
INSERT INTO sys_role_mutex (role_a_id, role_b_id, reason)
VALUES ('99990000-0000-0000-0000-00000000000b'::uuid, '99990000-0000-0000-0000-00000000000a'::uuid, 'B1 验收：反向重复（期望被拒）');
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT sp_d2;

\echo '--- E. 反自反 CHECK：A×A 必须被拒 ---'
SAVEPOINT sp_e;
\set ON_ERROR_STOP off
INSERT INTO sys_role_mutex (role_a_id, role_b_id, reason)
VALUES ('99990000-0000-0000-0000-00000000000a'::uuid, '99990000-0000-0000-0000-00000000000a'::uuid, 'B1 验收：自反（期望被拒）');
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT sp_e;

\echo '--- F. data_scope CHECK：非法值 6 必须被拒 ---'
SAVEPOINT sp_f;
\set ON_ERROR_STOP off
UPDATE sys_role SET data_scope = 6 WHERE role_code = 'B1_TEST_A';
\set ON_ERROR_STOP on
ROLLBACK TO SAVEPOINT sp_f;

\echo '--- G. 事务内落库结果（期望 mutex=1 行、mask=1 行，其余被拒）---'
SELECT (SELECT count(*) FROM sys_role_mutex) AS mutex_rows,
       (SELECT count(*) FROM sys_field_mask WHERE deleted_at IS NULL) AS mask_rows;

\echo '--- H. 合法 data_scope=5 必须成功（CHECK 不误伤）---'
UPDATE sys_role SET data_scope = 5 WHERE role_code = 'B1_TEST_A';
SELECT role_code, data_scope FROM sys_role WHERE role_code IN ('B1_TEST_A','B1_TEST_B') ORDER BY role_code;

ROLLBACK;
\echo '########## 事务已回滚：测试数据不残留 ##########'
SELECT (SELECT count(*) FROM sys_role_mutex) AS mutex_rows_after_rollback,
       (SELECT count(*) FROM sys_field_mask) AS mask_rows_after_rollback;
