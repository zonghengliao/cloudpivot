-- ============================================================================
-- V0448：采购计划 → 采购申请转换（采购链 M4-03）
--
--   ① 溯源列（两处，都只追加、都可空）：
--        biz_purchase_requisition.source_plan_id      申请单 ← 由哪张计划生成
--        biz_purchase_requisition_item.plan_item_id   申请明细 ← 由计划哪一行生成
--      这两列是「计划执行率」（M4-04）与「这张申请是谁派生的」的唯一抓手：
--      没有它们，`executed_qty` 加了却查不出加到哪张单上，对账只能靠备注里的字符串。
--   ② 1 行菜单：`business:erp:purchase-plan:to-requisition`（menu_type=3，
--      挂在 M4-02 建的采购计划页面行 `…004471` 下，sort=60）。
--      转换是**采购**的动作（把批准的计划变成需求单），所以只授 PURCHASE_MANAGER；
--      财务经理要复核的是申请本身（申请走 pr_approve），不是「转单」这个动作。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   实测最大号 = V0447（采购计划 CRUD + 审批，M4-02）⇒ 本卡顺延取 **V0448**。
--   sys_menu id 取 `…004477`（与 V0447 的 …4471–4476 连号，与既有段无交叠）。
--
-- ── 为什么是「加列」而不是新建关联表 ───────────────────────────────────
--   一次转换 = 一张计划 × N 个仓库 → N 张申请单；申请单本身已经承载
--   「仓库 / 部门 / 日期」这些单头语义，再建 `biz_pp_to_prq` 关联表只会让
--   「这张申请单从哪来」多一次 join，且明细级的对应关系（计划行 → 申请行）
--   仍然要在明细表里落一列。所以：
--     · 单头 `source_plan_id`：一张申请单只可能来自**一张**计划（转换按仓拆单，
--       同一张计划会派生多张申请单，但不会把两张计划的量混进同一张申请单）；
--     · 明细 `plan_item_id`：一行申请明细对应计划的一行（物料 × 仓库），
--       转换只做「搬运 + 拆仓」，不做同物料合并，所以一对一。
--   两列都**不加**外键：计划是软删的（`deleted_at`），加外键会让「删计划」失败，
--   而软删后追溯源单仍要读得到 —— 弱引用 + 索引是这里的正确形态（与 V0331 的
--   `po_id` / `po_item_id` 同口径）。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────
--   `ALTER TABLE … ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` /
--   菜单 `ON CONFLICT (id) DO UPDATE` / 授权 `ON CONFLICT DO NOTHING`；
--   末尾 DO 块自检（两列在位 + 两个索引在位 + 菜单 1 行且父级/码/类型对 + 授权 ≥1），
--   任一不符 → 迁移失败（不留半套结构）。
--   回滚见 V0448__purchase_plan_to_requisition.down.sql：先删授权与菜单，
--   再删索引，最后删两列。**列里有数据也照删**（down 是开发/演练用的复位手段，
--   生产不会跑 down；真在生产跑，丢掉的是「源单指针」而不是业务单据本身）。
-- ============================================================================

-- ---------- ① 申请单：源计划指针 ----------
ALTER TABLE biz_purchase_requisition
    ADD COLUMN IF NOT EXISTS source_plan_id UUID;

COMMENT ON COLUMN biz_purchase_requisition.source_plan_id IS
    '由哪张采购计划转换而来（弱引用 biz_purchase_plan.id；手工建单为 NULL）—— M4-03 溯源 + M4-04 执行率';

-- ---------- ② 申请明细：源计划行指针 ----------
ALTER TABLE biz_purchase_requisition_item
    ADD COLUMN IF NOT EXISTS plan_item_id UUID;

COMMENT ON COLUMN biz_purchase_requisition_item.plan_item_id IS
    '由计划哪一行（物料 × 仓库）转换而来（弱引用 biz_purchase_plan_item.id；手工建单为 NULL）';

-- 两张表都只查「非空」的值：部分索引比全表索引小得多（手工建单占绝大多数）
CREATE INDEX IF NOT EXISTS idx_biz_purchase_requisition_source_plan
    ON biz_purchase_requisition (source_plan_id) WHERE source_plan_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_prqi_plan_item
    ON biz_purchase_requisition_item (plan_item_id) WHERE plan_item_id IS NOT NULL;

-- ---------- ③ 菜单：采购计划页面下加一个「转采购申请」按钮 ----------
-- 权限码语义：`{模块}:{资源}:{动作}`；动作是 `to-requisition`（与采购申请的 `to-po` 同构）。
-- 页面显形码 `business:erp:purchase-plan:page` **不动**（约定 14：只给前端）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004477'::uuid, '计划转采购申请', 3,
     'business:erp:purchase-plan:to-requisition', NULL,
     '20000000-0000-0000-0000-000000004471'::uuid,
     NULL, 60, 1, NULL, '转采购申请', TRUE, 'business')
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

-- ---------- ④ 授权 ----------
-- 采购经理：转换是它的日常动作（计划批完 → 生成需求单）。
-- 财务经理 / 部门主管**不给**：它们要复核的是申请（pr_approve 流程按角色展开），
-- 不是「谁的键盘上按了转换按钮」；多授一个码就是多一条「能造单却没人审」的路。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-0000-0000-000000004477'::uuid
  FROM sys_role r
 WHERE r.role_code = 'PURCHASE_MANAGER' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    v_cnt   int;
    v_rows  int;
BEGIN
    -- 5.1 两列必须在位（改名 / 手滑写错表名都会在这里先炸）
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_purchase_requisition'
       AND column_name = 'source_plan_id';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0448 自检失败：biz_purchase_requisition.source_plan_id 未建出';
    END IF;
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_purchase_requisition_item'
       AND column_name = 'plan_item_id';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0448 自检失败：biz_purchase_requisition_item.plan_item_id 未建出';
    END IF;

    -- 5.2 两个部分索引必须在位（各自的 WHERE 条件单独校验，别写成一条 OR 混在一起）
    SELECT count(*) INTO v_cnt FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname = 'idx_biz_purchase_requisition_source_plan'
       AND indexdef LIKE '%WHERE (source_plan_id IS NOT NULL)%';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0448 自检失败：idx_biz_purchase_requisition_source_plan 不在位或缺部分条件';
    END IF;
    SELECT count(*) INTO v_cnt FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname = 'idx_biz_prqi_plan_item'
       AND indexdef LIKE '%WHERE (plan_item_id IS NOT NULL)%';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0448 自检失败：idx_biz_prqi_plan_item 不在位或缺部分条件';
    END IF;

    -- 5.3 菜单 1 行：按钮、挂采购计划页面行下、码逐字一致
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004477'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:purchase-plan:to-requisition'
       AND parent_id = '20000000-0000-0000-0000-000000004471'::uuid
       AND deleted_at IS NULL
       AND length(title) <= 12;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0448 自检失败：转采购申请按钮行（类型 / 码 / 父级 / 标题长度）不对';
    END IF;

    -- 5.4 父页面行必须还在（M4-02 建的 …004471；被删了则本按钮会变孤儿）
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004471'::uuid AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0448 自检失败：采购计划页面行 …004471 不在位（按钮会成孤儿）';
    END IF;

    -- 5.5 同父 sort 不许重复（否则侧栏顺序每次刷新都变）
    SELECT count(*) INTO v_rows FROM (
        SELECT sort_order FROM sys_menu
         WHERE parent_id = '20000000-0000-0000-0000-000000004471'::uuid AND deleted_at IS NULL
         GROUP BY sort_order HAVING count(*) > 1) dup;
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0448 自检失败：采购计划页面下的按钮 sort_order 有重复（% 组）', v_rows;
    END IF;

    -- 5.6 N20：除超管外至少一个角色能用（角色不存在时插入 0 行，这里不判红 —— 空库首启
    --     的角色由历史迁移种下，本迁移先跑也合法；但**角色在**时必须已授权）
    SELECT count(*) INTO v_rows FROM sys_role WHERE role_code = 'PURCHASE_MANAGER' AND deleted_at IS NULL;
    IF v_rows = 1 THEN
        SELECT count(*) INTO v_cnt FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE r.role_code = 'PURCHASE_MANAGER'
           AND rm.menu_id = '20000000-0000-0000-0000-000000004477'::uuid;
        IF v_cnt <> 1 THEN
            RAISE EXCEPTION 'V0448 自检失败：PURCHASE_MANAGER 没拿到转采购申请授权（实际 % 行）', v_cnt;
        END IF;
    END IF;

    RAISE NOTICE 'V0448 自检通过：采购计划 → 采购申请（溯源两列 + 1 行菜单 + 授权）已就绪';
END $$;
