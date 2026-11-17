-- ============================================================================
-- V0443：销售返利页「落地收尾」—— 把 V0438 故意留空的 path / component / is_visible 补齐，
--        并把 title 收敛成侧栏短名「销售返利」（一行两 tab：返利政策 + 返利结算）
--
-- 依据：kanban 卡 t_be03f4e7（M3-19【M3·销售】销售返利：前端政策与结算页）。
--       逐行镜像 V0312（SO-37 佣金两页的收尾写法：先落前端页 + 路由，再由迁移补菜单）。
--
-- # 一、为什么只有一个页面节点、两个 tab（**不是**两个菜单行）
--
--   ① V0438 建本模块时只种了 1 个页面节点（…4380，audit_code = …rebate-policy:list），
--      V0439 的 3 个结算按钮 + V0440 的 1 个开票按钮**全部挂在它下面**（sort 40/50/60/70）——
--      即三个后端模块（政策 / 结算 / 开票）在授权树上共用一个页面节点。
--   ② `scripts/verify/menu_invariants.sh` 的 ⑤ 是**菜单总数**闸：未删记录必须落在
--      [400, 540]。实测（V0442 之后、本卡开工前）未删菜单 = **540**，正顶上限 ——
--      为「返利结算」再插一行 `menu_type = 2` 立刻 541 → 闸门判红。
--   ③ 于是结算做成同一页的第二个 tab：侧边栏一个「销售返利」，进去两个 tab。
--      权限边界不受影响：政策 4 码 / 结算 3 码 / 开票 1 码仍是 8 个独立按钮码，
--      tab 显隐由前端按 `…:list` 判（后端零引用）。
--
-- # 二、为什么 title 要从「返利政策」改成「销售返利」
--
--   title 是**侧边栏显示名**（`docs/编码规范.md` §1.6.1）。这一行现在承载政策 + 结算两块，
--   叫「返利政策」会让人以为结算不在里面。短名 4 字，符合「2-6 字名词短语」；
--   口径说明留在本文件与该页头部提示条里。`name` 列不动（它是唯一标识 / 权限关联）。
--
-- # 三、施工顺序（不可交换）
--
--   ① `frontend/src/api/business/erp/rebate.ts`（政策 5 接口 + 结算 6 接口）
--   ② `frontend/src/views/business/erp/rebate/{policy-logic.ts, settle-logic.ts, index.vue}`
--   ③ `frontend/src/router/index.ts`：`path: 'business/erp/rebate-policy'`
--      + `component: () => import('@/views/business/erp/rebate/index.vue')`
--      （n08 把路由 `path: 'x'` 补成 `/x` 后与 `sys_menu.path` **整串比对** ⇒ 本文件写的
--       path 必须与路由逐字一致，多一段少一段都红）
--   ④ 本迁移：补 path / component / is_visible / title（先有路由，再填菜单 ——
--      `system::menu::tests::n08` 是真库档硬门禁，反序会让 n08 一直红）
--
-- # 四、幂等与回滚
--
--   幂等：每条 UPDATE 都带 `IS DISTINCT FROM`，在已补齐的库上重放 = 0 行更新；
--        末尾 DO 块自检（节点是页面 / 六列终态 / auth_code 没被换掉 /
--        7 个按钮仍挂在它下面 / 该 path 无第二条菜单），任一不符 → 迁移失败
--        （宁可启动失败，也不要「菜单点了 404」上线）。
--   回滚：V0443 down 把该节点还原成 V0438 的终态（path NULL、component NULL、
--        is_visible FALSE、title「返利政策」、sort 85、parent …2701，与 V0438 原文一致）。
--        不删任何 sys_menu 行、不删 sys_role_menu 授权（那是 V0438/V0439/V0440 的事）。
--
-- # 五、号段（开工实测复核）
--
--   写入本文件时 `backend/app/migrations/**` 最大号 = V0442（权限闸收口，FIX t_fa41e0e1）。
--   并行卡会抢号，**提交前**再
--   `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核
--   （跨目录全局唯一，重号 = 服务启动直接失败）。
-- ============================================================================

DO $$
DECLARE
    -- 返利页（V0438 …4380）
    v_page      uuid := '20000000-0000-0000-0000-000000004380'::uuid;
    -- 销售管理目录（V0267 …2701）
    v_parent    uuid := '20000000-0000-0000-0000-000000002701'::uuid;

    v_path      text := '/business/erp/rebate-policy';
    v_component text := 'business/erp/rebate/index';
    v_title     text := '销售返利';
    v_sort      int  := 85;

    v_type      int;
    v_code      text;
    v_count     int;
    v_updated   int := 0;
BEGIN
    -- ---------- 0) 前置：页面节点必须存在、是页面、权限码没被换掉 ----------
    SELECT menu_type, auth_code INTO v_type, v_code
      FROM sys_menu
     WHERE id = v_page AND deleted_at IS NULL;
    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0443 前置失败：返利页节点 …4380 不存在或已软删（V0438 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0443 前置失败：…4380 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:rebate-policy:list' THEN
        RAISE EXCEPTION 'V0443 前置失败：…4380 的 auth_code = %（应为 business:erp:rebate-policy:list）', v_code;
    END IF;

    -- ---------- 0.1) 前置：「销售管理」目录必须在（归位目标） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_parent AND menu_type = 1 AND deleted_at IS NULL;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0443 前置失败：「销售管理」目录 …2701 不存在或不是目录节点（V0267 没跑？）';
    END IF;

    -- ---------- 0.2) 前置：7 个按钮必须在它下面（政策 3 + 结算 3 + 开票 1） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 7 THEN
        RAISE EXCEPTION 'V0443 前置失败：返利页节点下的按钮 = %（期望 7：政策 create/update/delete + 结算 list/calc/delete + 开票 invoice），请先应用 V0438 / V0439 / V0440',
            v_count;
    END IF;

    -- ---------- 1) path ----------
    UPDATE sys_menu
       SET path = v_path, updated_at = now()
     WHERE id = v_page AND path IS DISTINCT FROM v_path;
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RAISE NOTICE 'V0443 ① path 更新 % 行', v_updated;

    -- ---------- 2) component ----------
    UPDATE sys_menu
       SET component = v_component, updated_at = now()
     WHERE id = v_page AND component IS DISTINCT FROM v_component;
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RAISE NOTICE 'V0443 ② component 更新 % 行', v_updated;

    -- ---------- 3) title（侧栏短名）+ 归位 + 排序 + 可见 ----------
    UPDATE sys_menu
       SET title      = v_title,
           parent_id  = v_parent,
           sort_order = v_sort,
           is_visible = TRUE,
           updated_at = now()
     WHERE id = v_page
       AND (title      IS DISTINCT FROM v_title
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort
         OR is_visible IS DISTINCT FROM TRUE);
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RAISE NOTICE 'V0443 ③ title/parent/sort/visible 更新 % 行', v_updated;

    -- ---------- 4) 自检：节点终态（八列逐列判，别只看「有 path」） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND menu_type = 2
       AND auth_code = 'business:erp:rebate-policy:list'
       AND path = v_path
       AND component = v_component
       AND parent_id = v_parent
       AND sort_order = v_sort
       AND is_visible = TRUE
       AND title = v_title;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0443 自检失败：返利页节点 …4380 的终态不符（命中 % 行，期望 1）', v_count;
    END IF;

    -- ---------- 5) 自检：7 个按钮仍在（顺序/归属都没被本次改动波及） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 7 THEN
        RAISE EXCEPTION 'V0443 自检失败：返利页节点下的按钮 = %（期望 7）', v_count;
    END IF;

    -- ---------- 5.1) 自检：三个结算按钮 + 开票按钮的授权码仍在（不搬动，只确认没丢） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_page
       AND deleted_at IS NULL
       AND auth_code IN ('business:erp:rebate-settlement:list',
                         'business:erp:rebate-settlement:calc',
                         'business:erp:rebate-settlement:delete',
                         'business:erp:rebate-settlement:invoice');
    IF v_count <> 4 THEN
        RAISE EXCEPTION 'V0443 自检失败：结算/开票 4 个按钮码命中 % 行（期望 4）', v_count;
    END IF;

    -- ---------- 6) 自检：该 path 在同库未被第二条菜单占用（同 path 两条 = 侧栏两个同名项） ----------
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE path = v_path AND deleted_at IS NULL AND id <> v_page;
    IF v_count <> 0 THEN
        RAISE EXCEPTION 'V0443 自检失败：已有 % 条其它菜单占用 path %', v_count, v_path;
    END IF;

    RAISE NOTICE 'V0443 完成：销售返利 % 已挂到「销售管理」目录（sort %，一行两 tab：返利政策 / 返利结算）',
        v_path, v_sort;
END $$;
