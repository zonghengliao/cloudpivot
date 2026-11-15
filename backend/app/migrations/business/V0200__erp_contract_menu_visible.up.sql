-- ============================================================
-- V0200：合同管理菜单上侧边栏（A5 落地页面时把 V0197 的 `is_visible` 置 TRUE）
--
-- 背景（V0197 文首写明的约定）：
--   A1 建菜单时前端页面还不存在（`views/business/erp/contract/` 由 A5 建），
--   挂上侧栏会给人一条点进去 404 的入口，所以先 `is_visible = FALSE`，
--   **权限码照样装载**（`load_permissions` 只过滤 `status`，不看 `is_visible`）。
--   A5 页面落地后「一条 UPDATE 把 is_visible 置 TRUE」即可 —— 就是本迁移。
--
-- 菜单层级（用户明确要求：不要「ERP」这一层）：
--   `parent_id` 保持 NULL ⇒ 根级页面，与「采购管理」(sort 62)、「仓库管理」(66)、
--   「财务管理」(72)、「基础数据」(76) 同级；`sort_order = 75` 让它落在
--   「采购管理」下面、「基础数据」上面。
--
-- 幂等：
--   只更新固定的菜单 id，且以 `is_visible = FALSE` 为前置条件；
--   重复执行 → 0 行受影响，不报错、不覆盖运维改过的标题 / 排序。
-- ============================================================

UPDATE sys_menu
   SET is_visible = TRUE
 WHERE id = '20000000-0000-0000-0000-000000001100'::uuid
   AND menu_type = 2
   AND deleted_at IS NULL
   AND is_visible = FALSE;

-- 自检：菜单必须可见、必须是根级、必须挂着 list 权限码
DO $$
DECLARE
    v_visible BOOLEAN;
    v_parent  UUID;
    v_path    TEXT;
    v_auth    TEXT;
BEGIN
    SELECT is_visible, parent_id, path, auth_code
      INTO v_visible, v_parent, v_path, v_auth
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001100'::uuid
       AND deleted_at IS NULL;

    IF v_visible IS NULL THEN
        RAISE EXCEPTION 'V0200 自检失败：合同管理菜单（…1100）不存在';
    END IF;
    IF v_visible IS NOT TRUE THEN
        RAISE EXCEPTION 'V0200 自检失败：合同管理菜单未置为可见';
    END IF;
    IF v_parent IS NOT NULL THEN
        RAISE EXCEPTION 'V0200 自检失败：合同管理菜单应为根级（parent_id IS NULL），实际 %', v_parent;
    END IF;
    IF v_path <> '/business/erp/contract' THEN
        RAISE EXCEPTION 'V0200 自检失败：合同管理菜单 path 异常（%）', v_path;
    END IF;
    IF v_auth <> 'erp:contract:list' THEN
        RAISE EXCEPTION 'V0200 自检失败：合同管理菜单 auth_code 异常（%）', v_auth;
    END IF;

    RAISE NOTICE 'V0200 自检通过：合同管理菜单已上侧边栏（根级，sort 75）';
END
$$;
