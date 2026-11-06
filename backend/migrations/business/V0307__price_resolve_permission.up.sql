-- ======================================================
-- V0307：取价解析接口的 1 个权限码（SO-33 / ARCH-7 §15.3.3）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-33 新增 1 个接口：
--
--     GET /business/erp/price/resolve    business:erp:price:resolve
--
-- 与 `price_handler.rs::PERM_RESOLVE` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ## 结构：一个（隐藏的）页面节点，无按钮
--
--   价格查询（menu_type=2，auth_code = business:erp:price:resolve，id …3071）
--
-- 取价是**只读计算接口**（没有增删改动作），所以只有一个节点、不挂 `menu_type=3` 按钮：
-- 挂按钮反而会造出「孤儿按钮」判据要处理的问题（`orphan_button_gate`）。
-- 菜单名叫「价格查询」——将来价格维护页（SO-32 遗留的 CRUD 卡）落地时可以把它
-- 改成那个页面的父节点，也可以直接复用本行。
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`
--
-- 与 V0261 / V0265 / V0284 / V0304 同一套口子：`system::menu::tests::n08` 是真库档
-- 硬门禁，`menu_type=2` 的 `path` 必须能在前端路由里解析；本卡不动前端，
-- 提前填 path 会让 n08 一直红。等价格页面卡落地后一条 UPDATE 收尾：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/price',
--            component = 'business/erp/price/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000003071'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：把取价授给哪些角色（销售 / 财务 / 仓管）
-- 是管理决策，不属于迁移该替人做的决定 —— 与 V0250 / V0261 / V0265 / V0284 / V0304 同口径。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-33 的 curl 验收就是用它。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检码值、可见性、无子节点，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「权限码半套」上线）。
-- 回滚：V0307__price_resolve_permission.down.sql 先删授权行再删菜单行。
-- ======================================================

-- ---------- 唯一节点：价格查询（隐藏，path/component 等前端卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003071'::uuid, '价格查询', 2,
     'business:erp:price:resolve', NULL, NULL,
     NULL, 80, 1, 'money',
     '取价解析（5 级确定性取价：客户协议价 → 客户专属阶梯 → 通用阶梯 → 物料标准售价 → 无价）。path/component/可见性由价格页面卡落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 自检：码值唯一、节点隐藏、未提前填 path、无子节点 ----------
DO $$
DECLARE
    v_rows      integer;
    v_auth      text;
    v_visible   boolean;
    v_path      text;
    v_children  integer;
BEGIN
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000003071'::uuid AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0307 自检失败：价格查询菜单行缺失（% 行）', v_rows;
    END IF;

    SELECT auth_code, is_visible, path INTO v_auth, v_visible, v_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000003071'::uuid;
    IF v_auth IS DISTINCT FROM 'business:erp:price:resolve' THEN
        RAISE EXCEPTION 'V0307 自检失败：auth_code 应为 business:erp:price:resolve，实际 %', v_auth;
    END IF;
    IF v_visible IS DISTINCT FROM FALSE THEN
        RAISE EXCEPTION 'V0307 自检失败：价格查询节点必须隐藏（前端路由未落地）';
    END IF;
    IF v_path IS NOT NULL THEN
        RAISE EXCEPTION 'V0307 自检失败：path 必须留 NULL（menu::tests::n08 会判红），实际 %', v_path;
    END IF;

    -- 同码不得有第二条承载行（否则授权面出现两个同名可勾项）
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE auth_code = 'business:erp:price:resolve' AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0307 自检失败：auth_code 承载行应为 1 条，实际 %', v_rows;
    END IF;

    -- 本迁移不挂按钮：有子节点说明有人手工加了，自检点出来
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003071'::uuid AND deleted_at IS NULL;
    IF v_children <> 0 THEN
        RAISE EXCEPTION 'V0307 自检失败：价格查询节点不应有子节点，实际 % 个', v_children;
    END IF;
END $$;
