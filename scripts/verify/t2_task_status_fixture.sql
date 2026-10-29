-- ======================================================
-- T2 接口走查夹具（在验证库 cloudpivot_t2v 上执行）
--
-- 造一套跨部门组织 + 4 个普通账号（密码哈希直接抄 admin，便于真登录），
-- 用于「状态机白名单 / 进度汇总 / 权限边界 / 可见性」的 HTTP 层走查。
--
-- 与 T1 夹具（t1_task_api_fixture.sql）的差异，逐条说明：
--   ① 角色只给**任务管理的 8 个节点**（页面 + 4 个 CRUD + T2 的 status/progress/subtask），
--      数据范围 data_scope=4（仅本人）—— 于是「无关用户看不见别人的任务」是默认行为，
--      不需要额外造部门层级来制造隔离；
--   ② 多造一个「协作人」账号（t2_collab，市场部），由走查脚本通过
--      `biz_task_member` 直接插入（T3 才有协作人接口）—— 用来验证
--      「协作人看得见但推不动状态」（403）；
--   ③ 三级嵌套 A→B→C 由本文件的「§5 三级嵌套」段**直接 SQL 造**：
--      T1 的 `validate_parent` 把业务深度限在 2 层（设计文档 §十二），
--      API 造不出第 3 层。T2 的父任务重算必须能沿 `parent_id` 逐级向上
--      （卡面「深度通常 ≤3」），所以用历史数据形态验证「第 3 层完成 → 两层祖先都重算」。
--
-- 幂等：先按固定 UUID / 标题前缀删一遍，再插。
-- ======================================================

-- ---------- 0) 清理（反外键序；只删本夹具造的东西） ----------
DELETE FROM sys_change_log
 WHERE resource = 'biz_task'
   AND resource_id IN (SELECT id::text FROM biz_task WHERE title LIKE 'T2走查-%');

DELETE FROM biz_task WHERE title LIKE 'T2走查-%';
DELETE FROM biz_task WHERE id IN ('7e000000-0000-0000-0000-000000000001'::uuid,
                                  '7e000000-0000-0000-0000-000000000002'::uuid,
                                  '7e000000-0000-0000-0000-000000000003'::uuid,
                                  '7e000000-0000-0000-0000-000000000004'::uuid);

DELETE FROM sys_user_role WHERE user_id IN (
    '7d000000-0000-0000-0000-000000000011'::uuid,
    '7d000000-0000-0000-0000-000000000012'::uuid,
    '7d000000-0000-0000-0000-000000000013'::uuid,
    '7d000000-0000-0000-0000-000000000014'::uuid);
DELETE FROM sys_user WHERE id IN (
    '7d000000-0000-0000-0000-000000000011'::uuid,
    '7d000000-0000-0000-0000-000000000012'::uuid,
    '7d000000-0000-0000-0000-000000000013'::uuid,
    '7d000000-0000-0000-0000-000000000014'::uuid);
DELETE FROM sys_role_menu WHERE role_id = '7b000000-0000-0000-0000-000000000011'::uuid;
DELETE FROM sys_role WHERE id = '7b000000-0000-0000-0000-000000000011'::uuid;
DELETE FROM sys_dept WHERE id IN (
    '7a000000-0000-0000-0000-000000000011'::uuid,
    '7a000000-0000-0000-0000-000000000013'::uuid);

-- ---------- 1) 部门 ----------
INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order) VALUES
    ('7a000000-0000-0000-0000-000000000011'::uuid, 'T2_R', 'T2研发部', NULL, 1, 10),
    ('7a000000-0000-0000-0000-000000000013'::uuid, 'T2_M', 'T2市场部', NULL, 1, 30);

-- ---------- 2) 角色（data_scope=4 仅本人） ----------
INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES
    ('7b000000-0000-0000-0000-000000000011'::uuid, 'T2_SELF', 'T2仅本人', 1, 4);

-- 新角色不在种子迁移的授权范围内（种子只在启动时给「当时已有的角色」授权），
-- 这里显式授权 —— 等价于运维在角色授权页勾上「工作任务」8 个节点。
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000760'::uuid),
                    ('20000000-0000-0000-0000-000000000761'::uuid),
                    ('20000000-0000-0000-0000-000000000762'::uuid),
                    ('20000000-0000-0000-0000-000000000763'::uuid),
                    ('20000000-0000-0000-0000-000000000764'::uuid),
                    ('20000000-0000-0000-0000-000000000765'::uuid),
                    ('20000000-0000-0000-0000-000000000766'::uuid),
                    ('20000000-0000-0000-0000-000000000767'::uuid)) AS m(id)
 WHERE r.id = '7b000000-0000-0000-0000-000000000011'::uuid
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

-- ---------- 3) 用户（密码哈希抄 admin） ----------
-- 验证库里的 admin 是 bootstrap 新建的，带 must_change_password=TRUE（首登改密），
-- 会让除改密/登出以外的接口全部 403。走查要拿 admin 令牌调接口，所以在这里关掉
-- ——**只对验证库**（cloudpivot_t2v）这么做。
UPDATE sys_user SET must_change_password = FALSE WHERE username = 'admin' AND deleted_at IS NULL;

INSERT INTO sys_user (id, username, password, real_name, dept_id, status, must_change_password)
SELECT u.id, u.username, a.password, u.real_name, u.dept_id, 1, FALSE
  FROM (VALUES
        ('7d000000-0000-0000-0000-000000000011'::uuid, 't2_owner',  'T2负责人（研发部）',
         '7a000000-0000-0000-0000-000000000011'::uuid),
        ('7d000000-0000-0000-0000-000000000012'::uuid, 't2_maker',  'T2创建人（研发部）',
         '7a000000-0000-0000-0000-000000000011'::uuid),
        ('7d000000-0000-0000-0000-000000000013'::uuid, 't2_other',  'T2无关用户（市场部）',
         '7a000000-0000-0000-0000-000000000013'::uuid),
        ('7d000000-0000-0000-0000-000000000014'::uuid, 't2_collab', 'T2协作人（市场部）',
         '7a000000-0000-0000-0000-000000000013'::uuid)
       ) AS u(id, username, real_name, dept_id)
 CROSS JOIN (SELECT password FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL) AS a;

INSERT INTO sys_user_role (user_id, role_id) VALUES
    ('7d000000-0000-0000-0000-000000000011'::uuid, '7b000000-0000-0000-0000-000000000011'::uuid),
    ('7d000000-0000-0000-0000-000000000012'::uuid, '7b000000-0000-0000-0000-000000000011'::uuid),
    ('7d000000-0000-0000-0000-000000000013'::uuid, '7b000000-0000-0000-0000-000000000011'::uuid),
    ('7d000000-0000-0000-0000-000000000014'::uuid, '7b000000-0000-0000-0000-000000000011'::uuid);

-- ---------- 4) 三级嵌套 A → B → C（第 3 层只能这么造，见文件头 ③） ----------
--   A（7e…01）2 个子任务：B（7e…02，2h，已完成链）与 D（7e…04，2h，进度 0）
--   B（7e…02）1 个子任务：C（7e…03，2h，走查脚本会把 C 推到 DONE）
--   期望：C 完成后 B = 100，A = (2×100 + 2×0) / 4 = 50
INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review,
                      owner_id, creator_id, dept_id, parent_id, estimate_hours)
VALUES
    ('7e000000-0000-0000-0000-000000000001'::uuid, 'TSK-T2V-900001', 'T2走查-三级A', 'DOING', 'P2', 0, FALSE,
     '7d000000-0000-0000-0000-000000000012'::uuid, '7d000000-0000-0000-0000-000000000012'::uuid,
     '7a000000-0000-0000-0000-000000000011'::uuid, NULL, NULL),
    ('7e000000-0000-0000-0000-000000000002'::uuid, 'TSK-T2V-900002', 'T2走查-三级B', 'DOING', 'P2', 0, FALSE,
     '7d000000-0000-0000-0000-000000000012'::uuid, '7d000000-0000-0000-0000-000000000012'::uuid,
     '7a000000-0000-0000-0000-000000000011'::uuid, '7e000000-0000-0000-0000-000000000001'::uuid, 2),
    ('7e000000-0000-0000-0000-000000000003'::uuid, 'TSK-T2V-900003', 'T2走查-三级C', 'TODO', 'P2', 0, FALSE,
     '7d000000-0000-0000-0000-000000000012'::uuid, '7d000000-0000-0000-0000-000000000012'::uuid,
     '7a000000-0000-0000-0000-000000000011'::uuid, '7e000000-0000-0000-0000-000000000002'::uuid, 2),
    ('7e000000-0000-0000-0000-000000000004'::uuid, 'TSK-T2V-900004', 'T2走查-三级D', 'TODO', 'P2', 0, FALSE,
     '7d000000-0000-0000-0000-000000000012'::uuid, '7d000000-0000-0000-0000-000000000012'::uuid,
     '7a000000-0000-0000-0000-000000000011'::uuid, '7e000000-0000-0000-0000-000000000001'::uuid, 2);

-- ---------- 5) 自检 ----------
DO $$
DECLARE
    n int;
BEGIN
    SELECT count(*) INTO n FROM sys_user
     WHERE username IN ('t2_owner', 't2_maker', 't2_other', 't2_collab') AND deleted_at IS NULL;
    IF n <> 4 THEN
        RAISE EXCEPTION 'T2 夹具失败：只建出 % 个账号（应为 4）', n;
    END IF;
    SELECT count(*) INTO n
      FROM sys_role_menu rm
     WHERE rm.role_id = '7b000000-0000-0000-0000-000000000011'::uuid;
    IF n <> 8 THEN
        RAISE EXCEPTION 'T2 夹具失败：T2_SELF 角色只拿到 % 个任务节点（应为 8：页面 + 7 按钮）', n;
    END IF;
    SELECT count(*) INTO n FROM biz_task
     WHERE id::text LIKE '7e000000-0000-0000-0000-00000000000%';
    IF n <> 4 THEN
        RAISE EXCEPTION 'T2 夹具失败：三级嵌套只建出 % 行（应为 4）', n;
    END IF;
END
$$;
