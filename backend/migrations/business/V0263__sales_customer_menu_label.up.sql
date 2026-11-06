-- ============================================================================
-- V0263：客户档案菜单的显示名收敛成短标签（`title`）
--
-- 依据：kanban 卡 t_3f4c05ae 的真机验收（`docs/evidence/20261106-so03-customer/round-1-1-list.png`）。
--
-- # 一、问题（真机截图实测，非推测）
--   V0261 把 `title` 写成一句「客户档案（销售链交易对手主档：编码 / 名称 / 联系人 / 信用额度 / 状态；
--   冻结只影响开新单）」。`sys_menu.title` 是**侧边栏显示名**（`name` 只在权限树里用），
--   于是菜单里被挤成 `客户档案（销售链交易对手` —— 选中态高亮还压着右边框，读起来像被截断的乱码。
--
--   同族对照（实测 sys_menu）：
--     /business/erp/warehouse | name=仓库管理 | title=仓库管理          ← 短标签
--     /business/erp/supplier  | name=供应商   | title=供应商            ← 短标签
--     /business/erp/contract  | name=合同管理 | title=合同管理（采购/销售/服务/租赁） ← 半长
--
-- # 二、修法
--   `title` 收敛成 `客户档案`（与 供应商 / 仓库管理 同口径）。那句业务说明放到**页面里**：
--   `views/business/erp/customer/index.vue` 的列表卡右上角常驻
--   「冻结只影响开新单，历史单据与引用不受影响」—— 用户真正需要它的地方是列表，不是侧边栏。
--   `name` 不动（权限树 / 菜单管理里的名字，长一点反而好认）。
--
-- # 三、幂等与回滚
--   幂等：`AND title IS DISTINCT FROM '客户档案'`；在已收窄的库上重放 0 行。
--   回滚：down 写回 V0262 的长 title（keep 结构不动）。
-- ============================================================================

DO $$
DECLARE
    v_page  uuid := '20000000-0000-0000-0000-000000002601'::uuid;
    v_title text := '客户档案';
    v_before text;
    n_upd   int := 0;
BEGIN
    SELECT title INTO v_before FROM sys_menu WHERE id = v_page AND deleted_at IS NULL;
    IF v_before IS NULL THEN
        RAISE EXCEPTION 'V0263 前置失败：客户档案页面节点 …2601 不存在（V0261 / V0262 未跑？）';
    END IF;

    UPDATE sys_menu
       SET title = v_title, updated_at = now()
     WHERE id = v_page
       AND title IS DISTINCT FROM v_title;
    GET DIAGNOSTICS n_upd = ROW_COUNT;

    -- 断言：侧边栏显示名必须是短标签（V0262 的长文案会让菜单被截断）
    IF EXISTS (SELECT 1 FROM sys_menu WHERE id = v_page AND title IS DISTINCT FROM v_title) THEN
        RAISE EXCEPTION 'V0263 断言失败：客户档案 title 未收敛成「%」', v_title;
    END IF;

    RAISE NOTICE 'V0263 完成：客户档案菜单显示名「%」→「%」（改 % 行）', v_before, v_title, n_upd;
END $$;
