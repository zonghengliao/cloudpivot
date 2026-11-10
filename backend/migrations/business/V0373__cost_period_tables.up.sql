-- ============================================================================
-- V0373：成本核算升级 · 期间两表（ARCH-9 §3.1 ⑨⑩）+ 自检
--
-- 卡：t_7efea068（COST-B1-3【成本】迁移 V0370/0371）—— 拆卡清单 §8.1 的 B1-3
--     「分摊两表 + 期间两表（含排除 REVERSED 的唯一键）」
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.1 ⑨⑩、§4.1 状态机
--       （该节 DDL 已在真库跑通并 ROLLBACK，见
--         docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql 与 .out）
--
-- 本迁移建 2 张表：
--   ⑨ biz_cost_period       成本核算批次（期间结算单）
--   ⑩ biz_cost_period_item  成本计算单（成本对象 × 期间）
--   分摊两表在 V0372（⑦⑧）；二期三表属 B1-4，出库/入库加列属 B1-5 —— 不在这里。
--
-- # ★ 本卡的核心验收：唯一键必须排除 REVERSED（还有 CANCELLED）
--
--   设计 §4.1 与附录 A「真库跑通过程中改掉的 3 个真错误」第 1 条：
--     最初写成 `SELECT ... UNIQUE (period_code)` ⇒ 红冲后该期间**永远无法重新结算**
--     （红冲行留在表里占着键，而状态机不允许把 REVERSED 改回去）⇒ 只能改库的死单。
--   正确形状：部分唯一索引
--       CREATE UNIQUE INDEX uk_biz_cost_period ON biz_cost_period (org_id, period_code)
--         WHERE deleted_at IS NULL AND status NOT IN ('REVERSED','CANCELLED');
--   语义：REVERSED / CANCELLED 行是**审计留痕**，不占「该期间当前有效批次」的位置。
--
--   下面行为自检 B1~B4 就是你卡面要求的实测（写在迁移里，部署即验）：
--     B1 插一条 REVERSED（2026-01）                        → 成功
--     B2 同期间再插一条 SETTLED（2026-01）                 → 成功（★「一条 REVERSED 一条 SETTLED 不冲突」）
--     B3 同期间再插一条 DRAFT                             → unique_violation（活跃批次仍唯一）
--     B4 同期间再插第二条 REVERSED                        → 成功（红冲留痕允许多条，设计附录 A 断言⑦）
--   另有 CANCELLED 排除、软删不占键、batch_no 复用三条。
--
-- # org_id 的哨兵默认值（设计 D-18）
--   唯一键是 `(org_id, period_code)` 而不是 `period_code`：多组织（ARCH-9-1）落地后
--   不需要换键（换键窗口的并发就是双单）。当前不上多组织 ⇒ 默认值全 0 哨兵，
--   所有行都落在同一组织，行为与 `period_code` 单列唯一一致。**不建 FK**（组织表属主数据）。
--
-- # 两种 CASCADE / RESTRICT 的取舍（设计 §3.4）
--   biz_cost_period_item.period_id     → biz_cost_period     ON DELETE CASCADE
--     （计算单是批次的从属明细，批次删了明细跟着走，避免孤儿）
--   biz_cost_period_item.cost_object_id → biz_cost_object    ON DELETE RESTRICT
--     （成本对象是主数据，有计算单就不许删）
--   自检 A5 用 confdeltype 逐条断言：'c' = CASCADE、'r' = RESTRICT。
--
-- # 幂等约定
--   表 / 索引一律 IF NOT EXISTS；注释走 COMMENT ON（重复执行结果不变）；
--   自检探针行按 batch_no LIKE 'SELFCHECK-B3-%' 删干净再用 ⇒ 重复执行零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：biz_cost_object 不在就直说「先回滚/先跑哪张卡」
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0373 拒绝执行：biz_cost_object 不存在，请先执行 V0370（COST-B1-1）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- ⑨ biz_cost_period —— 成本核算批次（期间结算单）
--    一个期间（yyyy-MM）同一组织同时只允许一个**活跃**批次（唯一键见下）。
--    状态机见设计 §4.1：DRAFT → SUBMITTED → APPROVED → SETTLED → CLOSED，另有 CANCELLED / REVERSED。
--    下面 4 条状态关联 CHECK 是「死单/假结算」的库层防线：
--      ck_..._appr   ：SUBMITTED 之后必须挂审批实例（不许绕过审批流直接改状态）
--      ck_..._settle ：SETTLED 之后必须有 settled_at + finish_voucher_id（不许「说结算了但没有凭证」）
--      ck_..._rev    ：REVERSED 必须有 reversed_at + reverse_reason（红冲要留痕）
--      ck_..._cancel ：CANCELLED 必须有 cancel_reason（作废要说原因）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_period (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id               UUID          NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    period_code          VARCHAR(7)    NOT NULL,
    batch_no             VARCHAR(32)   NOT NULL,                     -- CS-yyyyMM-0001
    status               VARCHAR(12)   NOT NULL DEFAULT 'DRAFT',
    labor_source         VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',    -- MANUAL / PAYROLL（二选一，防双计提）
    object_count         INT           NOT NULL DEFAULT 0,
    total_material       NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_labor          NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_overhead       NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_cost           NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_finished       NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_wip            NUMERIC(18,4) NOT NULL DEFAULT 0,
    approval_instance_id UUID,
    labor_voucher_id     UUID,
    overhead_voucher_id  UUID,
    finish_voucher_id    UUID,
    settled_at           TIMESTAMPTZ,
    settled_by           UUID,
    reversed_at          TIMESTAMPTZ,
    reversed_by          UUID,
    reverse_reason       VARCHAR(255),
    cancel_reason        VARCHAR(255),
    remark               TEXT,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_period_code   CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_period_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','SETTLED','CLOSED','CANCELLED','REVERSED')),
    CONSTRAINT ck_biz_cost_period_labor  CHECK (labor_source IN ('MANUAL','PAYROLL')),
    -- 总成本 = 料 + 工 + 费（三个要素合计口径）
    CONSTRAINT ck_biz_cost_period_sum    CHECK (total_cost = total_material + total_labor + total_overhead),
    -- 总成本 = 完工转出 + 期末在制（成本守恒，口径同 period_item 的行级守恒）
    CONSTRAINT ck_biz_cost_period_cons   CHECK (total_cost = total_finished + total_wip),
    CONSTRAINT ck_biz_cost_period_appr   CHECK (status NOT IN ('SUBMITTED','APPROVED','SETTLED','CLOSED','REVERSED')
                                                OR approval_instance_id IS NOT NULL),
    CONSTRAINT ck_biz_cost_period_settle CHECK (status NOT IN ('SETTLED','CLOSED','REVERSED')
                                                OR (settled_at IS NOT NULL AND finish_voucher_id IS NOT NULL)),
    CONSTRAINT ck_biz_cost_period_rev    CHECK (status <> 'REVERSED'
                                                OR (reversed_at IS NOT NULL AND reverse_reason IS NOT NULL)),
    CONSTRAINT ck_biz_cost_period_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);

COMMENT ON TABLE  biz_cost_period IS '成本核算批次（期间结算单）：一个期间一个活跃批次；红冲/作废行留表不占位（可重建同期间批次）';
COMMENT ON COLUMN biz_cost_period.org_id IS '组织 id：当前哨兵默认值（未上多组织）；唯一键含本列，多组织落地时不必换键（设计 D-18）';
COMMENT ON COLUMN biz_cost_period.period_code IS '会计期间 yyyy-MM：与 biz_accounting_period 同期，结账预检第 5 项 blocker COST_UNSETTLED 按它查';
COMMENT ON COLUMN biz_cost_period.batch_no IS '批次号 CS-yyyyMM-0001（号段 CS，B1-7 种）：红冲重算会生成新批次号，旧号保留';
COMMENT ON COLUMN biz_cost_period.status IS 'DRAFT/SUBMITTED/APPROVED/SETTLED/CLOSED/CANCELLED/REVERSED（§4.1 状态机；APPROVED 之后数据冻结）';
COMMENT ON COLUMN biz_cost_period.labor_source IS '人工成本来源：MANUAL 手工/导入工时台账，PAYROLL 工资表；二选一防同一批人工计提两次';
COMMENT ON COLUMN biz_cost_period.approval_instance_id IS '审批实例（弱引用）：SUBMITTED 之后必填，堵「绕过审批流直接改状态」';
COMMENT ON COLUMN biz_cost_period.finish_voucher_id IS '完工入库结转凭证（弱引用）：SETTLED 必填，堵「说结算了但没有凭证」的死单';
COMMENT ON COLUMN biz_cost_period.total_finished IS '完工转出合计（Σ period_item.finished_cost），与 total_wip 一起守恒到 total_cost';
COMMENT ON COLUMN biz_cost_period.reverse_reason IS '红冲原因：REVERSED 必填（审计留痕）';

-- ★ 唯一键排除 REVERSED / CANCELLED：红冲/作废行留在表里（审计），但不再占位 ——
--   否则红冲后永远无法重新结算该期间（死单）。真库已实测两种情形（附录 A 断言⑦）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_period
    ON biz_cost_period (org_id, period_code)
    WHERE deleted_at IS NULL AND status NOT IN ('REVERSED','CANCELLED');
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_period_no
    ON biz_cost_period (batch_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_period_status
    ON biz_cost_period (status, period_code DESC);

-- ----------------------------------------------------------------------------
-- ⑩ biz_cost_period_item —— 成本计算单（成本对象 × 期间）
--    product_id 与 cost_object.product_id 冗余存一份：成本对象事后改品，
--    历史计算单仍显示当时算的是哪个产品（错也是可追溯的错）。
--    行级成本守恒（ck_biz_cost_item_cons）是**库层**硬防线：
--        完工转出 + 期末在制 = 期初在制 + 本期投入（料 + 工 + 费）
--    算错一格就插不进去 —— 这是约当产量法最容易出错的地方（§4.6）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_period_item (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id          UUID          NOT NULL REFERENCES biz_cost_period(id) ON DELETE CASCADE,
    cost_object_id     UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    product_id         UUID          NOT NULL,                        -- 与 cost_object.product_id 冗余（防对象改品）
    wip_beg_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_beg_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,
    completed_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_end_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,
    completion_pct     NUMERIC(5,2)  NOT NULL DEFAULT 0,
    equiv_material     NUMERIC(18,4) NOT NULL DEFAULT 0,
    equiv_conversion   NUMERIC(18,4) NOT NULL DEFAULT 0,
    mat_cur            NUMERIC(18,4) NOT NULL DEFAULT 0,
    labor_cur          NUMERIC(18,4) NOT NULL DEFAULT 0,
    oh_cur             NUMERIC(18,4) NOT NULL DEFAULT 0,
    unit_material      NUMERIC(18,6) NOT NULL DEFAULT 0,
    unit_labor         NUMERIC(18,6) NOT NULL DEFAULT 0,
    unit_overhead      NUMERIC(18,6) NOT NULL DEFAULT 0,
    finished_cost      NUMERIC(18,4) NOT NULL DEFAULT 0,
    wip_end_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,
    unit_finished_cost NUMERIC(18,6) NOT NULL DEFAULT 0,             -- → stock_in_item.cost_price
    stock_in_id        UUID,
    status             VARCHAR(12)   NOT NULL DEFAULT 'CALC',        -- CALC / SETTLED
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_cost_item_status  CHECK (status IN ('CALC','SETTLED')),
    CONSTRAINT ck_biz_cost_item_equiv   CHECK (equiv_material >= 0 AND equiv_conversion >= 0),
    CONSTRAINT ck_biz_cost_item_units   CHECK (unit_material >= 0 AND unit_labor >= 0 AND unit_overhead >= 0),
    CONSTRAINT ck_biz_cost_item_money   CHECK (finished_cost >= 0 AND wip_end_cost >= 0 AND wip_beg_cost >= 0),
    CONSTRAINT ck_biz_cost_item_pct     CHECK (completion_pct >= 0 AND completion_pct <= 100),
    -- ★ 成本守恒（库层）：完工转出 + 期末在制 = 期初在制 + 本期投入
    CONSTRAINT ck_biz_cost_item_cons    CHECK (
        finished_cost + wip_end_cost = wip_beg_cost + mat_cur + labor_cur + oh_cur),
    -- 结算行必须有生产入库单（否则「状态说入库了但单据不存在」）
    CONSTRAINT ck_biz_cost_item_settled CHECK (status <> 'SETTLED' OR stock_in_id IS NOT NULL)
);

COMMENT ON TABLE  biz_cost_period_item IS '成本计算单：成本对象×期间，含约当产量与完工/在制拆分；行级成本守恒由 CHECK 硬拦';
COMMENT ON COLUMN biz_cost_period_item.period_id IS '所属成本核算批次（ON DELETE CASCADE：计算单是批次的从属明细）';
COMMENT ON COLUMN biz_cost_period_item.cost_object_id IS '成本对象（ON DELETE RESTRICT：有计算单的主数据不许删）';
COMMENT ON COLUMN biz_cost_period_item.product_id IS '产品 id 冗余快照：成本对象事后改品，历史计算单仍能说明算的是哪个产品';
COMMENT ON COLUMN biz_cost_period_item.equiv_conversion IS '约当产量（转换成本）：完工数量 + 期末在制数量 × 完工进度（§4.6.1 加权平均法）';
COMMENT ON COLUMN biz_cost_period_item.unit_finished_cost IS '完工单位成本（18,6）：结算时写进 biz_stock_in_item.cost_price，库存账永远只是下游';
COMMENT ON COLUMN biz_cost_period_item.status IS 'CALC 已计算可重算 / SETTLED 已结算（必须挂 stock_in_id）';
COMMENT ON COLUMN biz_cost_period_item.stock_in_id IS '生产入库单（弱引用 biz_stock_in.id）：SETTLED 必填';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_item    ON biz_cost_period_item (period_id, cost_object_id);
CREATE INDEX IF NOT EXISTS idx_biz_cost_item_obj      ON biz_cost_period_item (cost_object_id);

-- 重算 = 删本期间计算单再重插：`(period_id, cost_object_id)` 唯一保证「一个批次里一个对象只有一张单」（§4.6.3）

-- ----------------------------------------------------------------------------
-- ① 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    分两类断言：
--      A. 形态：表 / ★唯一索引的谓词 / CHECK 数量 / 外键删除动作（CASCADE vs RESTRICT）/
--         金额精度 / org_id 哨兵默认值
--      B. 行为：真插数据，把每条关键约束的**反面**跑一遍；核心是 ★B1~B4
--         「同期间一条 REVERSED + 一条 SETTLED 不冲突、活跃批次仍唯一」。
--         探针行断言后立即清理，重复执行零残留。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_def  TEXT;
    v_norm TEXT;
    v_n    INT;
    v_fmt  TEXT;
    v_col  TEXT;
    v_want TEXT;
BEGIN
    -- ===== A. 形态 =====

    -- A1 两张表都在
    IF to_regclass('public.biz_cost_period') IS NULL THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period 未建出来';
    END IF;
    IF to_regclass('public.biz_cost_period_item') IS NULL THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period_item 未建出来';
    END IF;

    -- A2 ★ uk_biz_cost_period：UNIQUE + 软删 + 「状态排除 REVERSED / CANCELLED」
    --    这是本卡的验收核心。谓词比对先归一化：pg_indexes.indexdef 会把部分索引条件
    --    多套一层括号（`WHERE ((deleted_at IS NULL) AND ((status)::text <> ALL ...))`），
    --    所以空格和括号都去掉，只比「语义里确实出现了两个排除状态」。
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_period';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period 不是 UNIQUE 索引：%', v_def;
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()'']', '', 'g');
    IF v_norm NOT LIKE '%org_id,period_code%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period 的列组合不是 (org_id, period_code)（多组织那天不必换键）：%', v_def;
    END IF;
    IF v_norm NOT LIKE '%deleted_atISNULL%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period 缺软删条件：%', v_def;
    END IF;
    IF v_norm NOT LIKE '%REVERSED%' OR v_norm NOT LIKE '%CANCELLED%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period 未排除 REVERSED/CANCELLED —— 红冲后该期间永远无法重新结算（死单，设计附录 A 真错误 1）：%', v_def;
    END IF;

    -- A3 uk_biz_cost_period_no / uk_biz_cost_item：必须 UNIQUE
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_period_no';
    IF v_def IS NULL OR v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period_no 不存在或不是 UNIQUE：%', coalesce(v_def, '(缺)');
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%WHEREdeleted_atISNULL%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_period_no 缺软删条件（红冲后重建批次会撞批次号）：%', v_def;
    END IF;

    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_item';
    IF v_def IS NULL OR v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_item 不存在或不是 UNIQUE：%', coalesce(v_def, '(缺)');
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%period_id,cost_object_id%' THEN
        RAISE EXCEPTION 'V0373 自检失败：uk_biz_cost_item 的列组合不是 (period_id, cost_object_id)：%', v_def;
    END IF;

    -- A4 CHECK 约束按名单点名核对（漏一个 = 一条脏数据入口）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_period'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_period_code','ck_biz_cost_period_status','ck_biz_cost_period_labor',
                       'ck_biz_cost_period_sum','ck_biz_cost_period_cons','ck_biz_cost_period_appr',
                       'ck_biz_cost_period_settle','ck_biz_cost_period_rev','ck_biz_cost_period_cancel');
    IF v_n <> 9 THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period 的 9 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_period_item'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_item_status','ck_biz_cost_item_equiv','ck_biz_cost_item_units',
                       'ck_biz_cost_item_money','ck_biz_cost_item_pct','ck_biz_cost_item_cons',
                       'ck_biz_cost_item_settled');
    IF v_n <> 7 THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period_item 的 7 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    -- A5 外键：恰好 2 条（period_item → period 用 CASCADE、→ cost_object 用 RESTRICT）；
    --    biz_cost_period 自己 0 条外键（审批/凭证/组织全是弱引用）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_period'::regclass;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period 上有 % 条外键（审批/凭证/组织必须弱引用，设计 §3.4）', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_period_item'::regclass;
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period_item 的外键应为 2 条（period / cost_object），实际 %', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_period_item'::regclass
       AND confrelid = 'biz_cost_period'::regclass AND confdeltype = 'c';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0373 自检失败：period_item.period_id → biz_cost_period 必须是 ON DELETE CASCADE（删批次会留下孤儿计算单）';
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_period_item'::regclass
       AND confrelid = 'biz_cost_object'::regclass AND confdeltype = 'r';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0373 自检失败：period_item.cost_object_id → biz_cost_object 必须是 ON DELETE RESTRICT（删对象会带走计算单）';
    END IF;

    -- A6 金额 / 精度：设计 §3.1 全篇金额数量 NUMERIC(18,4)、单价费率 NUMERIC(18,6)
    FOR v_col, v_want IN
        SELECT t.col, t.want FROM (VALUES
            ('biz_cost_period.total_cost',              'numeric(18,4)'),
            ('biz_cost_period.total_finished',          'numeric(18,4)'),
            ('biz_cost_period_item.wip_beg_cost',       'numeric(18,4)'),
            ('biz_cost_period_item.finished_cost',      'numeric(18,4)'),
            ('biz_cost_period_item.wip_end_cost',       'numeric(18,4)'),
            ('biz_cost_period_item.equiv_conversion',   'numeric(18,4)'),
            ('biz_cost_period_item.completion_pct',     'numeric(5,2)'),
            ('biz_cost_period_item.unit_material',      'numeric(18,6)'),
            ('biz_cost_period_item.unit_finished_cost', 'numeric(18,6)')
        ) AS t(col, want)
    LOOP
        SELECT format_type(a.atttypid, a.atttypmod) INTO v_fmt
          FROM pg_attribute a
         WHERE a.attrelid = split_part(v_col, '.', 1)::regclass
           AND a.attname  = split_part(v_col, '.', 2)
           AND NOT a.attisdropped;
        IF v_fmt IS DISTINCT FROM v_want THEN
            RAISE EXCEPTION 'V0373 自检失败：% 的类型是 %，设计要求 %（4 位尾差倒挤，scale 掉到 2 会把差额放到分）',
                v_col, coalesce(v_fmt, '(缺列)'), v_want;
        END IF;
    END LOOP;

    -- A7 org_id 哨兵默认值（D-18：多组织落地前所有行同组织，行为=period_code 单列唯一）
    SELECT pg_get_expr(d.adbin, d.adrelid) INTO v_def
      FROM pg_attrdef d
      JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
     WHERE d.adrelid = 'biz_cost_period'::regclass AND a.attname = 'org_id';
    IF v_def IS NULL OR v_def NOT LIKE '%00000000-0000-0000-0000-000000000000%' THEN
        RAISE EXCEPTION 'V0373 自检失败：biz_cost_period.org_id 的默认值不是全 0 哨兵 uuid（实际 %）—— 唯一键 (org_id, period_code) 依赖它', coalesce(v_def, '(无默认值)');
    END IF;

    RAISE NOTICE 'V0373 形态断言通过（★ 唯一键已排除 REVERSED / CANCELLED）';
END $$;

-- ----------------------------------------------------------------------------
-- ② 行为自检：把每条关键约束的**反面**真跑一遍
--
--    ★ 本块 B1~B4 就是卡面验收「插入两条同期间记录，一条 REVERSED 一条 SETTLED → 不冲突」
--      的**可执行版本**（部署即验，不靠人工记得跑）。
--      同时反向验证「同期间两张活跃批次必被拒」——只放行红冲行，不放行双单。
--
--    探针键：成本对象 SELFCHECK-B3-PD / 批次号 SELFCHECK-B3-CS-* /
--            邀请实例与凭证用全 0 占位 uuid（弱引用，不校验父行存在）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_obj   UUID := '00000000-0000-0000-0000-0000000b0375';
    v_dept  UUID := '00000000-0000-0000-0000-0000000b0376';
    v_appr  UUID := '00000000-0000-0000-0000-0000000b03a1';
    v_vch   UUID := '00000000-0000-0000-0000-0000000b03a2';
    v_p1    UUID;   -- 2026-01 的 REVERSED 行
    v_p2    UUID;   -- 2026-01 的 SETTLED 行
    v_p3    UUID;   -- 2026-01 的 DRAFT 行（软删用）
    v_n     INT;
    v_hit   BOOLEAN;
BEGIN
    -- ===== 准备：清掉上次残留的探针行（先删计算单，再删批次，最后删父对象） =====
    DELETE FROM biz_cost_period_item WHERE cost_object_id = v_obj;
    DELETE FROM biz_cost_period      WHERE batch_no LIKE 'SELFCHECK-B3-%';
    DELETE FROM biz_cost_object      WHERE id = v_obj;

    INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date)
    VALUES (v_obj, 'SELFCHECK-B3-PD', gen_random_uuid(), gen_random_uuid(), 10,
            v_dept, '自检车间', DATE '2026-01-01');

    -- ===== B1 插一条 REVERSED（2026-01，红冲留痕行） =====
    INSERT INTO biz_cost_period (period_code, batch_no, status, approval_instance_id,
                                 settled_at, finish_voucher_id, reversed_at, reverse_reason, remark)
    VALUES ('2026-01', 'SELFCHECK-B3-CS-0101', 'REVERSED', v_appr,
            now(), v_vch, now(), '自检红冲', 'V0373-SELFCHECK')
    RETURNING id INTO v_p1;

    -- ===== B2 ★ 同期间再插一条 SETTLED —— 必须成功（REVERSED 不占位） =====
    INSERT INTO biz_cost_period (period_code, batch_no, status, approval_instance_id,
                                 settled_at, finish_voucher_id, remark)
    VALUES ('2026-01', 'SELFCHECK-B3-CS-0102', 'SETTLED', v_appr,
            now(), v_vch, 'V0373-SELFCHECK')
    RETURNING id INTO v_p2;

    SELECT count(*) INTO v_n FROM biz_cost_period
     WHERE period_code = '2026-01' AND batch_no LIKE 'SELFCHECK-B3-%';
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0373 自检失败：同期间一条 REVERSED + 一条 SETTLED 应共存（期望 2 行，实际 %）', v_n;
    END IF;

    -- ===== B3 同期间第二条**活跃**批次必须被拒（否则就是双单） =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, remark)
        VALUES ('2026-01', 'SELFCHECK-B3-CS-0103', 'DRAFT', 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：同期间第二名活跃批次写入成功（uk_biz_cost_period 未生效 ⇒ 同月双结算）';
    END IF;

    -- ===== B4 同期间第二条 REVERSED 允许（红冲留痕可以多条，设计附录 A 断言⑦） =====
    INSERT INTO biz_cost_period (period_code, batch_no, status, approval_instance_id,
                                 settled_at, finish_voucher_id, reversed_at, reverse_reason, remark)
    VALUES ('2026-01', 'SELFCHECK-B3-CS-0104', 'REVERSED', v_appr,
            now(), v_vch, now(), '自检二次红冲', 'V0373-SELFCHECK');

    SELECT count(*) INTO v_n FROM biz_cost_period
     WHERE period_code = '2026-01' AND status = 'REVERSED' AND batch_no LIKE 'SELFCHECK-B3-%';
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0373 自检失败：同期间两条 REVERSED 留痕应共存（期望 2 行，实际 %）', v_n;
    END IF;

    -- ===== B5 CANCELLED 同样不占位：作废后可以重开同期间批次 =====
    INSERT INTO biz_cost_period (period_code, batch_no, status, cancel_reason, remark)
    VALUES ('2026-02', 'SELFCHECK-B3-CS-0201', 'CANCELLED', '自检作废', 'V0373-SELFCHECK');
    INSERT INTO biz_cost_period (period_code, batch_no, status, remark)
    VALUES ('2026-02', 'SELFCHECK-B3-CS-0202', 'DRAFT', 'V0373-SELFCHECK');

    SELECT count(*) INTO v_n FROM biz_cost_period
     WHERE period_code = '2026-02' AND batch_no LIKE 'SELFCHECK-B3-%';
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0373 自检失败：CANCELLED 后重开同期间批次失败（期望 2 行，实际 %）', v_n;
    END IF;

    -- ===== B6 软删行不占位：删了当月草稿可以再建（且 batch_no 可复用） =====
    INSERT INTO biz_cost_period (id, period_code, batch_no, status, remark)
    VALUES ('00000000-0000-0000-0000-0000000b0377', '2026-03', 'SELFCHECK-B3-CS-0301', 'DRAFT',
            'V0373-SELFCHECK')
    RETURNING id INTO v_p3;
    UPDATE biz_cost_period SET deleted_at = now() WHERE id = v_p3;
    INSERT INTO biz_cost_period (period_code, batch_no, status, remark)
    VALUES ('2026-03', 'SELFCHECK-B3-CS-0301', 'DRAFT', 'V0373-SELFCHECK');

    SELECT count(*) INTO v_n FROM biz_cost_period
     WHERE period_code = '2026-03' AND batch_no = 'SELFCHECK-B3-CS-0301' AND deleted_at IS NULL;
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0373 自检失败：软删后同期间同批次号重建失败（期望 1 条活行，实际 %）', v_n;
    END IF;

    -- ===== B7 状态机 / 状态关联 CHECK 的四面反证 =====
    -- ① 状态值域
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, remark)
        VALUES ('2026-04', 'SELFCHECK-B3-CS-0401', 'FINISHED', 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：status=FINISHED 写入成功（ck_biz_cost_period_status 未生效）';
    END IF;

    -- ② SETTLED 没 settled_at / finish_voucher_id（「说结算了但没凭证」的死单）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, approval_instance_id, remark)
        VALUES ('2026-04', 'SELFCHECK-B3-CS-0402', 'SETTLED', v_appr, 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：SETTLED 不带 settled_at / finish_voucher_id 写入成功（ck_biz_cost_period_settle 未生效 ⇒ 假结算死单）';
    END IF;

    -- ③ APPROVED 之后必须挂审批实例（不许绕过审批流改状态）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, remark)
        VALUES ('2026-04', 'SELFCHECK-B3-CS-0403', 'APPROVED', 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：APPROVED 没有审批实例就写入成功（ck_biz_cost_period_appr 未生效 ⇒ 可绕过审批流）';
    END IF;

    -- ④ REVERSED 没有红冲原因（红冲要留痕）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, approval_instance_id,
                                     settled_at, finish_voucher_id, reversed_at, remark)
        VALUES ('2026-04', 'SELFCHECK-B3-CS-0404', 'REVERSED', v_appr,
                now(), v_vch, now(), 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：REVERSED 不带 reverse_reason 写入成功（ck_biz_cost_period_rev 未生效）';
    END IF;

    -- ⑤ 合计口径：total_cost ≠ 料+工+费
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, total_material, total_cost, remark)
        VALUES ('2026-04', 'SELFCHECK-B3-CS-0405', 'DRAFT', 50, 100, 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：total_cost ≠ 料+工+费 写入成功（ck_biz_cost_period_sum 未生效）';
    END IF;

    -- ⑥ 合计口径：total_cost ≠ 完工 + 在制
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, total_cost, total_finished, remark)
        VALUES ('2026-04', 'SELFCHECK-B3-CS-0406', 'DRAFT', 100, 90, 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：total_cost ≠ 完工+在制 写入成功（ck_biz_cost_period_cons 未生效 ⇒ 期间成本不守恒）';
    END IF;

    -- ⑦ 期间格式 yyyy-MM
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period (period_code, batch_no, status, remark)
        VALUES ('2026-13', 'SELFCHECK-B3-CS-0407', 'DRAFT', 'V0373-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：period_code=2026-13 写入成功（ck_biz_cost_period_code 未生效）';
    END IF;

    -- ===== B8 计算单：唯一键 / 成本守恒 / 结算必挂入库单 / 约当进度值域 =====
    -- 正面：料 100 + 工 60 + 费 40 = 200，完工 150 + 期末在制 50 = 200 ⇒ 守恒
    INSERT INTO biz_cost_period_item (period_id, cost_object_id, product_id, completed_qty,
                                      wip_beg_cost, mat_cur, labor_cur, oh_cur,
                                      finished_cost, wip_end_cost, completion_pct,
                                      equiv_material, equiv_conversion,
                                      unit_material, unit_labor, unit_overhead, unit_finished_cost)
    VALUES (v_p2, v_obj, gen_random_uuid(), 10,
            0, 100, 60, 40, 150, 50, 50, 12, 10, 10, 6, 4, 12.5);

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period_item (period_id, cost_object_id, product_id)
        VALUES (v_p2, v_obj, gen_random_uuid());
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：同批次同成本对象第二张计算单写入成功（uk_biz_cost_item 未生效 ⇒ 重算会翻倍）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period_item (period_id, cost_object_id, product_id,
                                          wip_beg_cost, mat_cur, labor_cur, oh_cur,
                                          finished_cost, wip_end_cost)
        VALUES (v_p1, v_obj, gen_random_uuid(), 0, 100, 60, 40, 150, 40);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：完工+在制 ≠ 期初+投入 写入成功（ck_biz_cost_item_cons 未生效 ⇒ 约当产量算错也不拦）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period_item (period_id, cost_object_id, product_id, status)
        VALUES (v_p1, v_obj, gen_random_uuid(), 'SETTLED');
        v_hit := true;
    EXCEPTION WHEN unique_violation OR check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：SETTLED 计算单不带 stock_in_id 写入成功（ck_biz_cost_item_settled 未生效 ⇒ 假入库）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_period_item (period_id, cost_object_id, product_id, completion_pct)
        VALUES (v_p1, v_obj, gen_random_uuid(), 150);
        v_hit := true;
    EXCEPTION WHEN unique_violation OR check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：completion_pct=150 写入成功（ck_biz_cost_item_pct 未生效）';
    END IF;

    -- ===== B9 外键删除动作：删批次级联删明细；有明细时删成本对象被 RESTRICT 拦 =====
    v_hit := false;
    BEGIN
        DELETE FROM biz_cost_object WHERE id = v_obj;
        v_hit := true;
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0373 自检失败：有计算单时成本对象被删掉（外键不是 ON DELETE RESTRICT）';
    END IF;

    -- 删掉 2026-01 的四条批次（含挂计算单的那条）⇒ 计算单必须跟着 CASCADE 消失
    DELETE FROM biz_cost_period WHERE period_code = '2026-01' AND batch_no LIKE 'SELFCHECK-B3-%';
    SELECT count(*) INTO v_n FROM biz_cost_period_item WHERE cost_object_id = v_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0373 自检失败：批次删除后计算单残留 % 行（period_id 外键不是 ON DELETE CASCADE）', v_n;
    END IF;

    -- ===== 清理探针（重复执行零残留） =====
    DELETE FROM biz_cost_period_item WHERE cost_object_id = v_obj;
    DELETE FROM biz_cost_period      WHERE batch_no LIKE 'SELFCHECK-B3-%';
    DELETE FROM biz_cost_object      WHERE id = v_obj;

    SELECT count(*) INTO v_n FROM biz_cost_period_item WHERE cost_object_id = v_obj;
    SELECT v_n + count(*) INTO v_n FROM biz_cost_period WHERE batch_no LIKE 'SELFCHECK-B3-%';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_object WHERE id = v_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0373 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0373 完成：期间两表建成，形态断言 + 9 组行为断言全过（★ 同期间 REVERSED + SETTLED 共存、活跃批次唯一），探针已清理';
END $$;
