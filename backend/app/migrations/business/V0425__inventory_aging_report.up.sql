-- ============================================================================
-- V0425__inventory_aging_report（M2-09 · 库龄分析：报表（0-30/31-60/61-90/90+）· 2026-11-16）
--
-- 卡：t_4e2cb6f6（M2-09【M2·仓库】库龄分析：报表（0-30/31-60/61-90/90+））
-- 代码：`backend/app/src/business/erp/inventory_aging/report/{dto,model,handler}.rs`
-- 拆卡依据：`docs/planning/20261113-新卡清单.md` §M2 的 M2-09 行 —— 计算（M2-08）已经落了
--   库函数 `biz_inventory_aging_layers(...)`，本卡只做**分段报表**；呆滞料（M2-10）、
--   单测扩充（M2-11）、前端页（M2-12）都不在本卡。
--
-- ── 本迁移只做一件事：给报表接口开权限码 ───────────────────────────────────
--
--   权限码 `business:erp:inventory-aging:report`（menu_type = 3 按钮，挂 0955「即时库存」）
--   承载 `GET /business/erp/inventory-aging/report`（按 物料 × 仓库 汇总 0-30/31-60/61-90/90+）。
--
--   **本迁移不建表、不建函数、不碰业务数据**：库龄层的唯一实现是 V0424 的
--   `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`，报表只是在它
--   外面 `GROUP BY 物料, 仓库`。把 FIFO 再写一遍（哪怕只是「顺手」）就会出现
--   「明细说这层剩 120、报表说 110」而没有任何地方报错 —— 这是 M2-08 定下的口径，
--   M2-09 只许复用、不许重算。
--
-- ── 为什么要单独开一个码（不复用 `inventory-aging:list`）────────────────────
--
--   `inventory-aging:list`（V0424）是**批次层明细**的读码：一层一行，可以按批号钻取到
--   「哪张入库单的哪一批货压了多久」。本卡的报表是**聚合**：按物料 × 仓库给出各库龄档的
--   数量与参考金额 —— 它是另一张表（能看明细的人未必该看全公司的库存金额分布），
--   与应收账龄 `business:erp:receivable:aging` / 应付账龄
--   `business:erp:report:payable-aging` 单开报表码同一个取向。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的标题）；
--   · 授权：`NOT EXISTS` 去重、按码判重（重放不增长）；
--   · 父节点用 `auth_code` 反查（菜单 reorg 后仍挂在对的页下）；
--   · down：只删本迁移种的授权行与菜单行（函数 / 流水 / 单据一行不动）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：M2-08 的函数必须在、父页面必须唯一、码不能被别人占了
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_fn     integer;
    n_parent integer;
    n_code   integer;
BEGIN
    -- 报表算的是 FIFO 层，函数没建出来就没有数据源（说明 V0424 没跑或回滚过）
    SELECT count(*) INTO n_fn
      FROM pg_proc p
      JOIN pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public'
       AND p.proname = 'biz_inventory_aging_layers';
    IF n_fn <> 1 THEN
        RAISE EXCEPTION 'V0425: 找不到函数 biz_inventory_aging_layers（实得 % 个）—— 报表必须复用 M2-08 的库龄层函数', n_fn;
    END IF;

    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0425: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 库龄报表按钮要挂在它下面', n_parent;
    END IF;

    -- 同一个码只能有一个承载节点（多于一个会让角色授权页出现两处同码勾选）
    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:report'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0425: business:erp:inventory-aging:report 已有 % 个承载节点（应为 0 或 1）', n_code;
    END IF;
    RAISE NOTICE 'V0425: 前置检查通过（库龄层函数在；库龄报表码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-0000-0000-000000004250'::uuid,
       'business:erp:inventory-aging:report',
       3,
       'business:erp:inventory-aging:report',
       NULL,
       p.id,
       NULL,
       50,          -- 与兄弟按钮「库存巡检」(10) /「在途库存查询」(20) /「在途对账」(30) /「库龄批次层查询」(40) 并列
       1,
       NULL,
       '库龄分段报表',
       FALSE,
       'business'
  FROM sys_menu p
 WHERE p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 授权：跟「库龄批次层查询」的持有角色走（数据驱动）+ 超管
--
--    为什么锚 `inventory-aging:list` 而不是 `inventory:list`：报表与明细是同一本库龄账的
--    两个视图，能看明细的角色才该看聚合；反而「只给看结存数量」的角色不该顺手拿到
--    库存金额分布。锚错了的后果是**多授权**（比少授权难发现）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm_src.role_id, m.id
  FROM sys_menu m
  JOIN sys_role_menu rm_src
    ON rm_src.menu_id IN (SELECT id FROM sys_menu
                           WHERE auth_code = 'business:erp:inventory-aging:list'
                             AND deleted_at IS NULL)
  JOIN sys_role r ON r.id = rm_src.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004250'::uuid
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
   AND m.id = '20000000-0000-0000-0000-000000004250'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ④ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_sig      text;
    v_cols     bigint;
    n_btn      integer;
    n_parent   integer;
    n_same     integer;
    n_dup_sort integer;
    n_roles    integer;
    n_missing  integer;
    n_granted  integer;
    v          text;
BEGIN
    -- ===== 4.1 库龄层函数的出参列够报表用（少一列报表就会静默算出 0）=====
    SELECT pg_get_function_result(p.oid) INTO v_sig
      FROM pg_proc p
      JOIN pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public'
       AND p.proname = 'biz_inventory_aging_layers';
    IF v_sig IS NULL THEN
        RAISE EXCEPTION 'V0425 自检失败：函数 biz_inventory_aging_layers 不存在';
    END IF;
    IF position('material_id' IN v_sig) = 0
       OR position('warehouse_id' IN v_sig) = 0
       OR position('qty_remaining' IN v_sig) = 0
       OR position('amount_remaining' IN v_sig) = 0
       OR position('inbound_date' IN v_sig) = 0 THEN
        RAISE EXCEPTION 'V0425 自检失败：函数出参缺报表必需列（material_id / warehouse_id / qty_remaining / amount_remaining / inbound_date），实得 %', v_sig;
    END IF;

    -- ===== 4.2 行为探针：**报表真正要跑的那个形状**能执行 =====
    -- 与 `report::model::cells` 同形（WITH 层 → 算 age_days → 按物料/仓库/档位聚合），
    -- 只不过基准日写 CURRENT_DATE、物料/仓库给 NULL。空库时 0 行，但那证明了
    -- 「函数 + 日期算术 + GROUP BY + CASE 分档」这条链路在库层真的跑得通
    -- （只 CREATE 不调用的话，列名打错要等到第一次出表才炸）。
    SELECT count(*) INTO v_cols
      FROM (
            WITH layers AS (
                SELECT l.material_id, l.warehouse_id, l.inbound_date, l.qty_remaining, l.amount_remaining
                  FROM biz_inventory_aging_layers(CURRENT_DATE, NULL::uuid, NULL::uuid) l
                 WHERE l.qty_remaining > 0
            ),
            with_age AS (
                SELECT w.*, (CURRENT_DATE - w.inbound_date) AS age_days FROM layers w
            )
            SELECT material_id, warehouse_id,
                   CASE WHEN age_days <= 30 THEN 'D0_30'
                        WHEN age_days <= 60 THEN 'D31_60'
                        WHEN age_days <= 90 THEN 'D61_90'
                        ELSE 'OVER_90' END AS bucket,
                   count(*) AS layer_count,
                   sum(qty_remaining) AS qty,
                   sum(amount_remaining) AS amount
              FROM with_age
             GROUP BY material_id, warehouse_id,
                   CASE WHEN age_days <= 30 THEN 'D0_30'
                        WHEN age_days <= 60 THEN 'D31_60'
                        WHEN age_days <= 90 THEN 'D61_90'
                        ELSE 'OVER_90' END
           ) probe;
    RAISE NOTICE 'V0425: 报表聚合探针执行成功（当前 % 行聚合结果）', v_cols;

    -- ===== 4.3 权限点存在、类型 / 码 / 显示名正确 =====
    SELECT count(*) INTO n_btn
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004250'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:inventory-aging:report'
       AND name = 'business:erp:inventory-aging:report'
       AND title = '库龄分段报表'
       AND path IS NULL          -- 按钮不进侧栏 ⇒ 无 path（type=2 才需要）
       AND is_visible = FALSE
       AND deleted_at IS NULL;
    IF n_btn <> 1 THEN
        RAISE EXCEPTION 'V0425 自检失败：库龄报表按钮节点缺失或字段不对（menu_type 必须 3、path 必须 NULL、is_visible 必须 FALSE）';
    END IF;

    -- ===== 4.4 必须挂在「即时库存」页面下（挂错页面 = 授权树上找不到）=====
    SELECT count(*) INTO n_parent
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000004250'::uuid
       AND p.menu_type = 2
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0425 自检失败：库龄报表按钮没有挂在「即时库存」页面下';
    END IF;

    -- ===== 4.5 码全局唯一（重复会让角色授权合并成两条勾选）=====
    SELECT count(*) INTO n_same
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:report'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0425 自检失败：business:erp:inventory-aging:report 出现 % 次（应为 1）', n_same;
    END IF;

    -- ===== 4.6 同父 sort_order 不重复（AGENTS.md 约定 12）=====
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT parent_id FROM sys_menu
                                 WHERE id = '20000000-0000-0000-0000-000000004250'::uuid)
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0425 自检失败：同父节点下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 4.7 授权：持库龄明细码的角色必须都拿到报表码 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0425：库里还没有任何角色（迁移先于种子），跳过授权核对；种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_missing, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id IN (SELECT id FROM sys_menu
                                              WHERE auth_code = 'business:erp:inventory-aging:list'
                                                AND deleted_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000004250'::uuid);
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0425 自检失败：持库龄明细码但没拿到报表码的角色 % 个 → %',
                n_missing, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004250'::uuid
        ) THEN
            RAISE NOTICE 'V0425：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；重放本迁移即可补齐）';
        END IF;
    END IF;

    SELECT count(*) INTO n_granted FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004250'::uuid;
    RAISE NOTICE 'V0425: 自检通过（库龄分段报表码 1 个承载节点、挂即时库存页、授权行 % 条）', n_granted;
END $$;
