-- ============================================================================
-- V0371：成本核算升级 · 归集四表（ARCH-9 §3.1 ③④⑤⑥）+ 自检
--
-- 卡：t_76333a39（COST-B1-2【成本】迁移）—— 拆卡清单 §8.1 的 B1-2「归集四表 + 自检」
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.1 ③④⑤⑥
--       （该节 DDL 已在真库跑通并 ROLLBACK，见
--         docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql 与 .out）
--
-- 本卡范围：**只有**归集链的 4 张表 + 索引 / CHECK / 注释 + 末尾自检：
--     ③ biz_cost_collect   成本归集明细（料 / 工 / 费三路写同一张表，靠 element 分流）
--     ④ biz_cost_labor     工时台账（工时 × 工资率 = 人工成本）
--     ⑤ biz_cost_overhead  手工制造费用台账（折旧 / 报销 / 手工）
--     ⑥ biz_cost_rate      工资率 / 机器费率（按 对象 > 车间 > 公司 取数）
--   分摊两表（⑦⑧）与期间两表（⑨⑩）属 B1-3（V0372/V0373），二期三表属 B1-4，
--   出库/入库加列属 B1-5 —— 一行都不在本文件里。
--
-- # 号段为什么是 V0371，而不是卡面写的 V0369
--   卡面标题与设计稿 §8.1 写 B1-2=V0369，但该号**已经被别的卡用掉**：
--     V0368__receipt_refund_page_and_price_page（收款退款页 + 价格页）
--     V0369__perf_list_order_indexes（N-8 性能基线排序索引）
--     V0370__cost_lot_and_cost_object（B1-1，本卡的前置）
--   nik 迁移加载器对版本号做**跨目录全局唯一**校验
--   （backend/vendor/nik-db/src/migrate/loader.rs::load_from_sources「版本号跨目录全局唯一」），
--   重号会让服务启动直接报错；AGENTS 约定 6 也要求动手前先 `ls backend/migrations/*/` 核对号段。
--   故顺延到下一个空号 V0371（B1-3 起为 V0372/V0373/V0374）。
--
-- # 「五张表」还是「四张表」（卡面验收项与设计稿的差异，已实测核对）
--   卡面验收写「五张表结构符合 §3」，但 §8.1 把 B1-2 定义为「归集四表 + 自检」，
--   §3.1 里属于本卡的正好是 ③④⑤⑥ 四节；第 5 张 biz_cost_alloc_rule 是 §3.1 ⑦，
--   归 B1-3（卡 t_7efea068 的正文写「分摊两表 + 期间两表」）。
--   按「设计已定稿、只做本卡写的事」⇒ 本迁移建 **4** 张表；B1-3 再建 ⑦⑧⑨⑩。
--
-- # 金额 / 数量列的精度（卡面验收项「金额 numeric(20,2)」与设计稿的冲突，按设计稿落地）
--   设计稿 §3.1 全篇用 NUMERIC(18,4) 存金额与数量、NUMERIC(18,6) 存单价/费率，
--   卡面验收却写「金额列 numeric(20,2)、数量 numeric(20,4)」。两者只能二选一，实测结论：
--     · 设计稿的 CHECK 依赖 4 位小数：ck_biz_cost_labor_amount
--       `amount = round(hours * rate, 4)`；rate 本身是 NUMERIC(18,6)（月薪 ÷ 工时的商）。
--       若 amount 降成 scale=2，则 hours=1、rate=33.333333 这类**真实工资率**算出的
--       amount=33.3333 落不进 2 位小数列，CHECK 恒假 ⇒ 工时行根本插不进去（本卡自检与
--       演练脚本里已把这条反证跑给你看：02_up_down_drill.out 第 6 步）。
--     · 约当产量 / 分摊尾差（§4.4.4、§4.6.2）全部按 4 位小数倒挤，scale=2 会把尾差放大到分。
--     · 全仓口径：backend/migrations 里 210 处 NUMERIC(18,4)、0 处 numeric(20,2)（列定义），
--       前置卡 V0370 的 plan_qty / completed_qty / wip_qty 也是 NUMERIC(18,4)。
--   ⇒ 本迁移**逐字采用 §3.1**（金额、数量 18,4；单价、费率 18,6），并把精度写进自检
--     （format_type 断言），偏差已在卡内汇报里具名列出。
--
-- # 幂等约定
--   表 / 索引一律 IF NOT EXISTS；注释走 COMMENT ON（重复执行结果不变）；
--   自检先用固定探针键**删一遍再用**，断言后立即清理 ⇒ 重复执行零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：biz_cost_object 不在就直说「先回滚/先跑哪张卡」
--    否则 PG 只会抛 `relation "biz_cost_object" does not exist`，看不出依赖关系。
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0371 拒绝执行：biz_cost_object 不存在，请先执行 V0370（COST-B1-1）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- ③ biz_cost_collect —— 成本归集明细
--    料 / 工 / 费三路都往这张表写，靠 element 分流；cost_object_id 为空表示
--    「车间费用池」（只有折旧 / 报销 / 手工费用这三类来源允许空，等分摊后再落到对象）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_collect (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cost_object_id UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,  -- 空=车间费用池（待分摊）
    period_code    VARCHAR(7)    NOT NULL,                           -- 归属会计期间 yyyy-MM
    element        VARCHAR(12)   NOT NULL,                           -- MATERIAL / LABOR / OVERHEAD
    source_type    VARCHAR(16)   NOT NULL,                           -- STOCK_OUT/LABOR_TICKET/OVERHEAD_TICKET/DEPRECIATION/EXPENSE/ALLOC/MANUAL
    source_id      UUID,                                             -- 来源行 id（MANUAL 为空）
    source_no      VARCHAR(32),                                      -- 来源单号快照
    line_no        INT           NOT NULL DEFAULT 0,                 -- 来源行号（幂等键组成）
    occur_date     DATE          NOT NULL,
    material_id    UUID,                                             -- 料行物料（弱引用）
    qty            NUMERIC(18,4),                                    -- 料行数量
    unit_price     NUMERIC(18,6),                                    -- 料行单价（移动加权出库单价快照）
    amount         NUMERIC(18,4) NOT NULL,                           -- 归集金额（不含税）
    dept_id        UUID,                                             -- 费行归属车间
    dept_name      VARCHAR(120),                                     -- 车间名快照
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_collect_element CHECK (element IN ('MATERIAL','LABOR','OVERHEAD')),
    CONSTRAINT ck_biz_cost_collect_src     CHECK (source_type IN
        ('STOCK_OUT','LABOR_TICKET','OVERHEAD_TICKET','DEPRECIATION','EXPENSE','ALLOC','MANUAL')),
    CONSTRAINT ck_biz_cost_collect_amount  CHECK (amount >= 0),
    CONSTRAINT ck_biz_cost_collect_qty     CHECK (qty IS NULL OR qty >= 0),
    CONSTRAINT ck_biz_cost_collect_period  CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    -- 料行必须有物料与数量；工/费行不许挂物料（防止「一张表两种语义」）
    CONSTRAINT ck_biz_cost_collect_mat     CHECK (
        (element = 'MATERIAL' AND material_id IS NOT NULL AND qty IS NOT NULL)
     OR (element <> 'MATERIAL' AND material_id IS NULL AND qty IS NULL)),
    -- 归属约束：只有「车间费用池」三类来源允许不挂成本对象（分摊前），其余必须挂
    CONSTRAINT ck_biz_cost_collect_obj     CHECK (
        cost_object_id IS NOT NULL
     OR (element = 'OVERHEAD' AND source_type IN ('DEPRECIATION','EXPENSE','OVERHEAD_TICKET')))
);

COMMENT ON TABLE  biz_cost_collect IS '成本归集明细：料/工/费三路归集的唯一落点；cost_object_id 为空=车间费用池（待分摊）';
COMMENT ON COLUMN biz_cost_collect.cost_object_id IS '成本对象（物理 FK，ON DELETE RESTRICT）：为空仅允许折旧/报销/手工费用三类池行';
COMMENT ON COLUMN biz_cost_collect.period_code IS '归属会计期间 yyyy-MM（按过账期间归集，见设计 §4.10 跨期补录口径）';
COMMENT ON COLUMN biz_cost_collect.element IS '要素：MATERIAL 料 / LABOR 工 / OVERHEAD 费';
COMMENT ON COLUMN biz_cost_collect.source_type IS '来源：STOCK_OUT 领料 / LABOR_TICKET 工时 / OVERHEAD_TICKET 费用 / DEPRECIATION 折旧 / EXPENSE 报销 / ALLOC 分摊转入 / MANUAL 手工';
COMMENT ON COLUMN biz_cost_collect.source_no IS '来源单号快照：历史/审计口径，按 AGENTS 约定 13 存快照而不 join';
COMMENT ON COLUMN biz_cost_collect.unit_price IS '料行单价快照（移动加权出库单价）：物料改名/调价后批成本仍可追溯';
COMMENT ON COLUMN biz_cost_collect.dept_name IS '车间名快照：池行按车间归集，车间改名后仍能回溯归属';

-- 幂等键：同一来源行只能归集一次（重算靠 DELETE 本期间再插，不靠 upsert）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_collect_src
    ON biz_cost_collect (source_type, source_id, line_no)
    WHERE deleted_at IS NULL AND source_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_collect_obj    ON biz_cost_collect (cost_object_id, period_code, element);
CREATE INDEX IF NOT EXISTS idx_biz_cost_collect_period ON biz_cost_collect (period_code);

-- ----------------------------------------------------------------------------
-- ④ biz_cost_labor —— 工时台账
--    没有 MES 时人工成本的唯一来源（设计 §4.5：导入模板 8 列 / 手工录 MES）。
--    结算是只取 status='CONFIRMED' 的行（§4.5.2 硬规则 2）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_labor (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code    VARCHAR(7)    NOT NULL,
    work_date      DATE          NOT NULL,
    cost_object_id UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    employee_id    UUID,                                             -- 弱引用 biz_employee.id（ARCH9-2）
    employee_name  VARCHAR(50)   NOT NULL,                           -- 姓名快照
    dept_id        UUID          NOT NULL,                           -- 工作中心
    dept_name      VARCHAR(120)  NOT NULL,                           -- 工作中心名快照
    hours          NUMERIC(10,2) NOT NULL,                           -- 人工工时
    machine_hours  NUMERIC(10,2) NOT NULL DEFAULT 0,                  -- 机器工时（另一分摊基数）
    rate           NUMERIC(18,6) NOT NULL,                           -- 工资率快照（元/小时）
    amount         NUMERIC(18,4) NOT NULL,                           -- = round(hours × rate, 4)
    source_type    VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',          -- MANUAL / IMPORT / MES
    status         VARCHAR(12)   NOT NULL DEFAULT 'DRAFT',           -- DRAFT / CONFIRMED（结算只取 CONFIRMED）
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_labor_hours  CHECK (hours > 0 AND machine_hours >= 0),
    CONSTRAINT ck_biz_cost_labor_rate   CHECK (rate >= 0),
    CONSTRAINT ck_biz_cost_labor_src    CHECK (source_type IN ('MANUAL','IMPORT','MES')),
    CONSTRAINT ck_biz_cost_labor_status CHECK (status IN ('DRAFT','CONFIRMED')),
    CONSTRAINT ck_biz_cost_labor_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    -- 金额必须等于工时×工资率（4 位四舍五入）：堵「改工时忘改金额」
    CONSTRAINT ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))
);

COMMENT ON TABLE  biz_cost_labor IS '工时台账：人工成本的唯一来源（无 MES 时靠导入/手工）；结算只取 status=CONFIRMED';
COMMENT ON COLUMN biz_cost_labor.employee_name IS '姓名快照：成本记录属审计/历史口径（AGENTS 约定 13），不是 join 实时名';
COMMENT ON COLUMN biz_cost_labor.dept_name IS '工作中心名快照：车间改名后历史工时仍能回溯';
COMMENT ON COLUMN biz_cost_labor.rate IS '工资率快照（元/小时）：取数优先级 成本对象 > 车间 > 公司，见 biz_cost_rate 与设计 §4.5.4';
COMMENT ON COLUMN biz_cost_labor.status IS 'DRAFT 可改 / CONFIRMED 只读；已结算期间的归集行只读，改数必须走红冲（设计 R-8）';

CREATE INDEX IF NOT EXISTS idx_biz_cost_labor_obj    ON biz_cost_labor (cost_object_id, period_code);
CREATE INDEX IF NOT EXISTS idx_biz_cost_labor_emp    ON biz_cost_labor (employee_id, work_date);
CREATE INDEX IF NOT EXISTS idx_biz_cost_labor_period ON biz_cost_labor (period_code, status);

-- ----------------------------------------------------------------------------
-- ⑤ biz_cost_overhead —— 手工制造费用台账
--    cost_object_id 为空 = 车间级（进 4101 费用池待分摊）；非空 = 直接计入该对象。
--    debit_account_code 必须落在 4101.*（制造费用科目），否则费用进不了成本（风险 R-2）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_overhead (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code         VARCHAR(7)    NOT NULL,
    cost_object_id      UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,  -- 空=车间级
    dept_id             UUID          NOT NULL,
    dept_name           VARCHAR(120)  NOT NULL,
    expense_item        VARCHAR(16)   NOT NULL,                      -- 字典 cost_expense_item
    debit_account_code  VARCHAR(30)   NOT NULL DEFAULT '4101.09',
    credit_account_code VARCHAR(30)   NOT NULL,                      -- 对方科目（1002/2241.xx/1801…）
    amount              NUMERIC(18,4) NOT NULL,
    source_type         VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',     -- MANUAL / IMPORT
    remark              VARCHAR(255),
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_oh_item   CHECK (expense_item IN
        ('DEPRECIATION','UTILITY','RENT','REPAIR','WORKSHOP_ADMIN','CONSUMABLE','OTHER')),
    CONSTRAINT ck_biz_cost_oh_src    CHECK (source_type IN ('MANUAL','IMPORT')),
    CONSTRAINT ck_biz_cost_oh_amount CHECK (amount > 0),
    CONSTRAINT ck_biz_cost_oh_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_oh_debit  CHECK (debit_account_code LIKE '4101.%')
);

COMMENT ON TABLE  biz_cost_overhead IS '手工制造费用台账：折旧/水电/房租/修理/车间管理/易耗/其他；空对象=车间级费用池';
COMMENT ON COLUMN biz_cost_overhead.cost_object_id IS '成本对象（物理 FK，ON DELETE RESTRICT）：为空表示车间级，进 4101 池等分摊';
COMMENT ON COLUMN biz_cost_overhead.debit_account_code IS '借方科目，必须 4101.*（制造费用末级）；配成 5602 会让费用永远进不了成本（风险 R-2）';
COMMENT ON COLUMN biz_cost_overhead.credit_account_code IS '对方科目（1002 银行 / 2241.xx 应付 / 1801 累计折旧 …）';
COMMENT ON COLUMN biz_cost_overhead.dept_name IS '车间名快照：审计口径';

CREATE INDEX IF NOT EXISTS idx_biz_cost_oh_period ON biz_cost_overhead (period_code, dept_id);
CREATE INDEX IF NOT EXISTS idx_biz_cost_oh_obj    ON biz_cost_overhead (cost_object_id) WHERE cost_object_id IS NOT NULL;

-- ----------------------------------------------------------------------------
-- ⑥ biz_cost_rate —— 工资率 / 机器费率（版本化生效期）
--    lookup 顺序：COST_OBJECT > DEPT > COMPANY（设计 §4.5.4）；
--    同一 (类型, 范围, 范围 id, 生效起) 只允许一条活行 —— 同日两个费率会让取数随机。
--    注意 scope_id 是**弱引用**（可能是 dept / cost_object，两种目标），刻意不建 FK。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_rate (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rate_type      VARCHAR(12)   NOT NULL,                           -- LABOR / MACHINE
    scope          VARCHAR(12)   NOT NULL,                           -- COMPANY / DEPT / COST_OBJECT
    scope_id       UUID,                                             -- COMPANY 为空
    rate           NUMERIC(18,6) NOT NULL,
    effective_from DATE          NOT NULL,
    effective_to   DATE,
    status         SMALLINT      NOT NULL DEFAULT 1,
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_rate_type   CHECK (rate_type IN ('LABOR','MACHINE')),
    CONSTRAINT ck_biz_cost_rate_scope  CHECK (scope IN ('COMPANY','DEPT','COST_OBJECT')),
    CONSTRAINT ck_biz_cost_rate_rate   CHECK (rate >= 0),
    CONSTRAINT ck_biz_cost_rate_status CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_cost_rate_dates  CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_rate_scope_id CHECK (
        (scope = 'COMPANY' AND scope_id IS NULL) OR (scope <> 'COMPANY' AND scope_id IS NOT NULL))
);

COMMENT ON TABLE  biz_cost_rate IS '工资率 / 机器费率（版本化生效期）：取数优先级 成本对象 > 车间 > 公司';
COMMENT ON COLUMN biz_cost_rate.scope IS '作用范围：COMPANY 全公司 / DEPT 车间 / COST_OBJECT 成本对象';
COMMENT ON COLUMN biz_cost_rate.scope_id IS '范围 id（弱引用：DEPT→sys_dept.id，COST_OBJECT→biz_cost_object.id，COMPANY 为空；刻意不建 FK）';
COMMENT ON COLUMN biz_cost_rate.rate IS '费率（元/小时）：工资率 6 位小数（月薪÷工时必然除不尽，2 位会算错人工成本）';

-- 同日同范围只许一条活行；软删后可以重配
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_rate
    ON biz_cost_rate (rate_type, scope, coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid), effective_from)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_rate_lookup ON biz_cost_rate (rate_type, status, effective_from DESC);

-- ----------------------------------------------------------------------------
-- ⑦ 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    分两类断言：
--      A. 形态：表 / 唯一索引 / CHECK 数量 / 外键动作 / 金额列精度
--         —— 防「唯一键漏软删条件」「FK 写成 CASCADE」「amount 写成 scale=2」这类
--         不会报错、只会以后算错账（或静默丢行）的偏差。
--      B. 行为：真插数据，把每条关键约束的反面跑一遍（重号、料行缺物料、
--         池行挂错来源、金额≠工时×费率、科目不在 4101、费率范围错配、
--         父对象被 RESTRICT 拦下）。探针行断言后立即清理，重复执行零残留。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_def  TEXT;
    v_norm TEXT;
    v_want TEXT;
    v_n    INT;
    v_fmt  TEXT;
BEGIN
    -- ===== A. 形态 =====

    -- A1 四张表都在
    FOREACH v_def IN ARRAY ARRAY['biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate']
    LOOP
        IF to_regclass('public.' || v_def) IS NULL THEN
            RAISE EXCEPTION 'V0371 自检失败：% 未建出来', v_def;
        END IF;
    END LOOP;

    -- A2 唯一索引必须是「UNIQUE + 带软删条件」的部分索引
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_collect_src';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0371 自检失败：uk_biz_cost_collect_src 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0371 自检失败：uk_biz_cost_collect_src 不是 UNIQUE 索引：%', v_def;
    END IF;
    -- 谓词比对先归一化：pg 会把部分索引条件渲染成
    --   `WHERE ((deleted_at IS NULL) AND (source_id IS NOT NULL))`（多一层括号），
    -- 所以空格和括号都去掉再比，只比「语义形状」。
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%WHEREdeleted_atISNULLANDsource_idISNOTNULL%' THEN
        RAISE EXCEPTION 'V0371 自检失败：uk_biz_cost_collect_src 缺「软删 + source_id 非空」谓词（MANUAL 行不许共享空 source_id 撞键）：%', v_def;
    END IF;

    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_rate';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0371 自检失败：uk_biz_cost_rate 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0371 自检失败：uk_biz_cost_rate 不是 UNIQUE 索引：%', v_def;
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%WHEREdeleted_atISNULL%' THEN
        RAISE EXCEPTION 'V0371 自检失败：uk_biz_cost_rate 缺软删条件（换费率时只能改行不能新配）：%', v_def;
    END IF;

    -- A3 CHECK 约束按名单点名核对（漏一个 = 一条脏数据入口）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_collect'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_collect_element','ck_biz_cost_collect_src','ck_biz_cost_collect_amount',
                       'ck_biz_cost_collect_qty','ck_biz_cost_collect_period','ck_biz_cost_collect_mat',
                       'ck_biz_cost_collect_obj');
    IF v_n <> 7 THEN
        RAISE EXCEPTION 'V0371 自检失败：biz_cost_collect 的 7 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_labor'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_labor_hours','ck_biz_cost_labor_rate','ck_biz_cost_labor_src',
                       'ck_biz_cost_labor_status','ck_biz_cost_labor_period','ck_biz_cost_labor_amount');
    IF v_n <> 6 THEN
        RAISE EXCEPTION 'V0371 自检失败：biz_cost_labor 的 6 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_overhead'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_oh_item','ck_biz_cost_oh_src','ck_biz_cost_oh_amount',
                       'ck_biz_cost_oh_period','ck_biz_cost_oh_debit');
    IF v_n <> 5 THEN
        RAISE EXCEPTION 'V0371 自检失败：biz_cost_overhead 的 5 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_rate'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_rate_type','ck_biz_cost_rate_scope','ck_biz_cost_rate_rate',
                       'ck_biz_cost_rate_status','ck_biz_cost_rate_dates','ck_biz_cost_rate_scope_id');
    IF v_n <> 6 THEN
        RAISE EXCEPTION 'V0371 自检失败：biz_cost_rate 的 6 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    -- A4 period_code 的 yyyy-MM 正则四条都在（期间是全部成本口径的分组键）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'c' AND conname IN ('ck_biz_cost_collect_period','ck_biz_cost_labor_period',
                                         'ck_biz_cost_oh_period') AND pg_get_constraintdef(oid) LIKE '%0[1-9]%';
    IF v_n <> 3 THEN
        RAISE EXCEPTION 'V0371 自检失败：period_code 的 yyyy-MM 正则 CHECK 只落库 % 条（应为 3：collect/labor/overhead）', v_n;
    END IF;
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'c' AND conname = 'ck_biz_cost_rate_dates' AND pg_get_constraintdef(oid) LIKE '%effective_to%';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0371 自检失败：ck_biz_cost_rate_dates 缺失（费率有效期会倒挂）';
    END IF;

    -- A5 外键：恰好 3 条，全部指向 biz_cost_object，且全部 ON DELETE RESTRICT（confdeltype='r'）
    SELECT count(*) INTO v_n FROM pg_constraint f
     WHERE f.contype = 'f'
       AND f.conrelid IN ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,'biz_cost_overhead'::regclass)
       AND f.confrelid = 'biz_cost_object'::regclass;
    IF v_n <> 3 THEN
        RAISE EXCEPTION 'V0371 自检失败：指向 biz_cost_object 的外键应有 3 条（collect/labor/overhead），实际 %', v_n;
    END IF;
    SELECT count(*) INTO v_n FROM pg_constraint f
     WHERE f.contype = 'f'
       AND f.conrelid IN ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,'biz_cost_overhead'::regclass)
       AND f.confdeltype <> 'r';
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0371 自检失败：有 % 条外键不是 ON DELETE RESTRICT（删成本对象会静默带走归集行）', v_n;
    END IF;
    -- 模块内不许出现别的物理外键（主数据 / 单据一律弱引用，设计 §3.4）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f'
       AND conrelid IN ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,
                        'biz_cost_overhead'::regclass,'biz_cost_rate'::regclass);
    IF v_n <> 3 THEN
        RAISE EXCEPTION 'V0371 自检失败：四张表上一共有 % 条外键（应为 3 条，全部指向 biz_cost_object）：主数据/单据必须弱引用', v_n;
    END IF;

    -- A6 金额 / 数量精度（这是卡内偏差的固化点，见文件头说明）
    --    金额、数量 NUMERIC(18,4)；单价、费率 NUMERIC(18,6)
    FOR v_def, v_want IN
        SELECT t.col, t.want FROM (VALUES
            ('biz_cost_collect.amount',        'numeric(18,4)'),
            ('biz_cost_collect.qty',           'numeric(18,4)'),
            ('biz_cost_collect.unit_price',    'numeric(18,6)'),
            ('biz_cost_labor.amount',          'numeric(18,4)'),
            ('biz_cost_labor.hours',           'numeric(10,2)'),
            ('biz_cost_labor.machine_hours',   'numeric(10,2)'),
            ('biz_cost_labor.rate',            'numeric(18,6)'),
            ('biz_cost_overhead.amount',       'numeric(18,4)'),
            ('biz_cost_rate.rate',             'numeric(18,6)')
        ) AS t(col, want)
    LOOP
        SELECT format_type(a.atttypid, a.atttypmod) INTO v_fmt
          FROM pg_attribute a
         WHERE a.attrelid = split_part(v_def, '.', 1)::regclass
           AND a.attname  = split_part(v_def, '.', 2)
           AND NOT a.attisdropped;
        IF v_fmt IS DISTINCT FROM v_want THEN
            RAISE EXCEPTION 'V0371 自检失败：% 的类型是 %，设计要求 %（金额/数量 scale 掉到 2 会让 round(…,4) 的 CHECK 恒假、工时行插不进去）',
                v_def, coalesce(v_fmt, '(缺列)'), v_want;
        END IF;
    END LOOP;

    RAISE NOTICE 'V0371 形态断言通过';
END $$;

-- ----------------------------------------------------------------------------
-- ⑧ 行为自检：把每条关键约束的**反面**真跑一遍
--
--    为什么非要做：形态对了不代表约束真在拦。`CHECK (amount = round(hours*rate,4))`
--    这种带四则运算的约束，写错一个括号就变成恒真（永远不拦），表结构照样「符合设计」。
--    这里每条断言都用一个期望失败的 INSERT / DELETE 反证。
--
--    探针键（固定 UUID，删干净再用）：
--      成本对象 SELFCHECK-B2-CB / 来源行 c05b3；所有探针行 remark='V0371-SELFCHECK'
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_obj  UUID := '00000000-0000-0000-0000-0000000c05b2';   -- 探针成本对象
    v_src  UUID := '00000000-0000-0000-0000-0000000c05b3';   -- 探针来源行 id
    v_mat  UUID := '00000000-0000-0000-0000-0000000c05b4';
    v_wh   UUID := '00000000-0000-0000-0000-0000000c05b5';
    v_dept UUID := '00000000-0000-0000-0000-0000000c05b6';
    v_n    INT;
    v_hit  BOOLEAN;
BEGIN
    -- ===== 准备：清掉上次残留的探针行，再建一个成本对象（外键要求父行真实存在） =====
    DELETE FROM biz_cost_collect  WHERE source_id = v_src OR remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_labor    WHERE remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_overhead WHERE remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_rate     WHERE remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_object   WHERE object_no = 'SELFCHECK-B2-CB';

    INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date)
    VALUES (v_obj, 'SELFCHECK-B2-CB', v_mat, v_wh, 10, v_dept, '自检车间', DATE '2026-01-01');

    -- ===== B1 归集幂等键：同 (source_type, source_id, line_no) 只能一条活行 =====
    INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type, source_id,
                                  source_no, line_no, occur_date, material_id, qty, unit_price, amount, remark)
    VALUES (v_obj, '2026-01', 'MATERIAL', 'STOCK_OUT', v_src,
            'SO-SELFCHECK', 1, DATE '2026-01-05', v_mat, 10, 12.5, 125, 'V0371-SELFCHECK');

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type, source_id,
                                      source_no, line_no, occur_date, material_id, qty, unit_price, amount, remark)
        VALUES (v_obj, '2026-01', 'MATERIAL', 'STOCK_OUT', v_src,
                'SO-SELFCHECK', 1, DATE '2026-01-05', v_mat, 10, 12.5, 125, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：同一来源行被归集两次竟然成功（uk_biz_cost_collect_src 未生效 ⇒ 重算会翻倍进成本）';
    END IF;

    -- ===== B2 软删行不占幂等键：作废后可以重新归集 =====
    UPDATE biz_cost_collect SET deleted_at = now()
     WHERE source_type = 'STOCK_OUT' AND source_id = v_src AND line_no = 1 AND deleted_at IS NULL;
    INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type, source_id,
                                  source_no, line_no, occur_date, material_id, qty, unit_price, amount, remark)
    VALUES (v_obj, '2026-01', 'MATERIAL', 'STOCK_OUT', v_src,
            'SO-SELFCHECK', 1, DATE '2026-01-05', v_mat, 10, 12.5, 125, 'V0371-SELFCHECK');
    SELECT count(*) INTO v_n FROM biz_cost_collect WHERE source_id = v_src;
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0371 自检失败：软删后重新归集没成功（期望 2 行，实际 %）', v_n;
    END IF;

    -- ===== B3 一张表两种语义：料行必须有物料+数量，工/费行不许挂物料 =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      occur_date, qty, amount, remark)
        VALUES (v_obj, '2026-01', 'MATERIAL', 'MANUAL', DATE '2026-01-05', 1, 10, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：料行不带 material_id 写入成功（ck_biz_cost_collect_mat 未生效）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      occur_date, material_id, qty, amount, remark)
        VALUES (v_obj, '2026-01', 'LABOR', 'LABOR_TICKET', DATE '2026-01-05', v_mat, 1, 10, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：工行挂了物料竟然写入成功（ck_biz_cost_collect_mat 未生效）';
    END IF;

    -- ===== B4 归属：只有折旧/报销/手工费用三类池行允许不挂对象 =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_collect (period_code, element, source_type,
                                      occur_date, material_id, qty, amount, remark)
        VALUES ('2026-01', 'MATERIAL', 'STOCK_OUT', DATE '2026-01-05', v_mat, 1, 10, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：领料行不挂成本对象竟然写入成功（ck_biz_cost_collect_obj 未生效 ⇒ 料成本会掉进无主池）';
    END IF;

    -- 正面：车间费用池（折旧来源）允许不挂对象、不带 source_id
    INSERT INTO biz_cost_collect (period_code, element, source_type, occur_date, amount, dept_id, remark)
    VALUES ('2026-01', 'OVERHEAD', 'DEPRECIATION', DATE '2026-01-31', 88, v_dept, 'V0371-SELFCHECK');

    -- ===== B5 期间格式：yyyy-MM，写错月份直接拒 =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type,
                                      occur_date, material_id, qty, amount, remark)
        VALUES (v_obj, '2026-13', 'MATERIAL', 'MANUAL', DATE '2026-01-05', v_mat, 1, 10, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：period_code=2026-13 写入成功（ck_biz_cost_collect_period 未生效）';
    END IF;

    -- ===== B6 工时金额必须等于 工时×费率（4 位四舍五入） =====
    INSERT INTO biz_cost_labor (period_code, work_date, cost_object_id, employee_name, dept_id, dept_name,
                                hours, machine_hours, rate, amount, status, remark)
    VALUES ('2026-01', DATE '2026-01-06', v_obj, '自检张三', v_dept, '自检车间',
            8, 2, 50, 400, 'CONFIRMED', 'V0371-SELFCHECK');

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_labor (period_code, work_date, cost_object_id, employee_name, dept_id, dept_name,
                                    hours, machine_hours, rate, amount, remark)
        VALUES ('2026-01', DATE '2026-01-06', v_obj, '自检李四', v_dept, '自检车间',
                8, 0, 50, 401, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：amount≠hours×rate 写入成功（ck_biz_cost_labor_amount 未生效 ⇒ 改工时忘改金额会静默算错人工成本）';
    END IF;

    -- 正面：真实工资率除不尽，amount 必须留 4 位小数（这正是 amount 不能用 scale=2 的原因）
    INSERT INTO biz_cost_labor (period_code, work_date, cost_object_id, employee_name, dept_id, dept_name,
                                hours, machine_hours, rate, amount, status, remark)
    VALUES ('2026-01', DATE '2026-01-07', v_obj, '自检王五', v_dept, '自检车间',
            1, 0, 33.333333, 33.3333, 'DRAFT', 'V0371-SELFCHECK');

    -- 工时必须 > 0（0 工时行会让分摊基数恒 0，最后除零）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_labor (period_code, work_date, cost_object_id, employee_name, dept_id, dept_name,
                                    hours, machine_hours, rate, amount, remark)
        VALUES ('2026-01', DATE '2026-01-07', v_obj, '自检赵六', v_dept, '自检车间',
                0, 0, 50, 0, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：hours=0 写入成功（ck_biz_cost_labor_hours 未生效）';
    END IF;

    -- ===== B7 费用台账：借方必须 4101.*，费用类别必须在值域内 =====
    INSERT INTO biz_cost_overhead (period_code, dept_id, dept_name, expense_item, debit_account_code,
                                   credit_account_code, amount, remark)
    VALUES ('2026-01', v_dept, '自检车间', 'UTILITY', '4101.01', '1002', 500, 'V0371-SELFCHECK');

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_overhead (period_code, dept_id, dept_name, expense_item, debit_account_code,
                                       credit_account_code, amount, remark)
        VALUES ('2026-01', v_dept, '自检车间', 'UTILITY', '5602.01', '1002', 500, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：制造费用借方记 5602.01 竟然成功（ck_biz_cost_oh_debit 未生效 ⇒ 费用永远进不了成本，风险 R-2）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_overhead (period_code, dept_id, dept_name, expense_item, debit_account_code,
                                       credit_account_code, amount, remark)
        VALUES ('2026-01', v_dept, '自检车间', 'TRAVEL', '4101.09', '1002', 500, 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：费用类别 TRAVEL 不在值域内却写入成功（ck_biz_cost_oh_item 未生效）';
    END IF;

    -- ===== B8 费率：范围与 scope_id 必须配对，同日同范围只许一条活行 =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_rate (rate_type, scope, scope_id, rate, effective_from, remark)
        VALUES ('LABOR', 'COMPANY', v_dept, 50, DATE '2026-01-01', 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：COMPANY 费率带了 scope_id 却写入成功（ck_biz_cost_rate_scope_id 未生效）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_rate (rate_type, scope, rate, effective_from, remark)
        VALUES ('LABOR', 'DEPT', 50, DATE '2026-01-01', 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：DEPT 费率不带 scope_id 却写入成功（ck_biz_cost_rate_scope_id 未生效）';
    END IF;

    INSERT INTO biz_cost_rate (rate_type, scope, rate, effective_from, remark)
    VALUES ('LABOR', 'COMPANY', 50, DATE '2026-01-01', 'V0371-SELFCHECK');

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_rate (rate_type, scope, rate, effective_from, remark)
        VALUES ('LABOR', 'COMPANY', 60, DATE '2026-01-01', 'V0371-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：同日同范围第二条活费率写入成功（uk_biz_cost_rate 未生效 ⇒ 取数随机）';
    END IF;

    -- 软删后可以重配（换费率 = 作废旧行 + 新建，不靠改历史）
    UPDATE biz_cost_rate SET deleted_at = now()
     WHERE rate_type = 'LABOR' AND scope = 'COMPANY' AND effective_from = DATE '2026-01-01'
       AND deleted_at IS NULL;
    INSERT INTO biz_cost_rate (rate_type, scope, rate, effective_from, remark)
    VALUES ('LABOR', 'COMPANY', 60, DATE '2026-01-01', 'V0371-SELFCHECK');

    -- ===== B9 外键 ON DELETE RESTRICT：有归集行时删成本对象必须被拦 =====
    -- 注意 SQLSTATE：RESTRICT 违反抛的是 **23001 restrict_violation**，不是 23503
    -- foreign_key_violation（23503 只在「子行引用不存在的父行」时抛）。两个都接住：
    -- 以后有人把 RESTRICT 改成 NO ACTION，这里抛的就是 23503。
    v_hit := false;
    BEGIN
        DELETE FROM biz_cost_object WHERE id = v_obj;
        v_hit := true;
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0371 自检失败：有归集行时成本对象被删掉（外键不是 ON DELETE RESTRICT ⇒ 留下孤儿成本）';
    END IF;

    -- ===== 清理探针（重复执行零残留） =====
    DELETE FROM biz_cost_collect  WHERE source_id = v_src OR remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_labor    WHERE remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_overhead WHERE remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_rate     WHERE remark = 'V0371-SELFCHECK';
    DELETE FROM biz_cost_object   WHERE id = v_obj;

    SELECT count(*) INTO v_n FROM biz_cost_collect
     WHERE source_id = v_src OR remark = 'V0371-SELFCHECK' OR cost_object_id = v_obj;
    SELECT v_n + count(*) INTO v_n FROM biz_cost_labor    WHERE remark = 'V0371-SELFCHECK';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_overhead WHERE remark = 'V0371-SELFCHECK';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_rate     WHERE remark = 'V0371-SELFCHECK';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_object   WHERE id = v_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0371 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0371 完成：归集四表建成，形态断言 + 9 组行为断言全过，探针已清理';
END $$;
