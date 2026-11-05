-- ============================================================================
-- 计划展开：把「角色 × (path,menu_type) × mode」展开成要写进 sys_role_menu 的
-- (role_id, menu_id) 集合 —— 建临时表 `_sel`；同时定义要新建的 3 条按钮码 `_new_menus`。
--
-- 展开规则（顺序即安全边界）：
--   1. 目标节点必须**唯一命中**（否则 RAISE，计划与库不一致就别动手）
--   2. FULL → 节点 + 全部后代（递归）
--      READ → 节点 + 只读动作的后代（白名单见 _read_actions）
--      NODE → 只有节点
--   3. **祖先闭包**：非超管侧边栏靠 sys_menu 树渲染，父目录没授权整棵子树会被
--      build_tree 静默丢掉（A 卡 §1.5 踩过）⇒ 每个被授节点都要把祖先目录补上
--   4. 只增不减：本脚本不产出任何 DELETE（回退走 20_grant.down.sql 按确定性 id 删）
-- ============================================================================

-- 只读动作白名单（末段动作名；动作是授权的最小单位，整页 READ 时只放这些）
DROP TABLE IF EXISTS _read_actions;
CREATE TEMP TABLE _read_actions (action text PRIMARY KEY);
INSERT INTO _read_actions VALUES
  ('list'), ('read'), ('detail'), ('view'), ('export'), ('download'), ('mine'),
  ('template'), ('aux-balance'), ('trial-balance'), ('health'), ('self-check'),
  ('cancel-impact');

-- ---------- 1) 目标节点（唯一命中断言） ----------
DROP TABLE IF EXISTS _target;
CREATE TABLE _target AS
SELECT p.role_code, p.mode, m.id, m.auth_code, m.menu_type, m.path
  FROM _plan p
  JOIN sys_menu m ON m.path = p.path AND m.menu_type = p.mtype AND m.deleted_at IS NULL;

DO $$
DECLARE miss text;
BEGIN
SELECT string_agg(p.role_code || ':' || p.path || '(' || p.mtype || ')', ', ') INTO miss
  FROM _plan p
 WHERE NOT EXISTS (SELECT 1 FROM _target t
                    WHERE t.role_code = p.role_code AND t.path = p.path AND t.menu_type = p.mtype);
IF miss IS NOT NULL THEN
    RAISE EXCEPTION '计划与库不一致：以下条目没有唯一命中 sys_menu → %', miss;
END IF;
END $$;

-- ---------- 2) 后代展开 + 3) 只读过滤 ----------
DROP TABLE IF EXISTS _desc;
CREATE TABLE _desc AS
WITH RECURSIVE d AS (
    SELECT t.role_code, t.mode, m.id, m.auth_code, m.menu_type
      FROM _target t
      JOIN sys_menu m ON m.parent_id = t.id AND m.deleted_at IS NULL
    UNION ALL
    SELECT d.role_code, d.mode, m.id, m.auth_code, m.menu_type
      FROM d JOIN sys_menu m ON m.parent_id = d.id AND m.deleted_at IS NULL
)
SELECT * FROM d;

DROP TABLE IF EXISTS _kept;
CREATE TABLE _kept AS
SELECT DISTINCT role_code, id FROM (
    SELECT role_code, id FROM _target WHERE mode IN ('FULL', 'NODE')
    UNION ALL
    SELECT role_code, id FROM _target WHERE mode = 'READ'   -- 页面自身（list 码）总是要
    UNION ALL
    SELECT d.role_code, d.id FROM _desc d WHERE d.mode = 'FULL'
    UNION ALL
    SELECT d.role_code, d.id FROM _desc d
      JOIN sys_menu m ON m.id = d.id
     WHERE d.mode = 'READ'
       AND (d.menu_type IN (1, 2)
            OR substring(m.auth_code from '([^:]+)$') IN (SELECT action FROM _read_actions))
) x;

-- ---------- 3) 祖先闭包 ----------
DROP TABLE IF EXISTS _anc;
CREATE TABLE _anc AS
WITH RECURSIVE up AS (
    SELECT k.role_code, m.parent_id AS id
      FROM _kept k JOIN sys_menu m ON m.id = k.id
     WHERE m.parent_id IS NOT NULL
    UNION
    SELECT up.role_code, m.parent_id
      FROM up JOIN sys_menu m ON m.id = up.id
     WHERE m.parent_id IS NOT NULL
)
SELECT DISTINCT role_code, id FROM up WHERE id IS NOT NULL;

DROP TABLE IF EXISTS _sel;
CREATE TABLE _sel AS
SELECT role_code, id FROM _kept
UNION
SELECT role_code, id FROM _anc;

-- ---------- 4) 安全断言 ----------
DO $$
DECLARE n int;
BEGIN
    -- 4.1 计划里出现 SUPER_ADMIN 直接炸（目标态：全平台只有 admin 一个超管，永不扩员）
    SELECT count(*) INTO n FROM _plan WHERE role_code = 'SUPER_ADMIN';
    IF n > 0 THEN RAISE EXCEPTION '禁止在计划里出现 SUPER_ADMIN（目标态只有 admin 一个）'; END IF;

    -- 4.2 平台治理页绝不能被授给业务角色（用户/角色/部门/菜单/数据权限/脱敏/互斥/变更日志/
    --     存储配置/支持台）—— 这是一条硬红线，写在代码里而不是靠人记
    SELECT count(*) INTO n
      FROM _sel s JOIN sys_menu m ON m.id = s.id
     WHERE m.path IN ('/system/user','/system/role','/system/dept','/system/menu',
                      '/system/data-scope','/system/field-mask','/system/role-mutex',
                      '/system/change-log','/business/storage/backend',
                      '/support/config','/support/dict','/support/file','/support/job',
                      '/support/log','/support/online');
    IF n > 0 THEN RAISE EXCEPTION '计划越界：命中 % 个平台治理节点（严禁授给业务角色）', n; END IF;

    -- 4.3 结果非空（空计划说明展开写错了，别静默成功）
    SELECT count(*) INTO n FROM _sel;
    IF n = 0 THEN RAISE EXCEPTION '展开结果为空：计划/展开逻辑有问题'; END IF;
END $$;

-- ---------- 5) 需新建的按钮码：记账凭证 审核/记账/反审核 ----------
-- 这三个码在 backend 里是**真实 handler 权限**（voucher_write.rs:1589/1614/1638），但
-- sys_menu 里没有对应行 ⇒ 除超管通配外任何角色都拿不到（财务经理有 list 却审不了凭证）。
-- 用固定 uuid（20000000-...-0b000x）便于幂等与精确回退。
DROP TABLE IF EXISTS _new_menus;
CREATE TEMP TABLE _new_menus (
    id uuid PRIMARY KEY, name text, auth_code text, sort_order int, parent_path text
);
INSERT INTO _new_menus VALUES
 ('20000000-0000-4000-8000-0000000b0001', 'business:erp:voucher:audit',    'business:erp:voucher:audit',    60, '/business/erp/voucher'),
 ('20000000-0000-4000-8000-0000000b0002', 'business:erp:voucher:post',     'business:erp:voucher:post',     70, '/business/erp/voucher'),
 ('20000000-0000-4000-8000-0000000b0003', 'business:erp:voucher:un-audit', 'business:erp:voucher:un-audit', 80, '/business/erp/voucher');

-- ---------- 6) 把「新建按钮码 → 财务经理」并入同一条授权集合 ----------
-- 落进 _sel 后，up 与 down 用的是同一份集合：up 插、down 按确定性 id 删，天然对平。
-- 断言：这三条码要么都不在（首次施工），要么都在（幂等重放）。
-- 只存在一部分 → 有人手工动过库，交人判断，别让脚本猜。
DO $$
DECLARE v int; v_p uuid;
BEGIN
    SELECT count(*) INTO v FROM sys_menu WHERE auth_code IN (SELECT auth_code FROM _new_menus) AND deleted_at IS NULL;
    IF v NOT IN (0, 3) THEN
        RAISE EXCEPTION '凭证审核/记账按钮码已存在 % 条（预期 0=首次 或 3=已施工）→ 形状不明，停手交人判断', v;
    END IF;
    SELECT id INTO v_p FROM sys_menu WHERE path = '/business/erp/voucher' AND menu_type = 2 AND deleted_at IS NULL;
    IF v_p IS NULL THEN
        RAISE EXCEPTION '找不到记账凭证页 /business/erp/voucher（新建按钮码的父节点）';
    END IF;
END $$;

INSERT INTO _sel (role_code, id) SELECT 'FINANCE_MANAGER', id FROM _new_menus;
