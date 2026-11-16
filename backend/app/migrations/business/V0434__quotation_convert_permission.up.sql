-- ============================================================================
-- V0434__quotation_convert_permission（M3-03 · 报价单 → 销售订单：1 个权限码 · 2026-11-17）
--
-- 卡：t_de446ead（M3-03【M3·销售】报价单：→ 销售订单转换（带行））
-- 前置：V0432（biz_quotation / biz_quotation_line）、V0433（报价单 5 个权限码 + 页面节点 …4331）
--
-- ── 本卡只做一件事 ──────────────────────────────────────────────────────────
-- 转单接口 `POST /business/erp/quotation/{id}/convert` 需要自己的权限码
-- `business:erp:quotation:convert`（与 `quotation_handler::PERM_CONVERT` 逐字一致）。
--
-- 为什么不复用 `quotation:status` / `quotation:create`：
--   · 转单会**建一张要出库、要占客户信用、要开票的销售订单**，这是报价域里权限最重的动作；
--     销售员可以自己报价、自己标「客户已接受」，但「把报价变成订单」通常要销售经理点头
--     （现实里报价折扣能谈到公司亏本）。共用 `:status` 等于把这两件事绑死，将来要拆就得改接口码。
--   · `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]` 里的码
--     必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**（除超管通配外谁都 403，且不报错）。
--     所以新接口必须自带一行菜单。
--
-- ── 挂载位置 / 排序 ─────────────────────────────────────────────────────────
-- 父 = 报价单页面节点（`…4331`，menu_type=2）。同级已有按钮 sort：10 create / 20 update /
-- 30 delete / 40 status ⇒ 本卡取 **50**（约定 12：同父 sort 必须唯一，顺序追加）。
-- 与 V0433 同口径：`path` / `component` 留 NULL、`is_visible = FALSE`（按钮行不进侧栏）。
--
-- ── 不静默扩权 ──────────────────────────────────────────────────────────────
-- 本迁移**不写** `sys_role_menu`：转单授给哪些角色（销售员是否可自转 / 是否要销售经理）
-- 是管理决策，不属于迁移该替人做的决定 —— 与 V0250 / V0261 / V0265 / V0433 同口径。
-- 现有角色（含 HR 那套）一行都不会因此获得转单能力；超管靠 `*:*:*` 通配先可用。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检（码唯一 / 父正确 /
-- 同父 sort 不撞车 / 按钮计数 = 5），任一不符 → 迁移失败（宁可启动失败，也不要半套权限上线）。
-- down：先删 `sys_role_menu` 授权行，再删按钮行；可反复执行。
--
-- ── 号段（开工实测复核）────────────────────────────────────────────────────
-- 写本文件时 backend/app/migrations/business/ 最大号 = V0433（M3-02 报价单权限码）。
-- 并行卡会抢号，**提交前**再 `ls backend/app/migrations/business | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：报价单表与页面节点必须在
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_quotation') IS NULL THEN
        RAISE EXCEPTION 'V0434: biz_quotation 不存在，请先应用 V0432（销售报价单表结构）';
    END IF;
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0434: sys_menu 不存在（系统基线迁移没跑到）';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000004331'::uuid
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0434: 报价单页面节点（…4331）不存在，请先应用 V0433（报价单 5 个权限码）';
    END IF;
    RAISE NOTICE 'V0434: 前置齐备，开始种转单权限码';
END $$;

-- ---------------------------------------------------------------------------
-- ② 转单按钮（挂在报价单页面节点下，sort = 50）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004336'::uuid, 'business:erp:quotation:convert', 3,
     'business:erp:quotation:convert', NULL,
     '20000000-0000-0000-0000-000000004331'::uuid, NULL,
     50, 1, NULL,
     '转销售订单（只有「客户已接受」能转；明细整列搬、金额不重算；已转过再点回原订单；订单备注保留报价单号）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 自检：码唯一 / 父正确 / 同父 sort 不撞车 / 页面下按钮 = 5 / 不越权种授权
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_rows      integer;
    v_parent    uuid;
    v_sort      integer;
    v_children  integer;
    v_clash     integer;
    v_grants    integer;
BEGIN
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE auth_code = 'business:erp:quotation:convert'
       AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0434 自检失败：权限码 business:erp:quotation:convert 应恰好 1 行，实际 %', v_rows;
    END IF;

    SELECT parent_id, sort_order INTO v_parent, v_sort
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004336'::uuid;
    IF v_parent IS DISTINCT FROM '20000000-0000-0000-0000-000000004331'::uuid THEN
        RAISE EXCEPTION 'V0434 自检失败：转单按钮必须挂在报价单页面节点（…4331）下，实际 %', v_parent;
    END IF;
    IF v_sort <> 50 THEN
        RAISE EXCEPTION 'V0434 自检失败：转单按钮 sort_order 应为 50，实际 %', v_sort;
    END IF;

    -- 同父 menu_type=3 的 sort 不许重复（约定 12 / menu_invariants ⑦）
    SELECT count(*) INTO v_clash
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004331'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL
       AND sort_order = 50
       AND id <> '20000000-0000-0000-0000-000000004336'::uuid;
    IF v_clash <> 0 THEN
        RAISE EXCEPTION 'V0434 自检失败：报价单页下已存在 sort_order = 50 的按钮（% 行），必须换一个空位', v_clash;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004331'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;
    IF v_children <> 5 THEN
        RAISE EXCEPTION 'V0434 自检失败：报价单页下应有 5 个按钮（V0433 的 4 个 + 转单），实际 %', v_children;
    END IF;

    -- 不静默扩权：本迁移一行授权都不写（授权是管理决策）
    IF to_regclass('public.sys_role_menu') IS NOT NULL THEN
        SELECT count(*) INTO v_grants
          FROM sys_role_menu
         WHERE menu_id = '20000000-0000-0000-0000-000000004336'::uuid;
        IF v_grants <> 0 THEN
            RAISE EXCEPTION 'V0434 自检失败：迁移不应写 sys_role_menu（当前 % 行）', v_grants;
        END IF;
    END IF;

    RAISE NOTICE 'V0434：转单权限码已种（按钮挂报价单页下，sort=50，未授权任何角色）';
END $$;
