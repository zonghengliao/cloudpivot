-- ============================================================================
-- V0426__inventory_slow_moving（M2-10 · 库龄：呆滞料标记（超 N 天无出库）· 2026-11-16）
--
-- 卡：t_bb0fe3c6（M2-10【M2·仓库】库龄：呆滞料标记（超 N 天无出库））
-- 代码：`backend/app/src/business/erp/inventory_aging/slow_moving/{dto,model,handler}.rs`
-- 拆卡依据：`docs/planning/20261113-新卡清单.md` §M2 的 M2-10 行 —— 计算（M2-08）与
--   分段报表（M2-09）已落地，本卡只做**呆滞料标记**；单测扩充（M2-11）、前端页（M2-12）不在本卡。
--
-- ── 本迁移做两件事（都不建表、不建函数、不碰业务数据）────────────────────────
--
--   ① 阈值参数 `sys_config.erp.inventory.slow-moving-days = '90'`（NUMBER）
--      —— 「超 N 天无出库」里的 N：库龄**严格大于**该值且仍有剩余的批次视为呆滞；
--   ② 权限码 `business:erp:inventory-aging:slow-moving`（menu_type = 3 按钮，挂 0955「即时库存」）
--      —— 承载 `GET /business/erp/inventory-aging/slow-moving`。
--
-- ── 为什么 N 存 sys_config 而不是新开一张 biz_slow_moving_config 表 ─────────────
--
--   一个整数阈值不值得一张表（本仓已有同一类先例：`erp.fx.gain_loss_account` V0411、
--   `inventory_opening_equity_account` V0328）。而且 `sys_config` 已经带了：
--   · 「系统参数」页（`/support/config`，权限码 `support:config:update`）的查 / 改 / 审计；
--   · 写操作触发运行时快照 reload（改完立刻生效，不用重启）；
--   · `uk_sys_config_key` 部分唯一索引（`deleted_at IS NULL`）保证只有一个阈值。
--   所以**不新开写接口**：ERP 侧只读这个键，改值走参数页（与 fx_settings 同一取舍）。
--
-- ── 为什么阈值缺省是 90 ─────────────────────────────────────────────────────
--
--   与 M2-09 分段报表的「90 天以上」档对齐（库龄 91 天起）。这样 N=90 时本接口的每格
--   `qty_slow` **逐格等于**报表 OVER_90 档的数量 —— 两张表可以互相对拍，
--   而不是「两个都叫 >90 天、数字却不一样」（真库用例
--   `default_threshold_matches_report_over_90` 拿真账钉住这件事）。
--
-- ── 为什么算法一行都不写在这里 ─────────────────────────────────────────────
--
--   呆滞 = 库龄层按天数切一刀，库龄层本身是 V0424 的
--   `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`。
--   任何在本迁移里「顺手」再写一遍 FIFO 的做法，都会让「明细说这层剩 120、呆滞说 110」
--   变成无人报警的静默分叉。本迁移只用 `DO` 块**探针调用**那个函数验证形状，不复制它的逻辑。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--
--   · 参数：`NOT EXISTS (同键且未软删)` 才插入（重放不覆盖运维改过的阈值）；
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的标题）；
--   · 授权：`NOT EXISTS` 去重、按码判重（重放不增长）；
--   · 父节点用 `auth_code` 反查（菜单 reorg 后仍挂在对的页下）；
--   · down：只删本迁移种的授权行 / 菜单行 / 参数行（函数 / 流水 / 单据 / 其它参数一行不动）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：底座三张表在、库龄函数在、父页面唯一、码没被别人占用
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_tab    integer;
    n_fn     integer;
    n_parent integer;
    n_code   integer;
BEGIN
    SELECT count(*) INTO n_tab
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public'
       AND c.relname IN ('sys_config', 'sys_menu', 'sys_role_menu');
    IF n_tab <> 3 THEN
        RAISE EXCEPTION 'V0426: 系统底座表不全（sys_config / sys_menu / sys_role_menu 实得 % 张），请先应用系统底座迁移', n_tab;
    END IF;

    -- 呆滞判定要在库龄层上切，函数没建出来就没有数据源（V0424 没跑或回滚过）
    SELECT count(*) INTO n_fn
      FROM pg_proc p
      JOIN pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public'
       AND p.proname = 'biz_inventory_aging_layers';
    IF n_fn <> 1 THEN
        RAISE EXCEPTION 'V0426: 找不到函数 biz_inventory_aging_layers（实得 % 个）—— 呆滞料必须复用 M2-08 的库龄层函数', n_fn;
    END IF;

    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0426: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 呆滞料按钮要挂在它下面', n_parent;
    END IF;

    -- 同一个码只能有一个承载节点（多于一个会让角色授权页出现两处同码勾选）
    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:slow-moving'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0426: business:erp:inventory-aging:slow-moving 已有 % 个承载节点（应为 0 或 1）', n_code;
    END IF;
    RAISE NOTICE 'V0426: 前置检查通过（底座表在、库龄层函数在、即时库存页唯一、呆滞码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 阈值参数：erp.inventory.slow-moving-days = '90'
--    值只存整数文本（`config_type = NUMBER`），单位固定为「天」。
-- ---------------------------------------------------------------------------
INSERT INTO sys_config (id, config_key, config_value, config_type, description, category,
                        sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
SELECT gen_random_uuid(),
       'erp.inventory.slow-moving-days',
       '90',
       'NUMBER',
       '呆滞料判定阈值（天）：库龄严格大于该值且仍有剩余的入库批次视为呆滞（缺省 90，与库龄报表「90 天以上」档对齐；范围 1-3650）',
       'erp_inventory',
       10,
       2,
       TRUE,
       FALSE,
       FALSE,
       1
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_config
         WHERE config_key = 'erp.inventory.slow-moving-days'
           AND deleted_at IS NULL
 );

-- ---------------------------------------------------------------------------
-- ③ 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-0000-0000-000000004260'::uuid,
       'business:erp:inventory-aging:slow-moving',
       3,
       'business:erp:inventory-aging:slow-moving',
       NULL,
       p.id,
       NULL,
       60,          -- 同父：库存巡检(10) / 在途库存查询(20) / 在途对账(30) / 库龄批次层查询(40) / 库龄分段报表(50)
       1,
       NULL,
       '呆滞料标记',
       FALSE,
       'business'
  FROM sys_menu p
 WHERE p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 授权：跟「库龄分段报表」的持有角色走（数据驱动）+ 超管
--
--    为什么锚报表码而不是 `inventory:list`：呆滞料是库龄账的第三张视图
--    （明细 → 分布 → 处置清单），能看分布的角色才该直接拿到处置清单；
--    锚到「能看结存数量」会把呆滞金额顺带授出去（多授权比少授权难发现）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm_src.role_id, m.id
  FROM sys_menu m
  JOIN sys_role_menu rm_src
    ON rm_src.menu_id IN (SELECT id FROM sys_menu
                           WHERE auth_code = 'business:erp:inventory-aging:report'
                             AND deleted_at IS NULL)
  JOIN sys_role r ON r.id = rm_src.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004260'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = rm_src.role_id AND rm.menu_id = m.id
   );

-- 超管显式补一行（只为权限树可读；它靠 *:*:*
-- 通配本来就能进）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
 CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id = '20000000-0000-0000-0000-000000004260'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ⑤ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_value    text;
    v_days     integer;
    n_cfg      integer;
    n_rows     bigint;
    n_btn      integer;
    n_parent   integer;
    n_same     integer;
    n_dup_sort integer;
    n_roles    integer;
    n_missing  integer;
    n_granted  integer;
    v          text;
BEGIN
    -- ===== 5.1 阈值参数在、类型 / 取值可用 =====
    SELECT count(*), max(config_value::text) INTO n_cfg, v_value
      FROM sys_config
     WHERE config_key = 'erp.inventory.slow-moving-days'
       AND deleted_at IS NULL
       AND status = 1;
    IF n_cfg <> 1 THEN
        RAISE EXCEPTION 'V0426 自检失败：阈值参数 erp.inventory.slow-moving-days 应有 1 行启用配置，实得 % 行', n_cfg;
    END IF;
    -- 运维可能在重放前把值改成别的合法整数；只要求「能解析且落在 1..3650」
    BEGIN
        v_days := v_value::integer;
    EXCEPTION WHEN others THEN
        RAISE EXCEPTION 'V0426 自检失败：阈值参数值 `%` 不是整数（呆滞判定无法执行）', v_value;
    END;
    IF v_days < 1 OR v_days > 3650 THEN
        RAISE EXCEPTION 'V0426 自检失败：阈值参数值 % 超出范围 1..3650', v_days;
    END IF;

    -- ===== 5.2 行为探针：**呆滞聚合真正要跑的那个形状**能执行 =====
    -- 与 `slow_moving::model::cells_sql` 同形（库函数 → age_days / is_slow → 按物料仓库条件聚合），
    -- 只不过基准日写 CURRENT_DATE、物料/仓库给 NULL、阈值取参数里的值。
    -- 空库时 0 行，但证明了「函数 + 日期算术 + 布尔列 + FILTER 条件聚合」这条链路在库层跑得通
    -- （只 CREATE 不调用的话，列名打错要等到第一次出表才炸）。
    EXECUTE format($probe$
        SELECT count(*) FROM (
            WITH layers AS (
                SELECT l.material_id, l.warehouse_id, l.inbound_date, l.qty_remaining, l.amount_remaining
                  FROM biz_inventory_aging_layers(CURRENT_DATE, NULL::uuid, NULL::uuid) l
                 WHERE l.qty_remaining > 0
            ),
            aged AS (
                SELECT a.material_id, a.warehouse_id, a.inbound_date, a.qty_remaining, a.amount_remaining,
                       (CURRENT_DATE - a.inbound_date) AS age_days,
                       ((CURRENT_DATE - a.inbound_date) > %s) AS is_slow
                  FROM layers a
            )
            SELECT material_id, warehouse_id,
                   count(*)::bigint AS layer_count,
                   sum(qty_remaining) AS qty_on_hand,
                   count(*) FILTER (WHERE is_slow)::bigint AS slow_layer_count,
                   coalesce(sum(qty_remaining) FILTER (WHERE is_slow), 0) AS qty_slow
              FROM aged
             GROUP BY material_id, warehouse_id
        ) probe
    $probe$, v_days) INTO n_rows;
    RAISE NOTICE 'V0426: 呆滞聚合探针执行成功（阈值 % 天，当前 % 行聚合结果）', v_days, n_rows;

    -- ===== 5.3 参数键唯一（部分唯一索引 + 软删可复用，这里钉住「未软删只有一行」）=====
    SELECT count(*) INTO n_same
      FROM sys_config
     WHERE config_key = 'erp.inventory.slow-moving-days'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0426 自检失败：参数键 erp.inventory.slow-moving-days 未软删行有 % 条（应为 1）', n_same;
    END IF;

    -- ===== 5.4 权限点存在、类型 / 码 / 显示名正确 =====
    SELECT count(*) INTO n_btn
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004260'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:inventory-aging:slow-moving'
       AND name = 'business:erp:inventory-aging:slow-moving'
       AND title = '呆滞料标记'
       AND path IS NULL          -- 按钮不进侧栏 ⇒ 无 path（type=2 才需要）
       AND is_visible = FALSE
       AND deleted_at IS NULL;
    IF n_btn <> 1 THEN
        RAISE EXCEPTION 'V0426 自检失败：呆滞料按钮节点缺失或字段不对（menu_type 必须 3、path 必须 NULL、is_visible 必须 FALSE、title 必须「呆滞料标记」）';
    END IF;

    -- ===== 5.5 必须挂在「即时库存」页面下（挂错页面 = 授权树上找不到）=====
    SELECT count(*) INTO n_parent
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000004260'::uuid
       AND p.menu_type = 2
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0426 自检失败：呆滞料按钮没有挂在「即时库存」页面下';
    END IF;

    -- ===== 5.6 码全局唯一（重复会让角色授权合并成两条勾选）=====
    SELECT count(*) INTO n_same
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:slow-moving'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0426 自检失败：business:erp:inventory-aging:slow-moving 出现 % 次（应为 1）', n_same;
    END IF;

    -- ===== 5.7 同父 sort_order 不重复（AGENTS.md 约定 12）=====
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT parent_id FROM sys_menu
                                 WHERE id = '20000000-0000-0000-0000-000000004260'::uuid)
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0426 自检失败：同父节点下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 5.8 授权：持报表码的角色必须都拿到呆滞码 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0426：库里还没有任何角色（迁移先于种子），跳过授权核对；种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_missing, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id IN (SELECT id FROM sys_menu
                                              WHERE auth_code = 'business:erp:inventory-aging:report'
                                                AND deleted_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000004260'::uuid);
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0426 自检失败：持库龄报表码但没拿到呆滞码的角色 % 个 → %',
                n_missing, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004260'::uuid
        ) THEN
            RAISE NOTICE 'V0426：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；重放本迁移即可补齐）';
        END IF;
    END IF;

    SELECT count(*) INTO n_granted FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004260'::uuid;
    RAISE NOTICE 'V0426: 自检通过（阈值参数 % 天 · 呆滞码 1 个承载节点、挂即时库存页、授权行 % 条）', v_days, n_granted;
END $$;
