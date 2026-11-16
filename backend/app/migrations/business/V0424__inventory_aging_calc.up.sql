-- ============================================================================
-- V0424__inventory_aging_calc（M2-08 · 库龄：计算（按批次入库时间）· 2026-11-16）
--
-- 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））
-- 代码：`backend/app/src/business/erp/inventory_aging/{dto,model,handler,fifo}.rs`
-- 拆卡依据：`docs/planning/20261113-新卡清单.md` §M2 的 M2-08 行 —— 报表（0-30/31-60/
--   61-90/90+）是 M2-09、呆滞料标记是 M2-10、单测扩充是 M2-11、前端页是 M2-12。
--
-- ── 本迁移做两件事（同一件事的两半，缺一半服务起不来）────────────────────────
--
--   ① 库函数 `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`
--      —— **FIFO 批次库龄的唯一实现**：把结存按「先入先出」摊回各入库层，
--         给出每层的入库日期 / 原始数量 / 已被吃掉的数量 / 剩余数量 / 层单价；
--   ② 权限码 `business:erp:inventory-aging:list`（menu_type = 3 按钮，挂 0955「即时库存」）
--      —— 承载 `GET /business/erp/inventory-aging/page`（批次层分页）。
--
-- ── 为什么把 FIFO 写进库函数，而不是各调用方自己写一段 SQL ───────────────────
--
--   库龄层是**推导**出来的（`biz_inventory_txn` 里没有「层」这张表，也不该有：
--   另存层账就要做出库时的层消耗记账，是另一个量级的改造）。推导逻辑是：
--
--     层剩余 = GREATEST(0, LEAST(层入库量, 该层累计入库 − 该格出库总量))
--
--   这一行公式如果被报表（M2-09）、呆滞料（M2-10）、明细（M2-08）各写一遍，
--   只要排序键 / 日期口径 / 夹 0 有一处漂，就会出现「明细说这层剩 120、报表说 110」
--   而**没有任何地方报错**。所以算法在库层只留一份，三张卡都 `FROM` 这个函数：
--
--       M2-08 明细：SELECT … FROM biz_inventory_aging_layers(…) l WHERE …
--       M2-09 报表：SELECT 档位, SUM(qty_remaining) … FROM biz_inventory_aging_layers(…) GROUP BY 物料, 仓库
--       M2-10 呆滞：SELECT … WHERE p_as_of − inbound_date >= N
--
--   Rust 侧另有一份**纯逻辑参考实现**（`inventory_aging/fifo.rs`），只用来和本函数
--   逐层对拍（真库用例 `sql_matches_reference_allocator`）—— 两份实现互证是这类
--   「累计 + 夹 0」逻辑唯一可靠的验收方式。
--
-- ── 「入库时间」取业务日期，不取过账时间 ─────────────────────────────────────
--
--   流水 `created_at` 是**过账那一刻**的墙钟时间（`stockin_handler_post.rs` 里
--   `let now = Utc::now()`），入库单的 `in_date` 才是「这批货哪天进的仓」，两者可以
--   差几天（补录 / 审批跨周）。所以层日期 COALESCE 到来源单据的业务日期：
--
--     doc_type = 'STOCK_IN'       → biz_stock_in.in_date        （采购入库 / 销售退货回库 / 完工入库）
--     doc_type = 'STOCK_OUT'      → biz_stock_out.out_date      （出库 / 采购退货）
--     doc_type = 'STOCK_TRANSFER' → biz_stock_transfer.transfer_date（调拨两段）
--     doc_type = 'STOCK_TAKE'     → biz_stock_take.take_date    （盘盈盘亏）
--     doc_type = 'OPENING'        → period_code 的月初           （期初 = 期间起点）
--     其它（MATERIAL_SERIAL / SERIAL_BATCH …）→ created_at 换算成 CST 日期（已知近似）
--
--   `doc_type='STOCK_IN'` 的 `doc_id` **一定**指向一张真实 `biz_stock_in`（销售退货回库、
--   完工入库也都会先建一张 `biz_stock_in`，见 `sales_return_posting.rs` 与
--   `cost_settle_finish.rs`），所以 LEFT JOIN 用主键等值不会有半连接歧义；
--   批号从 `biz_stock_in_item.lot_no`（`(stock_in_id, line_no)` 唯一）取。
--
-- ── 口径（写死，别在调用方再解释一遍）────────────────────────────────────────
--
--   · 出库按 **FIFO 由老到新**吃掉层（先入先出 ⇒ 结存是较新的层）；
--   · 同一格的排序键 = `(层日期, created_at, id)`：同一天入库的多张单按过账时刻排，
--     同一时刻按 id 排（**必须**有全序，否则翻页会漏行 / 重复行）；
--   · 出库量 > 入库量（脏数据 / 允许负库存的仓）⇒ 所有层剩余 0，**不编造负数层**；
--     此时 `Σ剩余 < biz_inventory.qty` 就是「这格账不干净」的信号；
--   · `p_as_of` 之前的判据用**业务日期**（不是过账时间）：期末库龄要还原成
--     「那天账上是什么样」，靠过账时间做不到；
--   · 物料 / 仓库走**入参**（而不是让调用方在外面 `WHERE`）：一是单物料查询只算
--     这一格（走 idx_biz_inv_txn_mat），二是避免「外面过滤了但里面算了全表」。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--
--   · 函数：`CREATE OR REPLACE FUNCTION`（重放不报错；签名没变时可原地替换）；
--   · 菜单：固定 UUID + `ON CONFLICT (id) DO NOTHING`（重放不覆盖运维改过的标题）；
--   · 授权：`NOT EXISTS` 去重、按码判重；
--   · 父节点用 `auth_code` 反查（菜单 reorg 后仍挂在对的页下）；
--   · down：`DROP FUNCTION IF EXISTS` + 只删本迁移种的授权行与菜单行。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 库函数：FIFO 批次库龄层
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION biz_inventory_aging_layers(
    p_as_of        date,
    p_material_id  uuid DEFAULT NULL,
    p_warehouse_id uuid DEFAULT NULL
)
RETURNS TABLE (
    layer_id         uuid,
    material_id      uuid,
    warehouse_id     uuid,
    lot_no           varchar(64),
    doc_type         varchar(24),
    doc_no           varchar(32),
    line_no          integer,
    biz_type         varchar(24),
    inbound_date     date,
    posted_at        timestamptz,
    qty_in           numeric(18, 4),
    qty_consumed     numeric(18, 4),
    qty_remaining    numeric(18, 4),
    unit_cost        numeric(18, 6),
    amount_remaining numeric(18, 4)
)
LANGUAGE sql
STABLE
AS $$
WITH dated AS (
    -- 每笔流水的**业务日期** + 批号（见文件头「入库时间取业务日期」表）
    SELECT t.id,
           t.material_id,
           t.warehouse_id,
           t.txn_type,
           t.biz_type,
           t.doc_type,
           t.doc_no,
           t.line_no,
           t.qty,
           t.unit_cost,
           t.created_at,
           COALESCE(si.in_date,
                    so.out_date,
                    tr.transfer_date,
                    st.take_date,
                    CASE WHEN t.period_code ~ '^[0-9]{4}-[0-9]{2}$'
                         THEN (t.period_code || '-01')::date END,
                    (t.created_at AT TIME ZONE 'Asia/Shanghai')::date) AS biz_date,
           sii.lot_no
      FROM biz_inventory_txn t
      LEFT JOIN biz_stock_in      si  ON t.doc_type = 'STOCK_IN'       AND si.id  = t.doc_id
      LEFT JOIN biz_stock_in_item sii ON t.doc_type = 'STOCK_IN'       AND sii.stock_in_id = t.doc_id
                                                                       AND sii.line_no = t.line_no
      LEFT JOIN biz_stock_out     so  ON t.doc_type = 'STOCK_OUT'      AND so.id  = t.doc_id
      LEFT JOIN biz_stock_transfer tr ON t.doc_type = 'STOCK_TRANSFER' AND tr.id  = t.doc_id
      LEFT JOIN biz_stock_take    st  ON t.doc_type = 'STOCK_TAKE'     AND st.id  = t.doc_id
     WHERE (p_material_id  IS NULL OR t.material_id  = p_material_id)
       AND (p_warehouse_id IS NULL OR t.warehouse_id = p_warehouse_id)
),
within AS (
    -- 基准日快照：只要业务日期 <= p_as_of 的流水（期末库龄就靠这一句）
    SELECT * FROM dated WHERE biz_date <= p_as_of
),
layers AS (
    -- 入库层 + 累计入库量（同一格 = 同一 物料 × 仓库）
    SELECT w.*,
           SUM(w.qty) OVER (PARTITION BY w.material_id, w.warehouse_id
                                ORDER BY w.biz_date, w.created_at, w.id) AS cum_in
      FROM within w
     WHERE w.txn_type = 'IN'
),
outs AS (
    -- 该格在基准日之前被吃掉多少（恒正，按 FIFO 由老到新摊）
    SELECT w.material_id, w.warehouse_id, SUM(w.qty) AS out_qty
      FROM within w
     WHERE w.txn_type = 'OUT'
     GROUP BY w.material_id, w.warehouse_id
),
joined AS (
    SELECT l.id,
           l.material_id,
           l.warehouse_id,
           l.lot_no,
           l.doc_type,
           l.doc_no,
           l.line_no,
           l.biz_type,
           l.biz_date AS inbound_date,
           l.created_at AS posted_at,
           l.qty AS qty_in,
           l.unit_cost,
           -- FIFO：本层被吃到的部分 = 出库总量越过「本层之前累计」的那一段
           GREATEST(0::numeric,
                    LEAST(l.qty, l.cum_in - COALESCE(o.out_qty, 0::numeric))) AS qty_remaining
      FROM layers l
      LEFT JOIN outs o ON o.material_id = l.material_id AND o.warehouse_id = l.warehouse_id
)
SELECT j.id,
       j.material_id,
       j.warehouse_id,
       j.lot_no,
       j.doc_type,
       j.doc_no,
       j.line_no,
       j.biz_type,
       j.inbound_date,
       j.posted_at,
       j.qty_in,
       j.qty_in - j.qty_remaining,
       j.qty_remaining,
       j.unit_cost,
       ROUND(j.qty_remaining * j.unit_cost, 4)
  FROM joined j
$$;

COMMENT ON FUNCTION biz_inventory_aging_layers(date, uuid, uuid) IS
    'FIFO 批次库龄层（M2-08）：结存按先入先出摊回 biz_inventory_txn 的 IN 流水；'
    '层日期取来源单据业务日期（STOCK_IN.in_date / STOCK_OUT.out_date / STOCK_TRANSFER.transfer_date / '
    'STOCK_TAKE.take_date / OPENING 的期初月初），其余 doc_type 回落 created_at(CST)。'
    'p_as_of = 基准日（含当天）；p_material_id / p_warehouse_id 为 NULL 表示全量。'
    'M2-09（分段报表）/ M2-10（呆滞料）必须复用本函数，不要另写一份 FIFO。';

-- ---------------------------------------------------------------------------
-- ② 前置护栏：父页面（即时库存）必须在，且码不能被别人占用
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
        RAISE EXCEPTION 'V0424: 找不到唯一的「即时库存」页面行（auth_code=business:erp:inventory:list，实得 % 行）—— 库龄按钮要挂在它下面', n_parent;
    END IF;

    -- 同一个码只能有一个承载节点（多于一个会让角色授权页出现两处同码勾选）
    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:list'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0424: business:erp:inventory-aging:list 已有 % 个承载节点（应为 0 或 1）', n_code;
    END IF;
    RAISE NOTICE 'V0424: 前置检查通过（即时库存页在；库龄码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ③ 权限点（menu_type = 3，不进侧栏；title 是授权树里的显示名）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-0000-0000-000000004240'::uuid,
       'business:erp:inventory-aging:list',
       3,
       'business:erp:inventory-aging:list',
       NULL,
       p.id,
       NULL,
       40,          -- 与兄弟按钮「库存巡检」(10) /「在途库存查询」(20) /「在途对账」(30) 并列
       1,
       NULL,
       '库龄批次层查询',
       FALSE,
       'business'
  FROM sys_menu p
 WHERE p.auth_code = 'business:erp:inventory:list'
   AND p.menu_type = 2
   AND p.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 授权：跟 `inventory:list` 的持有角色走（数据驱动）+ 超管
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm_src.role_id, m.id
  FROM sys_menu m
  JOIN sys_role_menu rm_src
    ON rm_src.menu_id IN (SELECT id FROM sys_menu
                           WHERE auth_code = 'business:erp:inventory:list'
                             AND deleted_at IS NULL)
  JOIN sys_role r ON r.id = rm_src.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004240'::uuid
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
   AND m.id = '20000000-0000-0000-0000-000000004240'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ⑤ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_sig        text;
    v_rows       bigint;
    n_btn        integer;
    n_parent     integer;
    n_same       integer;
    n_dup_sort   integer;
    n_roles      integer;
    n_missing    integer;
    n_granted    integer;
    v            text;
BEGIN
    -- ===== 5.1 函数存在，且出参列齐（少一列调用方就会静默拿到 NULL）=====
    SELECT pg_get_function_result(p.oid) INTO v_sig
      FROM pg_proc p
      JOIN pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public'
       AND p.proname = 'biz_inventory_aging_layers';
    IF v_sig IS NULL THEN
        RAISE EXCEPTION 'V0424 自检失败：函数 biz_inventory_aging_layers 不存在';
    END IF;
    IF position('qty_remaining' IN v_sig) = 0
       OR position('qty_consumed' IN v_sig) = 0
       OR position('inbound_date' IN v_sig) = 0
       OR position('amount_remaining' IN v_sig) = 0 THEN
        RAISE EXCEPTION 'V0424 自检失败：函数出参缺列（实得 %）', v_sig;
    END IF;

    -- ===== 5.2 行为探针：真调一次（空物料 = 0 行，但必须不报错）=====
    -- 走一个必然不存在的物料 id：命中索引、0 行、毫秒级 —— 但它证明函数体真的能执行
    -- （只 CREATE 不调用的话，函数体里引用了不存在的列也要等到第一次调用才炸）。
    SELECT count(*) INTO v_rows
      FROM biz_inventory_aging_layers(CURRENT_DATE,
                                      '00000000-0000-0000-0000-000000000000'::uuid,
                                      NULL::uuid);
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0424 自检失败：空物料的库龄层应 0 行，实得 % 行', v_rows;
    END IF;

    -- ===== 5.3 权限点存在、类型 / 码 / 显示名正确 =====
    SELECT count(*) INTO n_btn
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004240'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:inventory-aging:list'
       AND name = 'business:erp:inventory-aging:list'
       AND title = '库龄批次层查询'
       AND path IS NULL          -- 按钮不进侧栏 ⇒ 无 path（type=2 才需要）
       AND is_visible = FALSE
       AND deleted_at IS NULL;
    IF n_btn <> 1 THEN
        RAISE EXCEPTION 'V0424 自检失败：库龄按钮节点缺失或字段不对（menu_type 必须 3、path 必须 NULL、is_visible 必须 FALSE）';
    END IF;

    -- ===== 5.4 必须挂在「即时库存」页面下（挂错页面 = 授权树上找不到）=====
    SELECT count(*) INTO n_parent
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000004240'::uuid
       AND p.menu_type = 2
       AND p.auth_code = 'business:erp:inventory:list'
       AND p.deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0424 自检失败：库龄按钮没有挂在「即时库存」页面下';
    END IF;

    -- ===== 5.5 码全局唯一（重复会让角色授权合并成两条勾选）=====
    SELECT count(*) INTO n_same
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:list'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0424 自检失败：business:erp:inventory-aging:list 出现 % 次（应为 1）', n_same;
    END IF;

    -- ===== 5.6 同父 sort_order 不重复（AGENTS.md 约定 12）=====
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = (SELECT parent_id FROM sys_menu
                                 WHERE id = '20000000-0000-0000-0000-000000004240'::uuid)
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0424 自检失败：同父节点下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 5.7 授权：持 inventory:list 的角色必须都拿到库龄码 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0424：库里还没有任何角色（迁移先于种子），跳过授权核对；种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_missing, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id IN (SELECT id FROM sys_menu
                                              WHERE auth_code = 'business:erp:inventory:list'
                                                AND deleted_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000004240'::uuid);
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0424 自检失败：持 inventory:list 但没拿到库龄码的角色 % 个 → %',
                n_missing, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004240'::uuid
        ) THEN
            RAISE NOTICE 'V0424：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；重放本迁移即可补齐）';
        END IF;

        SELECT count(DISTINCT rm.role_id) INTO n_granted
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE rm.menu_id = '20000000-0000-0000-0000-000000004240'::uuid;
        RAISE NOTICE 'V0424：授权核对通过（持有库龄码的角色 % 个，缺口 0）', n_granted;
    END IF;

    RAISE NOTICE 'V0424 自检通过：FIFO 库龄函数 biz_inventory_aging_layers + 1 个权限码（business:erp:inventory-aging:list，menu_type=3、sort_order=40）';
END $$;
