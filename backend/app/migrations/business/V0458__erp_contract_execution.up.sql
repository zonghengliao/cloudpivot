-- ============================================================================
-- V0458：合同「执行量统计」—— 已执行 / 合同额（采购链 M4-19）
--
--   ① 视图 `v_contract_execution_order`  单据级：一行 = 一张挂在合同上的单据
--                （采购单 `biz_purchase_order` / 销售单 `biz_sales_order`，含作废 / 软删行）
--   ② 视图 `v_contract_execution`        单头级：一张存活合同一行（已执行额 / 剩余 / 执行率 / 单据数）
--   ③ 菜单 1 行：按钮 `business:erp:contract:execution`（`…004498`，menu_type=3）
--                挂在**已存在**的「合同管理」页面行 `…001100` 下（V0197 / V0200 建的）
--   ④ 角色授权：采购经理（采购侧控盘）/ 销售主管（销售侧控盘）/ 财务经理（复核口径）
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`）──
--   写入本文件时全局最大号 = V0457（供应商评估页）⇒ 本卡顺延取 **V0458**。
--   sys_menu id：V0452 占用 …4480 / …4481、V0454 占用 …4482–…4487、V0455 占用 …4488、
--   V0456 占用 …4490–…4493、V0457 占用 …4494–…4497 ⇒ 本迁移取 …4498，无交叠。
--   biz_seq 一行都不碰（执行量统计不需要取号）。
--
-- ── 为什么是「视图现算」而不是「缓存列」────────────────────────────────────
--   1) `V0195` 头注释「为什么没有 `supplier_id` / `used_amount` / `version` 列」已经把
--      这件事定死了：合同上的**累计已下单额必须现算**（ARCH-2 V-21/V-23，单一数据源）。
--      缓存列只在「有人通过 handler 改过单」的那一刻对，绕过 handler 改库（数据修复 /
--      导入 / 并发失败残留）就漂移 —— 报表拿漂移值当真相等于又多一份要对账的账。
--   2) 报表与守卫**必须读同一份真相**：采购单 `purchase_model::committed_amount`、
--      销售单 `sales_order_model::committed_amount` 都是
--      `Σ total_with_tax WHERE contract_id = ? AND status <> 'CANCELLED' AND deleted_at IS NULL`
--      的现算。本视图逐字采用同一口径 —— 否则会出现「报表说还剩 10、下单却被拒」的争议，
--      而争议一旦出现没人能解释哪边对。
--
-- ── 口径（逐条写死；后端 dto 的纯函数与这里逐字一致）─────────────────────────
--   · 计入的单据：`deleted_at IS NULL AND status <> 'CANCELLED'`
--     —— 草稿（DRAFT）**算**（单已生成、额度已占住，与守卫同口径）；
--        审批中 / 已批准 / 部分收货 / 已收货 / 已关闭都算（关闭只是不再执行，额度已经占住）；
--        作废 / 软删**不算**（链已断），只进 `released_doc_count` 供解释「为什么已执行没涨」。
--   · 已执行额 `executed_amount` = Σ 单据 `total_with_tax`（**价税合计**）
--     —— 与两个守卫的 `committed_amount` 完全同口径（换口径比金额 = 白送 / 白扣一个税率）。
--   · 已执行净额 `executed_amount_net` = Σ 单据 `total_amount`（**不含税**，仅供财务报表并排看；
--     执行率 / 剩余额**一律按含税口径**算，与守卫一致）。
--   · 合同额 `amount`（`biz_contract.amount`，DDL `ck_biz_contract_amount` 保证 > 0）。
--   · 剩余额 `remaining_amount` = `amount − executed_amount`（**可为负**：守卫只拦「新单」，
--     已成立的单据被作废恢复 / 合同额被改小 / 历史数据绕过 handler 改库都会出现负数；
--     报表必须让它显形，不许截成 0）。
--   · 执行率 `exec_amount_rate` = executed_amount / amount（6 位小数）；分母恒 > 0 ⇒ 恒有值。
--   · 执行状态 `exec_state`：
--       NO_DOWNSTREAM（合同类型没有下游执行单据：SERVICE / LEASE 目前无单据可挂）
--       NOT_STARTED（已执行 0） / PARTIAL（0 < 已执行 < 合同额）
--       DONE（恰好用满） / OVER（超合同额，率 > 100%）
--   · 下游单据类型 `downstream_type`：PURCHASE→`PURCHASE_ORDER`、SALES→`SALES_ORDER`、
--     SERVICE / LEASE→NULL。**按合同类型分派**，不按「谁挂了 contract_id」猜 ——
--     采购单只能挂 PURCHASE 合同、销售单只能挂 SALES 合同（两个 handler 各自校验），
--     视图仍按类型分派是为了让「绕过 handler 写进库的错挂行」不进统计（宁可少算也不混算）。
--   · 币种：`currency_mismatch_doc_count` = 计入的单据里币种 ≠ 合同币种的行数。
--     **不改口径**（守卫也不看币种，总额照加），只把风险显形 —— 否则「USD 单挂在 CNY 合同上」
--     这类数据会静默把执行率算歪，报表上看不出任何异常。
--   · 有效期：`days_left` = `end_date − 当日`（`end_date` 为空 = NULL）；`expired` =
--     `end_date < 当日`。合同状态机（DRAFT / PENDING_APPROVAL / SIGNED / ACTIVE / COMPLETED /
--     TERMINATED）原样带出，不在这里另造状态。
--
-- ── 数据权限 ────────────────────────────────────────────────────────────────
--   SCOPE-EXEMPT：与合同列表同口径（`contract_model::CONTRACT_TARGET` = `ScopeTarget::unscoped`，
--   V0195 里 `dept_id` 是「归属部门」但合同表**没有**登记进 `SCOPED_TABLES`，数据权限属专业版）。
--   可见性由权限码 `business:erp:contract:execution` 控制，总览走 `scope::paginate_unscoped`，
--   明细按合同主键取。改动这条要单独裁决，不许在本卡顺手登记。
--
-- ── 菜单 ────────────────────────────────────────────────────────────────────
--   合同管理页面行（`business:erp:contract:list`，id `…001100`）**早就存在**（V0197 建、
--   V0200 放显形、V0241 前缀统一），本卡只加一条按钮行挂在它下面（sort_order=50，接在
--   create/update/delete/status 的 10/20/30/40 之后）。
--   页面显形码 `…:page` 与本卡无关：合同页沿用 `…:list` 判显形（ERP 尚未解耦页面码/接口码，
--   过渡期条款见 `docs/编码规范.md` §1.7.2 末尾）。
--
-- ── 幂等与回滚 ──────────────────────────────────────────────────────────────
--   `CREATE OR REPLACE VIEW`（重跑只换定义）/ 菜单 `ON CONFLICT (id) DO UPDATE` /
--   授权 `ON CONFLICT DO NOTHING`；末尾 DO 块自检：两个视图在位 + 关键列名齐全 +
--   **探针数据的逐格验算**（先造 → 先断言 → 再清，不留残留行）。
--   回滚见 V0458__erp_contract_execution.down.sql：先删授权、再删菜单、最后删两个视图。
-- ============================================================================

-- ---------- ① 单据级：一行 = 一张挂在合同上的单据（含作废 / 软删） ----------
CREATE OR REPLACE VIEW v_contract_execution_order AS
SELECT o.contract_id                                AS contract_id,
       'PURCHASE_ORDER'::text                       AS source_type,
       o.id                                         AS order_id,
       o.order_no                                   AS order_no,
       o.status                                     AS order_status,
       o.order_date                                 AS order_date,
       o.currency_code::text                        AS currency_code,
       o.total_amount                               AS total_amount,
       o.total_with_tax                             AS total_with_tax,
       o.deleted_at                                 AS order_deleted_at,
       -- 是否计入已执行额（唯一判据，与两个守卫的 committed_amount 逐字一致）
       (o.deleted_at IS NULL AND o.status <> 'CANCELLED') AS counted,
       -- 币种是否与合同一致（只显形，不改口径）
       (o.currency_code::text = c.currency_code::text)    AS currency_matched
  FROM biz_purchase_order o
  JOIN biz_contract c ON c.id = o.contract_id
 WHERE o.contract_id IS NOT NULL
UNION ALL
SELECT s.contract_id,
       'SALES_ORDER'::text,
       s.id,
       s.so_no,
       s.status,
       s.order_date,
       s.currency_code::text,
       s.total_amount,
       s.total_with_tax,
       s.deleted_at,
       (s.deleted_at IS NULL AND s.status <> 'CANCELLED'),
       (s.currency_code::text = c.currency_code::text)
  FROM biz_sales_order s
  JOIN biz_contract c ON c.id = s.contract_id
 WHERE s.contract_id IS NOT NULL;

COMMENT ON VIEW v_contract_execution_order IS
    '合同执行单据（M4-19）：一行 = 一张挂在合同上的单据（采购单 / 销售单）；counted = 是否计入已执行额（存活 deleted_at IS NULL 且未作废 status <> CANCELLED，与采购/销售两个 contract 额度守卫 committed_amount 同一判据）；currency_matched 只显形币种风险，不改口径';
COMMENT ON COLUMN v_contract_execution_order.counted IS
    '是否计入已执行额：deleted_at IS NULL AND status <> ''CANCELLED''（草稿也算 —— 额度已占住）；作废 / 软删只进 released_doc_count';
COMMENT ON COLUMN v_contract_execution_order.total_with_tax IS
    '单据价税合计：已执行额唯一口径（与守卫 committed_amount 的 SUM(total_with_tax) 逐字一致）';
COMMENT ON COLUMN v_contract_execution_order.currency_matched IS
    '单据币种是否等于合同币种：false = 币种串了（守卫也不看币种，报表只显形不换算、不剔除）';

-- ---------- ② 单头级：一张存活合同一行 ----------
CREATE OR REPLACE VIEW v_contract_execution AS
WITH agg AS (
    SELECT e.contract_id                                          AS contract_id,
           count(*) FILTER (WHERE e.counted)                      AS order_doc_count,
           count(*) FILTER (WHERE NOT e.counted)                  AS released_doc_count,
           count(*) FILTER (WHERE e.counted AND NOT e.currency_matched)
                                                                  AS currency_mismatch_doc_count,
           count(*) FILTER (WHERE e.counted AND e.source_type = 'PURCHASE_ORDER')
                                                                  AS purchase_doc_count,
           count(*) FILTER (WHERE e.counted AND e.source_type = 'SALES_ORDER')
                                                                  AS sales_doc_count,
           COALESCE(SUM(e.total_with_tax) FILTER (WHERE e.counted), 0)
                                                                  AS executed_amount,
           COALESCE(SUM(e.total_amount)   FILTER (WHERE e.counted), 0)
                                                                  AS executed_amount_net,
           max(e.order_date) FILTER (WHERE e.counted)             AS last_order_on
      FROM v_contract_execution_order e
     GROUP BY e.contract_id
),
base AS (
    SELECT c.id                                                   AS contract_id,
           c.contract_no                                          AS contract_no,
           c.contract_name                                        AS contract_name,
           c.contract_type                                        AS contract_type,
           c.status                                               AS status,
           c.party_a_id                                           AS party_a_id,
           c.party_a_name                                         AS party_a_name,
           c.party_b_type                                         AS party_b_type,
           c.party_b_id                                           AS party_b_id,
           c.party_b_name                                         AS party_b_name,
           -- 乙方现名（业务展示类 → 实时 join；快照为空时回退快照，见 AGENTS 约定 13）
           COALESCE(s.supplier_name, cu.name)                     AS party_b_current_name,
           c.owner_id                                             AS owner_id,
           u.real_name                                            AS owner_name,
           c.dept_id                                              AS dept_id,
           d.dept_name                                            AS dept_name,
           c.amount                                               AS amount,
           c.currency_code::text                                  AS currency_code,
           c.sign_date                                            AS sign_date,
           c.start_date                                           AS start_date,
           c.end_date                                             AS end_date,
           c.created_at                                           AS created_at,
           c.updated_at                                           AS updated_at,
           -- 下游单据类型按合同类型分派（SERVICE / LEASE 目前无下游单据）
           CASE c.contract_type
               WHEN 'PURCHASE' THEN 'PURCHASE_ORDER'
               WHEN 'SALES'    THEN 'SALES_ORDER'
               ELSE NULL
           END                                                    AS downstream_type,
           COALESCE(a.order_doc_count, 0)                         AS order_doc_count,
           COALESCE(a.released_doc_count, 0)                      AS released_doc_count,
           COALESCE(a.currency_mismatch_doc_count, 0)             AS currency_mismatch_doc_count,
           COALESCE(a.purchase_doc_count, 0)                      AS purchase_doc_count,
           COALESCE(a.sales_doc_count, 0)                         AS sales_doc_count,
           COALESCE(a.executed_amount, 0)                         AS executed_amount,
           COALESCE(a.executed_amount_net, 0)                     AS executed_amount_net,
           a.last_order_on                                        AS last_order_on,
           (c.end_date - CURRENT_DATE)                            AS days_left,
           (c.end_date IS NOT NULL AND c.end_date < CURRENT_DATE) AS expired,
           (c.status IN ('SIGNED', 'ACTIVE'))                     AS usable_for_order
      FROM biz_contract c
      LEFT JOIN biz_supplier s ON s.id = c.party_b_id AND c.party_b_type = 'SUPPLIER'
      LEFT JOIN biz_customer cu ON cu.id = c.party_b_id AND c.party_b_type = 'CUSTOMER'
      LEFT JOIN sys_user     u ON u.id = c.owner_id
      LEFT JOIN sys_dept     d ON d.id = c.dept_id
      LEFT JOIN agg          a ON a.contract_id = c.id
     WHERE c.deleted_at IS NULL
)
SELECT b.contract_id,
       b.contract_no,
       b.contract_name,
       b.contract_type,
       b.status,
       b.party_a_id,
       b.party_a_name,
       b.party_b_type,
       b.party_b_id,
       b.party_b_name,
       b.party_b_current_name,
       b.owner_id,
       b.owner_name,
       b.dept_id,
       b.dept_name,
       b.amount,
       b.currency_code,
       b.sign_date,
       b.start_date,
       b.end_date,
       b.downstream_type,
       b.order_doc_count,
       b.released_doc_count,
       b.currency_mismatch_doc_count,
       b.purchase_doc_count,
       b.sales_doc_count,
       b.executed_amount,
       b.executed_amount_net,
       b.amount - b.executed_amount                AS remaining_amount,
       round(b.executed_amount / b.amount, 6)      AS exec_amount_rate,
       CASE
           WHEN b.downstream_type IS NULL              THEN 'NO_DOWNSTREAM'
           WHEN b.executed_amount >  b.amount          THEN 'OVER'
           WHEN b.executed_amount >= b.amount          THEN 'DONE'
           WHEN b.executed_amount >  0                 THEN 'PARTIAL'
           ELSE 'NOT_STARTED'
       END                                          AS exec_state,
       b.last_order_on,
       b.days_left,
       b.expired,
       b.usable_for_order,
       b.created_at,
       b.updated_at
  FROM base b;

COMMENT ON VIEW v_contract_execution IS
    '合同执行量统计（M4-19）：一张存活合同一行；executed_amount = Σ 单据 total_with_tax（存活且未作废，与额度守卫同口径），remaining_amount = amount − executed_amount（可为负），exec_amount_rate = executed_amount / amount（6 位），exec_state ∈ NO_DOWNSTREAM / NOT_STARTED / PARTIAL / DONE / OVER；downstream_type 按合同类型分派（采购合同看采购单、销售合同看销售单、服务/租赁无下游）';
COMMENT ON COLUMN v_contract_execution.executed_amount IS
    '已执行额 = Σ 计入单据的 total_with_tax；判据 deleted_at IS NULL AND status <> ''CANCELLED''（草稿也算），与采购/销售单的合同额度守卫 committed_amount 逐字同口径';
COMMENT ON COLUMN v_contract_execution.remaining_amount IS
    '剩余可执行额 = amount − executed_amount；**可为负**（守卫只拦新单，合同额改小 / 单据恢复会越界）—— 报表必须显形，不许截成 0';
COMMENT ON COLUMN v_contract_execution.exec_state IS
    '执行状态：NO_DOWNSTREAM（服务/租赁合同无下游单据）/ NOT_STARTED（已执行 0）/ PARTIAL / DONE（恰好用满）/ OVER（超合同额）';
COMMENT ON COLUMN v_contract_execution.currency_mismatch_doc_count IS
    '计入的单据里币种 ≠ 合同币种的行数：只显形「USD 单挂 CNY 合同」这类串币，不改口径（守卫也不换算）';

-- ---------- ③ 菜单：执行量统计按钮（挂在已有的合同管理页面行下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004498'::uuid, '合同执行量', 3,
     'business:erp:contract:execution', NULL,
     '20000000-0000-0000-0000-000000001100'::uuid,
     NULL, 50, 1, NULL, '执行量统计', TRUE, 'business')
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
-- 采购经理：采购合同的执行进度是控盘口径（哪些合同快用满 / 已经超了）。
-- 销售主管：销售合同的执行进度同理（另一侧同一条报表）。
-- 财务经理：执行额是月度付款 / 收入确认的输入，复核口径与业务侧一致。
-- 部门主管 / 普通员工**不给**：合同是公司级文件（SCOPE-EXEMPT），部门诉求走各自的申请单。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000004498'::uuid)) AS m(id)
 WHERE r.role_code IN ('PURCHASE_MANAGER', 'SALES_SUPERVISOR', 'FINANCE_MANAGER')
   AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    v_cnt     integer;
    v_missing text;

    -- 探针数据（全部 UUID 固定，先造 → 先断言 → 再清）
    p_pc      uuid := '00000000-0000-4000-8000-0000045800a1';  -- 采购合同（1000）
    p_sc      uuid := '00000000-0000-4000-8000-0000045800a2';  -- 销售合同（500，恰好用满）
    p_vc      uuid := '00000000-0000-4000-8000-0000045800a3';  -- 服务合同（无下游）
    p_dc      uuid := '00000000-0000-4000-8000-0000045800a4';  -- 已软删采购合同（不许进报表）
    p_po1     uuid := '00000000-0000-4000-8000-0000045800b1';  -- 存活：吃 200
    p_po2     uuid := '00000000-0000-4000-8000-0000045800b2';  -- 作废：不吃
    p_po3     uuid := '00000000-0000-4000-8000-0000045800b3';  -- 软删：不吃
    p_po4     uuid := '00000000-0000-4000-8000-0000045800b4';  -- 存活但 USD（币种串了，仍计入）
    p_so1     uuid := '00000000-0000-4000-8000-0000045800c1';  -- 销售单：吃 500

    v_num     numeric;
    v_net     numeric;
    v_remain  numeric;
    v_rate    numeric;
    v_state   text;
    v_down    text;
    v_docs    integer;
    v_released integer;
    v_mismatch integer;
    v_date    date;
    v_expired boolean;
    v_usable  boolean;
BEGIN
    -- ⑤.1 两个视图必须在位
    IF to_regclass('public.v_contract_execution_order') IS NULL THEN
        RAISE EXCEPTION 'V0458 自检失败：视图 v_contract_execution_order 未建出';
    END IF;
    IF to_regclass('public.v_contract_execution') IS NULL THEN
        RAISE EXCEPTION 'V0458 自检失败：视图 v_contract_execution 未建出';
    END IF;

    -- ⑤.2 关键列名一个都不能少（改名时这里先炸，而不是等 handler 运行期 500）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('contract_id'),('source_type'),('order_id'),('order_no'),('order_status'),
                   ('order_date'),('currency_code'),('total_amount'),('total_with_tax'),
                   ('order_deleted_at'),('counted'),('currency_matched')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'v_contract_execution_order'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0458 自检失败：v_contract_execution_order 缺列：%', v_missing;
    END IF;
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('contract_id'),('contract_no'),('contract_name'),('contract_type'),('status'),
                   ('party_a_id'),('party_a_name'),('party_b_type'),('party_b_id'),('party_b_name'),
                   ('party_b_current_name'),('owner_id'),('owner_name'),('dept_id'),('dept_name'),
                   ('amount'),('currency_code'),('sign_date'),('start_date'),('end_date'),
                   ('downstream_type'),('order_doc_count'),('released_doc_count'),
                   ('currency_mismatch_doc_count'),('purchase_doc_count'),('sales_doc_count'),
                   ('executed_amount'),('executed_amount_net'),('remaining_amount'),
                   ('exec_amount_rate'),('exec_state'),('last_order_on'),('days_left'),
                   ('expired'),('usable_for_order'),('created_at'),('updated_at')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'v_contract_execution'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0458 自检失败：v_contract_execution 缺列：%', v_missing;
    END IF;

    -- ⑤.3 造探针：采购合同 1000（存活 200 + 作废 300 + 软删 400 + USD 单 100）
    INSERT INTO biz_contract
        (id, contract_no, contract_name, contract_type, party_b_type, party_b_id, party_b_name,
         amount, currency_code, sign_date, start_date, end_date, status)
    VALUES
        (p_pc, 'HT-19990101-945801', 'V0458 自检采购合同', 'PURCHASE', 'SUPPLIER',
         gen_random_uuid(), '探针供应商甲', 1000, 'CNY',
         CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE'),
        (p_sc, 'HT-19990101-945802', 'V0458 自检销售合同', 'SALES', 'CUSTOMER',
         gen_random_uuid(), '探针客户乙', 500, 'CNY',
         CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE'),
        (p_vc, 'HT-19990101-945803', 'V0458 自检服务合同', 'SERVICE', 'SUPPLIER',
         gen_random_uuid(), '探针供应商丙', 100, 'CNY',
         CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE'),
        (p_dc, 'HT-19990101-945804', 'V0458 自检已删合同', 'PURCHASE', 'SUPPLIER',
         gen_random_uuid(), '探针供应商丁', 700, 'CNY',
         CURRENT_DATE - 30, CURRENT_DATE - 30, CURRENT_DATE + 30, 'ACTIVE');
    UPDATE biz_contract SET deleted_at = now() WHERE id = p_dc;

    -- ⚠ `ck_biz_po_tax_sum` 要求 total_with_tax = total_amount + total_tax（精确恒等）
    -- ⚠ `ck_biz_po_contract_pair` 要求 purchase_type='CONTRACT' 时 contract_id 与
    --   contract_no_snapshot 同时非空
    INSERT INTO biz_purchase_order
        (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code,
         total_amount, total_tax, total_with_tax, purchase_type, contract_id, contract_no_snapshot)
    VALUES
        (p_po1, 'PO-19990101-945801', gen_random_uuid(), CURRENT_DATE - 10, gen_random_uuid(), 'APPROVED', 'CNY',
         177, 23, 200, 'CONTRACT', p_pc, 'HT-19990101-945801'),
        (p_po2, 'PO-19990101-945802', gen_random_uuid(), CURRENT_DATE - 10, gen_random_uuid(), 'CANCELLED', 'CNY',
         265.4867, 34.5133, 300, 'CONTRACT', p_pc, 'HT-19990101-945801'),
        (p_po3, 'PO-19990101-945803', gen_random_uuid(), CURRENT_DATE - 10, gen_random_uuid(), 'APPROVED', 'CNY',
         353.9823, 46.0177, 400, 'CONTRACT', p_pc, 'HT-19990101-945801'),
        (p_po4, 'PO-19990101-945804', gen_random_uuid(), CURRENT_DATE - 5, gen_random_uuid(), 'DRAFT', 'USD',
         88.4956, 11.5044, 100, 'CONTRACT', p_pc, 'HT-19990101-945801');
    UPDATE biz_purchase_order SET deleted_at = now() WHERE id = p_po3;

    -- 销售单：ck_sales_order_amount 同样要求 total_with_tax = total_amount + total_tax
    INSERT INTO biz_sales_order
        (id, so_no, customer_id, order_date, status, currency_code,
         total_amount, total_tax, total_with_tax, contract_id)
    VALUES
        (p_so1, 'SO-19990101-945801', gen_random_uuid(), CURRENT_DATE - 3, 'APPROVED', 'CNY',
         442.4779, 57.5221, 500, p_sc);

    -- ⑤.4 采购合同：已执行 200 + 100（USD 也照加，只显形）= 300；作废 300 与软删 400 不算
    SELECT executed_amount, executed_amount_net, remaining_amount, exec_amount_rate, exec_state,
           order_doc_count, released_doc_count, currency_mismatch_doc_count,
           downstream_type, last_order_on, expired, usable_for_order
      INTO v_num, v_net, v_remain, v_rate, v_state, v_docs, v_released, v_mismatch,
           v_down, v_date, v_expired, v_usable
      FROM v_contract_execution WHERE contract_id = p_pc;
    IF v_num <> 300 THEN
        RAISE EXCEPTION 'V0458 自检失败：采购合同已执行额应为 200 + 100 = 300（作废 300 / 软删 400 不许算），实际 %', v_num;
    END IF;
    IF v_net <> 265.4956 THEN
        RAISE EXCEPTION 'V0458 自检失败：已执行净额（不含税）应为 177 + 88.4956 = 265.4956，实际 %', v_net;
    END IF;
    IF v_remain <> 700 THEN
        RAISE EXCEPTION 'V0458 自检失败：剩余额应为 1000 − 300 = 700，实际 %', v_remain;
    END IF;
    IF v_rate <> 0.3 THEN
        RAISE EXCEPTION 'V0458 自检失败：执行率应为 0.3，实际 %', v_rate;
    END IF;
    IF v_state <> 'PARTIAL' THEN
        RAISE EXCEPTION 'V0458 自检失败：采购合同执行状态应为 PARTIAL，实际 %', v_state;
    END IF;
    IF v_docs <> 2 THEN
        RAISE EXCEPTION 'V0458 自检失败：计入单据数应为 2（存活 200 + 存活 USD 100），实际 %', v_docs;
    END IF;
    IF v_released <> 2 THEN
        RAISE EXCEPTION 'V0458 自检失败：释放单据数应为 2（作废 1 + 软删 1），实际 %', v_released;
    END IF;
    IF v_mismatch <> 1 THEN
        RAISE EXCEPTION 'V0458 自检失败：币种串了的计入单据应为 1（USD 单挂 CNY 合同），实际 %', v_mismatch;
    END IF;
    IF v_down IS DISTINCT FROM 'PURCHASE_ORDER' THEN
        RAISE EXCEPTION 'V0458 自检失败：采购合同的下游类型应为 PURCHASE_ORDER，实际 %', coalesce(v_down, 'NULL');
    END IF;
    IF v_date <> CURRENT_DATE - 5 THEN
        RAISE EXCEPTION 'V0458 自检失败：最近执行日应取计入单据里最晚的 order_date（D-5），实际 %', v_date;
    END IF;
    IF v_expired IS NOT FALSE OR v_usable IS NOT TRUE THEN
        RAISE EXCEPTION 'V0458 自检失败：探针采购合同（今天在效期内且 ACTIVE）应为未过期 / 可下单，实际 % / %', v_expired, v_usable;
    END IF;

    -- ⑤.5 销售合同：恰好用满（DONE、剩余 0、率 1），下游类型按合同类型分派
    SELECT executed_amount, remaining_amount, exec_amount_rate, exec_state, downstream_type, order_doc_count
      INTO v_num, v_remain, v_rate, v_state, v_down, v_docs
      FROM v_contract_execution WHERE contract_id = p_sc;
    IF v_num <> 500 OR v_remain <> 0 THEN
        RAISE EXCEPTION 'V0458 自检失败：销售合同应为已执行 500 / 剩余 0，实际 % / %', v_num, v_remain;
    END IF;
    IF v_rate <> 1 OR v_state <> 'DONE' THEN
        RAISE EXCEPTION 'V0458 自检失败：用满应是率 1 / DONE（边界：等于合同额不算 OVER），实际 % / %', v_rate, v_state;
    END IF;
    IF v_down IS DISTINCT FROM 'SALES_ORDER' THEN
        RAISE EXCEPTION 'V0458 自检失败：销售合同的下游类型应为 SALES_ORDER，实际 %', coalesce(v_down, 'NULL');
    END IF;
    IF v_docs <> 1 THEN
        RAISE EXCEPTION 'V0458 自检失败：销售合同计入单据应为 1，实际 %', v_docs;
    END IF;

    -- ⑤.6 服务合同：没有下游单据 ⇒ NO_DOWNSTREAM（而不是 NOT_STARTED —— 后者会被读成「该下单了」）
    SELECT executed_amount, remaining_amount, exec_state, downstream_type, order_doc_count
      INTO v_num, v_remain, v_state, v_down, v_docs
      FROM v_contract_execution WHERE contract_id = p_vc;
    IF v_state <> 'NO_DOWNSTREAM' OR v_down IS NOT NULL THEN
        RAISE EXCEPTION 'V0458 自检失败：服务合同应为 NO_DOWNSTREAM / downstream_type NULL，实际 % / %', v_state, coalesce(v_down, 'NULL');
    END IF;
    IF v_num <> 0 OR v_remain <> 100 THEN
        RAISE EXCEPTION 'V0458 自检失败：服务合同应为已执行 0 / 剩余 100（合同额原样），实际 % / %', v_num, v_remain;
    END IF;

    -- ⑤.7 已软删合同不许进报表
    SELECT count(*) INTO v_cnt FROM v_contract_execution WHERE contract_id = p_dc;
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0458 自检失败：已软删合同不该出现在报表里（实际 % 行）', v_cnt;
    END IF;

    -- ⑤.8 单头合计必须等于单据级明细里 counted 行之和（两套聚合不许分叉）
    SELECT COALESCE(SUM(total_with_tax), 0) INTO v_num
      FROM v_contract_execution_order WHERE contract_id = p_pc AND counted;
    SELECT executed_amount INTO v_net FROM v_contract_execution WHERE contract_id = p_pc;
    IF v_num <> v_net THEN
        RAISE EXCEPTION 'V0458 自检失败：单头已执行额 % 与单据级 counted 合计 % 不等', v_net, v_num;
    END IF;

    -- ⑤.9 清场：先删单据再删合同，删完逐项反查 0 残留
    DELETE FROM biz_sales_order    WHERE id = p_so1;
    DELETE FROM biz_purchase_order WHERE id IN (p_po1, p_po2, p_po3, p_po4);
    DELETE FROM biz_contract       WHERE id IN (p_pc, p_sc, p_vc, p_dc);
    SELECT count(*) INTO v_cnt FROM biz_contract WHERE id IN (p_pc, p_sc, p_vc, p_dc);
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0458 自检失败：探针合同还残留 % 行', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt FROM v_contract_execution_order
     WHERE order_id IN (p_po1, p_po2, p_po3, p_po4, p_so1);
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0458 自检失败：探针单据还残留在视图里 % 行', v_cnt;
    END IF;

    -- ⑤.10 菜单行在位，且类型 / 权限码 / 父级页面 / 排序 / 标题长度都对
    SELECT count(*) INTO v_cnt FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004498'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:contract:execution'
       AND parent_id = '20000000-0000-0000-0000-000000001100'::uuid
       AND sort_order = 50
       AND length(title) <= 12
       AND deleted_at IS NULL;
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0458 自检失败：执行量按钮行（类型 / 接口码 / 父级页面 / 排序 / 标题长度）不对';
    END IF;

    -- ⑤.11 授权：三个角色都挂上了
    SELECT count(*) INTO v_cnt
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = '20000000-0000-0000-0000-000000004498'::uuid
       AND r.role_code IN ('PURCHASE_MANAGER', 'SALES_SUPERVISOR', 'FINANCE_MANAGER');
    IF v_cnt <> 3 THEN
        RAISE EXCEPTION 'V0458 自检失败：授权应覆盖 3 个角色（采购经理 / 销售主管 / 财务经理），实际 %', v_cnt;
    END IF;

    RAISE NOTICE 'M4-19/V0458: 合同执行量统计（已执行 / 合同额 两视图 + 执行量按钮）自检通过';
END $$;
