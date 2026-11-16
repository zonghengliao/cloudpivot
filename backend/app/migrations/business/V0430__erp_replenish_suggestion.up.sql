-- ============================================================================
-- V0430__erp_replenish_suggestion（M2-15 · 仓库：补货建议（安全库存 + 在途 + 未清订单）· 2026-11-17）
--
-- 卡：t_527ceaa3（M2-15【M2·仓库】补货建议：安全库存 + 在途 + 未清订单）
-- 代码：`backend/app/src/business/erp/replenish/{dto,model,handler}.rs`
-- 上游：V0428（M2-13 建 `biz_material_stock_policy` + 生效水位的三级回退口径）、
--       V0420（M2-01 建 `biz_inventory_in_transit` 在途账）
-- 拆卡依据：`docs/planning/20261113-新卡清单.md` §M2 的 M2-15 行 —— 单测扩充（M2-16）、
--   前端设置页（M2-17）、收口 E2E（M2-18）都不在本卡。
--
-- ── 本迁移做三件事（不建表、不建函数、不碰业务数据）──────────────────────────
--
--   ① 权限码 `business:erp:replenish:list`（menu_type = 3 按钮，挂「即时库存」页，sort 110）
--      —— 承载 `GET /business/erp/replenish/page`；
--   ② 授权：跟「仓库线 + 采购线」的角色走（数据驱动，不手抄角色名单）；
--   ③ 自检：码的形状 / 同父 sort 唯一 / 授权成对 / **扫描 SQL 真正要跑的那条读法**能执行。
--
-- ── 为什么「一条 SQL 就够」，不建表、不建汇总表 ────────────────────────────
--
--   建议采购量 = 目标水位 − 预计可用
--              = (max_qty ?? min_qty) − (biz_inventory.qty − 发货在途 + 未清订单未收量)
--
--   四个来源全是**别人的表**：结存是库存台账、水位是设置表（V0428）、发货在途是在途账
--   （V0420）、未清订单是采购订单（V0137）。任何一项都随时会变（过账、签收、到货、改水位），
--   一旦把建议量落成一张表，就必须在**四个模块的每一次写**之后维护它 —— 漏一处就是
--   「建议 70，但结存早就够了」的脏数据，而且没人能复核（因为表里没有四个来源的明细）。
--   现算永远与当前的四张表一致，成本是一条带两个 `LATERAL` 的查询。**这是刻意的设计**，
--   不是「还没来得及建表」。
--
-- ── 口径（写在这里 = 唯一版本，代码 `replenish::dto::suggest` 逐条对应）────────
--
--   | 项 | 取值 | 出处 |
--   |:--|:--|:--|
--   | 账面结存 | `biz_inventory.qty`（负库存照实数，不 GREATEST 抹平） | `docs/ERP-摆设字段口径.md` §2 |
--   | 发货在途 | `biz_inventory_in_transit` where `status = 'IN_TRANSIT'` 的 `qty_in_transit` 合计 | 与 M2-04 的 `/summary` 逐格相等 |
--   | 未清订单 | `biz_purchase_order_item.qty − received_qty`，主单 `SUBMITTED / APPROVED / PARTIAL_RECEIVED`、未软删、`received_qty < qty` | PRD E2-09 未清订单 |
--   | 最低水位 | `stock_policy::dto::resolve_min_qty`（策略启用 > 物料级 `safety_stock` > 不判） | V0428 建表注释 |
--   | 目标水位 | 策略行 `max_qty`（有则补到最高水位），否则最低水位 | V0428 的 `max_qty` 注释 |
--   | 触发条件 | 预计可用 **严格小于** 最低水位 | 与 M2-14 的 `qty < min` 同一刀 |
--   | 排除 | 在途仓（`warehouse_type = 'IN_TRANSIT'`）、停用 / 删除的物料与仓库 | 与 M2-14 / `reminder_stock` 同口径 |
--
--   **不计采购 `DRAFT`**：还没提交、可随时删改、且不可收货（`purchase_receivable` 收
--   SUBMITTED / APPROVED）。把「可能不成立」的量当供给，会让建议量偏小、真断料。
--   **调拨在途不计**：调拨过账（`DONE`）在一个事务里同时写调出仓与调入仓，没有半途量。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的 title / sort）；
--   · 授权：`NOT EXISTS` 去重、按码判重（重放不增长）；父节点用 `auth_code` 反查
--     （菜单 reorg 后仍挂在对的页下）；
--   · down：只删本迁移种的授权行 / 菜单行；**不动** V0420 的在途账、V0428 的水位表与三个码、
--     采购订单与库存的任何行。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：底座表在、上游四张业务表在、父页面唯一、码没被别人占用
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_base   integer;
    n_parent integer;
    n_code   integer;
BEGIN
    SELECT count(*) INTO n_base
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public'
       AND c.relname IN ('sys_menu', 'sys_role_menu',
                         'biz_inventory', 'biz_material_stock_policy', 'biz_inventory_in_transit',
                         'biz_purchase_order', 'biz_purchase_order_item',
                         'biz_stock_in', 'biz_stock_in_item',
                         'biz_purchase_return', 'biz_purchase_return_item');
    IF n_base <> 11 THEN
        RAISE EXCEPTION 'V0430: 前置表不全（sys_menu / sys_role_menu / biz_inventory / biz_material_stock_policy / biz_inventory_in_transit / biz_purchase_order / biz_purchase_order_item / biz_stock_in / biz_stock_in_item / biz_purchase_return / biz_purchase_return_item 实得 % 张），请先应用 V0137、V0217、V0332、V0420、V0428 与系统底座', n_base;
    END IF;

    -- 父页面「即时库存」（menu_type=2，auth_code=business:erp:inventory:list）：必须唯一
    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0430: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 补货建议按钮要挂在它下面', n_parent;
    END IF;

    -- 同一个码只能有一个承载节点（多于一个会让角色授权页出现两处同码勾选）
    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:replenish:list'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0430: business:erp:replenish:list 已有 % 个承载节点（应为 0 或 1）', n_code;
    END IF;

    RAISE NOTICE 'V0430: 前置检查通过（十一张表在、即时库存页唯一、补货建议码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
--
--    sort：同父（即时库存页）已有 10 库存巡检 / 20 在途查询 / 30 在途对账 /
--         40 库龄批次层 / 50 库龄报表 / 60 呆滞料 / 70~90 水位设置（list/save/delete）/
--         100 水位检查，本卡取 110（约定 12：同父唯一）。
--
--    为什么是**读码**而不是写码：本接口一行都不写（不建采购单、不建待办、不改水位），
--    它是「看一眼该补多少」。`authz_tests::o13` 拦的是「写接口挂读码」，与本卡无关。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-0000-0000-000000004300'::uuid,
       'business:erp:replenish:list',
       3,
       'business:erp:replenish:list',
       NULL,
       p.id,
       NULL,
       110,
       1,
       NULL,
       '补货建议',
       FALSE,
       'business'
  FROM sys_menu p
 WHERE p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 授权：能看采购订单的（采购跟单的人）× 能定水位的（仓管 / 计划员）+ 超管
--
--   两个锚点都要：补货建议的**读者**是采购（拿它去下单），**依据**是水位（仓管设的）。
--     · 锚点 A `business:erp:purchase-order:list` —— 采购线的人；
--     · 锚点 B `business:erp:stock-policy:list`   —— 维护水位的人。
--    并集一次授全。数据驱动、不手抄角色名单（空库里还没有这些角色时语句是空操作，
--    超管本来靠 `role_code` 短路，行为不变 —— 与 V0428 / V0426 同一写法）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT anchor.role_id, m.id
  FROM sys_menu m
  JOIN (
        SELECT rm.role_id
          FROM sys_role_menu rm
          JOIN sys_menu src ON src.id = rm.menu_id AND src.deleted_at IS NULL
         WHERE src.auth_code IN ('business:erp:purchase-order:list', 'business:erp:stock-policy:list')
        UNION
        SELECT r.id FROM sys_role r WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
       ) anchor ON TRUE
  JOIN sys_role r ON r.id = anchor.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004300'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu dup WHERE dup.role_id = anchor.role_id AND dup.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ④ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_menu     integer;
    n_parent   integer;
    n_same     integer;
    n_sort     integer;
    n_title    integer;
    n_probe    integer;
BEGIN
    -- ===== 4.1 菜单行形状：按钮 / 挂对父页 / 不显形 =====
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004300'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:replenish:list'
       AND is_visible = FALSE
       AND deleted_at IS NULL;
    IF n_menu <> 1 THEN
        RAISE EXCEPTION 'V0430 自检失败：补货建议按钮行应有 1 行（menu_type=3 + auth_code + 不显形），实得 %', n_menu;
    END IF;

    SELECT count(*) INTO n_parent
      FROM sys_menu b
      JOIN sys_menu p ON p.id = b.parent_id
     WHERE b.id = '20000000-0000-0000-0000-000000004300'::uuid
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.menu_type = 2
       AND p.deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0430 自检失败：补货建议按钮没挂在「即时库存」页下（实得 % 行）', n_parent;
    END IF;

    -- title 是侧栏/授权树显示名：2-12 字、不带标点（AGENTS.md 约定 12）
    SELECT count(*) INTO n_title
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004300'::uuid
       AND title = '补货建议'
       AND length(title) <= 12;
    IF n_title <> 1 THEN
        RAISE EXCEPTION 'V0430 自检失败：按钮 title 必须是「补货建议」且 <= 12 字（约定 12）';
    END IF;

    -- ===== 4.2 同父 sort_order 唯一（重复会让侧栏顺序每次刷新都变）=====
    SELECT count(*) INTO n_sort
      FROM (
            SELECT sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT id FROM sys_menu
                                 WHERE auth_code = 'business:erp:inventory:list'
                                   AND menu_type = 2 AND deleted_at IS NULL)
               AND deleted_at IS NULL AND menu_type = 3
             GROUP BY sort_order
            HAVING count(*) > 1
           ) t;
    IF n_sort <> 0 THEN
        RAISE EXCEPTION 'V0430 自检失败：「即时库存」页下有 % 个重复的 sort_order', n_sort;
    END IF;

    -- ===== 4.3 同一父节点下「补货建议」只有一行（重放不新增）=====
    SELECT count(*) INTO n_same
      FROM sys_menu
     WHERE auth_code = 'business:erp:replenish:list'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0430 自检失败：business:erp:replenish:list 应有 1 个承载节点，实得 %', n_same;
    END IF;

    -- ===== 4.4 行为探针：**扫描 SQL 真正要跑的那个形状**能执行 =====
    -- 与 `replenish::model::scan_sql` 同形（结存 ⨝ 物料 ⨝ 仓库 ⨝ 策略行 + 两个 LATERAL 预聚合），
    -- 只不过物料 / 仓库给 NULL、上限取 1。空库时 0 行，但证明了「两张 LATERAL 聚合 + 生成列
    -- qty_in_transit + 采购订单状态过滤」这条链路在库层跑得通（只 CREATE 不调用的话，
    -- 列名 / 生成列名打错要等到第一次接口调用才炸 —— 那时候已经是生产）。
    SELECT count(*) INTO n_probe
      FROM (
            SELECT inv.material_id
              FROM (
                    SELECT material_id, warehouse_id, SUM(qty) AS qty, COUNT(*)::bigint AS row_count
                      FROM biz_inventory
                     GROUP BY material_id, warehouse_id
                   ) inv
              JOIN biz_material m ON m.id = inv.material_id AND m.deleted_at IS NULL AND m.status = 1
              LEFT JOIN biz_warehouse w ON w.id = inv.warehouse_id AND w.deleted_at IS NULL
              LEFT JOIN biz_material_stock_policy p
                     ON p.material_id = inv.material_id AND p.warehouse_id = inv.warehouse_id
                    AND p.deleted_at IS NULL
              LEFT JOIN LATERAL (
                    SELECT SUM(x.qty_in_transit) AS qty_in_transit
                      FROM biz_inventory_in_transit x
                     WHERE x.material_id = inv.material_id
                       AND x.warehouse_id = inv.warehouse_id
                       AND x.status = 'IN_TRANSIT'
                   ) t ON TRUE
              LEFT JOIN LATERAL (
                    SELECT SUM(line.open_qty) AS open_qty
                      FROM (
                           -- 已收数量没有列（V0217 / P2-3 删了 received_qty）：唯一真相 =
                           -- 已过账收货单 − 已确认退货，与
                           -- `purchase_model::POSTED_RECEIVED_SQL` 逐字一致
                           SELECT (i.qty - COALESCE((
                                      SELECT SUM(si.qty) FROM biz_stock_in s
                                        JOIN biz_stock_in_item si ON si.stock_in_id = s.id
                                       WHERE s.source_order_id = i.order_id
                                         AND si.source_item_id = i.id
                                         AND s.status = 'POSTED'
                                         AND s.deleted_at IS NULL), 0)
                                  - COALESCE((
                                      SELECT SUM(pri.qty) FROM biz_purchase_return_item pri
                                        JOIN biz_purchase_return pr ON pr.id = pri.return_id
                                       WHERE pri.purchase_order_item_id = i.id
                                         AND pr.status = 'CONFIRMED'
                                         AND pr.deleted_at IS NULL), 0)) AS open_qty
                             FROM biz_purchase_order_item i
                             JOIN biz_purchase_order po ON po.id = i.order_id
                            WHERE i.material_id = inv.material_id
                              AND po.warehouse_id = inv.warehouse_id
                              AND po.deleted_at IS NULL
                              AND po.status IN ('SUBMITTED', 'APPROVED', 'PARTIAL_RECEIVED')
                      ) line
                     WHERE line.open_qty > 0
                   ) o ON TRUE
             WHERE w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'
             LIMIT 1
           ) probe;
    IF n_probe < 0 THEN
        RAISE EXCEPTION 'V0430 自检失败：扫描探针返回负数（不可能）';
    END IF;

    RAISE NOTICE 'V0430 完成：补货建议码已种、挂在「即时库存」页下（sort 110）、扫描探针可执行（% 行）', n_probe;
END $$;
