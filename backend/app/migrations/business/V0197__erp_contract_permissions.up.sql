-- ============================================================
-- V0197：合同的菜单节点 + 5 个权限码（ARCH-2 附录 B 第 7 项）
--
-- 依据：`docs/architecture/ARCH-2-合同表数据模型与校验规则.md` 附录 B #7
--       `erp:contract:{list,create,update,delete,status}`，与 `sys_menu.auth_code`
--       一一对应，`SUPER_ADMIN` 短路放行。
--
-- # 为什么单独一条迁移，而不是塞进 V0195
--
--   实现卡（A1）明确要求 V0195 的 up **只放 §2.2 的 DDL**（表 + 索引 + COMMENT +
--   CT 取号），便于与 ARCH-2 §2.2 逐字对照、也便于日后 `diff` 检查有没有人"顺手加列"。
--   权限码是**菜单/授权域**的事，与表结构生命周期不同（表在、菜单可摘），故分成两条迁移。
--   V0196 已被 ERP-B2（内控开关）占用，本卡顺延到 V0197。
--
-- # 权限码前缀：`erp:contract:*`，不是 `business:erp:contract:*`
--
--   ⚠️ 与 V0141 的 32 个 `business:erp:*` 码**前缀不一致**，这是**照文档契约执行**，
--   不是笔误：ARCH-2 附录 B #7 与实现卡都逐字写了 `erp:contract:*`，而契约是
--   A2（handler）/ A3（采购单挂合同）/ A5（前端）的共同基线 —— 三张卡会按同一个字符串
--   写注解与前端按钮判权。这里若"顺手"改成 `business:erp:contract:*`，A2/A5 就会全部 403。
--   已在 A1 交付说明里向总管报告这处口径分歧，是否统一前缀由总管裁决（改则三卡一起改）。
--
-- # 菜单为什么先 `is_visible = FALSE`
--
--   A5 才建前端页面（`views/business/erp/contract/`）。页面不存在时把菜单挂到侧栏
--   会给人一条点进去 404 的入口。`is_visible = FALSE` 不进侧栏，但
--   **权限码照样装载**（`system::auth::model::load_permissions` 只过滤 `status`，
--   不看 `is_visible`），所以 A2 的接口注解与联调不受影响。A5 落地页面时把
--   本行的 `is_visible` 置 TRUE 即可（一条 UPDATE，不动权限码）。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖运维改过的标题 / 排序）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。ID 段 1100/1101–1104（09xx–10xx 已被
--   ERP 主数据与后续批次占满）。
-- ============================================================

-- ---------- ① 菜单（menu_type 2，auth_code = :list） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001100'::uuid, '合同管理', 2, 'erp:contract:list',
     '/business/erp/contract', NULL, 'business/erp/contract/index',
     75, 1, 'file', '合同管理（采购/销售/服务/租赁）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 按钮级权限（menu_type 3；name 与 auth_code 同值，与 V0141 一致） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001101'::uuid, 'erp:contract:create', 3,
     'erp:contract:create', NULL, '20000000-0000-0000-0000-000000001100'::uuid, NULL,
     10, 1, NULL, '新建合同', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001102'::uuid, 'erp:contract:update', 3,
     'erp:contract:update', NULL, '20000000-0000-0000-0000-000000001100'::uuid, NULL,
     20, 1, NULL, '编辑合同', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001103'::uuid, 'erp:contract:delete', 3,
     'erp:contract:delete', NULL, '20000000-0000-0000-0000-000000001100'::uuid, NULL,
     30, 1, NULL, '删除合同', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001104'::uuid, 'erp:contract:status', 3,
     'erp:contract:status', NULL, '20000000-0000-0000-0000-000000001100'::uuid, NULL,
     40, 1, NULL, '合同状态流转', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权：超管 + laoliu（与 V0141 同口径） ----------
-- 超管按 role_code 短路本就全通，写 sys_role_menu 只是让「角色授权」树的勾选状态正确。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id IN (
       '20000000-0000-0000-0000-000000001100',
       '20000000-0000-0000-0000-000000001101',
       '20000000-0000-0000-0000-000000001102',
       '20000000-0000-0000-0000-000000001103',
       '20000000-0000-0000-0000-000000001104'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- laoliu：用户名或角色名/码含 laoliu 两种口径都授；库里没有则空操作（不报错）。
-- 派生表列名取自 UNION 第一支（`role_id`），别写成 `r.id`（V0141 实测踩过）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT src.role_id, m.id
  FROM sys_menu m
  JOIN (
        SELECT ur.role_id AS role_id
          FROM sys_user_role ur
          JOIN sys_user u ON u.id = ur.user_id
         WHERE u.deleted_at IS NULL
           AND lower(u.username) = 'laoliu'
        UNION
        SELECT r2.id AS role_id
          FROM sys_role r2
         WHERE r2.deleted_at IS NULL
           AND (lower(r2.role_name) LIKE '%laoliu%' OR lower(r2.role_code) LIKE '%laoliu%')
       ) src ON TRUE
 WHERE m.id IN (
       '20000000-0000-0000-0000-000000001100',
       '20000000-0000-0000-0000-000000001101',
       '20000000-0000-0000-0000-000000001102',
       '20000000-0000-0000-0000-000000001103',
       '20000000-0000-0000-0000-000000001104'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = src.role_id AND rm.menu_id = m.id
   );

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    n_codes   integer;
    n_missing integer;
    v_missing text;
BEGIN
    SELECT count(DISTINCT auth_code) INTO n_codes
      FROM sys_menu
     WHERE auth_code LIKE 'erp:contract:%' AND deleted_at IS NULL
       AND status = 1;
    IF n_codes <> 5 THEN
        RAISE EXCEPTION 'V0197 自检失败：erp:contract:* 应有 5 个启用码，实际 %', n_codes;
    END IF;

    SELECT count(*) INTO n_missing
      FROM (VALUES ('erp:contract:list'), ('erp:contract:create'), ('erp:contract:update'),
                   ('erp:contract:delete'), ('erp:contract:status')) AS t(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
    IF n_missing <> 0 THEN
        SELECT string_agg(code, ', ') INTO v_missing
          FROM (VALUES ('erp:contract:list'), ('erp:contract:create'), ('erp:contract:update'),
                       ('erp:contract:delete'), ('erp:contract:status')) AS t(code)
         WHERE NOT EXISTS (
               SELECT 1 FROM sys_menu m
                WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
        RAISE EXCEPTION 'V0197 自检失败：缺权限码 %', v_missing;
    END IF;

    -- 菜单节点必须与前端路由口径一致（A5 落地时只改 is_visible，不改 path/component）
    SELECT count(*) INTO n_missing
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001100'::uuid
       AND (path <> '/business/erp/contract'
            OR component <> 'business/erp/contract/index'
            OR menu_type <> 2
            OR auth_code <> 'erp:contract:list');
    IF n_missing <> 0 THEN
        RAISE EXCEPTION 'V0197 自检失败：合同菜单节点字段与前端路由口径不一致';
    END IF;

    RAISE NOTICE 'V0197 自检通过：5 个 erp:contract:* 权限码 + 合同菜单节点';
END $$;
