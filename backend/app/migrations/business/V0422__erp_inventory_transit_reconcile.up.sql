-- ============================================================================
-- V0422__erp_inventory_transit_reconcile（M2-05 · 在途库存对账权限码 · 2026-11-16）
--
-- 卡：t_b545b37a（M2-05【M2·仓库】在途库存：对账（在途 + 已出 = 发货总量））
-- 代码：`backend/app/src/business/erp/inventory_transit/reconcile.rs`（对账取数 + 差异判定）
--       `backend/app/src/business/erp/inventory_transit/handler.rs`
--         GET /business/erp/inventory-transit/reconcile  在途对账（只读，返回逐单差异 + 汇总）
-- 表：`biz_inventory_in_transit` 已由 V0420（M2-01）建好，**本迁移不动表结构**
--     （不加列、不改列、不动索引 —— 只种权限码）。
--
-- ── 本迁移只做一件事：种 1 个权限码 + 1 个授权矩阵 ──────────────────────────
--
--   业务码：business:erp:inventory-transit:audit
--   承载行：`sys_menu` 的 **menu_type = 3（按钮）**，挂在「即时库存」页面下
--           （与 V0421 的在途查询按钮同父，sort_order = 30 接在它后面）
--
-- ── 为什么对账单开一个码，而不是复用 inventory-transit:list ─────────────────
--
--   ① 授权面不同（与 `V0143` 给「库存巡检」单开 `business:erp:inventory:audit`
--      同一个取向）：``:list`` 是「把在途明细翻给你看」，``:audit`` 是「给一句
--      『账对不对』的结论 + 逐单差异」。前者是台账读者，后者是**对账责任人**——
--      很多岗位该看数、但不该负责出对账结论（出了结论就要有人签字修账）。
--   ② 与巡检码的分界：`inventory:audit` **会改库**（`dry_run=false` 按序列号重算），
--      所以在采购/财务侧只给仓储负责人；在途对账**只读**（一行写 SQL 都没有），
--      因此它的授权可以放开到「能看在途的人」——两个码的授予链刻意不同。
--
-- ── 为什么是按钮（type=3）而不是新页面（type=2）────────────────────────────
--
--   前端在途页是 M2-07（另一张卡）。现在种 type=2 的 `path=/business/erp/inventory-transit`
--   会立刻在侧栏多出一个点不通的菜单（前端没路由 → 命中 SPA fallback 返回 HTML，
--   前端报「响应体格式不符合」）。所以本卡只种**权限点**：它在「角色-菜单」授权树里
--   可见可勾，不进侧栏；M2-07 接页面时按钮 `对账` 直接引本码，不需要再动菜单。
--
-- ── 授权口径：跟 `inventory-transit:list` 走（数据驱动，不手抄角色名单）───────
--
--   凡是**当前持有** `business:erp:inventory-transit:list`（V0421 的读码）的角色，
--   一并授予对账码 —— 「能看在途明细的人就该能给自己这本账对账」是同一条职责线。
--   手抄名单的风险是角色矩阵一变就漏/多；这里 `SELECT … FROM sys_role_menu` 现算，
--   跑一次就与当日矩阵对齐。
--   · 按**码**判重（角色已持有本码 ⇒ 整组跳过），重复执行不会长出第二行；
--   · 超管单独补（它靠 `*:*:*` 通配本可短路，显式授予只为权限树上能看清范围）；
--   · 空库首启：`sys_role` 里此时只有前序迁移建出来的业务角色（种子 `S__role_data.sql`
--     在**所有**版本化迁移之后才跑）；若一个角色都没有，自检块只打 NOTICE 不抛异常。
--
-- ── ID 段 ─────────────────────────────────────────────────────────────────
--
--   4220（4210 已被 V0421 的在途查询按钮占用）。4xxx 段与 09xx 段的插空互不干扰。
--
-- ── 幂等 / 回滚 ───────────────────────────────────────────────────────────
--
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的标题）；
--   · 授权：`NOT EXISTS` 去重，按码判重；
--   · 父节点用 `auth_code` **反查**而不是写死 0955：菜单 reorg 后仍挂在对的页下；
--   · down：只删本迁移种的授权行 + 菜单行，不碰任何业务数据，也不动 V0421 的码。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：父页面（即时库存）必须在，且码不能被别人占用
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_parent integer;
    n_code   integer;
BEGIN
    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0422: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 对账按钮要挂在它下面', n_parent;
    END IF;

    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-transit:audit'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0422: business:erp:inventory-transit:audit 已有 % 个承载节点（应为 0 或 1）', n_code;
    END IF;
    RAISE NOTICE 'V0422: 前置检查通过（即时库存页在；对账码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-0000-0000-000000004220'::uuid,
       'business:erp:inventory-transit:audit',
       3,
       'business:erp:inventory-transit:audit',
       NULL,
       p.id,
       NULL,
       30,          -- 兄弟按钮：库存巡检 10 / 在途库存查询 20 / 在途库存对账 30（同父唯一）
       1,
       NULL,
       '在途库存对账',
       FALSE,
       'business'
  FROM sys_menu p
 WHERE p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 授权：跟 `inventory-transit:list` 的持有角色走（数据驱动）+ 超管
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm_src.role_id, m.id
  FROM sys_menu m
  JOIN sys_role_menu rm_src
    ON rm_src.menu_id IN (SELECT id FROM sys_menu
                           WHERE auth_code = 'business:erp:inventory-transit:list'
                             AND deleted_at IS NULL)
  JOIN sys_role r ON r.id = rm_src.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004220'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = rm_src.role_id AND rm.menu_id = m.id
   );

-- 超管显式补一行（只为权限树可读；它靠 *:*:* 通配本来就能进）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
 CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id = '20000000-0000-0000-0000-000000004220'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ④ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_btn        integer;
    n_parent     integer;
    n_same       integer;
    n_roles      integer;
    n_missing    integer;
    n_granted    integer;
    n_dup_sort   integer;
    v            text;
BEGIN
    -- ===== 4.1 权限点存在、类型 / 码 / 显示名正确 =====
    SELECT count(*) INTO n_btn
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004220'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:inventory-transit:audit'
       AND name = 'business:erp:inventory-transit:audit'
       AND title = '在途库存对账'
       AND length(title) <= 12          -- 约定 12：title 是菜单名，不是功能说明
       AND path IS NULL                 -- 按钮不进侧栏 ⇒ 无 path
       AND is_visible = FALSE
       AND deleted_at IS NULL;
    IF n_btn <> 1 THEN
        RAISE EXCEPTION 'V0422 自检失败：对账按钮节点缺失或字段不对（menu_type 必须 3、path 必须 NULL、is_visible 必须 FALSE）';
    END IF;

    -- ===== 4.2 必须挂在「即时库存」页面下（挂错页面 = 授权树上找不到）=====
    SELECT count(*) INTO n_parent
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000004220'::uuid
       AND p.menu_type = 2
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0422 自检失败：对账按钮没有挂在「即时库存」页面下';
    END IF;

    -- ===== 4.3 码全局唯一（重复会让角色授权合并成两条勾选）=====
    SELECT count(*) INTO n_same
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-transit:audit'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0422 自检失败：business:erp:inventory-transit:audit 出现 % 次（应为 1）', n_same;
    END IF;

    -- ===== 4.4 同父 sort_order 不重复（AGENTS.md 约定 12：侧栏顺序不能每次刷）=====
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT parent_id FROM sys_menu
                                 WHERE id = '20000000-0000-0000-0000-000000004220'::uuid)
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0422 自检失败：同父节点下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 4.5 授权：持有在途读码的角色必须都拿到对账码（差集口径，不做计数相减）=====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0422：库里还没有任何角色（迁移先于种子），跳过授权核对；'
                     '种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_missing, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id IN (SELECT id FROM sys_menu
                                              WHERE auth_code = 'business:erp:inventory-transit:list'
                                                AND deleted_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000004220'::uuid);
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0422 自检失败：持在途读码但没拿到对账码的角色 % 个 → %',
                n_missing, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004220'::uuid
        ) THEN
            RAISE NOTICE 'V0422：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；'
                         '重放本迁移即可补齐）';
        END IF;

        SELECT count(DISTINCT rm.role_id) INTO n_granted
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE rm.menu_id = '20000000-0000-0000-0000-000000004220'::uuid;
        RAISE NOTICE 'V0422：授权核对通过（持有对账码的角色 % 个，缺口 0）', n_granted;
    END IF;

    RAISE NOTICE 'V0422 自检通过：1 个权限码（business:erp:inventory-transit:audit）挂在「即时库存」页下，menu_type=3、sort_order=30';
END $$;
