-- ============================================================================
-- V0446：采购计划（M4-01）—— 表结构：计划单头 + 计划明细（物料 × 仓库）
--
--   ① biz_purchase_plan       采购计划单头（年度 / 月度；同一「类型 + 年度 + 月 + 编制部门」唯一）
--   ② biz_purchase_plan_item  计划明细（**物料 × 仓库** 一格一行；计划量 + 已执行量）
--   ③ biz_seq：PPL（PPL-yyyyMMdd-000001，按 CST 天滚动）
--   ④ 索引 + 末尾 DO 块自检（列 / 部分唯一索引 / CHECK 真能拒坏数据 / 默认值 / 取号行）
--
-- ── 本卡只交付表结构（卡面「本卡只做这一件事，不夹带」）───────────────────
--   不做：CRUD / 审批 / 计划 → 申请转换 / 执行率接口 / 权限码 / 菜单 / 路由 / 前端。
--   这些是 M4-02（CRUD + 审批）、M4-03（转申请）、M4-04（执行率）、M4-06（前端）。
--   本迁移**不写一行 sys_menu**（也就不会顶破 menu_invariants 的 400–540 上限），
--   权限码一个不种 —— 页面码与接口码由 M4-02 那卡按约定 14 一起落。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail`）
--   写入本文件时全局最大号 = V0445（V0445__sales_rebate_payable_account），V0446 空闲。
--   biz_seq 已用 seq_type 里没有 PPL；biz_seq 行 id 取 …4461（与既有 …3335 / …3180 段不交叠）。
--
-- ── 表口径（卡面逐条落地，差异与理由都写在这里）──────────────────────────
--   1) 卡面「年度/月度」⇒ 一个 `plan_type` 轴 + `plan_year` / `plan_month` 两列，
--      用 CHECK 把三者的自洽写死：ANNUAL 必须没有月、MONTHLY 必须有 1–12 的月。
--      **不**造 `period_code` 冗余列：年度写 'yyyy'、月度写 'yyyy-MM' 两套格式拼一张表，
--      迟早出现「类型说 ANNUAL 而期间是 2026-03」，聚合与排序还得再解析一次字符串。
--   2) 卡面「物料 × 仓库」⇒ 粒度落在**明细**：唯一索引 `uk_biz_pp_item_cell`
--      (plan_id, material_id, warehouse_id)。同一张计划里同一个物料挂在两个仓库是**两行**
--      （这正是本卡的粒度语），同物料同仓库出现两行则必然是把计划量灌了两遍 —— 库层直接拒。
--   3) 卡面「计划量与已执行量」⇒ 明细 `planned_qty` / `executed_qty`，单头另存
--      `total_planned_qty` / `total_executed_qty` 两个汇总（列表面板要，不想每次 GROUP BY）。
--      汇总由服务端在写明细的同一事务里重算（M4-02 的 model 提供 recalc），
--      **不许客户端传**；`total_executed_qty` 的执行回写是 M4-03/M4-04 的事。
--   4) 「已执行量」= 这份计划已经变成下游单据的数量（转采购申请 / 下单），不是入库量。
--      CHECK 只兜 `executed_qty >= 0`，**不**兜 `executed_qty <= planned_qty`：
--      超计划执行在现实里就是会发生（临时加量），拦在库层等于让用户改计划才能下单，
--      露头应该在 M4-04 的执行率报表里（执行率 > 100%），不是在插行时报 500。
--   5) 金额三列（明细 `planned_price` / `planned_amount`，单头 `total_planned_amount`）：
--      卡面没写钱，但「计划 → 采购申请」转换（M4-03）要用预估单价预填 `est_price`，
--      没这列转换只能填 0，预算与比价全部失真。精度与采购申请单对齐
--      （`est_price NUMERIC(18,6)` / 金额 `NUMERIC(18,4)`），金额 = qty × price 由服务端算。
--   6) `parent_plan_id` 自引用：月度计划可挂到年度计划下（年度 → 月度分解）。
--      **库层只做弱约束**（不许自指），「月度的父必须是年度、且年月落在父的年度内」是
--      跨行语义，CHECK 做不到（要触发器），由 M4-02 的 handler 在写入前判 —— 这里写清楚，
--      免得后来人以为库层已经兜住了。
--   7) 状态域只给三个：DRAFT（编制中）/ SUBMITTED（审批中）/ APPROVED（批准，可执行）。
--      **不造** CLOSED / CANCELLED —— 卡面没有「关闭」动作，造了就是没有入口的死状态
--      （与 V0331 不造「作废」同一口径）。真需要归档时**追加**迁移扩 CHECK，不改这张。
--      `SUBMITTED` 必须有审批实例（否则「谁批的」查不出来）；APPROVED 不强制 ——
--      显式跳过审批的路径与采购申请单同口径。
--   8) 数据权限：`biz_purchase_plan(_item)` 与采购申请单同口径 **SCOPE-EXEMPT** ——
--      计划是公司级滚动文件（采购要看全公司），可见性交给 M4-02 的权限码。
--      所以本卡**不**登记 `SCOPED_TABLES`（登记了启动自检就会去校 dept 列，而这里的
--      `dept_id` 是「编制部门」快照语义，不是数据范围列）。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 用 WHERE NOT EXISTS。
--   末尾 DO 块自检：表 / 列数 / 部分唯一索引 / 坏数据被拒（逐条独立）/ 默认值 /
--   正样本能插 / 取号行，任一不符 → 迁移失败（不留半套结构）。
--   回滚见 V0446__purchase_plan.down.sql：**有数据就拒绝回滚**（新表里的每一行都是真数据，
--   没有「演示数据」可丢），再删索引 / 表 / 取号行。
-- ============================================================================

-- ---------- ① biz_purchase_plan（采购计划单头） ----------
CREATE TABLE IF NOT EXISTS biz_purchase_plan (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    plan_no              VARCHAR(32)   NOT NULL,                              -- 计划单号：PPL-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    plan_name            VARCHAR(128)  NOT NULL,                              -- 计划名称（如「2026 年度采购计划」「2026 年 11 月采购计划」）
    plan_type            VARCHAR(16)   NOT NULL,                              -- ANNUAL 年度 / MONTHLY 月度
    plan_year            INT           NOT NULL,                              -- 计划年度（1990–2200）
    plan_month           INT,                                                 -- 月度计划 1–12；年度计划必须 NULL（ck_biz_pp_period）
    parent_plan_id       UUID,                                                -- 月度计划挂到年度计划下（分解关系；弱约束，见头注释 6)
    dept_id              UUID,                                                -- 编制部门（弱引用 sys_dept.id；可空 = 公司级计划）
    dept_name            VARCHAR(128),                                        -- 编制部门名快照
    planner_id           UUID,                                                -- 编制人（弱引用 sys_user.id）
    planner_name         VARCHAR(64),                                         -- 编制人姓名快照（审计/历史类，按约定 13 存快照）
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',              -- DRAFT / SUBMITTED / APPROVED
    approval_instance_id UUID,                                                -- 提交审批的流程实例 id（SUBMITTED 必需）
    total_planned_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 计划量合计（服务端按明细重算，客户端不许传）
    total_executed_qty   NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 已执行量合计（服务端按明细重算）
    total_planned_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 计划金额合计（不含税，服务端按明细重算）
    remark               TEXT,                                                -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：创建时间
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：更新时间（model 层维护）
    create_by            UUID,                                                -- 审计：创建人
    update_by            UUID,                                                -- 审计：更新人
    deleted_at           TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_pp_no_fmt
        CHECK (plan_no ~ '^PPL-[0-9]{8}-[0-9]{6}$'),
    CONSTRAINT ck_biz_pp_name
        CHECK (btrim(plan_name) <> ''),
    CONSTRAINT ck_biz_pp_type
        CHECK (plan_type IN ('ANNUAL','MONTHLY')),
    -- 「年度/月度」的自洽：年度没有月；月度必须有 1–12 的月。
    -- ⚠ `plan_month IS NOT NULL` 不能省：`MONTHLY AND plan_month BETWEEN 1 AND 12`
    -- 在 plan_month 为 NULL 时求值成 NULL，而 CHECK 只拒 FALSE —— 少了这半句，
    -- 「月度计划没有月份」会被放进来（本卡的 db 用例 `annual_plan_has_no_month_…` 钉住这点）。
    CONSTRAINT ck_biz_pp_period
        CHECK ((plan_type = 'ANNUAL'  AND plan_month IS NULL)
            OR (plan_type = 'MONTHLY' AND plan_month IS NOT NULL
                                      AND plan_month BETWEEN 1 AND 12)),
    CONSTRAINT ck_biz_pp_year
        CHECK (plan_year BETWEEN 1990 AND 2200),
    CONSTRAINT ck_biz_pp_status
        CHECK (status IN ('DRAFT','SUBMITTED','APPROVED')),
    -- 审批中必须有审批实例（「谁批的」要查得出来）
    CONSTRAINT ck_biz_pp_instance
        CHECK (status <> 'SUBMITTED' OR approval_instance_id IS NOT NULL),
    -- 自引用不许指自己（跨行语义由 handler 保证，见头注释 6)
    CONSTRAINT ck_biz_pp_parent
        CHECK (parent_plan_id IS NULL OR parent_plan_id <> id),
    CONSTRAINT ck_biz_pp_totals
        CHECK (total_planned_qty >= 0 AND total_executed_qty >= 0 AND total_planned_amount >= 0)
);

COMMENT ON TABLE  biz_purchase_plan IS '采购计划单头（M4-01 表结构；年度 / 月度采购计划，明细粒度 = 物料 × 仓库）';
COMMENT ON COLUMN biz_purchase_plan.plan_no IS '计划单号：PPL-yyyyMMdd-000001（biz_seq.seq_type=PPL，按北京时间天滚动）';
COMMENT ON COLUMN biz_purchase_plan.plan_month IS '月度计划 1–12；年度计划必须为 NULL（ck_biz_pp_period 把类型与月份绑死）';
COMMENT ON COLUMN biz_purchase_plan.parent_plan_id IS '月度计划挂到的年度计划（年度→月度分解）；库层只拦自指，「父必须是年度且年月落在父年内」由 handler 判';
COMMENT ON COLUMN biz_purchase_plan.status IS 'DRAFT 编制中 / SUBMITTED 审批中（必有审批实例）/ APPROVED 批准（可转采购申请，M4-03）';
COMMENT ON COLUMN biz_purchase_plan.total_planned_qty IS '计划量合计 = Σ明细 planned_qty（服务端在写明细的同一事务里重算，客户端不许传）';
COMMENT ON COLUMN biz_purchase_plan.total_executed_qty IS '已执行量合计 = Σ明细 executed_qty（执行回写属 M4-03/M4-04）';

-- 编号唯一（存活行唯一，与 uk_biz_purchase_requisition_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_purchase_plan_no
    ON biz_purchase_plan (plan_no) WHERE deleted_at IS NULL;
-- 「同一类型 + 同一期间 + 同一编制部门」只能有一条存活计划：
-- 否则同一期间会有两张计划各自算一遍执行率（与 V0318 销售目标同构的坑）。
-- COALESCE 把「年度无月」「公司级无部门」的 NULL 折成定值 ——
-- NULL 在唯一索引里互不相等，不折算等于没约束。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_purchase_plan_period
    ON biz_purchase_plan (plan_type,
                          plan_year,
                          COALESCE(plan_month, 0),
                          COALESCE(dept_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_purchase_plan_period
    ON biz_purchase_plan (plan_year DESC, plan_month DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_purchase_plan_status
    ON biz_purchase_plan (status, plan_year DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_purchase_plan_parent
    ON biz_purchase_plan (parent_plan_id) WHERE parent_plan_id IS NOT NULL;

-- ---------- ② biz_purchase_plan_item（计划明细：物料 × 仓库） ----------
-- 明细随主单整体重建（与采购申请 / 采购订单同口径：先删后插，事务内），故不设 deleted_at。
CREATE TABLE IF NOT EXISTS biz_purchase_plan_item (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    plan_id        UUID          NOT NULL,                              -- 所属计划
    line_no        INT           NOT NULL,                              -- 行号（1 起，单内唯一）
    material_id    UUID          NOT NULL,                              -- 物料（弱引用 biz_material.id）
    warehouse_id   UUID          NOT NULL,                              -- 仓库（弱引用 biz_warehouse.id）—— 本卡的粒度轴之一
    unit_id        UUID          NOT NULL,                              -- 计量单位（建单时由物料带出，客户端不许传）
    planned_qty    NUMERIC(18,4) NOT NULL,                              -- 计划量（> 0）
    executed_qty   NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 已执行量（>= 0；超计划不拦，见头注释 4)
    planned_price  NUMERIC(18,6) NOT NULL DEFAULT 0,                    -- 预估不含税单价（转申请时预填 est_price）
    planned_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 计划金额（不含税）= planned_qty × planned_price，服务端算
    remark         TEXT,                                                -- 行备注
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,    -- 审计：更新时间（执行量回写时刷新）

    CONSTRAINT fk_biz_ppi_plan FOREIGN KEY (plan_id)
        REFERENCES biz_purchase_plan (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_ppi_line  CHECK (line_no > 0),
    CONSTRAINT ck_biz_ppi_qty   CHECK (planned_qty > 0),
    CONSTRAINT ck_biz_ppi_exec  CHECK (executed_qty >= 0),
    CONSTRAINT ck_biz_ppi_price CHECK (planned_price >= 0 AND planned_amount >= 0)
);

COMMENT ON TABLE  biz_purchase_plan_item IS '采购计划明细（一行 = 物料 × 仓库；计划量 + 已执行量，M4-01 表结构）';
COMMENT ON COLUMN biz_purchase_plan_item.warehouse_id IS '仓库 —— 与 material_id 一起构成计划粒度（uk_biz_pp_item_cell）';
COMMENT ON COLUMN biz_purchase_plan_item.planned_qty IS '计划量（> 0）：ck_biz_ppi_qty 拒 0 与负数';
COMMENT ON COLUMN biz_purchase_plan_item.executed_qty IS '已执行量（>= 0）：已转采购申请 / 下单的数量，由 M4-03/M4-04 回写；刻意不拦「超计划」';
COMMENT ON COLUMN biz_purchase_plan_item.planned_amount IS '计划金额（不含税）= planned_qty × planned_price，服务端算，客户端传了也覆盖';

-- 卡面粒度：同一张计划里「物料 × 仓库」只有一行
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_pp_item_cell
    ON biz_purchase_plan_item (plan_id, material_id, warehouse_id);
-- 行号单内唯一（与 uk_biz_prqi_line 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_pp_item_line
    ON biz_purchase_plan_item (plan_id, line_no);
-- 执行率 / 按物料汇总：跨计划的同物料同仓库要能一把捞出来
CREATE INDEX IF NOT EXISTS idx_biz_pp_item_material
    ON biz_purchase_plan_item (material_id, warehouse_id);

-- ---------- ③ 取号规则 PPL ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000004461'::uuid, 'PPL', '采购计划单号', 'PPL', 'yyyyMMdd', '', 0, 1, '-', 1,
       '采购计划单号：PPL-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PPL' AND deleted_at IS NULL);

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    v_cnt     integer;
    v_name    text;
    v_missing text;
    probe     uuid := gen_random_uuid();
    probe2    uuid := gen_random_uuid();
    item      uuid := gen_random_uuid();
BEGIN
    -- ④.1 两张表必须建出来
    IF to_regclass('public.biz_purchase_plan') IS NULL THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_purchase_plan 未建出';
    END IF;
    IF to_regclass('public.biz_purchase_plan_item') IS NULL THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_purchase_plan_item 未建出';
    END IF;

    -- ④.2 列数（单头 22 列 / 明细 13 列）
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_purchase_plan';
    IF v_cnt <> 22 THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_purchase_plan 列数 % ≠ 22', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_purchase_plan_item';
    IF v_cnt <> 13 THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_purchase_plan_item 列数 % ≠ 13', v_cnt;
    END IF;

    -- ④.3 三个部分唯一索引必须带 WHERE deleted_at IS NULL（软删后可重建）
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_purchase_plan_no','uk_biz_purchase_plan_period')
       AND indexdef LIKE '%WHERE (deleted_at IS NULL)%';
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0446 自检失败：两张部分唯一索引只建出 % 个带软删条件的', v_cnt;
    END IF;
    -- 明细的两个唯一索引（无 deleted_at，全量唯一）
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_pp_item_cell','uk_biz_pp_item_line');
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0446 自检失败：明细唯一索引只建出 % / 2 个', v_cnt;
    END IF;
    -- 外键必须在（删单头连带删明细）
    SELECT count(*) INTO v_cnt
      FROM information_schema.table_constraints
     WHERE table_schema = 'public' AND table_name = 'biz_purchase_plan_item'
       AND constraint_type = 'FOREIGN KEY' AND constraint_name = 'fk_biz_ppi_plan';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0446 自检失败：明细表外键 fk_biz_ppi_plan 不在位';
    END IF;

    -- ④.4 坏数据必须被拒（逐条独立，任一「能插进去」即失败）
    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month)
        VALUES ('PPL-20260101-000001', '探针', 'QUARTERLY', 2026, NULL);
        RAISE EXCEPTION 'V0446 自检失败：非法 plan_type 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month)
        VALUES ('PPL-20260101-000002', '探针', 'ANNUAL', 2026, 3);
        RAISE EXCEPTION 'V0446 自检失败：年度计划带月份被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month)
        VALUES ('PPL-20260101-000003', '探针', 'MONTHLY', 2026, 13);
        RAISE EXCEPTION 'V0446 自检失败：月度计划 13 月被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year)
        VALUES ('PPL-20260101-000004', '探针', 'ANNUAL', 1800);
        RAISE EXCEPTION 'V0446 自检失败：越界计划年度被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, status)
        VALUES ('PPL-20260101-000005', '探针', 'ANNUAL', 2026, 'SUBMITTED');
        RAISE EXCEPTION 'V0446 自检失败：SUBMITTED 无审批实例被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, status)
        VALUES ('PPL-20260101-000006', '探针', 'ANNUAL', 2026, 'CLOSED');
        RAISE EXCEPTION 'V0446 自检失败：状态域外的 CLOSED 被接受（本卡刻意不造死状态）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year)
        VALUES ('PPL-2026-1', '探针', 'ANNUAL', 2026);
        RAISE EXCEPTION 'V0446 自检失败：不合法的计划单号格式被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year)
        VALUES ('PPL-20260101-000007', '   ', 'ANNUAL', 2026);
        RAISE EXCEPTION 'V0446 自检失败：空白计划名被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- ④.5 正样本：默认值对、合计默认 0、同期间同部门第二条被唯一索引拒、自指被拒
    INSERT INTO biz_purchase_plan (id, plan_no, plan_name, plan_type, plan_year, plan_month, dept_id)
    VALUES (probe, 'PPL-20260101-000101', '2026 年 11 月采购计划', 'MONTHLY', 2026, 11, NULL);
    IF (SELECT status FROM biz_purchase_plan WHERE id = probe) <> 'DRAFT' THEN
        RAISE EXCEPTION 'V0446 自检失败：status 默认值不是 DRAFT';
    END IF;
    IF (SELECT total_planned_qty FROM biz_purchase_plan WHERE id = probe) <> 0
       OR (SELECT total_executed_qty FROM biz_purchase_plan WHERE id = probe) <> 0
       OR (SELECT total_planned_amount FROM biz_purchase_plan WHERE id = probe) <> 0 THEN
        RAISE EXCEPTION 'V0446 自检失败：三个合计列的默认值不是 0';
    END IF;

    BEGIN
        INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month)
        VALUES ('PPL-20260101-000102', '同期间的第二张月度计划', 'MONTHLY', 2026, 11);
        RAISE EXCEPTION 'V0446 自检失败：同类型 + 同期间 + 同部门（都为空部门）的第二张计划被接受（uk_biz_purchase_plan_period 失效）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    BEGIN
        UPDATE biz_purchase_plan SET parent_plan_id = id WHERE id = probe;
        RAISE EXCEPTION 'V0446 自检失败：parent_plan_id 自指被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- ④.6 明细：默认值 / 粒度唯一 / 计划量必须 > 0 / 外键
    INSERT INTO biz_purchase_plan_item (id, plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty)
    VALUES (item, probe, 1, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 100);
    IF (SELECT executed_qty FROM biz_purchase_plan_item WHERE id = item) <> 0 THEN
        RAISE EXCEPTION 'V0446 自检失败：executed_qty 默认值不是 0';
    END IF;
    IF (SELECT planned_price FROM biz_purchase_plan_item WHERE id = item) <> 0
       OR (SELECT planned_amount FROM biz_purchase_plan_item WHERE id = item) <> 0 THEN
        RAISE EXCEPTION 'V0446 自检失败：金额列默认值不是 0';
    END IF;

    BEGIN
        INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty)
        SELECT plan_id, 2, material_id, warehouse_id, unit_id, 50
          FROM biz_purchase_plan_item WHERE id = item;
        RAISE EXCEPTION 'V0446 自检失败：同一计划里「同物料 + 同仓库」的第二行被接受（uk_biz_pp_item_cell 失效，计划量会被灌两遍）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty)
        VALUES (probe, 2, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 0);
        RAISE EXCEPTION 'V0446 自检失败：计划量 0 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty, executed_qty)
        VALUES (probe, 2, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 10, -1);
        RAISE EXCEPTION 'V0446 自检失败：负已执行量被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty)
        VALUES (probe2, 1, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 1);
        RAISE EXCEPTION 'V0446 自检失败：plan_id 指向不存在的计划被接受（外键失效）';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    -- ④.7 删单头连带删明细（ON DELETE CASCADE）
    DELETE FROM biz_purchase_plan WHERE id = probe;
    SELECT count(*) INTO v_cnt FROM biz_purchase_plan_item WHERE id = item;
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0446 自检失败：删单头后明细还留着（ON DELETE CASCADE 失效）';
    END IF;

    -- ④.8 取号规则只有一行
    SELECT count(*) INTO v_cnt FROM biz_seq
     WHERE seq_type = 'PPL' AND deleted_at IS NULL AND prefix = 'PPL' AND separator = '-';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_seq(PPL) 种下 % 行（期望 1）', v_cnt;
    END IF;

    -- ④.9 关键列名一个都不能少（迁移改名时这里会先炸，而不是等到 handler 运行期）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('plan_no'),('plan_name'),('plan_type'),('plan_year'),('plan_month'),
                   ('parent_plan_id'),('dept_id'),('planner_id'),('status'),('approval_instance_id'),
                   ('total_planned_qty'),('total_executed_qty'),('total_planned_amount'),('deleted_at')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_purchase_plan'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_purchase_plan 缺列：%', v_missing;
    END IF;
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('plan_id'),('line_no'),('material_id'),('warehouse_id'),('unit_id'),
                   ('planned_qty'),('executed_qty'),('planned_price'),('planned_amount')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_purchase_plan_item'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0446 自检失败：biz_purchase_plan_item 缺列：%', v_missing;
    END IF;

    SELECT c.relname INTO v_name FROM pg_class c
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public' AND c.relname = 'biz_purchase_plan_item';
    RAISE NOTICE 'M4-01/V0446: 采购计划两表（物料 × 仓库，计划量 / 已执行量）+ 取号 PPL 自检通过（明细表 % 在位）', v_name;
END $$;
