-- ============================================================================
-- V0457：供应商评估页面（采购链 M4-16）—— 3 行菜单 + 采购/财务角色授权
--
--   ① 页面行（menu_type=2，path `/business/erp/supplier-evaluation`）挂**显形码**
--      `business:erp:supplier-evaluation:page`：只给前端判侧栏显隐与路由 `meta.permission`，
--      后端**零引用**（AGENTS 约定 14；已在 permission_audit_tests::ROUTE_LEVEL_ONLY 登记）。
--   ② 两个接口码各一条按钮行（menu_type=3），与 handler 的 #[nik::permission] 注解逐个对应：
--        · supplier-evaluation:list —— GET /page、GET /rank、GET /{id}
--        · supplier-evaluation:save —— POST /（打分录入 / 重打分覆盖）、GET /delivery-suggestion
--   ③ 授权：PURCHASE_MANAGER 拿全部 3 行（评估是采购的日常动作，N20 要求 :list 至少一个非超管角色）；
--      FINANCE_MANAGER 拿页面行 + 列表码（财务要复核评级与排名，但**不给**打分入口 ——
--      自己写分又自己复核就是自审）。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   实测最大号 = V0456（框架协议维护 M4-11b）⇒ 本卡顺延取 **V0457**。
--   菜单 id 取 …004495–004497（V0455 用到 …004488、V0456 用到 …004490–004494；
--   全库 grep `00000000449[0-9]` 确认 495–497 未占用）；采购管理目录 …000946。
--   同父 sort：采购管理下已用 10/20/25/26/30/35/36/37/38/39/40/42/50/90 ⇒ 页面行取 **43**（空档）。
--
-- ── 本卡的范围（卡面「本卡只做这一件事，不夹带」）────────────────────────────
--   做：供应商评估的**页面 + 它需要的一组接口**（列表 / 详情 / 打分录入 / 排名 / 交期参考分）
--       + 2 个接口码 + 1 个页面显形码 + 3 行菜单 + 角色授权。
--   不做：表结构（V0454 属 M4-12）、打分与评级口径（M4-13）、准入 / 黑名单（M4-14）、
--       评估单测（M4-15）、采购订单 contract_id（M4-17）。
--       为什么接口落在本卡：M4-13 README §四明确「M4-16 页面要的列表 / 详情 / 打分 / 排名 /
--       交期参考分需要一组 dto + handler + 路由 + 权限码 —— 建议照 M4-11→M4-11b 的做法另开
--       后端接口卡，不要把权限码塞进前端卡里现造」。本卡之前的链上没有这张后端卡，
--       而页面离开接口就是空壳（点了必然 404），所以按 M4-11b 先例**同卡交付最小接口面**：
--       只做页面用得到的 5 条路由，不做导出 / 批量重评级等页面没有入口的能力。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────────
--   菜单 `ON CONFLICT (id) DO UPDATE`（改 title / sort 后重跑迁移即生效）；
--   授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`（角色不存在时自然插 0 行）；
--   末尾 DO 块自检：行数 / 页面行形状 / 接口码行 / 同父 sort 唯一 / 授权 —— 任一条不符就
--   迁移失败（服务启动直接失败），不允许「菜单少一行但没人发现」。
-- ============================================================================

-- ---------- ① 菜单：1 页面行 + 2 按钮行 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004495'::uuid, 'supplier-evaluation', 2,
     'business:erp:supplier-evaluation:page', '/business/erp/supplier-evaluation',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/supplier-evaluation/index',
     43, 1, 'chart-bar', '供应商评估', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004496'::uuid, 'supplier-evaluation:list', 3,
     'business:erp:supplier-evaluation:list', NULL,
     '20000000-0000-0000-0000-000000004495'::uuid, NULL,
     10, 1, NULL, '查看台账排名', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000004497'::uuid, 'supplier-evaluation:save', 3,
     'business:erp:supplier-evaluation:save', NULL,
     '20000000-0000-0000-0000-000000004495'::uuid, NULL,
     20, 1, NULL, '评估录入', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ② 角色授权 ----------
-- 采购经理：全部 3 行（评估录入 + 台账 + 排名）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.id IN (
        '20000000-0000-0000-0000-000000004495'::uuid,
        '20000000-0000-0000-0000-000000004496'::uuid,
        '20000000-0000-0000-0000-000000004497'::uuid)
 WHERE r.role_code = 'PURCHASE_MANAGER' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- 财务经理：页面行 + 列表码（看台账与排名），**不含** save（自己打分自己复核 = 自审）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m ON m.id IN (
        '20000000-0000-0000-0000-000000004495'::uuid,
        '20000000-0000-0000-0000-000000004496'::uuid)
 WHERE r.role_code = 'FINANCE_MANAGER' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ③ 自检（任一条不符 → 迁移失败 → 启动失败） ----------
DO $$
DECLARE
    n_rows INT;
BEGIN
    -- 3.1 三行菜单都在（未删）
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000004495'::uuid,
                  '20000000-0000-0000-0000-000000004496'::uuid,
                  '20000000-0000-0000-0000-000000004497'::uuid)
       AND deleted_at IS NULL;
    IF n_rows <> 3 THEN
        RAISE EXCEPTION 'V0457 自检失败：供应商评估菜单应有 3 行（1 页面 + 2 按钮），实际 %', n_rows;
    END IF;

    -- 3.2 页面行：type=2、显形码、父级=采购管理、path 对、标题 ≤ 12 字
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004495'::uuid
       AND menu_type = 2
       AND auth_code = 'business:erp:supplier-evaluation:page'
       AND parent_id = '20000000-0000-0000-0000-000000000946'::uuid
       AND path = '/business/erp/supplier-evaluation'
       AND component = 'business/erp/supplier-evaluation/index'
       AND deleted_at IS NULL
       AND length(title) <= 12;
    IF n_rows <> 1 THEN
        RAISE EXCEPTION 'V0457 自检失败：页面行（显形码 / 父级 / path / component / 标题长度）不对';
    END IF;

    -- 3.3 两个接口码按钮行挂在页面行下
    SELECT count(*) INTO n_rows FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004495'::uuid
       AND menu_type = 3 AND deleted_at IS NULL
       AND auth_code IN ('business:erp:supplier-evaluation:list',
                         'business:erp:supplier-evaluation:save');
    IF n_rows <> 2 THEN
        RAISE EXCEPTION 'V0457 自检失败：接口码按钮应有 2 行，实际 %', n_rows;
    END IF;

    -- 3.4 同父 sort_order 必须唯一（采购管理目录下）
    SELECT count(*) INTO n_rows FROM (
        SELECT sort_order FROM sys_menu
         WHERE parent_id = '20000000-0000-0000-0000-000000000946'::uuid AND deleted_at IS NULL
         GROUP BY sort_order HAVING count(*) > 1) dup;
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0457 自检失败：采购管理目录下 sort_order 有重复（% 组）', n_rows;
    END IF;

    -- 3.5 采购经理拿到全部 3 行（N20：除超管外至少一个角色能用）
    SELECT count(*) INTO n_rows FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code = 'PURCHASE_MANAGER'
       AND rm.menu_id IN (SELECT unnest(ARRAY[
             '20000000-0000-0000-0000-000000004495'::uuid,
             '20000000-0000-0000-0000-000000004496'::uuid,
             '20000000-0000-0000-0000-000000004497'::uuid]));
    IF n_rows <> 3 THEN
        RAISE EXCEPTION 'V0457 自检失败：PURCHASE_MANAGER 应拿到 3 个菜单授权，实际 %', n_rows;
    END IF;

    RAISE NOTICE 'V0457 自检通过：供应商评估（3 行菜单 + 采购/财务授权）已就绪';
END $$;
