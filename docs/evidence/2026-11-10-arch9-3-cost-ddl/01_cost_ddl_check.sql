-- =============================================================================
-- ARCH9-3 成本核算升级（分批法 / 标准成本 / 品种法）—— 设计稿 DDL 真库验证脚本
--
-- 干什么：把设计文档 §3 里写出的**全部** DDL（13 张新表 + 4 类改造 + 6 类种子）
--         原样在真实 PG 上跑一遍，并用 DO 块逐条断言「结果形态」，最后整段 ROLLBACK。
--
-- 为什么这样做：写进文档却没跑过的 DDL 一定有错（ARCH9-2 实测踩到 3 个真错误：
--   漏科目导致凭证借方解析为空 / 父科目翻末级写漏 / 配置列类型写错）。
--   本脚本把「文档 DDL」与「库能不能吃」这件事在提交前就锁死。
--
-- 幂等：整段在事务里，结尾 ROLLBACK —— 库里零残留（脚本末尾有残留断言）。
-- 用法：
--   PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
--     -f docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql
-- =============================================================================

\set ON_ERROR_STOP on
\timing off
BEGIN;

-- =============================================================================
-- 第一部分：13 张新表（一期 10 张 / 二期 3 张）
-- =============================================================================

-- -----------------------------------------------------------------------------
-- ① biz_lot —— 批次主数据（一期）
--
-- 现状（实测）：库里**没有** biz_lot 这张表，批号只是三处自由文本列：
--   biz_stock_in_item.lot_no  VARCHAR(64)  (V0145:225)
--   biz_stock_out_item.lot_no VARCHAR(64)  (V0203:141)
--   biz_material_serial.batch_no VARCHAR(64) (V0138:80)
-- 分批法要「批次号贯穿采购/生产/销售」，自由文本做不到（同批异写即断链），
-- 所以要一张主数据。注意：**不给上面三处加物理外键**（存量 lot_no 质量不可控，
-- 强 FK 会让历史单据无法更新），改为 handler 校验 + 巡检报告。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_lot (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    lot_no          VARCHAR(64)   NOT NULL,                          -- 批号（业务键，物料级唯一）
    material_id     UUID          NOT NULL,                          -- 物料（弱引用 biz_material.id，不建 FK）
    source_type     VARCHAR(16)   NOT NULL DEFAULT 'MANUAL',         -- PURCHASE 采购批 / PRODUCTION 生产批 / OPENING 期初 / MANUAL 手工建
    source_doc_type VARCHAR(24),                                     -- 来源单据类型：STOCK_IN / COST_PERIOD / 空
    source_doc_id   UUID,                                            -- 来源单据 id
    source_doc_no   VARCHAR(32),                                     -- 来源单号快照（审计口径，不 join）
    supplier_id     UUID,                                            -- 采购批的供应商（弱引用）
    prod_date       DATE,                                            -- 生产日期 / 到货日期
    expire_date     DATE,                                            -- 失效日期（食品医药用）
    qc_status       VARCHAR(12)   NOT NULL DEFAULT 'NA',             -- NA 不适用 / PENDING 待检 / PASSED 合格 / REJECTED 不合格
    status          SMALLINT      NOT NULL DEFAULT 1,                -- 1 启用 / 0 停用
    remark          TEXT,
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,

    CONSTRAINT ck_biz_lot_source CHECK (source_type IN ('PURCHASE','PRODUCTION','OPENING','MANUAL')),
    CONSTRAINT ck_biz_lot_qc     CHECK (qc_status IN ('NA','PENDING','PASSED','REJECTED')),
    CONSTRAINT ck_biz_lot_status CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_lot_dates  CHECK (expire_date IS NULL OR prod_date IS NULL OR expire_date >= prod_date)
);

COMMENT ON TABLE  biz_lot IS '批次主数据（物料级批号）：分批法的批次锚点；采购批 / 生产批共用一张表，靠 source_type 区分';
COMMENT ON COLUMN biz_lot.lot_no IS '批号，与 biz_stock_in_item.lot_no / biz_stock_out_item.lot_no / biz_material_serial.batch_no 同字面值；物料内唯一';
COMMENT ON COLUMN biz_lot.source_doc_no IS '来源单号快照：历史/审计类字段，按 AGENTS 约定 13 存快照而非 join';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_lot_material_no
    ON biz_lot (material_id, lot_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_lot_no     ON biz_lot (lot_no);
CREATE INDEX IF NOT EXISTS idx_biz_lot_source ON biz_lot (source_doc_type, source_doc_id);


-- -----------------------------------------------------------------------------
-- ② biz_cost_object —— 成本对象（一期：生产批）
--
-- 成本对象 = 「料工费往哪儿归集」的那个筐。分批法里这个筐就是一批货。
-- 值域预留三种（BATCH 批次 / ORDER 订单 / PRODUCT 产品），**一期只写 BATCH**：
--   BATCH   一期实现（按生产批，可携带 source_doc_no = 客户订单号 ⇒ 覆盖「按订单生产」）
--   ORDER   三期（真正按工单核算，需 ARCH-4 MES 的 biz_mes_wo）
--   PRODUCT 三期（品种法：按产品归集，多期连续生产）
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_object (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    object_no           VARCHAR(32)   NOT NULL,                      -- 成本对象号（生产批号）：CB-yyyyMMdd-000001
    object_type         VARCHAR(12)   NOT NULL DEFAULT 'BATCH',      -- BATCH / ORDER / PRODUCT（一期只写 BATCH）
    product_id          UUID          NOT NULL,                      -- 产出品物料（弱引用 biz_material.id）
    lot_no              VARCHAR(64),                                 -- 产出批号（完工入库时写 biz_lot / 明细 lot_no）
    target_warehouse_id UUID          NOT NULL,                      -- 完工入库仓（弱引用 biz_warehouse.id）
    target_location_id  UUID,                                        -- 完工入库库位（可空，弱引用 biz_inventory_location）
    plan_qty            NUMERIC(18,4) NOT NULL,                      -- 计划产量
    completed_qty       NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 累计完工数量（结算时回写）
    wip_qty             NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 期末在制数量（结算时手填）
    completion_pct      NUMERIC(5,2)  NOT NULL DEFAULT 0,            -- 期末在制完工进度%（约当产量用）
    material_input_mode VARCHAR(16)   NOT NULL DEFAULT 'ONE_TIME',   -- 投料方式：ONE_TIME 开工一次投入（一期唯一实现）/ AS_PROGRESS 随加工投入
    source_doc_no       VARCHAR(32),                                 -- 上游订单号（无 MES 时手填客户订单/生产通知单号）
    customer_id         UUID,                                        -- 按订单生产时的客户（弱引用 biz_customer.id）
    workshop_dept_id    UUID          NOT NULL,                      -- 生产车间（弱引用 sys_dept.id）
    workshop_dept_name  VARCHAR(120)  NOT NULL,                      -- 车间名快照（审计/历史口径，AGENTS 约定 13）
    start_date          DATE          NOT NULL,                      -- 开工日期
    due_date            DATE,                                        -- 计划完工日期
    status              VARCHAR(12)   NOT NULL DEFAULT 'OPEN',       -- OPEN 在产 / CLOSED 关批 / CANCELLED 作废
    closed_at           TIMESTAMPTZ,
    closed_by           UUID,
    remark              TEXT,
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,

    CONSTRAINT ck_biz_cost_object_type   CHECK (object_type IN ('BATCH','ORDER','PRODUCT')),
    CONSTRAINT ck_biz_cost_object_status CHECK (status IN ('OPEN','CLOSED','CANCELLED')),
    CONSTRAINT ck_biz_cost_object_input  CHECK (material_input_mode IN ('ONE_TIME','AS_PROGRESS')),
    CONSTRAINT ck_biz_cost_object_qty    CHECK (plan_qty > 0 AND completed_qty >= 0 AND wip_qty >= 0),
    CONSTRAINT ck_biz_cost_object_pct    CHECK (completion_pct >= 0 AND completion_pct <= 100),
    CONSTRAINT ck_biz_cost_object_dates  CHECK (due_date IS NULL OR due_date >= start_date),
    CONSTRAINT ck_biz_cost_object_closed CHECK (status <> 'CLOSED' OR closed_at IS NOT NULL)
);

COMMENT ON TABLE  biz_cost_object IS '成本对象：料工费归集主体。一期=BATCH（生产批），ORDER/PRODUCT 值域预留给三期';
COMMENT ON COLUMN biz_cost_object.wip_qty IS '期末在制数量，结算时必填（无 MES 时无法自动采集，见设计 §4.5 数据源）';
COMMENT ON COLUMN biz_cost_object.completion_pct IS '期末在制完工进度%，约当产量法用；材料一次投入时材料约当按 100% 另算';
COMMENT ON COLUMN biz_cost_object.workshop_dept_name IS '车间名快照：成本记录属审计/历史口径';
COMMENT ON COLUMN biz_cost_object.lot_no IS '产出批号：结算生成完工入库时作为 lot_no 写入入库明细，并自动建 biz_lot 行（source_type=PRODUCTION）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_object_no
    ON biz_cost_object (object_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_product  ON biz_cost_object (product_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_workshop ON biz_cost_object (workshop_dept_id, start_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_cost_object_lot      ON biz_cost_object (lot_no) WHERE lot_no IS NOT NULL;


-- -----------------------------------------------------------------------------
-- ③ biz_cost_collect —— 成本归集明细（一期核心表）
--
-- 「一张表把料工费都收进来」：一行 = 一个成本对象 × 一个成本要素 × 一条来源明细。
-- 料：source_type='STOCK_OUT'（领料出库明细行，金额 = 移动加权实际成本）
-- 工：source_type='LABOR_TICKET'（biz_cost_labor 汇总行）
-- 费：source_type='ALLOC'（分摊结果）/ 'MANUAL'（直接计入对象的车间费用）
-- 幂等键：(source_type, source_id, line_no) —— 同一来源明细重复归集必然撞键。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_collect (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cost_object_id UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,  -- 空=车间费用池（待分摊）
    period_code    VARCHAR(7)    NOT NULL,                           -- 归属会计期间 yyyy-MM
    element        VARCHAR(12)   NOT NULL,                           -- MATERIAL 料 / LABOR 工 / OVERHEAD 费
    source_type    VARCHAR(16)   NOT NULL,                           -- STOCK_OUT/LABOR_TICKET/OVERHEAD_TICKET/DEPRECIATION/EXPENSE/ALLOC/MANUAL
    source_id      UUID,                                             -- 来源行 id（MANUAL 行为空）
    source_no      VARCHAR(32),                                      -- 来源单号快照
    line_no        INT           NOT NULL DEFAULT 0,                 -- 来源行号（幂等键组成部分）
    occur_date     DATE          NOT NULL,                           -- 业务发生日期
    material_id    UUID,                                             -- 料行的物料（弱引用 biz_material.id）
    qty            NUMERIC(18,4),                                    -- 数量（料行）
    unit_price     NUMERIC(18,6),                                    -- 单价（料行=移动加权出库单价快照）
    amount         NUMERIC(18,4) NOT NULL,                           -- 归集金额（不含税）
    dept_id        UUID,                                             -- 费用归属车间（费行）
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

COMMENT ON TABLE  biz_cost_collect IS '成本归集明细：成本对象 × 成本要素 × 来源明细；料来自出库明细，工来自工时台账，费来自分摊结果或手工';
COMMENT ON COLUMN biz_cost_collect.unit_price IS '料行单价快照＝出库时的移动加权单价（不是重算值，取账上真实差额的分摊单价）';
COMMENT ON COLUMN biz_cost_collect.source_no IS '来源单号快照：成本表属审计口径，来源单被改名/作废后仍要能追溯';

-- 幂等键：同一来源行只能归集一次（MANUAL 行 source_id 为空，不受此键约束）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_collect_src
    ON biz_cost_collect (source_type, source_id, line_no)
    WHERE deleted_at IS NULL AND source_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_collect_obj ON biz_cost_collect (cost_object_id, period_code, element);
CREATE INDEX IF NOT EXISTS idx_biz_cost_collect_period ON biz_cost_collect (period_code);


-- -----------------------------------------------------------------------------
-- ④ biz_cost_labor —— 工时台账（一期「工」的数据源）
--
-- 没有 MES 就没有报工记录，直接人工只能人录 / Excel 导入。本表就是那个入口。
-- 金额 = 工时 × 工资率，**库层 CHECK 锁死等式**：改了工时忘了改金额必然被拒。
-- 未来 MES 上线：source_type='MES'，一行对一条 biz_work_log，字段可直接映射。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_labor (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code    VARCHAR(7)    NOT NULL,
    work_date      DATE          NOT NULL,
    cost_object_id UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    employee_id    UUID,                                             -- 员工（弱引用 biz_employee.id；ARCH9-2 未上线时为空=按班组计）
    employee_name  VARCHAR(50)   NOT NULL,                           -- 姓名快照（历史/审计口径）
    dept_id        UUID          NOT NULL,                           -- 工作中心（弱引用 sys_dept.id）
    dept_name      VARCHAR(120)  NOT NULL,                           -- 工作中心名快照
    hours          NUMERIC(10,2) NOT NULL,                           -- 人工工时
    machine_hours  NUMERIC(10,2) NOT NULL DEFAULT 0,                  -- 机器工时（按机器工时分摊的基数）
    rate           NUMERIC(18,6) NOT NULL,                           -- 工资率快照（元/小时，取自 biz_cost_rate）
    amount         NUMERIC(18,4) NOT NULL,                           -- = round(hours × rate, 4)
    source_type    VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',          -- MANUAL / IMPORT / MES
    status         VARCHAR(12)   NOT NULL DEFAULT 'DRAFT',           -- DRAFT 可改 / CONFIRMED 已确认（结算只取 CONFIRMED）
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

COMMENT ON TABLE  biz_cost_labor IS '工时台账：无 MES 时直接人工的唯一数据源（手工录入 / Excel 导入）；金额＝工时×工资率由库层 CHECK 锁死';
COMMENT ON COLUMN biz_cost_labor.machine_hours IS '机器工时：按「机器小时」分摊制造费用时的基数；与人工工时分开存（一人看多机时二者不等）';
COMMENT ON COLUMN biz_cost_labor.employee_name IS '姓名快照：成本表属历史/审计口径，员工改名或离职后仍要能回溯';

CREATE INDEX IF NOT EXISTS idx_biz_cost_labor_obj  ON biz_cost_labor (cost_object_id, period_code);
CREATE INDEX IF NOT EXISTS idx_biz_cost_labor_emp  ON biz_cost_labor (employee_id, work_date);
CREATE INDEX IF NOT EXISTS idx_biz_cost_labor_period ON biz_cost_labor (period_code, status);


-- -----------------------------------------------------------------------------
-- ⑤ biz_cost_overhead —— 手工制造费用台账（一期「费」的第三条腿）
--
-- 三个费用来源，各走各的路，不许混：
--   ① 有单据且已入 4101：折旧（biz_depreciation）/ 报销（biz_expense_report）→ **不落本表**，
--      由结算按车间从来源明细归集（避免双计）；
--   ② 无单据的车间费用（房租摊销、水电现金付、修理零工）→ 落本表，结算时生成「借 4101 / 贷 对方科目」；
--   ③ 直接计入某个成本对象的费用（如该批专用模具摊销）→ 落本表且 cost_object_id 非空。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_overhead (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code         VARCHAR(7)    NOT NULL,
    cost_object_id      UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,  -- 空=车间级，由分摊规则摊
    dept_id             UUID          NOT NULL,                      -- 车间（弱引用 sys_dept.id）
    dept_name           VARCHAR(120)  NOT NULL,                      -- 车间名快照
    expense_item        VARCHAR(16)   NOT NULL,                      -- 费用项目（字典 cost_expense_item）
    debit_account_code  VARCHAR(30)   NOT NULL DEFAULT '4101.09',    -- 借方制造费用科目（4101.01~09）
    credit_account_code VARCHAR(30)   NOT NULL,                      -- 贷方对方科目（1002/2241.xx/1801 …）
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

COMMENT ON TABLE  biz_cost_overhead IS '手工制造费用台账：只收「没有已入账单据」的车间费用；折旧/报销不落本表（避免与来源单据双计）';
COMMENT ON COLUMN biz_cost_overhead.credit_account_code IS '对方科目必填：本表金额要由结算生成「借 4101.xx / 贷 对方科目」凭证，没有对方科目就是一笔无头账';

CREATE INDEX IF NOT EXISTS idx_biz_cost_oh_period ON biz_cost_overhead (period_code, dept_id);
CREATE INDEX IF NOT EXISTS idx_biz_cost_oh_obj    ON biz_cost_overhead (cost_object_id) WHERE cost_object_id IS NOT NULL;


-- -----------------------------------------------------------------------------
-- ⑥ biz_cost_rate —— 工资率 / 机器费率配置（按期间版本）
--
-- 一期：人工费率只有这一处来源（手工维护），**不读 ARCH9-2 的薪资表**
-- （薪资模块未上线，且「个人工资」≠「成本工资率」，后者是全车间综合费率）。
-- 取数优先级：COST_OBJECT > DEPT > COMPANY（同生效期内取最具体的一档）。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_rate (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rate_type      VARCHAR(12)   NOT NULL,                           -- LABOR 人工费率 / MACHINE 机器费率
    scope          VARCHAR(12)   NOT NULL,                           -- COMPANY 公司级 / DEPT 车间级 / COST_OBJECT 对象级
    scope_id       UUID,                                             -- DEPT→sys_dept.id / COST_OBJECT→biz_cost_object.id；COMPANY 为空
    rate           NUMERIC(18,6) NOT NULL,                           -- 元/小时
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
        (scope = 'COMPANY' AND scope_id IS NULL)
     OR (scope <> 'COMPANY' AND scope_id IS NOT NULL))
);

COMMENT ON TABLE biz_cost_rate IS '人工/机器费率配置：按 scope 分级、按生效期版本化；工时台账落库时把 rate 快照下来（调价不改历史）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_rate
    ON biz_cost_rate (rate_type, scope, coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid), effective_from)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_rate_lookup ON biz_cost_rate (rate_type, status, effective_from DESC);


-- -----------------------------------------------------------------------------
-- ⑦ biz_cost_alloc_rule —— 制造费用分摊规则（一期：一次分配）
--
-- 规则 = 「哪个车间的哪类制造费用，按什么标准，摊给该车间本期在产的哪些成本对象」。
-- 二期才做二次分配（辅助车间 → 生产车间），三期才做分步结转（上步半成品 → 下步）。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_alloc_rule (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_code        VARCHAR(32)   NOT NULL,                         -- 规则编码（业务键）
    rule_name        VARCHAR(120)  NOT NULL,
    biz_type         VARCHAR(16)   NOT NULL DEFAULT 'OVERHEAD',      -- 一期只有 OVERHEAD；三期加 STEP_TRANSFER
    source_dept_id   UUID          NOT NULL,                         -- 被摊车间（弱引用 sys_dept.id）
    source_dept_name VARCHAR(120)  NOT NULL,                         -- 车间名快照
    account_code     VARCHAR(30)   NOT NULL,                         -- 被摊的制造费用科目（4101.01~09）
    basis            VARCHAR(16)   NOT NULL,                         -- 分配标准
    priority         INT           NOT NULL DEFAULT 10,              -- 同车间多规则时的先后（不重叠时无意义）
    effective_from   DATE          NOT NULL,
    effective_to     DATE,
    status           SMALLINT      NOT NULL DEFAULT 1,
    remark           VARCHAR(255),
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,

    CONSTRAINT ck_biz_cost_rule_biz    CHECK (biz_type IN ('OVERHEAD')),
    CONSTRAINT ck_biz_cost_rule_basis  CHECK (basis IN ('LABOR_HOURS','MACHINE_HOURS','OUTPUT_QTY','MATERIAL_COST')),
    CONSTRAINT ck_biz_cost_rule_status CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_cost_rule_dates  CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_rule_account CHECK (account_code LIKE '4101.%')
);

COMMENT ON TABLE biz_cost_alloc_rule IS '制造费用分摊规则：一次分配（车间费用 → 成本对象）；basis 四值＝工时/机器工时/产量/直接材料成本';
COMMENT ON COLUMN biz_cost_alloc_rule.basis IS 'LABOR_HOURS 按人工工时 / MACHINE_HOURS 按机器工时 / OUTPUT_QTY 按完工产量 / MATERIAL_COST 按直接材料成本';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_rule_code
    ON biz_cost_alloc_rule (rule_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_rule_dept ON biz_cost_alloc_rule (source_dept_id, account_code, status);


-- -----------------------------------------------------------------------------
-- ⑧ biz_cost_alloc_result —— 分摊结果（一期：落表，可重算、可追溯）
--
-- 为什么不「算完就算」：分摊是成本考核的口径来源，必须能回答「这 1800 元凭什么摊给这一批」。
-- 三个快照列（basis_value / basis_total / pool_amount）保证重算前后可对比。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_alloc_result (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code     VARCHAR(7)    NOT NULL,
    rule_id         UUID          NOT NULL REFERENCES biz_cost_alloc_rule(id) ON DELETE RESTRICT,
    cost_object_id  UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    dept_id         UUID          NOT NULL,                          -- 被摊车间
    dept_name       VARCHAR(120)  NOT NULL,                          -- 车间名快照
    account_code    VARCHAR(30)   NOT NULL,                          -- 费用科目
    basis_value     NUMERIC(18,4) NOT NULL,                          -- 本对象在该标准下的基数
    basis_total     NUMERIC(18,4) NOT NULL,                          -- 全车间基数合计（快照）
    pool_amount     NUMERIC(18,4) NOT NULL,                          -- 待分摊总额（快照）
    alloc_amount    NUMERIC(18,4) NOT NULL,                          -- 分摊到本对象的金额（含尾差调整）
    rounding_adj    NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 尾差调整额（最大余数法：仅最大者非 0）
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,

    CONSTRAINT ck_biz_cost_alloc_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_alloc_num    CHECK (basis_value >= 0 AND basis_total >= 0
                                               AND pool_amount >= 0 AND alloc_amount >= 0),
    CONSTRAINT ck_biz_cost_alloc_base   CHECK (basis_total > 0 OR alloc_amount = 0)
);

COMMENT ON TABLE  biz_cost_alloc_result IS '分摊结果：一行＝某规则摊给某成本对象的金额；basis_value/basis_total/pool_amount 三快照供复核';
COMMENT ON COLUMN biz_cost_alloc_result.rounding_adj IS '尾差调整：分摊总额按最大余数法分到分，合计严格等于 pool_amount（守恒由结算断言）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_alloc_result
    ON biz_cost_alloc_result (period_code, rule_id, cost_object_id, account_code);
CREATE INDEX IF NOT EXISTS idx_biz_cost_alloc_result_period ON biz_cost_alloc_result (period_code, dept_id);


-- -----------------------------------------------------------------------------
-- ⑨ biz_cost_period —— 成本核算批次（一个期间一张结算单，高危动作）
--
-- org_id NOT NULL DEFAULT 哨兵：多组织（ARCH9-1）上线后唯一键 (org_id, period_code)
-- 直接生效，不需要再改索引 —— 若只写 (period_code) 的唯一键，多组织那天必须换键，
-- 而换键期间并发跑两次结算就会生成两张单（真事故）。哨兵默认值是现在就付的小成本。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_period (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                   UUID          NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    period_code              VARCHAR(7)    NOT NULL,
    batch_no                 VARCHAR(32)   NOT NULL,                 -- 成本核算批次号 CS-yyyyMM-0001
    status                   VARCHAR(12)   NOT NULL DEFAULT 'DRAFT',
    labor_source             VARCHAR(12)   NOT NULL DEFAULT 'MANUAL',-- MANUAL 手工工时台账 / PAYROLL 薪资单（ARCH9-2 上线后）
    object_count             INT           NOT NULL DEFAULT 0,
    total_material           NUMERIC(18,4) NOT NULL DEFAULT 0,       -- 本期投入：料
    total_labor              NUMERIC(18,4) NOT NULL DEFAULT 0,       -- 本期投入：工
    total_overhead           NUMERIC(18,4) NOT NULL DEFAULT 0,       -- 本期投入：费
    total_cost               NUMERIC(18,4) NOT NULL DEFAULT 0,       -- = 料+工+费
    total_finished           NUMERIC(18,4) NOT NULL DEFAULT 0,       -- 完工转出合计
    total_wip                NUMERIC(18,4) NOT NULL DEFAULT 0,       -- 期末在制合计
    approval_instance_id     UUID,
    labor_voucher_id         UUID,                                   -- 人工计提凭证
    overhead_voucher_id      UUID,                                   -- 制造费用分摊凭证
    finish_voucher_id        UUID,                                   -- 完工入库结转凭证
    settled_at               TIMESTAMPTZ,
    settled_by               UUID,
    reversed_at              TIMESTAMPTZ,
    reversed_by              UUID,
    reverse_reason           VARCHAR(255),
    cancel_reason            VARCHAR(255),
    remark                   TEXT,
    created_at               TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at               TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by                UUID,
    update_by                UUID,
    deleted_at               TIMESTAMPTZ,

    CONSTRAINT ck_biz_cost_period_code   CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_period_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','SETTLED','CLOSED','CANCELLED','REVERSED')),
    CONSTRAINT ck_biz_cost_period_labor  CHECK (labor_source IN ('MANUAL','PAYROLL')),
    CONSTRAINT ck_biz_cost_period_sum    CHECK (total_cost = total_material + total_labor + total_overhead),
    CONSTRAINT ck_biz_cost_period_cons   CHECK (total_cost = total_finished + total_wip),
    CONSTRAINT ck_biz_cost_period_appr   CHECK (status NOT IN ('SUBMITTED','APPROVED','SETTLED','CLOSED','REVERSED')
                                                OR approval_instance_id IS NOT NULL),
    CONSTRAINT ck_biz_cost_period_settle CHECK (status NOT IN ('SETTLED','CLOSED','REVERSED')
                                                OR (settled_at IS NOT NULL AND finish_voucher_id IS NOT NULL)),
    CONSTRAINT ck_biz_cost_period_rev    CHECK (status <> 'REVERSED'
                                                OR (reversed_at IS NOT NULL AND reverse_reason IS NOT NULL)),
    CONSTRAINT ck_biz_cost_period_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);

COMMENT ON TABLE  biz_cost_period IS '成本核算批次：一个期间一张结算单；审批通过后一次事务内完成「入库 + 三张凭证」';
COMMENT ON COLUMN biz_cost_period.labor_source IS '人工来源快照：MANUAL=手工工时台账（一期）/ PAYROLL=薪资单（ARCH9-2 上线后切换，二选一，防双计提）';
COMMENT ON COLUMN biz_cost_period.total_wip IS '期末在制合计＝4001 生产成本期末余额（巡检 C-COST-3 勾稽）';

-- ★ 唯一键口径（设计 §4.10 红冲重算）：一个期间同时只能有**一张活跃批次**，
--   但红冲 / 作废的历史行必须留在表里（审计），所以唯一键排除 REVERSED / CANCELLED。
--   若写成「period_code 全表唯一」，红冲后就再也无法重新结算该期间 —— 那是死单。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_period
    ON biz_cost_period (org_id, period_code)
    WHERE deleted_at IS NULL AND status NOT IN ('REVERSED','CANCELLED');
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_period_no ON biz_cost_period (batch_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_period_status ON biz_cost_period (status, period_code DESC);


-- -----------------------------------------------------------------------------
-- ⑩ biz_cost_period_item —— 成本计算单（成本对象 × 期间）
--
-- 约当产量法的全部中间量都在这里落库：不做「算完只留结果」——
-- 期末审计问「这 27.8888 是怎么来的」，答案必须能从这一行复算出来。
-- 守恒式在库层锁死：完工转出 + 期末在制 = 期初在制 + 本期投入。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_period_item (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_id          UUID          NOT NULL REFERENCES biz_cost_period(id) ON DELETE CASCADE,
    cost_object_id     UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    product_id         UUID          NOT NULL,                        -- 产出品（弱引用；与 cost_object.product_id 冗余，防对象改品）
    wip_beg_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 期初在制数量
    wip_beg_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 期初在制成本
    completed_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 本期完工数量
    wip_end_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 期末在制数量
    completion_pct     NUMERIC(5,2)  NOT NULL DEFAULT 0,              -- 期末在制完工进度%
    equiv_material     NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 材料约当产量
    equiv_conversion   NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 加工约当产量（人工/费用共用）
    mat_cur            NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 本期投入料
    labor_cur          NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 本期投入工
    oh_cur             NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 本期投入费
    unit_material      NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 材料单位成本
    unit_labor         NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 人工单位成本
    unit_overhead      NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 费用单位成本
    finished_cost      NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 完工转出成本
    wip_end_cost       NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 期末在制成本
    unit_finished_cost NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 完工单位成本（→ stock_in.cost_price）
    stock_in_id        UUID,                                          -- 结算生成的完工入库单
    status             VARCHAR(12)   NOT NULL DEFAULT 'CALC',         -- CALC 已计算 / SETTLED 已生成入库与凭证
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
    CONSTRAINT ck_biz_cost_item_settled CHECK (status <> 'SETTLED' OR stock_in_id IS NOT NULL)
);

COMMENT ON TABLE  biz_cost_period_item IS '成本计算单：约当产量法的全部中间量落库，可复算；守恒式由 ck_biz_cost_item_cons 在库层锁死';
COMMENT ON COLUMN biz_cost_period_item.unit_finished_cost IS '完工单位成本，结算时写入完工入库明细 cost_price（存货计价的唯一入口）';
COMMENT ON COLUMN biz_cost_period_item.equiv_material IS '材料约当产量：一次投料时 = 完工数 + 期末在制数（进度按 100%）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_item ON biz_cost_period_item (period_id, cost_object_id);
CREATE INDEX IF NOT EXISTS idx_biz_cost_item_obj ON biz_cost_period_item (cost_object_id);


-- =============================================================================
-- 第二部分：二期 3 张表（标准成本 + 差异分析）
-- 设计写明「二期落地」，DDL 现在就给全并验证，免得到二期才发现表结构不通。
-- =============================================================================

-- -----------------------------------------------------------------------------
-- ⑪ biz_cost_standard —— 标准成本卡（卡头）
--
-- 口径决议（§0 D-8）：二期**不改存货计价**，标准成本只用于差异分析与考核；
-- 「按标准成本计价 + 差异入 1404 分摊」属推翻移动加权的独立决策（ARCH-4 D9 明确不碰），
-- 需要单独 ADR，本卡不做。所以本表**不参与任何存货金额计算**。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_standard (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id        UUID          NOT NULL,                        -- 产出品（弱引用 biz_material.id）
    version_no         INT           NOT NULL DEFAULT 1,
    effective_from     DATE          NOT NULL,
    effective_to       DATE,
    std_material_cost  NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 单位标准材料成本
    std_labor_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 单位标准人工成本
    std_overhead_cost  NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 单位标准制造费用
    std_total_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 合计（= 三项之和，库层 CHECK）
    std_labor_hours    NUMERIC(18,4) NOT NULL DEFAULT 0,              -- 单位标准工时（效率差异的基准）
    std_labor_rate     NUMERIC(18,6) NOT NULL DEFAULT 0,              -- 标准工资率
    status             SMALLINT      NOT NULL DEFAULT 1,
    remark             TEXT,
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,

    CONSTRAINT ck_biz_cost_std_money CHECK (std_material_cost >= 0 AND std_labor_cost >= 0
                                            AND std_overhead_cost >= 0 AND std_total_cost >= 0),
    CONSTRAINT ck_biz_cost_std_sum   CHECK (std_total_cost = std_material_cost + std_labor_cost + std_overhead_cost),
    CONSTRAINT ck_biz_cost_std_dates CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_std_ver   CHECK (version_no >= 1),
    CONSTRAINT ck_biz_cost_std_labor CHECK (std_labor_cost = round(std_labor_hours * std_labor_rate, 6))
);

COMMENT ON TABLE biz_cost_standard IS '标准成本卡（二期）：单位标准料/工/费，按生效期版本化；只用于差异分析，不改存货计价';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std
    ON biz_cost_standard (material_id, effective_from) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_std_lookup ON biz_cost_standard (material_id, status, effective_from DESC);


-- -----------------------------------------------------------------------------
-- ⑫ biz_cost_standard_item —— 标准成本卡明细（单位标准用量与标准价）
-- 材料量差需要「标准用量」，所以必须有 BOM 级明细，不能只有总额。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_standard_item (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    standard_id          UUID          NOT NULL REFERENCES biz_cost_standard(id) ON DELETE CASCADE,
    line_no              INT           NOT NULL,
    component_material_id UUID         NOT NULL,                      -- 组成材料（弱引用）
    std_qty              NUMERIC(18,6) NOT NULL,                      -- 单位标准用量
    std_price            NUMERIC(18,6) NOT NULL,                      -- 标准单价
    std_amount           NUMERIC(18,6) NOT NULL,                      -- = std_qty × std_price（库层 CHECK）
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_cost_std_item_num CHECK (std_qty > 0 AND std_price >= 0 AND std_amount >= 0),
    CONSTRAINT ck_biz_cost_std_item_cal CHECK (std_amount = round(std_qty * std_price, 6))
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std_item ON biz_cost_standard_item (standard_id, line_no);


-- -----------------------------------------------------------------------------
-- ⑬ biz_cost_variance —— 差异明细（二期）
--
-- 六个差异类型，每个都能由「实际 - 标准」两因素恒等式验证：
--   MAT_PRICE   材料价差 = (实际单价 - 标准单价) × 实际用量
--   MAT_QTY     材料量差 = (实际用量 - 标准用量) × 标准单价
--   LABOR_RATE  工资率差异 = (实际工资率 - 标准工资率) × 实际工时
--   LABOR_EFF   人工效率差异 = (实际工时 - 标准工时) × 标准工资率
--   OH_SPEND    制造费用耗费差异 = 实际制造费用 - 预算制造费用
--   OH_VOLUME   制造费用产量差异 = 预算制造费用 - 标准分配额
-- 前四项两两相加 ≡ 实际成本 - 标准成本（§4.9 给了恒等式与真库演算）。
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_variance (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code      VARCHAR(7)    NOT NULL,
    cost_object_id   UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    standard_id      UUID          REFERENCES biz_cost_standard(id) ON DELETE RESTRICT,
    variance_type    VARCHAR(12)   NOT NULL,
    element          VARCHAR(12)   NOT NULL,                          -- MATERIAL / LABOR / OVERHEAD
    material_id      UUID,                                            -- 材料类差异的物料
    actual_qty       NUMERIC(18,6),
    std_qty          NUMERIC(18,6),
    actual_price     NUMERIC(18,6),
    std_price        NUMERIC(18,6),
    amount           NUMERIC(18,4) NOT NULL,                          -- 差异金额（正=不利，负=有利）
    direction        VARCHAR(12)   NOT NULL,                          -- FAVORABLE 有利 / UNFAVORABLE 不利
    remark           VARCHAR(255),
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,

    CONSTRAINT ck_biz_cost_var_type CHECK (variance_type IN
        ('MAT_PRICE','MAT_QTY','LABOR_RATE','LABOR_EFF','OH_SPEND','OH_VOLUME')),
    CONSTRAINT ck_biz_cost_var_elem CHECK (element IN ('MATERIAL','LABOR','OVERHEAD')),
    CONSTRAINT ck_biz_cost_var_dir  CHECK (direction IN ('FAVORABLE','UNFAVORABLE')),
    CONSTRAINT ck_biz_cost_var_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_var_sign CHECK (
        (direction = 'UNFAVORABLE' AND amount >= 0) OR (direction = 'FAVORABLE' AND amount <= 0))
);

CREATE INDEX IF NOT EXISTS idx_biz_cost_var_period ON biz_cost_variance (period_code, variance_type);
CREATE INDEX IF NOT EXISTS idx_biz_cost_var_obj    ON biz_cost_variance (cost_object_id, period_code);


-- =============================================================================
-- 第三部分：改造现有表（4 处，全部「只加列 / 只加宽值域」，不改已有值）
-- =============================================================================

-- ---------- ① biz_stock_out：领料挂成本对象 ----------
-- 存量行为完全不变：cost_object_id 为空 ⇒ 过账借方仍是 6401（现有口径）。
-- 挂上对象 ⇒ 借方改 4001.01 直接材料（见 §4.7 领料凭证改造）。
ALTER TABLE biz_stock_out ADD COLUMN IF NOT EXISTS cost_object_id UUID;
COMMENT ON COLUMN biz_stock_out.cost_object_id IS
    '成本对象（生产批）：领料给生产批时必填；为空＝非生产领用，过账借方仍走 6401（存量行为不变）';
CREATE INDEX IF NOT EXISTS idx_biz_so_cost_obj ON biz_stock_out (cost_object_id)
    WHERE cost_object_id IS NOT NULL;

-- ---------- ② biz_stock_in：完工入库（in_type 加 PRODUCTION）----------
-- 实测依据：ck_biz_si_type 现 4 值（PURCHASE/RETURN/OTHER/SOURCELESS，V0330:53），
-- 自然没有「生产入库」；而库存流水词表 TxnBizType::ProductionIn='PRODUCTION_IN'
-- 早在 doc_kind.rs:687 就定义了、**没有任何写入口**（grep 实测）—— 框架早就预留了这条路。
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS cost_object_id UUID;
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS cost_period_id UUID;
COMMENT ON COLUMN biz_stock_in.cost_object_id IS '完工入库对应的成本对象（仅 in_type=PRODUCTION 有值）';
COMMENT ON COLUMN biz_stock_in.cost_period_id IS '产出它的成本核算批次（勾稽 C-COST-2：入库金额 = 结转凭证贷方）';

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_type
    CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER', 'SOURCELESS', 'PRODUCTION'));

-- 生产入库必须有成本对象与核算批次，且**不许有供应商**（原 ck_biz_si_supplier_scope
-- 的 ELSE 分支已要求 supplier_id IS NULL，这里只补「必须有对象/批次」这一向）
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_production;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_production
    CHECK (in_type <> 'PRODUCTION'
           OR (cost_object_id IS NOT NULL AND cost_period_id IS NOT NULL
               AND supplier_id IS NULL AND is_sourceless = FALSE));

CREATE INDEX IF NOT EXISTS idx_biz_si_cost_obj ON biz_stock_in (cost_object_id)
    WHERE cost_object_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_si_cost_period ON biz_stock_in (cost_period_id)
    WHERE cost_period_id IS NOT NULL;

-- ---------- ③ biz_account：4001/4101 翻非末级 + 补种末级明细 ----------
-- 实测：4001 生产成本 is_leaf=1、4101 制造费用 is_leaf=1，均无子科目；
-- 而凭证闸只允许末级科目记账（voucher.rs::post_voucher 的 non_leaf 检查），
-- 所以「借 4001.01 直接材料」必须先有子科目、并把父科目翻成非末级。
--
-- ★ 跨卡重叠（ARCH9-2）：它要种 2211.01~03 并翻 2211 非末级；本卡人工凭证要贷 2211.01，
--   也种 2211.01 / 翻 2211。两边都是幂等的条件 UPDATE + ON CONFLICT DO NOTHING，
--   谁先上线结果都一致；自检断言写「结果形态」（2211 非末级 且 有末级子科目），
--   不写「本卡刚翻的」，否则并行上线时后跑的那张卡会误报失败。
UPDATE biz_account SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP
 WHERE account_code IN ('4001','4101') AND is_leaf = 1;

INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status)
SELECT v.code, v.name, p.id, p.level + 1, p.account_category, p.balance_direction, 1, 1
  FROM (VALUES
        ('4001.01', '生产成本-直接材料', '4001'),
        ('4001.02', '生产成本-直接人工', '4001'),
        ('4001.03', '生产成本-制造费用', '4001'),
        ('4101.01', '制造费用-折旧费',   '4101'),
        ('4101.02', '制造费用-水电费',   '4101'),
        ('4101.03', '制造费用-修理费',   '4101'),
        ('4101.04', '制造费用-车间管理费','4101'),
        ('4101.09', '制造费用-其他',     '4101')
       ) AS v(code, name, parent_code)
  JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;

-- 人工凭证贷方 2211.01（与 ARCH9-2 同键，幂等）
UPDATE biz_account SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP
 WHERE account_code = '2211' AND is_leaf = 1;
INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status)
SELECT '2211.01', '应付职工薪酬-工资', p.id, p.level + 1, p.account_category, p.balance_direction, 1, 1
  FROM biz_account p WHERE p.account_code = '2211' AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;

-- ---------- ④ 菜单 + 权限码 ----------
-- 挂载点实测：财务管理 = 20000000-…-e02（子项 sort 10/20/30/40/45/46/47/50），
-- 仓库管理 = 20000000-…-e01（子项 sort 10/20/30/40/50/60/70）。
-- AGENTS 约定 12：title 2-6 字、同父 sort 唯一（这里取 60/62/64/66/68 与 80）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  ('20000000-0000-4000-8000-00000000c001'::uuid, '成本对象', 2, 'business:erp:cost-object:list',
   '/business/erp/cost-object', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-object/index', 60, 1, 'layers', '成本对象', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c002'::uuid, '工时台账', 2, 'business:erp:cost-labor:list',
   '/business/erp/cost-labor', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-labor/index', 62, 1, 'time', '工时台账', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c003'::uuid, '费用台账', 2, 'business:erp:cost-overhead:list',
   '/business/erp/cost-overhead', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-overhead/index', 64, 1, 'money', '费用台账', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c004'::uuid, '分摊规则', 2, 'business:erp:cost-alloc-rule:list',
   '/business/erp/cost-alloc-rule', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-alloc-rule/index', 66, 1, 'share', '分摊规则', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c005'::uuid, '成本核算', 2, 'business:erp:cost-period:list',
   '/business/erp/cost-period', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-period/index', 68, 1, 'calculator', '成本核算', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c006'::uuid, '批次管理', 2, 'business:erp:lot:list',
   '/business/erp/lot', '20000000-0000-0000-0000-000000000e01'::uuid,
   'business/erp/lot/index', 80, 1, 'barcode', '批次管理', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 按钮权限（menu_type=3；is_visible=FALSE 不进侧边栏）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  ('20000000-0000-4000-8000-00000000c101'::uuid,'business:erp:cost-object:create',3,'business:erp:cost-object:create',NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,10,1,NULL,'新建成本对象',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c102'::uuid,'business:erp:cost-object:update',3,'business:erp:cost-object:update',NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,20,1,NULL,'修改成本对象',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c103'::uuid,'business:erp:cost-object:delete',3,'business:erp:cost-object:delete',NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,30,1,NULL,'删除成本对象',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c104'::uuid,'business:erp:cost-object:close', 3,'business:erp:cost-object:close', NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,40,1,NULL,'关批',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c111'::uuid,'business:erp:cost-labor:create',  3,'business:erp:cost-labor:create', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,10,1,NULL,'新增工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c112'::uuid,'business:erp:cost-labor:update',  3,'business:erp:cost-labor:update', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,20,1,NULL,'修改工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c113'::uuid,'business:erp:cost-labor:delete',  3,'business:erp:cost-labor:delete', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,30,1,NULL,'删除工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c114'::uuid,'business:erp:cost-labor:import',  3,'business:erp:cost-labor:import', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,40,1,NULL,'导入工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c121'::uuid,'business:erp:cost-overhead:create',3,'business:erp:cost-overhead:create',NULL,'20000000-0000-4000-8000-00000000c003'::uuid,NULL,10,1,NULL,'新增费用',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c122'::uuid,'business:erp:cost-overhead:delete',3,'business:erp:cost-overhead:delete',NULL,'20000000-0000-4000-8000-00000000c003'::uuid,NULL,20,1,NULL,'删除费用',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c131'::uuid,'business:erp:cost-alloc-rule:create',3,'business:erp:cost-alloc-rule:create',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,10,1,NULL,'新增规则',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c132'::uuid,'business:erp:cost-alloc-rule:update',3,'business:erp:cost-alloc-rule:update',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,20,1,NULL,'修改规则',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c133'::uuid,'business:erp:cost-alloc-rule:delete',3,'business:erp:cost-alloc-rule:delete',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,30,1,NULL,'删除规则',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c134'::uuid,'business:erp:cost-alloc-rule:preview',3,'business:erp:cost-alloc-rule:preview',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,40,1,NULL,'分摊试算',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c141'::uuid,'business:erp:cost-period:calc',  3,'business:erp:cost-period:calc',  NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,10,1,NULL,'计算成本',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c142'::uuid,'business:erp:cost-period:submit',3,'business:erp:cost-period:submit',NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,20,1,NULL,'提交审批',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c143'::uuid,'business:erp:cost-period:settle',3,'business:erp:cost-period:settle',NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,30,1,NULL,'结算（入库+凭证）',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c144'::uuid,'business:erp:cost-period:reverse',3,'business:erp:cost-period:reverse',NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,40,1,NULL,'红冲结算',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c151'::uuid,'business:erp:lot:create',3,'business:erp:lot:create',NULL,'20000000-0000-4000-8000-00000000c006'::uuid,NULL,10,1,NULL,'新增批次',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c152'::uuid,'business:erp:lot:update',3,'business:erp:lot:update',NULL,'20000000-0000-4000-8000-00000000c006'::uuid,NULL,20,1,NULL,'修改批次',FALSE,'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 单号号段 ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no,
                     step, separator, status, remark)
VALUES
  ('20000000-0000-4000-8000-00000000c201'::uuid, 'COST_OBJECT', '成本对象号', 'CB', 'yyyyMMdd', '', 0, 1, '-', 1,
   '成本对象号：CB-yyyyMMdd-000001，按天（CST）滚动'),
  ('20000000-0000-4000-8000-00000000c202'::uuid, 'COST_PERIOD', '成本核算批次号', 'CS', 'yyyyMM', '', 0, 1, '-', 1,
   '成本核算批次号：CS-yyyyMM-0001，按月滚动（一个期间一张单）')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑥ 字典：费用项目（唯一落字典的成本项）----------
-- 为什么只有它落字典：费用项目随行业扩展（客户会加「模具摊销」「烤版费」），
-- 落字典免改代码；而「成本要素」「分摊标准」是算法分支（代码必须认识才算得出来），
-- 落字典只会制造「配了但算不了」的坑，所以走枚举 + 前端常量。
INSERT INTO sys_dict_type (dict_code, dict_name, description, status, sort_order)
VALUES ('cost_expense_item', '制造费用项目',
        'ARCH9-3 预置：折旧 / 水电 / 租金 / 修理 / 车间管理 / 机物料 / 其他', 1, 110)
ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING;

INSERT INTO sys_dict_item (dict_type_id, item_value, item_label, sort_order, is_default, status)
SELECT t.id, v.item_value, v.item_label, v.sort_order, v.is_default, 1
  FROM sys_dict_type t
  CROSS JOIN (VALUES
        ('DEPRECIATION',   '折旧费',     10, true),
        ('UTILITY',        '水电费',     20, false),
        ('RENT',           '租金',       30, false),
        ('REPAIR',         '修理费',     40, false),
        ('WORKSHOP_ADMIN', '车间管理费', 50, false),
        ('CONSUMABLE',     '机物料消耗', 60, false),
        ('OTHER',          '其他',       70, false)
       ) AS v(item_value, item_label, sort_order, is_default)
 WHERE t.dict_code = 'cost_expense_item' AND t.deleted_at IS NULL
ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO NOTHING;


-- =============================================================================
-- 第四部分：结果形态断言（跑不过就让事务失败 —— 迁移自检同款）
-- =============================================================================
DO $$
DECLARE
    n_tbl       INT;
    n_col       INT;
    n_acc       INT;
    n_leaf_bad  INT;
    n_menu      INT;
    n_btn       INT;
    n_dup_sort  INT;
    n_seq       INT;
    n_dict      INT;
    in_type_def TEXT;
BEGIN
    -- ① 13 张新表齐
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name IN (
        'biz_lot','biz_cost_object','biz_cost_collect','biz_cost_labor','biz_cost_overhead',
        'biz_cost_rate','biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period',
        'biz_cost_period_item','biz_cost_standard','biz_cost_standard_item','biz_cost_variance');
    IF n_tbl <> 13 THEN RAISE EXCEPTION '新表应有 13 张，实际 %', n_tbl; END IF;

    -- ② biz_stock_out.cost_object_id 在、biz_stock_in 两列在
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_name = 'biz_stock_out' AND column_name = 'cost_object_id';
    IF n_col <> 1 THEN RAISE EXCEPTION 'biz_stock_out.cost_object_id 缺失'; END IF;
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_name = 'biz_stock_in' AND column_name IN ('cost_object_id','cost_period_id');
    IF n_col <> 2 THEN RAISE EXCEPTION 'biz_stock_in 成本列应有 2 个，实际 %', n_col; END IF;

    -- ③ ck_biz_si_type 必须含 PRODUCTION（用约束定义文本判，不猜）
    SELECT pg_get_constraintdef(oid) INTO in_type_def FROM pg_constraint
     WHERE conname = 'ck_biz_si_type';
    IF in_type_def IS NULL OR position('PRODUCTION' IN in_type_def) = 0 THEN
        RAISE EXCEPTION 'ck_biz_si_type 未含 PRODUCTION：%', coalesce(in_type_def,'<不存在>');
    END IF;

    -- ④ 科目：4001/4101/2211 非末级；8 个制造费用/生产成本末级 + 2211.01 齐
    SELECT count(*) INTO n_leaf_bad FROM biz_account
     WHERE account_code IN ('4001','4101','2211') AND is_leaf <> 0;
    IF n_leaf_bad > 0 THEN
        RAISE EXCEPTION '4001/4101/2211 应翻为非末级，实际仍有 % 行是末级', n_leaf_bad;
    END IF;

    SELECT count(*) INTO n_acc FROM biz_account
     WHERE deleted_at IS NULL AND account_code IN (
        '4001.01','4001.02','4001.03','4101.01','4101.02','4101.03','4101.04','4101.09','2211.01')
       AND is_leaf = 1 AND level = 2;
    IF n_acc <> 9 THEN RAISE EXCEPTION '应新增 9 个末级科目，实际 %', n_acc; END IF;

    -- ⑤ 菜单：6 个页面 + 20 个按钮；同父 sort 不得重复（AGENTS 约定 12）
    SELECT count(*) INTO n_menu FROM sys_menu
     WHERE id IN (SELECT ('20000000-0000-4000-8000-00000000c00' || g)::uuid FROM generate_series(1,6) g);
    IF n_menu <> 6 THEN RAISE EXCEPTION '成本页面菜单应有 6 个，实际 %', n_menu; END IF;

    SELECT count(*) INTO n_btn FROM sys_menu WHERE parent_id IN (
        SELECT id FROM sys_menu WHERE path IN (
            '/business/erp/cost-object','/business/erp/cost-labor','/business/erp/cost-overhead',
            '/business/erp/cost-alloc-rule','/business/erp/cost-period','/business/erp/lot'));
    IF n_btn <> 20 THEN RAISE EXCEPTION '按钮权限应有 20 个，实际 %', n_btn; END IF;

    SELECT count(*) INTO n_dup_sort FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND menu_type IN (1,2)
           AND parent_id IN ('20000000-0000-0000-0000-000000000e01'::uuid,
                             '20000000-0000-0000-0000-000000000e02'::uuid)
         GROUP BY 1,2 HAVING count(*) > 1) d;
    IF n_dup_sort > 0 THEN RAISE EXCEPTION '同父 sort_order 重复 % 组（侧边栏顺序会抖）', n_dup_sort; END IF;

    -- ⑥ 号段 2 条、字典 1 类 7 项
    SELECT count(*) INTO n_seq FROM biz_seq
     WHERE seq_type IN ('COST_OBJECT','COST_PERIOD') AND deleted_at IS NULL;
    IF n_seq <> 2 THEN RAISE EXCEPTION '号段应有 2 条，实际 %', n_seq; END IF;

    SELECT count(*) INTO n_dict FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE t.dict_code = 'cost_expense_item' AND i.deleted_at IS NULL;
    IF n_dict <> 7 THEN RAISE EXCEPTION '费用项目字典应有 7 项，实际 %', n_dict; END IF;

    RAISE NOTICE '✅ 全部断言通过：13 表 / 4 类改造 / 6 类种子 / 周期唯一键口径';

    -- ⑦ 期间唯一键口径实测：红冲行不占位（能重算），活跃行占位（防并发双单）
    BEGIN
        INSERT INTO biz_cost_period (org_id, period_code, batch_no, status, approval_instance_id,
                                     settled_at, finish_voucher_id, reversed_at, reversed_by, reverse_reason)
        VALUES ('00000000-0000-0000-0000-000000000000', '2099-01', 'CS-209901-0001', 'REVERSED',
                gen_random_uuid(), CURRENT_TIMESTAMP, gen_random_uuid(),
                CURRENT_TIMESTAMP, gen_random_uuid(), '重算验证');
        INSERT INTO biz_cost_period (org_id, period_code, batch_no, status, approval_instance_id,
                                     settled_at, finish_voucher_id, reversed_at, reversed_by, reverse_reason)
        VALUES ('00000000-0000-0000-0000-000000000000', '2099-01', 'CS-209901-0002', 'REVERSED',
                gen_random_uuid(), CURRENT_TIMESTAMP, gen_random_uuid(),
                CURRENT_TIMESTAMP, gen_random_uuid(), '重算验证');
        -- 两张红冲行之后，同期间仍可建一张活跃批次
        INSERT INTO biz_cost_period (org_id, period_code, batch_no, status, approval_instance_id)
        VALUES ('00000000-0000-0000-0000-000000000000', '2099-01', 'CS-209901-0003', 'SUBMITTED',
                gen_random_uuid());
    EXCEPTION WHEN unique_violation THEN
        RAISE EXCEPTION '红冲后的期间不能再建活跃批次（uk_biz_cost_period 口径写错，会造成死单）';
    END;

    BEGIN
        INSERT INTO biz_cost_period (org_id, period_code, batch_no, status, approval_instance_id)
        VALUES ('00000000-0000-0000-0000-000000000000', '2099-01', 'CS-209901-0004', 'SUBMITTED',
                gen_random_uuid());
        RAISE EXCEPTION '同期间出现两张活跃批次却未被唯一键拦住（并发会生成双单）';
    EXCEPTION WHEN unique_violation THEN
        NULL;  -- 期望路径
    END;

    DELETE FROM biz_cost_period WHERE period_code = '2099-01';

    -- ⑧ 归集表归属约束实测：费用池行可以不挂对象；工/料行不挂必须被拒
    BEGIN
        INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty,
                                     workshop_dept_id, workshop_dept_name, start_date)
        VALUES ('cccccccc-0000-4000-8000-000000000001', 'CB-TEST-0001', gen_random_uuid(),
                gen_random_uuid(), 100, gen_random_uuid(), '一车间', CURRENT_DATE);
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type, source_id,
                                  line_no, occur_date, amount, dept_id, dept_name)
    VALUES (NULL, '2099-02', 'OVERHEAD', 'DEPRECIATION', gen_random_uuid(), 0, CURRENT_DATE,
            1000, gen_random_uuid(), '一车间');

    BEGIN
        INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type, source_id,
                                      line_no, occur_date, amount)
        VALUES (NULL, '2099-02', 'LABOR', 'LABOR_TICKET', gen_random_uuid(), 0, CURRENT_DATE, 500);
        RAISE EXCEPTION '工行不挂成本对象却写入成功（费用池约束失效）';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 期望路径
    END;

    DELETE FROM biz_cost_collect WHERE period_code = '2099-02';
    DELETE FROM biz_cost_object WHERE object_no = 'CB-TEST-0001';
END $$;

ROLLBACK;

-- =============================================================================
-- 第五部分：残留断言（ROLLBACK 之后 —— 库里必须什么都没有）
-- =============================================================================
DO $$
DECLARE n INT;
BEGIN
    SELECT count(*) INTO n FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name LIKE 'biz_cost_%';
    IF n <> 0 THEN RAISE EXCEPTION 'ROLLBACK 后仍残留 % 张 biz_cost_* 表（迁移不干净）', n; END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_name = 'biz_stock_out' AND column_name = 'cost_object_id';
    IF n <> 0 THEN RAISE EXCEPTION 'ROLLBACK 后 biz_stock_out.cost_object_id 仍在'; END IF;

    SELECT count(*) INTO n FROM biz_account WHERE account_code = '4001.01';
    IF n <> 0 THEN RAISE EXCEPTION 'ROLLBACK 后科目 4001.01 仍在'; END IF;

    RAISE NOTICE '✅ 零残留：ROLLBACK 干净';
END $$;
