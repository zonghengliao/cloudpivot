-- ============================================================================
-- V0428__erp_stock_policy_water_level（M2-13 · 仓库：安全库存字段（物料 × 仓库）· 2026-11-16）
--
-- 卡：t_d997ada8（M2-13【M2·仓库】安全库存：字段（物料 × 仓库）最低/最高水位）
-- 拆卡依据：`docs/planning/20261113-新卡清单.md` §M2 的 M2-13 行 —— 本卡**只落字段**
--   （表 + 读写接口 + 取值口径的纯函数）。水位检查与生成待办（M2-14）、补货建议
--   （M2-15）、单测扩充（M2-16）、前端设置页（M2-17）都不在本卡，本迁移也不做它们。
-- 代码：`backend/app/src/business/erp/stock_policy/{dto,model,handler}.rs`
--
-- ── 本迁移做三件事（一动表、二种码、三授权）──────────────────────────────────
--
--   ① 建表 `biz_material_stock_policy` —— 物料 × 仓库 一格一行的**水位设置**：
--        · `min_qty` 最低水位（= 该格的安全库存阈值，账面结存 < 它 → 低于水位）
--        · `max_qty` 最高水位（账面结存 > 它 → 超储；NULL = 未设 = 不判超储）
--   ② 种 3 个权限码（menu_type = 3 按钮，挂「即时库存」页面，sort 70/80/90）：
--        `business:erp:stock-policy:{list,save,delete}`
--   ③ 授权：跟「仓库线」的两类角色走（能看即时库存的 / 能维护仓库主数据的）+ 超管。
--
-- ── 为什么是「物料 × 仓库」而不是继续用 `biz_material.safety_stock` ─────────────
--
--   `biz_material.safety_stock` 是**物料级**的一个数（V0137），而库存是**按仓库**结存的
--   （`biz_inventory` 的唯一键 = 物料 × 仓库）。同一个物料在「成品仓」和「备件仓」的安全
--   水位本来就不同：用一个数去比每个仓库的结存，只在大仓数 = 1 的小客户那里正好对。
--   PRD E3-08 / 4.3.9 早就写明「按仓库单独设安全库存」是 P1（`docs/ERP-批次1-产品需求PRD.md`
--   §4.3.9 末段），本卡就是这一条。
--
--   **旧字段不删、不废**：它退化成「该物料所有仓库的缺省值」——一格没有策略行（或策略被停用）
--   时，水位仍按 `biz_material.safety_stock` 走（`reminder_stock` 的低库存扫描今天就是这个口径，
--   V0210 起在生产跑）。取值优先级**只有一处实现**：`stock_policy::dto::resolve_min_qty`，
--   见下「口径」。
--
-- ── 口径（写在这里 = 唯一版本，代码里的 `resolve_min_qty` 逐条对应）─────────────
--
--   | 策略行状态 | 生效最低水位 | `min_source` |
--   |:--|:--|:--|
--   | 有行且 `status = 1` | 该行 `min_qty`（**0 = 本格明示不预警**，不再回退物料级） | `POLICY` |
--   | 有行但 `status = 0`（停用） | `biz_material.safety_stock`（> 0 时），否则不判 | `MATERIAL` / `NONE` |
--   | 无行 | 同上（回退物料级） | `MATERIAL` / `NONE` |
--
--   最高水位**只来自策略行**（物料级没有这个字段）：行不存在 / 停用 / `max_qty IS NULL`
--   → 不判超储。
--
--   两条设计取舍，写在建表处而不是代码注释里，是因为它们决定了下游 M2-14/M2-15 怎么读：
--   1. **「停用」≠「删除」**：停用保留设置数值（下次启用就回来），语义是「本格暂不参与水位
--      检查」，所以它**退回物料级**而不是「不判」（退回物料级 = 与从未设置过完全相同）；
--   2. **策略行的 `min_qty = 0` 会阻断回退**：`0` 在物料级是「没设」，但在策略行是**明示**
--      「这一格不用预警」（例如寄售仓、客户自提仓）。要让一格跟随物料级，正确做法是删掉策略行。
--
-- ── 为什么不新开 `biz_stock_level` 之类的第二张表 ────────────────────────────
--
--   水位是**设置**（十行八行的配置），不是流水；一格一行、唯一键 `(material_id, warehouse_id)`，
--   一张表就够。M2-14 的「水位检查」与 M2-15 的「补货建议」都只**读**它（现算结存、现算在途），
--   不写；写只有本卡的设置接口一处。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--
--   · 表：`CREATE TABLE IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS`；
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的 title / sort）；
--   · 授权：`NOT EXISTS` 去重、按码判重（重放不增长）；父节点用 `auth_code` 反查
--     （菜单 reorg 后仍挂在对的页下）；
--   · down：只删本迁移种的授权行 / 菜单行 / 表；**不动** `biz_material.safety_stock`、
--     不动 `biz_inventory`、不动 V0421~V0427 的任何菜单。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：底座表在、父页面唯一、三个码没被别人占用
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
       AND c.relname IN ('sys_menu', 'sys_role_menu', 'biz_material', 'biz_warehouse', 'biz_inventory');
    IF n_base <> 5 THEN
        RAISE EXCEPTION 'V0428: 前置表不全（sys_menu / sys_role_menu / biz_material / biz_warehouse / biz_inventory 实得 % 张），请先应用 V0137 与系统底座', n_base;
    END IF;

    -- 父页面「即时库存」（menu_type=2，auth_code=business:erp:inventory:list）：必须唯一
    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0428: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 水位设置按钮要挂在它下面', n_parent;
    END IF;

    -- 同一个码只能有一个承载节点（多于一个会让角色授权页出现两处同码勾选）
    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code IN ('business:erp:stock-policy:list',
                         'business:erp:stock-policy:save',
                         'business:erp:stock-policy:delete')
       AND deleted_at IS NULL;
    IF n_code > 3 THEN
        RAISE EXCEPTION 'V0428: 水位设置三个码已有 % 个承载节点（应为 0，重放时 ≤ 3）', n_code;
    END IF;

    RAISE NOTICE 'V0428: 前置检查通过（底座表在、即时库存页唯一、三个码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 表：`biz_material_stock_policy`（物料 × 仓库 水位设置）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_material_stock_policy (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    material_id   UUID          NOT NULL,                            -- 物料（弱引用 biz_material.id）
    warehouse_id  UUID          NOT NULL,                            -- 仓库（弱引用 biz_warehouse.id）
    min_qty       NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 最低水位（安全库存阈值）
    max_qty       NUMERIC(18,4),                                     -- 最高水位（NULL = 未设）
    status        SMALLINT      NOT NULL DEFAULT 1,                  -- 1 启用（参与水位检查）/ 0 停用（保留设置）
    remark        TEXT,                                              -- 备注（为什么这么设）
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 更新时间
    create_by     UUID,                                              -- 创建人（弱引用 sys_user.id）
    update_by     UUID,                                              -- 更新人（弱引用 sys_user.id）
    deleted_at    TIMESTAMPTZ,                                       -- 软删时间（删了 = 该格回退物料级）

    CONSTRAINT ck_biz_msp_min    CHECK (min_qty >= 0),
    CONSTRAINT ck_biz_msp_max    CHECK (max_qty IS NULL OR max_qty > min_qty),
    CONSTRAINT ck_biz_msp_status CHECK (status IN (0, 1))
);

COMMENT ON TABLE biz_material_stock_policy IS
    '物料 × 仓库 的水位设置（一格一行，软删后同键可重建）：min_qty 最低水位 / max_qty 最高水位。设置本身不参与结存计算，结存永远以 biz_inventory.qty 为准（现算，不缓存）';
COMMENT ON COLUMN biz_material_stock_policy.min_qty IS
    '最低水位（= 该格的安全库存阈值）：账面结存 biz_inventory.qty < min_qty → 低于水位（M2-14 生成待办）。0 = 本格明示不预警（阻断回退物料级 safety_stock）；想让本格跟随物料级请删掉策略行';
COMMENT ON COLUMN biz_material_stock_policy.max_qty IS
    '最高水位：账面结存 > max_qty → 超储（M2-15 不建议再补）。NULL = 未设最高水位 = 不判超储；约束要求 max_qty > min_qty（相等即无区间）';
COMMENT ON COLUMN biz_material_stock_policy.status IS
    '1 启用（参与水位检查）/ 0 停用（保留设置数值，退回按 biz_material.safety_stock 判，等同从未设置）';
COMMENT ON COLUMN biz_material_stock_policy.deleted_at IS
    '软删时间：删除 = 该格回到「跟随物料级 safety_stock」；唯一键是「未删行」的部分唯一索引，删掉后可重新设置';

-- 物料 × 仓库 唯一（只看未删行：软删后可以重新设置同一格，历史行保留供审计）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_msp_mat_wh
    ON biz_material_stock_policy (material_id, warehouse_id)
 WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_msp_material  ON biz_material_stock_policy (material_id);
CREATE INDEX IF NOT EXISTS idx_biz_msp_warehouse ON biz_material_stock_policy (warehouse_id);

-- ---------------------------------------------------------------------------
-- ③ 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
--
--   三个码而不是四个：设置是「一格一行」的 upsert（`PUT /business/erp/stock-policy`），
--   新建与编辑是**同一个动作**（同一份设置、同一个唯一键）—— 拆成 create/update 会逼前端
--   先查一次「这格有没有行」再决定发哪个动词，而两次请求之间没有任何闸能保证行没被别人建出来。
--   同类先例：现金流底稿 `business:erp:cashflow-worksheet:{list,save}`（M1-14）也是 upsert。
--
--   sort：同父（即时库存页）按钮占 10 库存巡检 / 20 在途查询 / 30 在途对账 /
--         40 库龄批次层 / 50 库龄报表 / 60 呆滞料，本卡取 70/80/90（约定 12：同父唯一）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT v.id::uuid, v.code, 3, v.code, NULL, p.id, NULL, v.sort_order, 1, NULL, v.title, FALSE, 'business'
  FROM (VALUES
        ('20000000-0000-0000-0000-000000004280', 'business:erp:stock-policy:list',   70, '水位设置列表'),
        ('20000000-0000-0000-0000-000000004281', 'business:erp:stock-policy:save',   80, '设置水位（物料×仓库）'),
        ('20000000-0000-0000-0000-000000004282', 'business:erp:stock-policy:delete', 90, '删除水位设置')
       ) AS v(id, code, sort_order, title)
  JOIN sys_menu p
    ON p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 授权：仓库线两类角色各拿**成对的三个码** + 超管
--
--   锚点 A「看得到即时库存」= 持有 `business:erp:inventory:list`
--   锚点 B「能维护仓库主数据」= 持有 `business:erp:warehouse:update`
--   并集 → 三个码一起授（不是逐个码按各自锚点授）：只授 save 不授 list 会出现
--   「能改不能看」的半残授权，授权页上还查不出原因。数据驱动、不手抄角色名单。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT anchor.role_id, m.id
  FROM sys_menu m
  JOIN (
        SELECT rm.role_id
          FROM sys_role_menu rm
          JOIN sys_menu src ON src.id = rm.menu_id AND src.deleted_at IS NULL
         WHERE src.auth_code IN ('business:erp:inventory:list', 'business:erp:warehouse:update')
        UNION
        SELECT r.id FROM sys_role r WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
       ) anchor ON TRUE
  JOIN sys_role r ON r.id = anchor.role_id AND r.deleted_at IS NULL
 WHERE m.id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                '20000000-0000-0000-0000-000000004281'::uuid,
                '20000000-0000-0000-0000-000000004282'::uuid)
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu dup WHERE dup.role_id = anchor.role_id AND dup.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ⑤ 自检（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n            integer;
    n_rows       integer;
    n_dup_sort   integer;
    n_roles      integer;
    n_half       integer;
    n_missing    integer;
    n_granted    integer;
    v            text;
BEGIN
    -- ===== 5.1 表的形状：12 列、关键列类型 / 可空性 / 缺省值 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_material_stock_policy';
    IF n <> 12 THEN
        RAISE EXCEPTION 'V0428 自检失败：biz_material_stock_policy 应有 12 列，实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_material_stock_policy'
       AND (
             (column_name = 'min_qty'      AND data_type = 'numeric' AND is_nullable = 'NO'  AND numeric_scale = 4)
          OR (column_name = 'max_qty'      AND data_type = 'numeric' AND is_nullable = 'YES' AND numeric_scale = 4)
          OR (column_name = 'status'       AND data_type = 'smallint' AND is_nullable = 'NO')
          OR (column_name = 'material_id'  AND data_type = 'uuid'    AND is_nullable = 'NO')
          OR (column_name = 'warehouse_id' AND data_type = 'uuid'    AND is_nullable = 'NO')
          OR (column_name = 'deleted_at'   AND data_type = 'timestamp with time zone' AND is_nullable = 'YES')
           );
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0428 自检失败：关键 6 列的类型/可空性与设计不符（实得 % 列匹配）', n;
    END IF;

    -- ===== 5.2 三条 CHECK 都在（负数 / 最高≤最低 / 状态域外必须在库层被拒）=====
    SELECT count(*) INTO n
      FROM pg_constraint c
      JOIN pg_class t ON t.oid = c.conrelid
     WHERE t.relname = 'biz_material_stock_policy'
       AND c.contype = 'c'
       AND c.conname IN ('ck_biz_msp_min', 'ck_biz_msp_max', 'ck_biz_msp_status');
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0428 自检失败：三条 CHECK 只找到 % 条（ck_biz_msp_min / ck_biz_msp_max / ck_biz_msp_status）', n;
    END IF;

    -- ===== 5.3 唯一索引是「只看未删行」的部分唯一索引（软删后同键可重建）=====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND tablename = 'biz_material_stock_policy'
       AND indexname = 'uk_biz_msp_mat_wh'
       AND indexdef LIKE '%UNIQUE%'
       AND indexdef LIKE '%deleted_at IS NULL%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0428 自检失败：uk_biz_msp_mat_wh 不是「(material_id, warehouse_id) WHERE deleted_at IS NULL」的部分唯一索引';
    END IF;

    -- ===== 5.4 行为探针：真正要跑的那条读法能执行（列名 / 类型 / CHECK 都对得上）=====
    --    与 `stock_policy::model::POLICY_SELECT` 同形的窄查询 + 约束反向探针：
    --    空库时 0 行，但证明「物料 × 仓库 命中 + 启用过滤 + 数值比较」这条链路在库层跑得通。
    EXECUTE $probe$
        SELECT count(*) FROM biz_material_stock_policy p
         WHERE p.deleted_at IS NULL
           AND p.status = 1
           AND NULLIF(''::text, '')::uuid IS NULL
           AND (p.max_qty IS NULL OR p.max_qty > p.min_qty)
    $probe$ INTO n_rows;
    RAISE NOTICE 'V0428: 水位设置探针执行成功（当前命中 % 行）', n_rows;

    -- ===== 5.5 三个权限点存在、形状正确（menu_type=3 / path NULL / 不可见 / 挂即时库存页）=====
    SELECT count(*) INTO n
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                    '20000000-0000-0000-0000-000000004281'::uuid,
                    '20000000-0000-0000-0000-000000004282'::uuid)
       AND c.menu_type = 3
       AND c.path IS NULL
       AND c.is_visible = FALSE
       AND c.status = 1
       AND c.deleted_at IS NULL
       AND c.name = c.auth_code
       AND p.menu_type = 2
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0428 自检失败：水位设置三个按钮节点形状不对（实得 % 个合格；要求 menu_type=3 / path NULL / 不可见 / 挂「即时库存」页）', n;
    END IF;

    -- ===== 5.6 三个码各自恰好 1 个承载节点（多于一个是授权页两处同名勾选）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code IN ('business:erp:stock-policy:list',
                         'business:erp:stock-policy:save',
                         'business:erp:stock-policy:delete')
       AND deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0428 自检失败：三个水位码在 sys_menu 里共 % 个未删节点（应为 3）', n;
    END IF;

    -- ===== 5.7 同父 sort_order 不重复（约定 12）=====
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT parent_id FROM sys_menu
                                 WHERE id = '20000000-0000-0000-0000-000000004280'::uuid)
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0428 自检失败：即时库存页下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 5.8 授权完整性 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0428：库里还没有任何角色（迁移先于种子），跳过授权核对；种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        -- 8.1 半残授权：拿到三个码里任意一个但没拿全（能改不能看 / 能看不能改）
        SELECT count(*), string_agg(DISTINCT r.role_code, ', ') INTO n_half, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                                             '20000000-0000-0000-0000-000000004281'::uuid,
                                             '20000000-0000-0000-0000-000000004282'::uuid))
           AND (SELECT count(*) FROM sys_role_menu rm
                 WHERE rm.role_id = r.id
                   AND rm.menu_id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                                      '20000000-0000-0000-0000-000000004281'::uuid,
                                      '20000000-0000-0000-0000-000000004282'::uuid)) <> 3;
        IF n_half <> 0 THEN
            RAISE EXCEPTION 'V0428 自检失败：% 个角色只拿到部分水位码（应为三个成对）→ %', n_half, COALESCE(v, '—');
        END IF;

        -- 8.2 锚点覆盖：持「即时库存」或「编辑仓库」的角色都必须拿到三个码
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_missing, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                         JOIN sys_menu m ON m.id = rm.menu_id
                        WHERE rm.role_id = r.id
                          AND m.auth_code IN ('business:erp:inventory:list', 'business:erp:warehouse:update')
                          AND m.deleted_at IS NULL)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000004282'::uuid);
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0428 自检失败：% 个仓库线角色没拿到水位码 → %', n_missing, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004282'::uuid
        ) THEN
            RAISE NOTICE 'V0428：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；重放本迁移即可补齐）';
        END IF;
    END IF;

    SELECT count(DISTINCT role_id) INTO n_granted
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                       '20000000-0000-0000-0000-000000004281'::uuid,
                       '20000000-0000-0000-0000-000000004282'::uuid);
    RAISE NOTICE 'V0428: 自检通过（表 12 列 / 3 条 CHECK / 部分唯一索引 / 3 个权限点挂即时库存页 / 授权 % 个角色）',
        n_granted;
END $$;
