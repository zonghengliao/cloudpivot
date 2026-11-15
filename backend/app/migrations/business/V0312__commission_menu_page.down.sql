-- ============================================================================
-- V0312 down：把佣金两页还原成 V0310 的「只有权限码、没有页面」状态
--
-- 只还原菜单形状（path / component / is_visible / title / parent / sort），
-- **不删**任何 `sys_menu` 行、**不删**任何 `sys_role_menu` 授权 ——
-- 删权限码是 V0310 down 的事，本文件回滚后 V0310 的 7 个码 + 5 个按钮必须原样还在
-- （末尾断言会验证这一点，防止「回滚页面顺手把权限也删了」）。
--
-- 还原值逐字抄 V0310 的 INSERT（…3301 / …3311）：
--   path = NULL, component = NULL, parent_id = NULL, is_visible = FALSE,
--   sort = 88 / 92，title = 原文长标题（带「path/component/可见性由 SO-37 落地后补」尾巴）
-- ============================================================================

DO $$
DECLARE
    v_rule_page    uuid := '20000000-0000-0000-0000-000000003301'::uuid;
    v_settle_page  uuid := '20000000-0000-0000-0000-000000003311'::uuid;

    v_rule_title   text := '佣金规则（按销售员 / 物料类别 / 全局；百分比或固定额；起止金额 + 有效期）。path/component/可见性由 SO-37 落地后补';
    v_rule_sort    int  := 88;

    v_settle_title text := '佣金结算（按 销售员 × 期间 汇总；按已核销应收计佣；DRAFT/APPROVED/PAID）。path/component/可见性由 SO-37 落地后补';
    v_settle_sort  int  := 92;

    v_count        int;
    n_updated      int := 0;
BEGIN
    -- ---------- 0) 前置：两个节点必须还在 ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id IN (v_rule_page, v_settle_page) AND deleted_at IS NULL;
    IF v_count <> 2 THEN
        RAISE EXCEPTION 'V0312 down 前置失败：佣金两页节点只存在 % 个（期望 2；V0310 可能已被回滚）', v_count;
    END IF;

    -- ---------- 1) 佣金规则页：还原成 V0310 的形状 ----------
    UPDATE sys_menu
       SET path       = NULL,
           component  = NULL,
           parent_id  = NULL,
           sort_order = v_rule_sort,
           title      = v_rule_title,
           is_visible = FALSE,
           updated_at = now()
     WHERE id = v_rule_page
       AND (path       IS DISTINCT FROM NULL
         OR component  IS DISTINCT FROM NULL
         OR parent_id  IS DISTINCT FROM NULL
         OR sort_order IS DISTINCT FROM v_rule_sort
         OR title      IS DISTINCT FROM v_rule_title
         OR is_visible IS DISTINCT FROM FALSE);
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 down ① 规则页还原 % 行', n_updated;

    -- ---------- 2) 佣金结算页：还原成 V0310 的形状 ----------
    UPDATE sys_menu
       SET path       = NULL,
           component  = NULL,
           parent_id  = NULL,
           sort_order = v_settle_sort,
           title      = v_settle_title,
           is_visible = FALSE,
           updated_at = now()
     WHERE id = v_settle_page
       AND (path       IS DISTINCT FROM NULL
         OR component  IS DISTINCT FROM NULL
         OR parent_id  IS DISTINCT FROM NULL
         OR sort_order IS DISTINCT FROM v_settle_sort
         OR title      IS DISTINCT FROM v_settle_title
         OR is_visible IS DISTINCT FROM FALSE);
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 down ② 结算页还原 % 行', n_updated;

    -- ---------- 3) 自检：形状确实是 V0310 的样子 ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_rule_page
       AND path IS NULL AND component IS NULL AND parent_id IS NULL
       AND is_visible = FALSE AND sort_order = v_rule_sort
       AND title = v_rule_title;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0312 down 自检失败：佣金规则页未还原成 V0310 形状';
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_settle_page
       AND path IS NULL AND component IS NULL AND parent_id IS NULL
       AND is_visible = FALSE AND sort_order = v_settle_sort
       AND title = v_settle_title;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0312 down 自检失败：佣金结算页未还原成 V0310 形状';
    END IF;

    -- ---------- 4) 自检：权限码与按钮一个都没少（回滚页面不许带走权限） ----------
    --     逐条列出 V0310 的 7 个节点（页面 2 + 按钮 5）—— uuid 不能做算术，
    --     写 `v_rule_page + 1` 会直接语法错（PG 没有 uuid + int 运算符）
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id IN (
             '20000000-0000-0000-0000-000000003301'::uuid,   -- 佣金规则页
             '20000000-0000-0000-0000-000000003302'::uuid,   -- + create
             '20000000-0000-0000-0000-000000003303'::uuid,   -- + update
             '20000000-0000-0000-0000-000000003304'::uuid,   -- + delete
             '20000000-0000-0000-0000-000000003311'::uuid,   -- 佣金结算页
             '20000000-0000-0000-0000-000000003312'::uuid,   -- + create
             '20000000-0000-0000-0000-000000003313'::uuid    -- + status
           )
       AND deleted_at IS NULL;
    IF v_count <> 7 THEN
        RAISE EXCEPTION 'V0312 down 自检失败：V0310 的 7 个节点只存在 % 个（页面 2 + 按钮 5）', v_count;
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_rule_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 3 THEN
        RAISE EXCEPTION 'V0312 down 自检失败：佣金规则页下按钮 = %（期望 3）', v_count;
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_settle_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 2 THEN
        RAISE EXCEPTION 'V0312 down 自检失败：佣金结算页下按钮 = %（期望 2）', v_count;
    END IF;

    RAISE NOTICE 'V0312 down 完成：佣金两页已还原为隐藏节点（权限码与按钮保持不变）';
END $$;
