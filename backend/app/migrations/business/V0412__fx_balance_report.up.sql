-- ============================================================================
-- V0412__fx_balance_report（M1-07 · 多币种：外币余额表 · 2026-11-16）
--
-- 【本卡做什么】只种**一个页面权限码**（+ 它的菜单页面行与角色授权），不动表结构：
--   ① `business:erp:report:fx-balance`（页面行 menu_type=2，挂「财务管理」…0e02 下，sort_order = 50）
--   ② 授权 SUPER_ADMIN / FINANCE_MANAGER（谁看报表谁有这个码；空库首启时 sys_role 为空 → 空操作）
--   ③ 自检块（节点在位 / 一码一节点 / title 规范 / 同父 sort 唯一 / 授权到位），任一不符即
--      RAISE EXCEPTION 让整个迁移回滚 —— 「跑过了」和「验过了」是两件事。
--
-- 【为什么不建表：外币余额表是凭证的派生视图】
--   报表的每一列都能由 `biz_voucher_entry`（含 V0409 的 fc_* 原币列）聚合出来：
--   期初 / 本期借 / 本期贷 / 期末 + 折算。落一张 `biz_fx_balance` 快照 = **双真相**：
--     · 凭证被红冲（`reverse_posted`）而快照没跟着改 ⇒ 报表与账不符，且库里没有任何
--       约束能跨表断言（与 V0409 文件头「为什么不在 biz_voucher 上加原币合计」同一理由）；
--     · 结账后补做的调整分录会让快照永久停在旧值，而报表本该"打开就是对的"。
--   需要历史版本时靠「已结账期间只读 + 凭证留痕」，不靠复制数字。
--   ⇒ 幂等不靠台账表；取数口径写在 `fx_balance.rs` 模块文档。
--
-- 【为什么是一个**页面**码（menu_type=2），而不是挂总账页面的按钮行】
--   外币余额表是一张独立报表（与资产负债表 / 利润表 / 现金流量表 / 应付账龄同层级），
--   有自己的期间选择与币种过滤，不是「总账页面里的一个动作」：
--     · 挂成按钮（menu_type=3）会要求用户先进「总账」再点按钮才能看表，而报表是财务
--       月初的第一个动作，路径反了；
--     · 现有报表页（V0183 …1015 / V0184 …1017 / V0185 …1018 / V0255 …1600）全部是
--       menu_type=2 + `path=/business/erp/report/<报表>` + `component` 指向前端页面，
--       本卡照抄这条既有口径（新增报表不发明新结构）。
--   与 ERP 其它模块不同，报表页的 `auth_code` **就是**后端接口码（报表没有「列表 + 动作」
--   之分，`:page` / `:list` 解耦在 ERP 侧尚未落地，见编码规范 §1.7.2 的过渡期条款）。
--
-- 【为什么不动 `business:erp:report:export`】导出（xlsx）不在本卡范围（卡面「不夹带」）；
--   将来做导出时复用既有报表导出码，不新增码。
--
-- 【幂等】`INSERT … ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` 去重 + 自检块可反复执行；
--   探针行自清场（重放零残留）。
--
-- 【回滚】down 只删授权与本卡种的菜单行；不做「有数据就拒绝」的护栏 —— 本迁移没有任何
--   业务数据落地（报表不落表），删掉权限节点不会丢账。删完 SUPER_ADMIN 仍有 `*:*:*` 通配。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：sys_menu / sys_role_menu 必须在（缺表说明系统底座迁移没跑）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0412: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0412: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.biz_voucher_entry') IS NULL THEN
        RAISE EXCEPTION 'V0412: biz_voucher_entry 不存在，外币余额表的数据源缺失（先应用 V0149 / V0409）';
    END IF;
    RAISE NOTICE 'V0412: 前置表齐备，开始种外币余额表权限码';
END $$;

-- ---------------------------------------------------------------------------
-- ② 权限码 + 菜单页面行
--   parent_id = …0e02「财务管理」；与四张报表页同父（V0183/V0184/V0185/V0255）。
--   sort_order = 50：同父菜单页面已占 10/20/21/22/25/27/29/30/40/45/46/47/48/49/55，
--   50 是空的（约定 12：同父必须唯一）。
--   id = …001750：…0010xx 段被 payable / voucher / report / period / opening / yearend 占满；
--   0017xx 是 V0408 开的「外币」小段（V0408 用 1720、V0410 用 1730、V0411 用 1740），本卡续 1750。
--   title = 「外币余额表」（5 字，≤12；不带说明、不带标点）。
--   component = `business/erp/report/fx-balance/index`（与前端 views 目录逐字一致，
--   路由在 `frontend/src/router/index.ts` 手工注册 —— 两边必须同名，否则点菜单 404）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001750'::uuid, 'business:erp:report:fx-balance', 2,
     'business:erp:report:fx-balance', '/business/erp/report/fx-balance',
     '20000000-0000-0000-0000-000000000e02'::uuid,
     'business/erp/report/fx-balance/index',
     50, 1, 'currency-exchange', '外币余额表', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 财务经理（空库首启时 sys_role 为空 → 空操作，bootstrap 后重放本迁移补齐）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001750'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000001750'::uuid
   );

-- ---------------------------------------------------------------------------
-- ③ 迁移自检（节点 / 唯一性 / title / 同父排序 / 授权；任一不符即回滚）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n        integer;
    n_roles  integer;
    n_grant  integer;
BEGIN
    -- ===== 节点在位：码 / 类型 / 父节点 / 路径 / 组件 / 启用 / 未软删 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001750'::uuid
       AND auth_code = 'business:erp:report:fx-balance'
       AND name = 'business:erp:report:fx-balance'
       AND menu_type = 2
       AND parent_id = '20000000-0000-0000-0000-000000000e02'::uuid
       AND path = '/business/erp/report/fx-balance'
       AND component = 'business/erp/report/fx-balance/index'
       AND status = 1
       AND is_visible
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0412 自检失败：外币余额表权限节点不对（码授不出去 = 除超管没人能看报表）';
    END IF;

    -- ===== 一码一节点：同一个 auth_code 不许出现在多行（重复行会让授权页出现两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:report:fx-balance'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0412 自检失败：business:erp:report:fx-balance 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== path 唯一（menu_invariants 第 4 条）：同一路径两行 = 侧栏两个入口指向同一个页面 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE path = '/business/erp/report/fx-balance'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0412 自检失败：路径 /business/erp/report/fx-balance 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== title 规范：≤12 字、不是功能说明（约定 12 / menu_invariants 第 6 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001750'::uuid
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0412 自检失败：外币余额表节点的 title 不合菜单规范（≤12 字、不带标点/说明）';
    END IF;

    -- ===== 同父 sort_order 唯一（重复会让侧边栏顺序每次刷新都变，约定 12）=====
    SELECT count(*) INTO n
      FROM (
            SELECT sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000000e02'::uuid
               AND menu_type = 2
               AND deleted_at IS NULL
             GROUP BY sort_order
            HAVING count(*) > 1
           ) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0412 自检失败：「财务管理」下有 % 组重复的 sort_order（本卡用 50）', n;
    END IF;

    -- ===== 授权：有角色就该授到；角色不存在（空库首启）则跳过 =====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    SELECT count(*) INTO n_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = '20000000-0000-0000-0000-000000001750'::uuid
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    IF n_grant < n_roles THEN
        RAISE EXCEPTION 'V0412 自检失败：% 个目标角色只授到 % 行（超管/财务经理必须能看外币余额表）',
            n_roles, n_grant;
    END IF;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0412：sys_role 为空（空库首启，迁移先于 bootstrap），授权待重放补齐';
    END IF;

    RAISE NOTICE 'V0412 自检通过：权限码 business:erp:report:fx-balance + 菜单页面行（父=财务管理，sort=50）+ 授权 %/% 角色',
        n_grant, n_roles;
END $$;
