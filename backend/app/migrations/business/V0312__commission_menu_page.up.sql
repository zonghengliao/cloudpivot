-- ============================================================================
-- V0312：佣金两页「落地收尾」—— 把 V0310 故意留空的 path / component / is_visible 补齐，
--        并把两个页面挂到「销售管理」目录下（佣金规则 sort 80、佣金结算 sort 90）
--
-- 依据：kanban 卡 t_32ac0e6e（SO-37 佣金前端：规则配置 + 结算查询）。
--       逐行镜像 V0306（SO-31 退货页）/ V0308（SO-34 价格页）的收尾写法。
--
-- # 一、为什么分两步（V0310 留 NULL → 本迁移补全）
--   `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router`（真库档硬门禁）要求
--   `menu_type = 2 AND path IS NOT NULL AND path <> ''` 的每一行 path 都能在
--   `frontend/src/router/index.ts` 里解析。V0310 只种权限码时前端页与路由都不存在
--   ⇒ 先填 path 会让 n08 一直红（V0310 头注释原文：「SO-37 落地后一条 UPDATE 收尾」）。
--   本卡先落前端页 + 路由，再由本迁移补这一刀。
--
-- # 二、本次落地的前端资产（顺序即施工顺序，不可交换）
--   ① `frontend/src/api/business/erp/commission.ts`
--      （规则 CRUD 4 接口 + 结算 4 接口，字段逐字抄 `commission_handler.rs` /
--        `commission_rule_handler.rs` 与 `biz_commission_rule` 列名）
--   ② `frontend/src/views/business/erp/commission-rule/{index.vue, rule-logic.ts}`
--      （列表 9 列 + 服务端分页 + 3 过滤 / 新建·编辑弹窗（范围 / 类型 / 费率 / 起止金额 /
--        有效期）/ 停用·启用·删除；纯逻辑单测钉住「PERCENT 是百分数」「ALL 不带 scope_id」）
--   ③ `frontend/src/views/business/erp/commission/{index.vue, commission-logic.ts}`
--      （结算单列表 8 列 + 服务端分页 + 4 过滤 / 明细展开懒加载 / 生成结算单（期间 + 销售员）/
--        审批·发放 + 凭证口径提示）
--   ④ `frontend/src/router/index.ts`：
--        path: 'business/erp/commission-rule'  name: 'BusinessErpCommissionRule'
--        path: 'business/erp/commission'       name: 'BusinessErpCommission'
--      n08 把路由 `path: 'x'` 补成 `/x` 后与 `sys_menu.path` **整串比对**
--      ⇒ 本迁移写的 path 必须逐字一致（多一段少一段都红）。
--   ⑤ 本迁移：补 path / component / is_visible、**收敛 title 成短标签**、归位到
--      「销售管理」目录（V0267 种的 …2701；V0310 建节点时 parent_id 留空，
--      不归位就会在侧边栏根级多出两个孤儿页）。
--
-- # 三、为什么 title 必须改短
--   `sys_menu.title` 就是侧边栏显示名。V0310 把整段口径写进了 title
--   （「佣金规则（按销售员 / 物料类别 / 全局；百分比或固定额；起止金额 + 有效期）。…」），
--   SO-03 客户档案踩过同一个坑：菜单被挤成两行、省略号都没有、选中态压右边框。
--   短标签口径看同族（「供应商」「仓库管理」的 title == name）⇒ 这里 title 收敛成
--   「佣金规则」「佣金结算」，口径说明留在本文件与页面头部提示条里。
--
-- # 四、幂等与回滚
--   幂等：每条 UPDATE 都带 `IS DISTINCT FROM`，在已补齐的库上重放 = 0 行更新；
--        末尾 DO 块自检（两节点 menu_type=2 / 六列终态 / 5 个按钮仍挂在各自页面下 /
--        页面 auth_code 未被换掉），任一不符 → 迁移失败
--        （宁可启动失败，也不要「菜单点了 404」上线）。
--   回滚：V0312 down 把两节点还原成 V0310 的 NULL / NULL / FALSE / 原文长标题 /
--        parent NULL / sort 88·92，并断言「页面下按钮仍在」。
--        不删任何 `sys_menu` 行、不删 `sys_role_menu` 授权（那是 V0310 的事）。
-- ============================================================================

DO $$
DECLARE
    -- 佣金规则页（V0310 …3301）
    v_rule_page      uuid := '20000000-0000-0000-0000-000000003301'::uuid;
    -- 佣金结算页（V0310 …3311）
    v_settle_page    uuid := '20000000-0000-0000-0000-000000003311'::uuid;
    v_parent         uuid := '20000000-0000-0000-0000-000000002701'::uuid;  -- 销售管理（V0267）

    v_rule_path      text := '/business/erp/commission-rule';
    v_rule_component text := 'business/erp/commission-rule/index';
    v_rule_title     text := '佣金规则';
    v_rule_sort      int  := 80;

    v_settle_path      text := '/business/erp/commission';
    v_settle_component text := 'business/erp/commission/index';
    v_settle_title     text := '佣金结算';
    v_settle_sort      int  := 90;

    v_type        int;
    v_code        text;
    v_count       int;
    n_updated     int := 0;
BEGIN
    -- ---------- 0) 前置：两个页面节点必须存在、是页面、权限码没被换掉 ----------
    SELECT menu_type, auth_code INTO v_type, v_code
      FROM sys_menu
     WHERE id = v_rule_page AND deleted_at IS NULL;
    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0312 前置失败：佣金规则页面节点 …3301 不存在或已软删（V0310 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0312 前置失败：…3301 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:commission-rule:list' THEN
        RAISE EXCEPTION 'V0312 前置失败：…3301 的 auth_code = %（应为 business:erp:commission-rule:list）', v_code;
    END IF;

    SELECT menu_type, auth_code INTO v_type, v_code
      FROM sys_menu
     WHERE id = v_settle_page AND deleted_at IS NULL;
    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0312 前置失败：佣金结算页面节点 …3311 不存在或已软删（V0310 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0312 前置失败：…3311 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:commission-settle:list' THEN
        RAISE EXCEPTION 'V0312 前置失败：…3311 的 auth_code = %（应为 business:erp:commission-settle:list）', v_code;
    END IF;

    -- ---------- 0.1) 前置：「销售管理」目录必须在（归位目标） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_parent AND menu_type = 1 AND deleted_at IS NULL;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0312 前置失败：「销售管理」目录 …2701 不存在或不是目录节点（V0267 没跑？）';
    END IF;

    -- ---------- 1) 佣金规则页：path ----------
    UPDATE sys_menu
       SET path = v_rule_path, updated_at = now()
     WHERE id = v_rule_page AND path IS DISTINCT FROM v_rule_path;
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 ① 规则页 path 更新 % 行', n_updated;

    -- ---------- 2) 佣金规则页：component ----------
    UPDATE sys_menu
       SET component = v_rule_component, updated_at = now()
     WHERE id = v_rule_page AND component IS DISTINCT FROM v_rule_component;
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 ② 规则页 component 更新 % 行', n_updated;

    -- ---------- 3) 佣金规则页：title（短标签）+ 归位 + 排序 + 可见 ----------
    UPDATE sys_menu
       SET title       = v_rule_title,
           parent_id   = v_parent,
           sort_order  = v_rule_sort,
           is_visible  = TRUE,
           updated_at  = now()
     WHERE id = v_rule_page
       AND (title       IS DISTINCT FROM v_rule_title
         OR parent_id   IS DISTINCT FROM v_parent
         OR sort_order  IS DISTINCT FROM v_rule_sort
         OR is_visible  IS DISTINCT FROM TRUE);
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 ③ 规则页 title/parent/sort/visible 更新 % 行', n_updated;

    -- ---------- 4) 佣金结算页：path ----------
    UPDATE sys_menu
       SET path = v_settle_path, updated_at = now()
     WHERE id = v_settle_page AND path IS DISTINCT FROM v_settle_path;
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 ④ 结算页 path 更新 % 行', n_updated;

    -- ---------- 5) 佣金结算页：component ----------
    UPDATE sys_menu
       SET component = v_settle_component, updated_at = now()
     WHERE id = v_settle_page AND component IS DISTINCT FROM v_settle_component;
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 ⑤ 结算页 component 更新 % 行', n_updated;

    -- ---------- 6) 佣金结算页：title（短标签）+ 归位 + 排序 + 可见 ----------
    UPDATE sys_menu
       SET title       = v_settle_title,
           parent_id   = v_parent,
           sort_order  = v_settle_sort,
           is_visible  = TRUE,
           updated_at  = now()
     WHERE id = v_settle_page
       AND (title       IS DISTINCT FROM v_settle_title
         OR parent_id   IS DISTINCT FROM v_parent
         OR sort_order  IS DISTINCT FROM v_settle_sort
         OR is_visible  IS DISTINCT FROM TRUE);
    GET DIAGNOSTICS n_updated = ROW_COUNT;
    RAISE NOTICE 'V0312 ⑥ 结算页 title/parent/sort/visible 更新 % 行', n_updated;

    -- ---------- 7) 自检：两节点的终态（六列逐列判，别只看「有 path」） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_rule_page
       AND deleted_at IS NULL
       AND menu_type = 2
       AND path = v_rule_path
       AND component = v_rule_component
       AND parent_id = v_parent
       AND sort_order = v_rule_sort
       AND is_visible = TRUE
       AND title = v_rule_title;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0312 自检失败：佣金规则页 …3301 的终态不符（命中 % 行，期望 1）', v_count;
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_settle_page
       AND deleted_at IS NULL
       AND menu_type = 2
       AND path = v_settle_path
       AND component = v_settle_component
       AND parent_id = v_parent
       AND sort_order = v_settle_sort
       AND is_visible = TRUE
       AND title = v_settle_title;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0312 自检失败：佣金结算页 …3311 的终态不符（命中 % 行，期望 1）', v_count;
    END IF;

    -- ---------- 8) 自检：5 个按钮仍挂在各自页面下（回滚 / 误删会当场暴露） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_rule_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 3 THEN
        RAISE EXCEPTION 'V0312 自检失败：佣金规则页下的按钮 = %（期望 3：create/update/delete）', v_count;
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_settle_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 2 THEN
        RAISE EXCEPTION 'V0312 自检失败：佣金结算页下的按钮 = %（期望 2：create/status）', v_count;
    END IF;

    -- ---------- 9) 自检：两个页面 path 在同目录下不撞（同 path 两条菜单 = 侧边栏出现两个同名项） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE path = v_rule_path AND deleted_at IS NULL AND id <> v_rule_page;
    IF v_count <> 0 THEN
        RAISE EXCEPTION 'V0312 自检失败：已有 % 条其它菜单占用 path %', v_count, v_rule_path;
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE path = v_settle_path AND deleted_at IS NULL AND id <> v_settle_page;
    IF v_count <> 0 THEN
        RAISE EXCEPTION 'V0312 自检失败：已有 % 条其它菜单占用 path %', v_count, v_settle_path;
    END IF;

    RAISE NOTICE 'V0312 完成：佣金规则 /business/erp/commission-rule（sort 80）、佣金结算 /business/erp/commission（sort 90）已挂到「销售管理」目录';
END $$;
